// Copyright (c) 2016-2018 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use super::Entity;
use crate::etag;
use crate::range;
use crate::BoxError;
use bytes::Buf;
use futures_core::Stream;
use futures_util::stream::{self, StreamExt};
use http::header::{self, HeaderMap, HeaderValue};
use http::{self, Method, Request, Response, StatusCode};
use http_body::Body as HttpBody;
use httpdate::{fmt_http_date, parse_http_date};
use pin_project::pin_project;
use std::io::Write;
use std::ops::Range;
use std::pin::Pin;
use std::time::SystemTime;

const MAX_DECIMAL_U64_BYTES: usize = 20; // u64::max_value().to_string().len()

fn parse_modified_hdrs(
    etag: &Option<HeaderValue>,
    req_hdrs: &HeaderMap,
    last_modified: Option<SystemTime>,
) -> Result<(bool, bool), &'static str> {
    let precondition_failed = if !etag::any_match(etag, req_hdrs)? {
        true
    } else if let (Some(ref m), Some(since)) =
        (last_modified, req_hdrs.get(header::IF_UNMODIFIED_SINCE))
    {
        const ERR: &str = "Unparseable If-Unmodified-Since";
        *m > parse_http_date(since.to_str().map_err(|_| ERR)?).map_err(|_| ERR)?
    } else {
        false
    };

    let not_modified = match etag::none_match(etag, req_hdrs) {
        // See RFC 7233 section 14.26 <https://tools.ietf.org/html/rfc7233#section-14.26>:
        // "If none of the entity tags match, then the server MAY perform the
        // requested method as if the If-None-Match header field did not exist,
        // but MUST also ignore any If-Modified-Since header field(s) in the
        // request. That is, if no entity tags match, then the server MUST NOT
        // return a 304 (Not Modified) response."
        Some(true) => false,

        Some(false) => true,

        None => {
            if let (Some(ref m), Some(since)) =
                (last_modified, req_hdrs.get(header::IF_MODIFIED_SINCE))
            {
                const ERR: &str = "Unparseable If-Modified-Since";
                *m <= parse_http_date(since.to_str().map_err(|_| ERR)?).map_err(|_| ERR)?
            } else {
                false
            }
        }
    };
    Ok((precondition_failed, not_modified))
}

/// A [`http_body::Body`] implementation returned by [`serve`].
#[pin_project]
pub struct Body<D = bytes::Bytes, E = BoxError>(InnerBody<D, E>);

impl<D, E> Body<D, E> {
    pub fn empty() -> Self {
        Self(InnerBody::Once(None))
    }
}

impl<D: From<&'static [u8]>, E> From<&'static str> for Body<D, E> {
    fn from(value: &'static str) -> Self {
        Self(InnerBody::Once(Some(value.as_bytes().into())))
    }
}

impl<D: From<Vec<u8>>, E> From<String> for Body<D, E> {
    fn from(value: String) -> Self {
        Self(InnerBody::Once(Some(value.into_bytes().into())))
    }
}

impl<D, E> Stream for Body<D, E>
where
    D: Buf,
{
    type Item = Result<D, E>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let b: &mut InnerBody<D, E> = self.project().0;
        std::task::Poll::Ready(match std::mem::replace(b, InnerBody::Once(None)) {
            InnerBody::Once(mut o) => o.take().map(Ok),
            InnerBody::B {
                remaining,
                mut stream,
            } => match std::task::ready!(stream.as_mut().poll_next(cx)) {
                None => {
                    assert_eq!(remaining, 0);
                    None
                }

                e @ Some(Err(_)) => e,

                Some(Ok(d)) => {
                    let d_remaining = d.remaining();
                    match remaining.checked_sub(d_remaining as u64) {
                        None => {
                            panic!(
                                "body stream returned {} bytes; expected only {}",
                                d_remaining, remaining
                            );
                        }
                        Some(0) => Some(Ok(d)),
                        Some(n) => {
                            *b = InnerBody::B {
                                remaining: n,
                                stream,
                            };
                            Some(Ok(d))
                        }
                    }
                }
            },
        })
    }
}

