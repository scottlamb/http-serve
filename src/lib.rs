// Copyright (c) 2016-2020 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Helpers for serving HTTP GET and HEAD responses asynchronously with the
//! [http](http://crates.io/crates/http) crate and [tokio](https://crates.io/crates/tokio).
//! Works well with [hyper](https://crates.io/crates/hyper) 0.14.x.
//!
//! This crate supplies two ways to respond to HTTP GET and HEAD requests:
//!
//! *   the `serve` function can be used to serve an `Entity`, a trait representing reusable,
//!     byte-rangeable HTTP entities. `Entity` must be able to produce exactly the same data on
//!     every call, know its size in advance, and be able to produce portions of the data on demand.
//! *   the `streaming_body` function can be used to add a body to an otherwise-complete response.
//!     If a body is needed (on `GET` rather than `HEAD` requests), it returns a `BodyWriter`
//!     (which implements `std::io::Writer`). The caller should produce the complete body or call
//!     `BodyWriter::abort`, causing the HTTP stream to terminate abruptly.
//!
//! # Why two ways?
//!
//! They have pros and cons. This table shows some of them:
//!
//! <table>
//!   <tr><th><th><code>serve</code><th><code>streaming_body</code></tr>
//!   <tr><td>automatic byte range serving<td>yes<td>no [<a href="#range">1</a>]</tr>
//!   <tr><td>backpressure<td>yes<td>no [<a href="#backpressure">2</a>]</tr>
//!   <tr><td>conditional GET<td>yes<td>no [<a href="#conditional_get">3</a>]</tr>
//!   <tr><td>sends first byte before length known<td>no<td>yes</tr>
//!   <tr><td>automatic gzip content encoding<td>no<td>yes</tr>
//! </table>
//!
//! <a name="range">\[1\]</a>: `streaming_body` always sends the full body. Byte range serving
//! wouldn't make much sense with its interface. The application will generate all the bytes
//! every time anyway, and `http-serve`'s buffering logic would have to be complex
//! to handle multiple ranges well.
//!
//! <a name="backpressure">\[2\]</a>: `streaming_body` is often appended to while holding
//! a lock or open database transaction, where backpressure is undesired. It'd be
//! possible to add support for "wait points" where the caller explicitly wants backpressure. This
//! would make it more suitable for large streams, even infinite streams like
//! [Server-sent events](https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events).
//!
//! <a name="conditional_get">\[3\]</a>: `streaming_body` doesn't yet support
//! generating etags or honoring conditional GET requests. PRs welcome!
//!
//! Use `serve` when:
//!
//! *   metadata (length, etag, etc) and byte ranges can be regenerated cheaply and consistently
//!     via a lazy `Entity`, or
//! *   data can be fully buffered in memory or on disk and reused many times. You may want to
//!     create a pair of buffers for gzipped (for user-agents which specify `Accept-Encoding:
//!     gzip`) vs raw.
//!
//! Use `streaming_body` when regenerating the entire body each time a response is sent.
//!
//! Once you return a `hyper::server::Response` to hyper, your only way to signal error to the
//! client is to abruptly close the HTTP connection while sending the body. If you want the ability
//! to return a well-formatted error to the client while producing body bytes, you must buffer the
//! entire body in-memory before returning anything to hyper.
//!
//! If you are buffering a response in memory, `serve` requires copying the bytes (when using
//! `Data = Vec<u8>` or similar) or atomic reference-counting (with `Data = Arc<Vec<u8>>` or
//! similar). `streaming_body` doesn't need to keep its own copy for potential future use; it may
//! be cheaper because it can simply hand ownership of the existing `Vec<u8>`s to hyper.
//!
//! # Why the weird type bounds? Why not use `hyper::Body` and `bytes::Bytes` for everything?
//!
//! These bounds are compatible with `hyper::Body` and `bytes::Bytes`, and most callers will use
//! those types. **Note:** if you see an error like the one below, ensure you are using hyper's
//! `stream` feature:
//!
//! ```text
//! error[E0277]: the trait bound `Body: From<Box<(dyn futures::Stream<Item = Result<_, _>> +
//! std::marker::Send + 'static)>>` is not satisfied
//! ```
//!
//! `Cargo.toml` should look similar to the following:
//!
//! ```toml
//! hyper = { version = "0.14.7", features = ["stream"] }
//! ```
//!
//! There are times when it's desirable to have more flexible ownership provided by a
//! type such as `reffers::ARefs<'static, [u8]>`. One is `mmap`-based file serving:
//! `bytes::Bytes` would require copying the data in each chunk. An implementation with `ARefs`
//! could instead `mmap` and `mlock` the data on another thread and provide chunks which `munmap`
//! when dropped. In these cases, the caller can supply an alternate implementation of the
//! `http_body::Body` trait which uses a different `Data` type than `bytes::Bytes`.

