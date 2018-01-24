// Copyright (c) 2016-2018 Scott Lamb <slamb@slamb.org>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Helpers for serving HTTP GET and HEAD responses with [hyper](https://crates.io/crates/hyper)
//! 0.11.x and [tokio](https://crates.io/crates/tokio). A future version is likely to switch to
//! the interface of the [http](http://crates.io/crates/http) crate.
//!
//! This crate supplies two ways to respond to HTTP GET and HEAD requests:
//!
//! *   the `serve` function can be used to serve an `Entity`, a trait representing reusable,
//!     byte-rangeable HTTP entities. `Entity` must be able to produce exactly the same data on
//!     every call, know its size in advance, and be able to produce portions of the data on demand.
//! *   the `streaming_body` function can be used to add a body to an otherwise-complete response.
//!     If a body is needed, it returns a `BodyWriter` (which implements `std::io::Writer`). The
//!     caller should produce the complete body or call `BodyWriter::abort`, causing the HTTP
//!     stream to terminate abruptly.
//!
//! # Why two ways?
//!
//! They have pros and cons. This chart shows some of them:
//!
//! <table>
//!   <tr><th><th><code>serve</code><th><code>streaming_body</code></tr>
//!   <tr><td>automatic byte range serving<td>yes<td>no (always sends full body)</tr>
//!   <tr><td>backpressure<td>yes<td>no</tr>
//!   <tr><td>conditional GET<td>yes<td>unimplemented (always sends body)</tr>
//!   <tr><td>sends first byte before length known<td>no<td>yes</tr>
//!   <tr><td>automatic gzip content encoding<td>no<td>yes</tr>
//! </table>
//!
//! Use `serve` when:
//!
//! *   metadata (length, etag, etc) and byte ranges can be regenerated cheaply and consistently
//!     via a lazy `Entity`.
//! *   data can be fully buffered in memory or on disk and reused many times. You may want to
//!     create a pair of buffers for gzipped (for user-agents which specify `Accept-Encoding:
//!     gzip`) vs raw.
//!
//! Consider `streaming_body` if data would be fully buffered each time a response is sent.
//!
//! Once you return a `hyper::server::Response` to hyper, your only way to signal error to the
//! client is to abruptly close the HTTP connection while sending the body. If you want the ability
//! to return a well-formatted error to the client while producing body bytes, you must buffer the
//! entire body in-memory before returning anything to hyper.
//!
//! If you are buffering a response in memory, `serve` requires copying the bytes (when using
//! `Chunk = Vec<u8>` or similar) or atomic reference-counting (with `Chunk = Arc<Vec<u8>>` or
//! similar). `streaming_body` doesn't need to keep its own copy for potential future use; it may
//! be cheaper because it can simply hand ownership of the existing `Vec<u8>`s to hyper.
//!
//! # Why the weird type bounds? Why not use `hyper::Body` and `hyper::Chunk` for everything?
//!
//! `hyper::Chunk` is fine in most cases. There are times when it's desirable to have more flexible
//! ownership provided by a type such as `reffers::ARefs<'static, [u8]>`. One is `mmap`-based
//! file serving: a `hyper::Chunk` would require copying the data in each chunk. An implementation
//! with `ARefs` could instead `mmap` and `mlock` the data on another thread and provide chunks
//! which `munmap` when dropped.
//!
//! `hyper::Body` unfortunately can't be used with either `serve` or `streaming_body`; they need a
//! `Box<::futures::stream::Stream<Self::Chunk, ::hyper::Error> + Send + 'static>` or something
//! that can be constructed from that type.

extern crate flate2;
extern crate futures;
extern crate futures_cpupool;
extern crate hyper;
extern crate mime;
extern crate smallvec;
extern crate time;
extern crate unicase;

use futures::Stream;
use hyper::Error;
use hyper::header;
use hyper::server::{Request, Response};
use std::ops::Range;

mod chunker;
mod file;
mod gzip;
mod serving;

pub use file::ChunkedReadFile;
pub use gzip::BodyWriter;
pub use serving::serve;

