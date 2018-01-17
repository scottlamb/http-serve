// Copyright (c) 2018 Scott Lamb <slamb@slamb.org>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use hyper::{self, header};
use hyper::server::Response;
use chunker;
use std::mem;
use std::io::{self, Write};

/// A `std::io::Write` implementation that makes a chunked hyper response body stream.
/// Automatically applies `gzip` content encoding if requested by the client.
///
/// The stream is infinitely buffered; calls to `write` and `flush` never block. `flush` thus is a
/// hint that data should be sent to the client as soon as possible, but this shouldn't be expected
/// to happen before it returns. `write` and `flush` may return error; this indicates that the
/// client certainly won't receive any additional bytes, so the calling code should stop producing
/// them.
///
/// The infinite buffering avoids the need for calling code to deal with backpressure via futures
/// or blocking. Many applications anyway produce output while holding a lock or database
/// transaction that should finish quickly, so backpressure must be ignored anyway.
///
/// On drop, the stream will be "finished" (for gzip, this writes a special footer). There's no way
/// to know the complete stream was written successfully. It's inherent in the combination of
/// HTTP / TCP / Unix sockets / hyper anyway that only the client knows this.
pub struct BodyWriter<Chunk: From<Vec<u8>> + Send + 'static>(Inner<Chunk>);

enum Inner<Chunk>
where
    Chunk: From<Vec<u8>> + Send + 'static,
{
    Raw(chunker::BodyWriter<Chunk>),
    Gzipped(::flate2::write::GzEncoder<chunker::BodyWriter<Chunk>>),

    /// No more data should be sent. `abort()` or `drop()` has been called, or a previous call
    /// discovered that the receiver has been dropped.
    Dead,
}

impl<Chunk> BodyWriter<Chunk>
where
    Chunk: From<Vec<u8>> + Send + 'static,
{
    pub fn new<Body>(ae: Option<&header::AcceptEncoding>) -> (Self, Response<Body>)
    where
        Body: From<Box<::futures::Stream<Item = Chunk, Error = hyper::Error> + Send>>,
    {
        let (raw, body) = chunker::BodyWriter::with_chunk_size(4096);
        let should_gzip = super::should_gzip(ae);
        let mut resp = Response::<Body>::new()
            .with_body(body)
            .with_header(header::Vary::Items(vec![
                ::unicase::Ascii::new("accept-encoding".to_owned()),
            ]));
        let inner = match should_gzip {
            true => {
                resp.headers_mut().set(header::ContentEncoding(vec![
                    header::Encoding::Gzip,
                    header::Encoding::Chunked,
                ]));
                Inner::Gzipped(
                    ::flate2::GzBuilder::new().write(raw, ::flate2::Compression::fast()),
                )
            }
            false => Inner::Raw(raw),
        };
        (BodyWriter(inner), resp)
    }

    /// Causes the HTTP connection to be dropped abruptly.
    pub fn abort(&mut self) {
        match mem::replace(&mut self.0, Inner::Dead) {
            Inner::Dead => (),
            Inner::Raw(ref mut w) => w.abort(),
            Inner::Gzipped(ref mut g) => g.get_mut().abort(),
        };
    }
}

impl<Chunk> Write for BodyWriter<Chunk>
where
    Chunk: From<Vec<u8>> + Send + 'static,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let r = match self.0 {
            Inner::Dead => Err(io::Error::new(io::ErrorKind::BrokenPipe, "body is dead"))?,
            Inner::Raw(ref mut w) => w.write(buf),
            Inner::Gzipped(ref mut w) => w.write(buf),
        };
        if r.is_err() {
            self.0 = Inner::Dead;
        }
        r
    }

    fn flush(&mut self) -> io::Result<()> {
        let r = match self.0 {
            Inner::Dead => Err(io::Error::new(io::ErrorKind::BrokenPipe, "body is dead"))?,
            Inner::Raw(ref mut w) => w.flush(),
            Inner::Gzipped(ref mut w) => w.flush(),
        };
        if r.is_err() {
            self.0 = Inner::Dead;
        }
        r
    }
}

/*impl<Chunk> Drop for BodyWriter<Chunk> where Chunk: From<Vec<u8>> + Send + 'static {
    fn drop(&mut self) {
        let mut w = match mem::replace(&mut self.0, Inner::Dead) {
            Inner::Dead => return,
            Inner::Raw(w) => w,
            Inner::Gzipped(g) => {
                match g.finish() {
                    Ok(w) => w,
                    Err(_) => return,
                }
            },
        };
        let _ = w.flush();
    }
}*/