impl<D, E> HttpBody for Body<D, E>
where
    D: Buf,
{
    type Data = D;
    type Error = E;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        self.poll_next(cx).map_ok(http_body::Frame::data)
    }

    #[inline]
    fn is_end_stream(&self) -> bool {
        matches!(self.0, InnerBody::Once(None))
    }
}

/// Serves GET and HEAD requests for a given byte-ranged entity.
/// Handles conditional & subrange requests.
/// The caller is expected to have already determined the correct entity and appended
/// `Expires`, `Cache-Control`, and `Vary` headers if desired.
pub fn serve<Ent: Entity, BI>(
    entity: Ent,
    req: &Request<BI>,
) -> Response<Body<Ent::Data, Ent::Error>> {
    // serve takes entity itself for ownership, as needed for the multipart case. But to avoid
    // monomorphization code bloat when there are many implementations of Entity<Data, Error>,
    // delegate as much as possible to functions which take a reference to a trait object.
    match serve_inner(&entity, req.method(), req.headers()) {
        ServeInner::Simple(res) => res,
        ServeInner::Multipart {
            res,
            mut part_headers,
            ranges,
            len,
        } => {
            let bodies = stream::unfold(0, move |state| {
                std::future::ready(next_multipart_body_chunk(
                    state,
                    &entity,
                    &ranges[..],
                    &mut part_headers[..],
                ))
            });
            res.body(Body(InnerBody::B {
                remaining: len,
                stream: Box::pin(bodies.flatten()),
            }))
            .unwrap()
        }
    }
}

/// An instruction from `serve_inner` to `serve` on how to respond.
enum ServeInner<D, E> {
    Simple(Response<Body<D, E>>),
    Multipart {
        res: http::response::Builder,
        part_headers: Vec<Vec<u8>>,
        ranges: Vec<Range<u64>>,
        len: u64,
    },
}

/// Runs trait object-based inner logic for `serve`.
fn serve_inner<
    D: 'static + Send + Sync + Buf + From<Vec<u8>> + From<&'static [u8]>,
    E: 'static + Send + Sync,
