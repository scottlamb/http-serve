// Copyright (c) 2016-2018 Scott Lamb <slamb@slamb.org>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

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
use std::ops::Range;

mod chunker;
mod file;
mod gzip;
mod serving;

pub use file::ChunkedReadFile;
pub use gzip::BodyWriter;
pub use serving::serve;

/// A read-only HTTP entity for GET and HEAD serving.
pub trait Entity: 'static + Send {
    /// The type of a chunk.
    ///
    /// Commonly `::hyper::Chunk` or `Vec<u8>` but may be something more exotic such as
    /// `::reffers::ARefs<'static, [u8]>` to minimize copying.
    type Chunk: 'static + Send + AsRef<[u8]> + From<Vec<u8>> + From<&'static [u8]>;

    /// The type of the body stream. Commonly
    /// `Box<::futures::stream::Stream<Self::Chunk, ::hyper::Error> + Send>`.
    ///
    /// Note: unfortunately `::hyper::Body` is not possible because it doesn't implement
    /// `From<Box<Stream...>>`.
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
