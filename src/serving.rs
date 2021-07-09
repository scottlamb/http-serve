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
use bytes::Buf;
use futures_core::Stream;
use futures_util::stream::{self, StreamExt};
use http::header::{self, HeaderMap, HeaderValue};
use http::{self, Method, Request, Response, StatusCode};
use http_body::Body;
use httpdate::{fmt_http_date, parse_http_date};
use pin_project::pin_project;
use smallvec::SmallVec;
use std::future::Future;
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
    } else if let (Some(ref m), Some(ref since)) =
        (last_modified, req_hdrs.get(header::IF_UNMODIFIED_SINCE))
    {
        const ERR: &str = "Unparseable If-Unmodified-Since";
        *m > parse_http_date(since.to_str().map_err(|_| ERR)?).map_err(|_| ERR)?
    } else {
        false
    };

    let not_modified = if !etag::none_match(&etag, req_hdrs).unwrap_or(true) {
        true
    } else if let (Some(ref m), Some(ref since)) =
        (last_modified, req_hdrs.get(header::IF_MODIFIED_SINCE))
    {
        const ERR: &str = "Unparseable If-Modified-Since";
        *m <= parse_http_date(since.to_str().map_err(|_| ERR)?).map_err(|_| ERR)?
    } else {
        false
    };

    Ok((precondition_failed, not_modified))
}