/// A reusable, read-only, byte-rangeable HTTP entity for GET and HEAD serving.
/// Must return exactly the same data on every call.
pub trait Entity: 'static + Send {
    /// The type of a chunk.
    ///
    /// Commonly `::hyper::Chunk` or `Vec<u8>` but may be something more exotic such as
    /// `::reffers::ARefs<'static, [u8]>` to minimize copying.
    type Chunk: 'static + Send + AsRef<[u8]> + From<Vec<u8>> + From<&'static [u8]>;

    /// The type of the body stream. Commonly
    /// `Box<::futures::stream::Stream<Self::Chunk, ::hyper::Error> + Send + 'static>`.
    type Body: 'static
        + Send
        + Stream<Item = Self::Chunk, Error = Error>
        + From<Box<Stream<Item = Self::Chunk, Error = Error> + Send>>;

    /// Returns the length of the entity's body in bytes.
    fn len(&self) -> u64;

    /// Returns true iff the entity's body has length 0.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Gets the body bytes indicated by `range`.
    fn get_range(&self, range: Range<u64>) -> Self::Body;

    /// Adds entity headers such as `Content-Type` to the supplied `Headers` object.
    /// In particular, these headers are the "other representation header fields" described by [RFC
    /// 7233 section 4.1](https://tools.ietf.org/html/rfc7233#section-4.1); they should exclude
    /// `Content-Range`, `Date`, `Cache-Control`, `ETag`, `Expires`, `Content-Location`, and `Vary`.
    ///
    /// This function will be called only when that section says that headers such as
    /// `Content-Type` should be included in the response.
    fn add_headers(&self, &mut header::Headers);

    /// Returns an etag for this entity, if available.
    /// Implementations are encouraged to provide a strong etag. [RFC 7232 section
    /// 2.1](https://tools.ietf.org/html/rfc7232#section-2.1) notes that only strong etags
    /// are usable for sub-range retrieval.
    fn etag(&self) -> Option<header::EntityTag>;

    /// Returns the last modified time of this entity, if available.
    /// Note that `serve` may serve an earlier `Last-Modified:` date than the one returned here if
    /// this time is in the future, as required by [RFC 7232 section
    /// 2.2.1](https://tools.ietf.org/html/rfc7232#section-2.2.1).
    fn last_modified(&self) -> Option<header::HttpDate>;
}

/// Returns iff it's preferable to use `Content-Encoding: gzip` when responding to the given
/// request, rather than no content coding.
///
/// Use via `should_gzip(req.headers().get())`.
///
/// Follows the rules of [RFC 7231 section
/// 5.3.4](https://tools.ietf.org/html/rfc7231#section-5.3.4).
pub fn should_gzip(ae: Option<&header::AcceptEncoding>) -> bool {
    let qis = match ae {
        None => return false,
        Some(&header::AcceptEncoding(ref qis)) => qis,
    };
    let (mut gzip_q, mut identity_q, mut star_q) = (None, None, None);
    for qi in qis {
        match qi.item {
            header::Encoding::Gzip => {
                gzip_q = Some(qi.quality);
            }
            header::Encoding::Identity => {
                identity_q = Some(qi.quality);
            }
            header::Encoding::EncodingExt(ref e) if e == "*" => {
                star_q = Some(qi.quality);
            }
            _ => {}
        };
    }

    let gzip_q = gzip_q.or(star_q).unwrap_or(header::q(0));

    // "If the representation has no content-coding, then it is
    // acceptable by default unless specifically excluded by the
    // Accept-Encoding field stating either "identity;q=0" or "*;q=0"
    // without a more specific entry for "identity"."
    let identity_q = identity_q.or(star_q).unwrap_or(header::q(1));

    gzip_q > header::q(0) && gzip_q >= identity_q
}

pub struct StreamingBodyBuilder<'r, Chunk: 'static> {
    resp: &'r mut Response<Box<Stream<Item = Chunk, Error = Error> + Send + 'static>>,
    chunk_size: usize,
    gzip_level: u32,
    body_needed: bool,
}

/// Adds a streaming body to the given request if a body is needed.
///
/// Currently the body is added for non-HEAD requests. In the future, this may also follow
/// conditional GET rules, omitting the body and stripping out entity headers from the response as
/// desired.
pub fn streaming_body<'r, Chunk>(
    req: &Request,
    resp: &'r mut Response<Box<Stream<Item = Chunk, Error = Error> + Send + 'static>>,
) -> StreamingBodyBuilder<'r, Chunk>
where
    Chunk: From<Vec<u8>> + Send + 'static,
{
    StreamingBodyBuilder {
        resp,
        chunk_size: 4096,
        gzip_level: match should_gzip(req.headers().get()) {
            true => 6,
            false => 0,
        },
        body_needed: *req.method() != hyper::Method::Head,
    }
}