#![cfg_attr(docsrs, feature(doc_cfg))]

use bytes::Buf;
use futures_core::Stream;
use http::header::{self, HeaderMap, HeaderValue};
use std::ops::Range;
use std::str::FromStr;
use std::time::SystemTime;

/// Returns a HeaderValue for the given formatted data.
/// Caller must make two guarantees:
///    * The data fits within `max_len` (or the write will panic).
///    * The data are ASCII (or HeaderValue's safety will be violated).
macro_rules! unsafe_fmt_ascii_val {
    ($max_len:expr, $fmt:expr, $($arg:tt)+) => {{
        let mut buf = bytes::BytesMut::with_capacity($max_len);
        use std::fmt::Write;
        write!(buf, $fmt, $($arg)*).expect("fmt_val fits within provided max len");
        unsafe {
            http::header::HeaderValue::from_maybe_shared_unchecked(buf.freeze())
        }
    }}
}

mod chunker;

#[cfg(feature = "dir")]
#[cfg_attr(docsrs, doc(cfg(feature = "dir")))]
pub mod dir;

mod etag;
mod file;
mod gzip;
mod platform;
mod range;
mod serving;

pub use crate::file::ChunkedReadFile;
pub use crate::gzip::BodyWriter;
pub use crate::serving::serve;

/// A reusable, read-only, byte-rangeable HTTP entity for GET and HEAD serving.
/// Must return exactly the same data on every call.
pub trait Entity: 'static + Send + Sync {
    type Error: 'static + Send + Sync;

    /// The type of a data chunk.
    ///
    /// Commonly `bytes::Bytes` but may be something more exotic.
    type Data: 'static + Send + Sync + Buf + From<Vec<u8>> + From<&'static [u8]>;

    /// Returns the length of the entity's body in bytes.
    fn len(&self) -> u64;

    /// Returns true iff the entity's body has length 0.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Gets the body bytes indicated by `range`.
    fn get_range(
        &self,
        range: Range<u64>,
    ) -> Box<dyn Stream<Item = Result<Self::Data, Self::Error>> + Send + Sync>;

    /// Adds entity headers such as `Content-Type` to the supplied `Headers` object.
    /// In particular, these headers are the "other representation header fields" described by [RFC
    /// 7233 section 4.1](https://tools.ietf.org/html/rfc7233#section-4.1); they should exclude
    /// `Content-Range`, `Date`, `Cache-Control`, `ETag`, `Expires`, `Content-Location`, and `Vary`.
    ///
    /// This function will be called only when that section says that headers such as
    /// `Content-Type` should be included in the response.
    fn add_headers(&self, _: &mut HeaderMap);

    /// Returns an etag for this entity, if available.
    /// Implementations are encouraged to provide a strong etag. [RFC 7232 section
    /// 2.1](https://tools.ietf.org/html/rfc7232#section-2.1) notes that only strong etags
    /// are usable for sub-range retrieval.
    fn etag(&self) -> Option<HeaderValue>;

    /// Returns the last modified time of this entity, if available.
    /// Note that `serve` may serve an earlier `Last-Modified:` date than the one returned here if
    /// this time is in the future, as required by [RFC 7232 section
    /// 2.2.1](https://tools.ietf.org/html/rfc7232#section-2.2.1).
    fn last_modified(&self) -> Option<SystemTime>;
}

/// Parses an RFC 7231 section 5.3.1 `qvalue` into an integer in [0, 1000].
/// ```text
/// qvalue = ( "0" [ "." 0*3DIGIT ] )
///        / ( "1" [ "." 0*3("0") ] )
/// ```
fn parse_qvalue(s: &str) -> Result<u16, ()> {
    match s {
        "1" | "1." | "1.0" | "1.00" | "1.000" => return Ok(1000),
        "0" | "0." => return Ok(0),
        s if !s.starts_with("0.") => return Err(()),
        _ => {}
    };
    let v = &s[2..];
    let factor = match v.len() {
        1 /* 0.x */ => 100,
        2 /* 0.xx */ => 10,
        3 /* 0.xxx */ => 1,
        _ => return Err(()),
    };
    let v = u16::from_str(v).map_err(|_| ())?;
    let q = v * factor;
    Ok(q)
}