fn static_body<D, E>(s: &'static str) -> Box<dyn Stream<Item = Result<D, E>> + Send>
where
    D: 'static + Send + Buf + From<Vec<u8>> + From<&'static [u8]>,
    E: 'static + Send,
{
    Box::new(stream::once(futures_util::future::ok(s.as_bytes().into())))
}

fn empty_body<D, E>() -> Box<dyn Stream<Item = Result<D, E>> + Send>
where
    D: 'static + Send + Buf + From<Vec<u8>> + From<&'static [u8]>,
    E: 'static + Send,
{
    Box::new(stream::empty())
}

/// Serves GET and HEAD requests for a given byte-ranged entity.
/// Handles conditional & subrange requests.
/// The caller is expected to have already determined the correct entity and appended
/// `Expires`, `Cache-Control`, and `Vary` headers if desired.
pub fn serve<
    Ent: Entity,
    B: Body + From<Box<dyn Stream<Item = Result<Ent::Data, Ent::Error>> + Send>>,
    BI,
>(
    entity: Ent,
    req: &Request<BI>,
) -> Response<B> {
    // serve takes entity itself for ownership, as needed for the multipart case. But to avoid
    // monomorphization code bloat when there are many implementations of Entity<Data, Error>,
    // delegate as much as possible to functions which take a reference to a trait object.
    match serve_inner(&entity, req) {
        ServeInner::Simple(res) => res,
        ServeInner::Multipart {
            res,
            mut part_headers,
            ranges,
        } => {
            let bodies = stream::unfold(0, move |state| {
                next_multipart_body_chunk(state, &entity, &ranges[..], &mut part_headers[..])
            });
            let body = bodies.flatten();
            let body: Box<dyn Stream<Item = Result<Ent::Data, Ent::Error>> + Send> = Box::new(body);
            res.body(body.into()).unwrap()
        }
    }
}

/// An instruction from `serve_inner` to `serve` on how to respond.
enum ServeInner<B> {
    Simple(Response<B>),
    Multipart {
        res: http::response::Builder,
        part_headers: Vec<Vec<u8>>,
        ranges: SmallVec<[Range<u64>; 1]>,
    },
}

/// Runs trait object-based inner logic for `serve`.
fn serve_inner<
    D: 'static + Send + Sync + Buf + From<Vec<u8>> + From<&'static [u8]>,
    E: 'static + Send + Sync,
    B: Body + From<Box<dyn Stream<Item = Result<D, E>> + Send>>,
    BI,
>(
    ent: &dyn Entity<Error = E, Data = D>,
    req: &Request<BI>,
) -> ServeInner<B> {
    if *req.method() != Method::GET && *req.method() != Method::HEAD {
        return ServeInner::Simple(
            Response::builder()
                .status(StatusCode::METHOD_NOT_ALLOWED)
                .header(header::ALLOW, HeaderValue::from_static("get, head"))
                .body(static_body::<D, E>("This resource only supports GET and HEAD.").into())
                .unwrap(),
        );
    }

    let last_modified = ent.last_modified();
    let etag = ent.etag();

    let (precondition_failed, not_modified) =
        match parse_modified_hdrs(&etag, req.headers(), last_modified) {
            Err(s) => {
                return ServeInner::Simple(
                    Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(static_body::<D, E>(s).into())
                        .unwrap(),
                )
            }
            Ok(p) => p,
        };

    // See RFC 7233 section 4.1 <https://tools.ietf.org/html/rfc7233#section-4.1>: a Partial
    // Content response should include other representation header fields (aka entity-headers in
    // RFC 2616) iff the client didn't specify If-Range.
    let mut range_hdr = req.headers().get(header::RANGE);
    let include_entity_headers_on_range = match req.headers().get(header::IF_RANGE) {
        Some(ref if_range) => {
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
        return ServeInner::Simple(
            res.body(static_body::<D, E>("Precondition failed").into())
                .unwrap(),
        );
    }

    if not_modified {
        res = res.status(StatusCode::NOT_MODIFIED);
        return ServeInner::Simple(res.body(empty_body::<D, E>().into()).unwrap());
    }

    let len = ent.len();
    let (range, include_entity_headers) = match range::parse(range_hdr, len) {
        range::ResolvedRanges::None => (0..len, true),
        range::ResolvedRanges::Satisfiable(ranges) => {
            if ranges.len() == 1 {
                res = res.header(
                    header::CONTENT_RANGE,
                    unsafe_fmt_ascii_val!(
                        MAX_DECIMAL_U64_BYTES * 3 + "bytes -/".len(),
                        "bytes {}-{}/{}",
                        ranges[0].start,
                        ranges[0].end - 1,
                        len
                    ),
                );
                res = res.status(StatusCode::PARTIAL_CONTENT);
                (ranges[0].clone(), include_entity_headers_on_range)
            } else {
                // Before serving multiple ranges via multipart/byteranges, estimate the total
                // length. ("80" is the RFC's estimate of the size of each part's header.) If it's
                // more than simply serving the whole entity, do that instead.
                let est_len: u64 = ranges.iter().map(|r| 80 + r.end - r.start).sum();
                if est_len < len {
                    let (res, part_headers) = prepare_multipart(
                        ent,
                        res,
                        &ranges[..],
                        len,
                        include_entity_headers_on_range,
                    );
                    if *req.method() == Method::HEAD {
                        return ServeInner::Simple(res.body(empty_body::<D, E>().into()).unwrap());
                    }
                    return ServeInner::Multipart {
                        res,
                        part_headers,
                        ranges,
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
            return ServeInner::Simple(res.body(empty_body::<D, E>().into()).unwrap());
        }
    };
    res = res.header(
        header::CONTENT_LENGTH,
        unsafe_fmt_ascii_val!(MAX_DECIMAL_U64_BYTES, "{}", range.end - range.start),
    );
    let body = match *req.method() {
        Method::HEAD => empty_body::<D, E>(),
        _ => ent.get_range(range),
    };
    let mut res = res.body(body.into()).unwrap();
    if include_entity_headers {
        ent.add_headers(res.headers_mut());
    }
    ServeInner::Simple(res)
}

/// A body for use in the "stream of streams" (see `prepare_multipart` and its call site).
/// This avoids an extra allocation for the part headers and overall trailer.
#[pin_project(project=InnerBodyProj)]
enum InnerBody<D, E> {
    Once(Option<D>),

    // The box variant _holds_ a pin but isn't structurally pinned.
    B(Pin<Box<dyn Stream<Item = Result<D, E>> + Sync + Send>>),
}

impl<D, E> Stream for InnerBody<D, E> {
    type Item = Result<D, E>;
    fn poll_next(
        self: Pin<&mut Self>,
        ctx: &mut std::task::Context,
    ) -> std::task::Poll<Option<Result<D, E>>> {
        let mut this = self.project();
        match this {
            InnerBodyProj::Once(ref mut o) => std::task::Poll::Ready(o.take().map(Ok)),
            InnerBodyProj::B(b) => b.as_mut().poll_next(ctx),
        }
    }
}

/// Prepares to send a `multipart/mixed` response.
/// Returns the response builder (with overall headers added) and each part's headers.
fn prepare_multipart<D, E>(
    ent: &dyn Entity<Data = D, Error = E>,
    mut res: http::response::Builder,
    ranges: &[Range<u64>],
    len: u64,
    include_entity_headers: bool,
) -> (http::response::Builder, Vec<Vec<u8>>)
where
    D: 'static + Send + Sync + Buf + From<Vec<u8>> + From<&'static [u8]>,
    E: 'static + Send + Sync,
{
    let mut each_part_headers = Vec::new();
    if include_entity_headers {
        let mut h = http::header::HeaderMap::new();
        ent.add_headers(&mut h);
        each_part_headers.reserve(
            h.iter()
                .map(|(k, v)| k.as_str().len() + v.as_bytes().len() + 4)
                .sum::<usize>()
                + 2,
        );
        for (k, v) in &h {
            each_part_headers.extend_from_slice(k.as_str().as_bytes());
            each_part_headers.extend_from_slice(b": ");
            each_part_headers.extend_from_slice(v.as_bytes());
            each_part_headers.extend_from_slice(b"\r\n");
        }
    }
    each_part_headers.extend_from_slice(b"\r\n");

    let mut body_len = 0;
    let mut part_headers: Vec<Vec<u8>> = Vec::with_capacity(2 * ranges.len() + 1);
    for r in ranges {
        let mut buf = Vec::with_capacity(64 + each_part_headers.len());
        write!(
            &mut buf,
            "\r\n--B\r\nContent-Range: bytes {}-{}/{}\r\n",
            r.start,
            r.end - 1,
            len
        )
        .unwrap();
        buf.extend_from_slice(&each_part_headers);
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

    (res, part_headers)
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
) -> impl Future<Output = Option<(InnerBody<D, E>, usize)>>
where
    D: 'static + Send + Sync + Buf + From<Vec<u8>> + From<&'static [u8]>,
    E: 'static + Send + Sync,
{
    let i = state >> 1;
    let odd = (state & 1) == 1;
    let body = if i == ranges.len() && odd {
        return futures_util::future::ready(None);
    } else if i == ranges.len() {
        InnerBody::Once(Some(PART_TRAILER.into()))
    } else if odd {
        InnerBody::B(Pin::from(ent.get_range(ranges[i].clone())))
    } else {
        let v = std::mem::take(&mut part_headers[i]);
        InnerBody::Once(Some(v.into()))
    };
    futures_util::future::ready(Some((body, state + 1)))
}
