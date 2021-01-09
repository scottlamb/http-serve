// Copyright (c) 2016-2018 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use futures_channel::mpsc;
use futures_core::Stream;
use std::io::{self, Write};
use std::mem;

/// A `std::io::Write` implementation that makes a chunked hyper response body stream.
/// Raw in the sense that it doesn't apply content encoding and isn't particularly user-friendly:
/// unflushed data is ignored on drop.
///
/// Produces chunks of `Vec<u8>` or a type that implements `From<Vec<u8>>` such as
/// `reffers::ARefs<'static, [u8]>`. Currently the chunks are all of the capacity given in the
/// constructor. On flush, chunks may satisfy `0 < len < capacity`; otherwise they will satisfy
/// `0 < len == capacity`.
///
/// The stream is infinitely buffered; calls to `write` and `flush` never block. `flush` thus is a
/// hint that data should be sent to the client as soon as possible, but this shouldn't be expected
/// to happen before it returns.
pub(crate) struct BodyWriter<D, E>
where
    D: From<Vec<u8>> + Send + 'static,
    E: Send + 'static,
{
    sender: mpsc::UnboundedSender<Result<D, E>>,

    /// The next buffer to use. Invariant: capacity > len.
    buf: Vec<u8>,
}

impl<D, E> BodyWriter<D, E>
where
    D: From<Vec<u8>> + Send + 'static,
    E: Send + 'static,
{
    pub(crate) fn with_chunk_size(
        cap: usize,
    ) -> (Self, Box<dyn Stream<Item = Result<D, E>> + Send>) {
        assert!(cap > 0);
        let (snd, rcv) = mpsc::unbounded();
        let body = Box::new(rcv);
        (
            BodyWriter {
                sender: snd,
                buf: Vec::with_capacity(cap),
            },
            body,
        )
    }

    /// Causes the HTTP connection to be dropped abruptly with the given error.
    pub(crate) fn abort(&mut self, error: E) {
        // hyper drops the connection when the stream contains an error.
        let _ = self.sender.unbounded_send(Err(error));
    }

    /// Truncates the output buffer (for testing).
    #[cfg(test)]
    fn truncate(&mut self) {
        self.buf.clear()
    }
}

impl<D, E> Write for BodyWriter<D, E>
where
    D: From<Vec<u8>> + Send + 'static,
    E: Send + 'static,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let remaining = self.buf.capacity() - self.buf.len();
        let full = remaining <= buf.len();
        let bytes = if full { remaining } else { buf.len() };
        self.buf.extend_from_slice(&buf[0..bytes]);
        if full {
            self.flush()?;
        }
        Ok(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            let cap = self.buf.capacity();
            let full_buf = mem::replace(&mut self.buf, Vec::with_capacity(cap));
            if let Err(_) = self.sender.unbounded_send(Ok(full_buf.into())) {
                // If this error is returned, no further writes will succeed either.
                // Therefore, it's acceptable to just drop the full_buf (now e.into_inner())
                // rather than put it back as self.buf; it won't cause us to write a stream with
                // a gap.
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "receiver was dropped",
                ));
            }
        }
        Ok(())
    }
}

impl<D, E> Drop for BodyWriter<D, E>
where
    D: From<Vec<u8>> + Send + 'static,
    E: Send + 'static,
{
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::BodyWriter;
    use futures_core::Stream;
    use futures_util::{stream::StreamExt, stream::TryStreamExt};
    use std::io::Write;
    use std::pin::Pin;

    type BoxedError = Box<dyn std::error::Error + 'static + Send + Sync>;
    type BodyStream = Box<dyn Stream<Item = Result<Vec<u8>, BoxedError>> + Send>;

    async fn to_vec(s: BodyStream) -> Vec<u8> {
        Pin::from(s).try_concat().await.unwrap()
    }

    // A smaller-than-chunk-size write shouldn't be flushed on write, and there's currently no Drop
    // implementation to do it either.
    #[tokio::test]
    async fn small_no_flush() {
        let (mut w, body): (_, BodyStream) = BodyWriter::with_chunk_size(4);
        assert_eq!(w.write(b"1").unwrap(), 1);
        w.truncate();
        drop(w);
        assert_eq!(b"", &to_vec(body).await[..]);
    }

    // With a flush, the content should show up.
    #[tokio::test]
    async fn small_flush() {
        let (mut w, body): (_, BodyStream) = BodyWriter::with_chunk_size(4);
        assert_eq!(w.write(b"1").unwrap(), 1);
        w.flush().unwrap();
        drop(w);
        assert_eq!(b"1", &to_vec(body).await[..]);
    }

    // A write of exactly the chunk size should be automatically flushed.
    #[tokio::test]
    async fn chunk_write() {
        let (mut w, body): (_, BodyStream) = BodyWriter::with_chunk_size(4);
        assert_eq!(w.write(b"1234").unwrap(), 4);
        w.flush().unwrap();
        drop(w);
        assert_eq!(b"1234", &to_vec(body).await[..]);
    }

    // ...and everything should be set up for the next write as well.
    #[tokio::test]
    async fn chunk_double_write() {
        let (mut w, body): (_, BodyStream) = BodyWriter::with_chunk_size(4);
        assert_eq!(w.write(b"1234").unwrap(), 4);
        assert_eq!(w.write(b"5678").unwrap(), 4);
        w.flush().unwrap();
        drop(w);
        assert_eq!(b"12345678", &to_vec(body).await[..]);
    }

    // A larger-than-chunk-size write should be turned into a chunk-size write.
    #[tokio::test]
    async fn large_write() {
        let (mut w, body): (_, BodyStream) = BodyWriter::with_chunk_size(4);
        assert_eq!(w.write(b"123456").unwrap(), 4);
        drop(w);
        assert_eq!(b"1234", &to_vec(body).await[..]);
    }

    // ...similarly, one that uses all the remaining capacity of the chunk.
    #[tokio::test]
    async fn small_large_write() {
        let (mut w, body): (_, BodyStream) = BodyWriter::with_chunk_size(4);
        assert_eq!(w.write(b"1").unwrap(), 1);
        assert_eq!(w.write(b"2345").unwrap(), 3);
        drop(w);
        assert_eq!(b"1234", &to_vec(body).await[..]);
    }

    // Aborting should add an Err element to the stream, ignoring any unflushed bytes.
    #[tokio::test]
    async fn abort() {
        let (mut w, body): (_, BodyStream) = BodyWriter::with_chunk_size(4);
        w.write_all(b"12345").unwrap();
        w.truncate();
        w.abort(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            "asdf",
        )));
        drop(w);
        let items = Pin::<_>::from(body)
            .collect::<Vec<Result<Vec<u8>, BoxedError>>>()
            .await;
        assert_eq!(items.len(), 2);
        assert_eq!(b"1234", &items[0].as_ref().unwrap()[..]);
        items[1].as_ref().unwrap_err();
    }
}