/// Returns iff it's preferable to use `Content-Encoding: gzip` when responding to the given
/// request, rather than no content coding.
///
/// Use via `should_gzip(req.headers())`.
///
/// Follows the rules of [RFC 7231 section
/// 5.3.4](https://tools.ietf.org/html/rfc7231#section-5.3.4).
pub fn should_gzip(headers: &HeaderMap) -> bool {
    let v = match headers.get(header::ACCEPT_ENCODING) {
        None => return false,
        Some(v) => v,
    };
    let (mut gzip_q, mut identity_q, mut star_q) = (None, None, None);
    let parts = match v.to_str() {
        Ok(s) => s.split(','),
        Err(_) => return false,
    };
    for qi in parts {
        // Parse.
        let qi = qi.trim();
        let mut parts = qi.rsplitn(2, ';').map(|p| p.trim());
        let last_part = parts
            .next()
            .expect("rsplitn should return at least one part");
        let coding;
        let quality;
        match parts.next() {
            None => {
                coding = last_part;
                quality = 1000;
            }
            Some(c) => {
                if !last_part.starts_with("q=") {
                    return false; // unparseable.
                }
                let q = &last_part[2..];
                match parse_qvalue(q) {
                    Ok(q) => {
                        coding = c;
                        quality = q;
                    }
                    Err(_) => return false, // unparseable.
                };
            }
        }

        if coding == "gzip" {
            gzip_q = Some(quality);
        } else if coding == "identity" {
            identity_q = Some(quality);
        } else if coding == "*" {
            star_q = Some(quality);
        }
    }

    let gzip_q = gzip_q.or(star_q).unwrap_or(0);

    // "If the representation has no content-coding, then it is
    // acceptable by default unless specifically excluded by the
    // Accept-Encoding field stating either "identity;q=0" or "*;q=0"
    // without a more specific entry for "identity"."
    let identity_q = identity_q.or(star_q).unwrap_or(1);

    gzip_q > 0 && gzip_q >= identity_q
}

/// A builder returned by [streaming_body].
pub struct StreamingBodyBuilder {
    chunk_size: usize,
    gzip_level: u32,
    should_gzip: bool,
    body_needed: bool,
}


/// Creates a response and streaming body writer for the given request.
///
/// The streaming body writer is currently `Some(writer)` for `GET` requests and
/// `None` for `HEAD` requests. In the future, `streaming_body` may also support
/// conditional `GET` requests.
///
/// ```
/// # use http::{Request, Response, header::{self, HeaderValue}};
/// use std::io::Write as _;
///
/// fn respond(req: Request<hyper::Body>) -> std::io::Result<Response<hyper::Body>> {
///     let (mut resp, stream) = http_serve::streaming_body(&req).build();
///     if let Some(mut w) = stream {
///         write!(&mut w, "hello world")?;
///     }
///     resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
///     Ok(resp)
/// }
/// ```
///
/// The caller may also continue appending to `stream` after returning the response to `hyper`.
/// The response will end when `stream` is dropped. The only disadvantage to writing to the stream
/// after the fact is that there's no way to report mid-response errors other than abruptly closing
/// the TCP connection ([BodyWriter::abort]).
///
/// ```
/// # use http::{Request, Response, header::{self, HeaderValue}};
/// use std::io::Write as _;
///
/// fn respond(req: Request<hyper::Body>) -> std::io::Result<Response<hyper::Body>> {
///     let (mut resp, stream) = http_serve::streaming_body(&req).build();
///     if let Some(mut w) = stream {
///         tokio::spawn(async move {
///             for i in 0..10 {
///                 tokio::time::sleep(std::time::Duration::from_secs(1)).await;
///                 write!(&mut w, "write {}\n", i)?;
///             }
///             Ok::<_, std::io::Error>(())
///         });
///     }
///     resp.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
///     Ok(resp)
/// }
/// ```
pub fn streaming_body<T>(req: &http::Request<T>) -> StreamingBodyBuilder {
    StreamingBodyBuilder {
        chunk_size: 4096,
        gzip_level: 6,
        should_gzip: should_gzip(req.headers()),
        body_needed: *req.method() != http::method::Method::HEAD,
    }
}

impl StreamingBodyBuilder {
    /// Sets the size of a data chunk.
    ///
    /// This is a compromise between memory usage and efficiency. The default of 4096 is usually
    /// fine; increasing will likely only be noticeably more efficient when compression is off.
    pub fn with_chunk_size(self, chunk_size: usize) -> Self {
        StreamingBodyBuilder { chunk_size, ..self }
    }

    /// Sets the gzip compression level. Defaults to 6.
    ///
    /// `gzip_level` should be an integer between 0 and 9 (inclusive).
    /// 0 means no compression; 9 gives the best compression (but most CPU usage).
    ///
    /// This is only effective if the client supports compression.
    pub fn with_gzip_level(self, gzip_level: u32) -> Self {
        StreamingBodyBuilder { gzip_level, ..self }
    }