impl<'r, Chunk> StreamingBodyBuilder<'r, Chunk>
where
    Chunk: From<Vec<u8>> + Send + 'static,
{
    pub fn with_chunk_size(self, chunk_size: usize) -> Self {
        StreamingBodyBuilder { chunk_size, ..self }
    }

    pub fn with_gzip_level(self, level: u32) -> Self {
        StreamingBodyBuilder {
            gzip_level: if self.gzip_level == 0 { 0 } else { level },
            ..self
        }
    }

    pub fn build(self) -> Option<BodyWriter<Chunk>> {
        // Ensure there's a "Vary: accept-encoding" header. Add this item to the list if the
        // header is already present; add the header if absent.
        let add_hdr = match self.resp.headers_mut().get_mut() {
            Some(&mut header::Vary::Items(ref mut i)) => {
                i.push(::unicase::Ascii::new("accept-encoding".to_owned()));
                false
            }
            Some(&mut header::Vary::Any) => false,
            None => true,
        };
        if add_hdr {
            self.resp.headers_mut().set(header::Vary::Items(vec![
                ::unicase::Ascii::new("accept-encoding".to_owned()),
            ]));
        }

        if self.gzip_level > 0 {
            self.resp
                .headers_mut()
                .set(header::ContentEncoding(vec![header::Encoding::Gzip]));
        }

        if !self.body_needed {
            return None;
        }

        let (bw, body) = match self.gzip_level > 0 {
            true => BodyWriter::gzipped(self.chunk_size, flate2::Compression::new(self.gzip_level)),
            false => BodyWriter::raw(self.chunk_size),
        };
        self.resp.set_body(body);
        Some(bw)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn should_gzip() {
        use hyper::header::{q, qitem, AcceptEncoding, Encoding, QualityItem};

        // "A request without an Accept-Encoding header field implies that the
        // user agent has no preferences regarding content-codings. Although
        // this allows the server to use any content-coding in a response, it
        // does not imply that the user agent will be able to correctly process
        // all encodings." Identity seems safer; don't gzip.
        assert!(!super::should_gzip(None));

        // "If the representation's content-coding is one of the
        // content-codings listed in the Accept-Encoding field, then it is
        // acceptable unless it is accompanied by a qvalue of 0.  (As
        // defined in Section 5.3.1, a qvalue of 0 means "not acceptable".)"
        assert!(super::should_gzip(Some(&AcceptEncoding(vec![
            qitem(Encoding::Gzip),
        ]))));
        assert!(super::should_gzip(Some(&AcceptEncoding(vec![
            QualityItem::new(Encoding::Gzip, q(0.001)),
        ]))));
        assert!(!super::should_gzip(Some(&AcceptEncoding(vec![
            QualityItem::new(Encoding::Gzip, q(0)),
        ]))));

        // "An Accept-Encoding header field with a combined field-value that is
        // empty implies that the user agent does not want any content-coding in
        // response."
        assert!(!super::should_gzip(Some(&AcceptEncoding(vec![]))));

        // "The asterisk "*" symbol in an Accept-Encoding field
        // matches any available content-coding not explicitly listed in the
        // header field."
        assert!(super::should_gzip(Some(&AcceptEncoding(vec![
            qitem(Encoding::EncodingExt("*".to_owned())),
        ]))));
        assert!(!super::should_gzip(Some(&AcceptEncoding(vec![
            QualityItem::new(Encoding::Gzip, q(0)),
            qitem(Encoding::EncodingExt("*".to_owned())),
        ]))));
        assert!(super::should_gzip(Some(&AcceptEncoding(vec![
            QualityItem::new(Encoding::Identity, q(0)),
            qitem(Encoding::EncodingExt("*".to_owned())),
        ]))));

        // "If multiple content-codings are acceptable, then the acceptable
        // content-coding with the highest non-zero qvalue is preferred."
        assert!(super::should_gzip(Some(&AcceptEncoding(vec![
            QualityItem::new(Encoding::Identity, q(0.5)),
            QualityItem::new(Encoding::Gzip, q(1.0)),
        ]))));
        assert!(!super::should_gzip(Some(&AcceptEncoding(vec![
            QualityItem::new(Encoding::Identity, q(1.0)),
            QualityItem::new(Encoding::Gzip, q(0.5)),
        ]))));

        // "If an Accept-Encoding header field is present in a request
        // and none of the available representations for the response have a
        // content-coding that is listed as acceptable, the origin server SHOULD
        // send a response without any content-coding."
        assert!(!super::should_gzip(Some(&AcceptEncoding(vec![
            QualityItem::new(Encoding::EncodingExt("*".to_owned()), q(0.0)),
        ]))));
    }
}
