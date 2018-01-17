// Copyright (c) 2016-2018 Scott Lamb <slamb@slamb.org>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate futures;
extern crate futures_cpupool;
extern crate hyper;
extern crate mime;
extern crate smallvec;
extern crate time;

use futures::Stream;
use hyper::Error;
use hyper::header;
use std::ops::Range;

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

mod file;
mod serving;

pub use file::ChunkedReadFile;
pub use serving::serve;