    /// Returns the HTTP response and, if the request is a `GET`, a body writer.
    pub fn build<P, D, E>(self) -> (http::Response<P>, Option<BodyWriter<D, E>>)
    where
        D: From<Vec<u8>> + Send + Sync,
        E: Send + Sync,
        P: From<Box<dyn Stream<Item = Result<D, E>> + Send>>,
    {
        let (w, stream) = chunker::BodyWriter::with_chunk_size(self.chunk_size);
        let mut resp = http::Response::new(stream.into());
        resp.headers_mut()
            .append(header::VARY, HeaderValue::from_static("accept-encoding"));

        if self.should_gzip && self.gzip_level > 0 {
            resp.headers_mut()
                .append(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        }

        if !self.body_needed {
            return (resp, None);
        }

        let w = match self.should_gzip && self.gzip_level > 0 {
            true => BodyWriter::gzipped(w, flate2::Compression::new(self.gzip_level)),
            false => BodyWriter::raw(w),
        };

        (resp, Some(w))
    }
}

#[cfg(test)]
mod tests {
    use http::header::HeaderValue;
    use http::{self, header};

    fn ae_hdrs(value: &'static str) -> http::HeaderMap {
        let mut h = http::HeaderMap::new();
        h.insert(header::ACCEPT_ENCODING, HeaderValue::from_static(value));
        h
    }

    #[test]
    fn parse_qvalue() {
        use super::parse_qvalue;
        assert_eq!(parse_qvalue("0"), Ok(0));
        assert_eq!(parse_qvalue("0."), Ok(0));
        assert_eq!(parse_qvalue("0.0"), Ok(0));
        assert_eq!(parse_qvalue("0.00"), Ok(0));
        assert_eq!(parse_qvalue("0.000"), Ok(0));
        assert_eq!(parse_qvalue("0.0000"), Err(()));
        assert_eq!(parse_qvalue("0.2"), Ok(200));
        assert_eq!(parse_qvalue("0.23"), Ok(230));
        assert_eq!(parse_qvalue("0.234"), Ok(234));
        assert_eq!(parse_qvalue("1"), Ok(1000));
        assert_eq!(parse_qvalue("1."), Ok(1000));
        assert_eq!(parse_qvalue("1.0"), Ok(1000));
        assert_eq!(parse_qvalue("1.1"), Err(()));
        assert_eq!(parse_qvalue("1.00"), Ok(1000));
        assert_eq!(parse_qvalue("1.000"), Ok(1000));
        assert_eq!(parse_qvalue("1.001"), Err(()));
        assert_eq!(parse_qvalue("1.0000"), Err(()));
        assert_eq!(parse_qvalue("2"), Err(()));
    }

    #[test]
    fn should_gzip() {
        // "A request without an Accept-Encoding header field implies that the
        // user agent has no preferences regarding content-codings. Although
        // this allows the server to use any content-coding in a response, it
        // does not imply that the user agent will be able to correctly process
        // all encodings." Identity seems safer; don't gzip.
        assert!(!super::should_gzip(&header::HeaderMap::new()));

        // "If the representation's content-coding is one of the
        // content-codings listed in the Accept-Encoding field, then it is
        // acceptable unless it is accompanied by a qvalue of 0.  (As
        // defined in Section 5.3.1, a qvalue of 0 means "not acceptable".)"
        assert!(super::should_gzip(&ae_hdrs("gzip")));
        assert!(super::should_gzip(&ae_hdrs("gzip;q=0.001")));
        assert!(!super::should_gzip(&ae_hdrs("gzip;q=0")));

        // "An Accept-Encoding header field with a combined field-value that is
        // empty implies that the user agent does not want any content-coding in
        // response."
        assert!(!super::should_gzip(&ae_hdrs("")));

        // "The asterisk "*" symbol in an Accept-Encoding field
        // matches any available content-coding not explicitly listed in the
        // header field."
        assert!(super::should_gzip(&ae_hdrs("*")));
        assert!(!super::should_gzip(&ae_hdrs("gzip;q=0, *")));
        assert!(super::should_gzip(&ae_hdrs("identity=q=0, *")));

        // "If multiple content-codings are acceptable, then the acceptable
        // content-coding with the highest non-zero qvalue is preferred."
        assert!(super::should_gzip(&ae_hdrs("identity;q=0.5, gzip;q=1.0")));
        assert!(!super::should_gzip(&ae_hdrs("identity;q=1.0, gzip;q=0.5")));

        // "If an Accept-Encoding header field is present in a request
        // and none of the available representations for the response have a
        // content-coding that is listed as acceptable, the origin server SHOULD
        // send a response without any content-coding."
        assert!(!super::should_gzip(&ae_hdrs("*;q=0")));
    }
}