>(
    ent: &dyn Entity<Error = E, Data = D>,
    method: &http::Method,
    req_hdrs: &http::HeaderMap,
) -> ServeInner<D, E> {
    if method != Method::GET && method != Method::HEAD {
        return ServeInner::Simple(
            Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .header(header::ALLOW, HeaderValue::from_static("get, head"))
                .body(Body::from("This resource only supports GET and HEAD."))
                .unwrap(),
        );
    }

    let last_modified = ent.last_modified();
    let etag = ent.etag();

    let (precondition_failed, not_modified) =
        match parse_modified_hdrs(&etag, req_hdrs, last_modified) {
            Err(s) => {
                return ServeInner::Simple(
                    Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(Body::from(s))
                        .unwrap(),
                )
            }
            Ok(p) => p,
        };

    // See RFC 7233 section 4.1 <https://tools.ietf.org/html/rfc7233#section-4.1>: a Partial
    // Content response should include other representation header fields (aka entity-headers in
    // RFC 2616) iff the client didn't specify If-Range.
    let mut range_hdr = req_hdrs.get(header::RANGE);
    let include_entity_headers_on_range = match req_hdrs.get(header::IF_RANGE) {
        Some(if_range) => {
            let if_range = if_range.as_bytes();
            if if_range.starts_with(b"W/\"") || if_range.starts_with(b"\"") {
                // etag case.
                if let Some(ref some_etag) = etag {
                    if etag::strong_eq(if_range, some_etag.as_bytes()) {
                        false
                    } else {
                        range_hdr = None;
                        true
                    }
                } else {
                    range_hdr = None;
                    true
                }
            } else {
                // Date case.
                // Use the strong validation rules for an origin server:
                // <https://tools.ietf.org/html/rfc7232#section-2.2.2>.
                // The resource could have changed twice in the supplied second, so never match.
                range_hdr = None;
                true
            }
        }
        None => true,
    };

    let mut res =
        Response::builder().header(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    if let Some(m) = last_modified {
        // See RFC 7232 section 2.2.1 <https://tools.ietf.org/html/rfc7232#section-2.2.1>: the
        // Last-Modified must not exceed the Date. To guarantee this, set the Date now rather than
        // let hyper set it.
        let d = SystemTime::now();
        res = res.header(header::DATE, fmt_http_date(d));
        let clamped_m = std::cmp::min(m, d);
        res = res.header(header::LAST_MODIFIED, fmt_http_date(clamped_m));
    }
    if let Some(e) = etag {
        res = res.header(http::header::ETAG, e);
    }

    if precondition_failed {
        res = res.status(StatusCode::PRECONDITION_FAILED);
        return ServeInner::Simple(res.body(Body::from("Precondition failed")).unwrap());
    }

    if not_modified {
        res = res.status(StatusCode::NOT_MODIFIED);
        return ServeInner::Simple(res.body(Body::empty()).unwrap());
    }

    let len = ent.len();
    let (range, include_entity_headers) = match range::parse(range_hdr, len) {
        range::ResolvedRanges::None => (0..len, true),
        range::ResolvedRanges::Satisfiable(ranges) => {
            if let [range] = &ranges[..] {
                res = res.header(
                    header::CONTENT_RANGE,
                    unsafe_fmt_ascii_val!(
                        MAX_DECIMAL_U64_BYTES * 3 + "bytes -/".len(),
                        "bytes {}-{}/{}",
                        range.start,
                        range.end - 1,
                        len
                    ),
                );
                res = res.status(StatusCode::PARTIAL_CONTENT);
                (range.clone(), include_entity_headers_on_range)
            } else {
                let ranges = ranges.into_vec();

                // Before serving multiple ranges via multipart/byteranges, estimate the total
                // length. ("80" is the RFC's estimate of the size of each part's header.) If it's
                // more than simply serving the whole entity, do that instead.
                let est_len: u64 = ranges.iter().map(|r| 80 + r.end - r.start).sum();
                if est_len < len {
                    let (res, part_headers, len) = prepare_multipart(
                        res,
                        &ranges[..],
                        len,
                        include_entity_headers_on_range.then(|| {
                            let mut h = HeaderMap::new();
                            ent.add_headers(&mut h);
                            h
                        }),
                    );
                    if method == Method::HEAD {
                        return ServeInner::Simple(res.body(Body::empty()).unwrap());
                    }
                    return ServeInner::Multipart {
                        res,
                        part_headers,
                        ranges,
                        len,
                    };
                }

                (0..len, true)
            }
        }
        range::ResolvedRanges::NotSatisfiable => {
            res = res.header(
                http::header::CONTENT_RANGE,
                unsafe_fmt_ascii_val!(MAX_DECIMAL_U64_BYTES + "bytes */".len(), "bytes */{}", len),
            );
            res = res.status(StatusCode::RANGE_NOT_SATISFIABLE);
            return ServeInner::Simple(res.body(Body::empty()).unwrap());
        }
    };
    let len = range.end - range.start;
    res = res.header(
        header::CONTENT_LENGTH,
        unsafe_fmt_ascii_val!(MAX_DECIMAL_U64_BYTES, "{}", len),
    );
    let body = match *method {
        Method::HEAD => Body::empty(),
        _ => Body(InnerBody::B {
            remaining: range.end - range.start,
            stream: ent.get_range(range).into(),
        }),
    };
    let mut res = res.body(body).unwrap();
    if include_entity_headers {
        ent.add_headers(res.headers_mut());
    }
    ServeInner::Simple(res)
}

/// A body for use in the "stream of streams" (see `prepare_multipart` and its call site).
/// This avoids an extra allocation for the part headers and overall trailer.
enum InnerBody<D, E> {
    Once(Option<D>),

    // The box variant _holds_ a pin but isn't structurally pinned.
    B {
        remaining: u64,
        stream: Pin<Box<dyn Stream<Item = Result<D, E>> + Sync + Send>>,
    },
}

/// Prepares to send a `multipart/mixed` response.
/// Returns the response builder (with overall headers added), each part's headers, and overall len.
fn prepare_multipart(
    mut res: http::response::Builder,
    ranges: &[Range<u64>],
    len: u64,
    include_entity_headers: Option<http::header::HeaderMap>,
) -> (http::response::Builder, Vec<Vec<u8>>, u64) {
    let mut each_part_headers = Vec::new();
    if let Some(h) = include_entity_headers {
        each_part_headers.reserve(
            h.iter()
                .map(|(k, v)| k.as_str().len() + v.as_bytes().len() + 4)
                .sum::<usize>(),
        );
        for (k, v) in &h {
            each_part_headers.extend_from_slice(k.as_str().as_bytes());
            each_part_headers.extend_from_slice(b": ");
            each_part_headers.extend_from_slice(v.as_bytes());
            each_part_headers.extend_from_slice(b"\r\n");
        }
    }

    let mut body_len = 0;
    let mut part_headers: Vec<Vec<u8>> = Vec::with_capacity(ranges.len());
    for r in ranges {
        let mut buf = Vec::with_capacity(
            "\r\n--B\r\nContent-Range: bytes -/\r\n".len()
                + 3 * MAX_DECIMAL_U64_BYTES
                + each_part_headers.len()
                + "\r\n".len(),
        );
        write!(
            &mut buf,
            "\r\n--B\r\nContent-Range: bytes {}-{}/{}\r\n",
            r.start,
            r.end - 1,
            len
        )
        .unwrap();
        buf.extend_from_slice(&each_part_headers);
        buf.extend_from_slice(b"\r\n");
        body_len += buf.len() as u64 + r.end - r.start;
        part_headers.push(buf);
    }
    body_len += PART_TRAILER.len() as u64;

    res = res.header(
        header::CONTENT_LENGTH,
        unsafe_fmt_ascii_val!(MAX_DECIMAL_U64_BYTES, "{}", body_len),
    );
    res = res.header(
        header::CONTENT_TYPE,
        HeaderValue::from_static("multipart/byteranges; boundary=B"),
    );
    res = res.status(StatusCode::PARTIAL_CONTENT);

    (res, part_headers, body_len)
}

/// The trailer after all `multipart/byteranges` body parts.
const PART_TRAILER: &[u8] = b"\r\n--B--\r\n";

/// Produces a single chunk of the body and the following state, for use in an `unfold` call.
///
/// Alternates between portions of `part_headers` and their corresponding bodies, then the overall
/// trailer, then end the stream.
fn next_multipart_body_chunk<D, E>(
    state: usize,
    ent: &dyn Entity<Data = D, Error = E>,
    ranges: &[Range<u64>],
    part_headers: &mut [Vec<u8>],
) -> Option<(Body<D, E>, usize)>
where
    D: 'static + Send + Sync + Buf + From<Vec<u8>> + From<&'static [u8]>,
    E: 'static + Send + Sync,
{
    let i = state >> 1;
    let odd = (state & 1) == 1;
    let body = if i == ranges.len() && odd {
        return None;
    } else if i == ranges.len() {
        InnerBody::Once(Some(PART_TRAILER.into()))
    } else if odd {
        let r = &ranges[i];
        InnerBody::B {
            remaining: r.end - r.start,
            stream: Pin::from(ent.get_range(r.clone())),
        }
    } else {
        let v = std::mem::take(&mut part_headers[i]);
        InnerBody::Once(Some(v.into()))
    };
    Some((Body(body), state + 1))
}
