// Copyright (c) 2016-2018 Scott Lamb <slamb@slamb.org>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use futures::{self, Stream};
use futures::stream;
use futures::future;
use http;
use http::header::HeaderValue;
use hyper::{self, Error, Method};
use hyper::header;
use hyper::server::{Request, Response};
use range;
use smallvec::SmallVec;
use super::Entity;
use std::io::Write;
use std::ops::Range;
use std::time::SystemTime;

fn weak_etag_eq(mut a: &[u8], mut b: &[u8]) -> bool {
    if a.starts_with(b"W/") {
        a = &a[2..];
    }
    if b.starts_with(b"W/") {
        b = &b[2..];
    }
    a == b
}

fn strong_etag_eq(a: &[u8], b: &[u8]) -> bool {
    a == b && !a.starts_with(b"W/")
}

/// Returns true if `req` doesn't have an `If-None-Match` header matching `req`.
fn none_match(etag: &Option<HeaderValue>, req: &Request) -> bool {
    match req.headers().get::<header::IfNoneMatch>() {
        Some(&header::IfNoneMatch::Any) => false,
        Some(&header::IfNoneMatch::Items(ref items)) => {
            if let Some(ref some_etag) = *etag {
                for item in items {
                    // RFC 7232 section 3.2: A recipient MUST use the weak comparison function when
                    // comparing entity-tags for If-None-Match
                    if weak_etag_eq(item.to_string().as_bytes(), some_etag.as_bytes()) {
                        return false;
                    }
                }
            }
            true
        }
        None => true,
    }
}

/// Returns true if `req` has no `If-Match` header or one which matches `etag`.
fn any_match(etag: &Option<HeaderValue>, req: &Request) -> bool {
    match req.headers().get::<header::IfMatch>() {
        // The absent header and "If-Match: *" cases differ only when there is no entity to serve.
        // We always have an entity to serve, so consider them identical.
        None | Some(&header::IfMatch::Any) => true,
        Some(&header::IfMatch::Items(ref items)) => {
            if let Some(ref some_etag) = *etag {
                for item in items {
                    if strong_etag_eq(item.to_string().as_bytes(), some_etag.as_bytes()) {
                        return true;
                    }
                }
            }
            false
        }
    }
}

/// Serves GET and HEAD requests for a given byte-ranged entity.
/// Handles conditional & subrange requests.
/// The caller is expected to have already determined the correct entity and appended
/// `Expires`, `Cache-Control`, and `Vary` headers if desired.
pub fn serve<E: Entity>(e: E, req: &Request) -> Response<E::Body> {
    if *req.method() != Method::Get && *req.method() != Method::Head {
        let body: Box<Stream<Item = E::Chunk, Error = Error> + Send> = Box::new(stream::once(Ok(
            b"This resource only supports GET and HEAD."[..].into(),
        )));
        return Response::new()
            .with_status(hyper::StatusCode::MethodNotAllowed)
            .with_header(header::Allow(vec![Method::Get, Method::Head]))
            .with_body(body);
    }

    let last_modified = e.last_modified();
    let etag = e.etag();

    let precondition_failed = if !any_match(&etag, req) {
        true
    } else if let (Some(ref m), Some(&header::IfUnmodifiedSince(ref since))) =
        (last_modified, req.headers().get())
    {
        *m > (*since).into()
    } else {
        false
    };

    let not_modified = if !none_match(&etag, req) {
        true
    } else if let (Some(ref m), Some(&header::IfModifiedSince(ref since))) =
        (last_modified, req.headers().get())
    {
        *m <= (*since).into()
    } else {
        false
    };

    // See RFC 7233 section 3.3 <https://tools.ietf.org/html/rfc7233#section-3.2>: a Partial
    // Content response should include certain entity-headers or not based on the If-Range
    // response.
    let mut range_hdr = req.headers().get::<header::Range>();
    let include_entity_headers_on_range = match req.headers().get::<header::IfRange>() {
        Some(&header::IfRange::EntityTag(ref if_etag)) => {
            if let Some(ref some_etag) = etag {
                if strong_etag_eq(if_etag.to_string().as_bytes(), some_etag.as_bytes()) {
                    false
                } else {
                    range_hdr = None;
                    true
                }
            } else {
                range_hdr = None;
                true
            }
        }
        Some(&header::IfRange::Date(_)) => {
            // Use the strong validation rules for an origin server:
            // <https://tools.ietf.org/html/rfc7232#section-2.2.2>.
            // The resource could have changed twice in the supplied second, so never match.
            range_hdr = None;
            true
        }
        None => true,
    };

    let mut res = Response::new();
    res.headers_mut()
        .set(header::AcceptRanges(vec![header::RangeUnit::Bytes]));
    if let Some(m) = last_modified {
        // See RFC 7232 section 2.2.1 <https://tools.ietf.org/html/rfc7232#section-2.2.1>: the
        // Last-Modified must not exceed the Date. To guarantee this, set the Date now (if one
        // hasn't already been set) rather than let hyper set it.
        let d = if let Some(&header::Date(d)) = res.headers().get() {
            d
        } else {
            let d = SystemTime::now().into();
            res.headers_mut().set(header::Date(d));
            d
        };
        res.headers_mut()
            .set(header::LastModified(::std::cmp::min(m.into(), d)));
    }
    if let Some(e) = etag {
        res.headers_mut()
            .set_raw(http::header::ETAG.as_str(), e.as_bytes());
    }

    if precondition_failed {
        res.set_status(hyper::StatusCode::PreconditionFailed);
        let body: Box<Stream<Item = E::Chunk, Error = Error> + Send> =
            Box::new(stream::once(Ok(b"Precondition failed"[..].into())));
        return res.with_body(body);
    }

    if not_modified {
        res.set_status(hyper::StatusCode::NotModified);
        return res;
    }

    let len = e.len();
    let (range, include_entity_headers) = match range::parse(range_hdr, len) {
        range::ResolvedRanges::None => (0..len, true),
        range::ResolvedRanges::Satisfiable(rs) => {
            if rs.len() == 1 {
                res.headers_mut()
                    .set(header::ContentRange(header::ContentRangeSpec::Bytes {
                        range: Some((rs[0].start, rs[0].end - 1)),
                        instance_length: Some(len),
                    }));
                res.set_status(hyper::StatusCode::PartialContent);
                (rs[0].clone(), include_entity_headers_on_range)
            } else {
                // Before serving multiple ranges via multipart/byteranges, estimate the total
                // length. ("80" is the RFC's estimate of the size of each part's header.) If it's
                // more than simply serving the whole entity, do that instead.
                let est_len: u64 = rs.iter().map(|r| 80 + r.end - r.start).sum();
                if est_len < len {
                    return send_multipart(e, req, res, rs, len, include_entity_headers_on_range);
                }

                (0..len, true)
            }
        }
        range::ResolvedRanges::NotSatisfiable => {
            res.headers_mut()
                .set(header::ContentRange(header::ContentRangeSpec::Bytes {
                    range: None,
                    instance_length: Some(len),
                }));
            res.set_status(hyper::StatusCode::RangeNotSatisfiable);
            return res;
        }
    };
    if include_entity_headers {
        let mut headers = http::header::HeaderMap::new();
        e.add_headers(&mut headers);
        let hyper_headers: hyper::header::Headers = headers.into();
        res.headers_mut().extend(hyper_headers.iter());
    }
    res.headers_mut()
        .set(header::ContentLength(range.end - range.start));
    if *req.method() == Method::Head {
        return res;
    }

    res.with_body(e.get_range(range))
}

enum InnerBody<B, C> {
    Once(Option<C>),
    B(B),
}

impl<B, C> Stream for InnerBody<B, C>
where
    B: Stream<Item = C, Error = Error>,
{
    type Item = C;
    type Error = Error;
    fn poll(&mut self) -> ::futures::Poll<Option<C>, Error> {
        match *self {
            InnerBody::Once(ref mut o) => Ok(futures::Async::Ready(o.take())),
            InnerBody::B(ref mut b) => b.poll(),
        }
    }
}

fn send_multipart<E: Entity>(
    e: E,
    req: &Request,
    mut res: Response<E::Body>,
    rs: SmallVec<[Range<u64>; 1]>,
    len: u64,
    include_entity_headers: bool,
) -> Response<E::Body> {
    let mut body_len = 0;
    let mut each_part_headers = Vec::with_capacity(128);
    if include_entity_headers {
        let mut headers = http::header::HeaderMap::new();
        e.add_headers(&mut headers);
        let hyper_headers: hyper::header::Headers = headers.into();
        write!(&mut each_part_headers, "{}", &hyper_headers).unwrap();
    }
    each_part_headers.extend_from_slice(b"\r\n");

    let mut part_headers: Vec<Vec<u8>> = Vec::with_capacity(2 * rs.len() + 1);
    for r in &rs {
        let mut buf = Vec::with_capacity(64 + each_part_headers.len());
        write!(
            &mut buf,
            "\r\n--B\r\nContent-Range: bytes {}-{}/{}\r\n",
            r.start,
            r.end - 1,
            len
        ).unwrap();
        buf.extend_from_slice(&each_part_headers);
        body_len += buf.len() as u64 + r.end - r.start;
        part_headers.push(buf);
    }
    const TRAILER: &[u8] = b"\r\n--B--\r\n";
    body_len += TRAILER.len() as u64;

    res.headers_mut().set(header::ContentLength(body_len));
    res.headers_mut().set_raw(
        "Content-Type",
        vec![b"multipart/byteranges; boundary=B".to_vec()],
    );
    res.set_status(hyper::StatusCode::PartialContent);

    if *req.method() == Method::Head {
        return res;
    }

    // Create bodies, a stream of E::Body values as follows: each part's header and body
    // (the latter produced lazily), then the overall trailer.
    let bodies = ::futures::stream::unfold(0, move |state| {
        let i = state >> 1;
        let odd = (state & 1) == 1;
        let body = if i == rs.len() && odd {
            return None;
        } else if i == rs.len() {
            InnerBody::Once(Some(TRAILER.into()))
        } else if odd {
            InnerBody::B(e.get_range(rs[i].clone()))
        } else {
            let v = ::std::mem::replace(&mut part_headers[i], Vec::new());
            InnerBody::Once(Some(v.into()))
        };
        Some(future::ok::<_, Error>((body, state + 1)))
    });

    let body: Box<Stream<Item = E::Chunk, Error = Error> + Send> = Box::new(bodies.flatten());
    res.set_body(body);
    res
}
