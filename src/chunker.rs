// Copyright (c) 2016-2018 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::mem;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::Poll;

use http_body::SizeHint;

/// Shared between [`Writer`] and [`Reader`].
struct Shared<E> {
    state: SharedState<E>,

    /// The waker to notify when the receiver should be polled again.
    waker: Option<std::task::Waker>,
}

enum SharedState<E> {
    Ok {
        ready: VecDeque<Vec<u8>>,

        /// Current total size (`ready.iter().map(Vec::len).sum()`) of the ready queue.
        ready_bytes: usize,

        writer_dropped: bool,
    },
    Err(E),
    ReaderFused,
}

pub struct Reader<D, E> {
    shared: Arc<Mutex<Shared<E>>>,
    _marker: std::marker::PhantomData<fn(D)>,
}

impl<D, E> Reader<D, E> {
    pub fn size_hint(&self) -> SizeHint {
        let mut h = SizeHint::default();
        if let SharedState::Ok {
            ready_bytes,
            writer_dropped,
            ..
        } = &self.shared.lock().expect("not poisoned").state
        {
            let r = crate::as_u64(*ready_bytes);
            h.set_lower(r);
            if *writer_dropped {
                h.set_upper(r);
            }
        }
        h
    }

    pub fn is_end_stream(&self) -> bool {
        match self.shared.lock().expect("not poisoned").state {
            SharedState::Ok {
                ready_bytes,
                writer_dropped,
                ..
            } => ready_bytes == 0 && writer_dropped,
            SharedState::Err(_) => false,
            SharedState::ReaderFused => true,
        }
    }
}

impl<D, E> futures_core::Stream for Reader<D, E>
where
    D: From<Vec<u8>>,
{
    type Item = Result<D, E>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let shared = &self.as_mut().shared;
        let mut l = shared.lock().expect("not poisoned");
        match std::mem::replace(&mut l.state, SharedState::ReaderFused) {
            SharedState::Ok {
                mut ready,
                mut ready_bytes,
                writer_dropped,
            } => {
                if let Some(c) = ready.pop_front() {
                    ready_bytes -= c.len();
                    if !ready.is_empty() || !writer_dropped {
                        // more chunks may follow.
                        l.state = SharedState::Ok {
                            ready,
                            ready_bytes,
                            writer_dropped,
                        };
                        drop(l);
                    }
                    return Poll::Ready(Some(Ok(D::from(c))));
                }

                debug_assert_eq!(ready_bytes, 0);

                if !writer_dropped {
                    match l.waker.as_mut() {
                        Some(w) if !w.will_wake(cx.waker()) => w.clone_from(cx.waker()),
                        Some(_) => {}
                        None => l.waker = Some(cx.waker().clone()),
                    }
                    l.state = SharedState::Ok {
                        ready,
                        ready_bytes,
                        writer_dropped,
                    };
                    drop(l);
                    return Poll::Pending;
                }

                Poll::Ready(None)
            }
            SharedState::Err(e) => Poll::Ready(Some(Err(e))),
            SharedState::ReaderFused => Poll::Ready(None),
        }
    }
}

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
pub(crate) struct Writer<D, E>
where
    D: From<Vec<u8>> + Send + 'static,
    E: Send + 'static,
{
    shared: Arc<Mutex<Shared<E>>>,

    /// The next buffer to use.
    buf: Vec<u8>,

    /// Desired capacity of each buffer.
    cap: usize,

    _marker: std::marker::PhantomData<D>,
}

// impl<D, E> From<Body<D, E>> for crate::Body<D, E>
// where
//     D: Send + 'static,
//     E: Send + 'static,
// {
//     fn from(value: Body<D, E>) -> Self {
//         crate::Body::unchecked_len_stream(Box::pin(value))
//     }
// }

impl<D, E> Writer<D, E>
where
    D: From<Vec<u8>> + Send + 'static,
    E: Send + 'static,
{
    pub(crate) fn with_chunk_size(cap: usize) -> (Self, Reader<D, E>) {
        assert!(cap > 0);
        let shared = Arc::new(Mutex::new(Shared {
            state: SharedState::Ok {
                ready: VecDeque::new(),
                ready_bytes: 0,
                writer_dropped: false,
            },
            waker: None,
        }));
        (
            Writer {
                shared: shared.clone(),
                buf: Vec::new(),
                cap,
                _marker: std::marker::PhantomData,
            },
            Reader {
                shared,
                _marker: std::marker::PhantomData,
            },
        )
    }

    /// Causes the HTTP connection to be dropped abruptly with the given error.
    pub(crate) fn abort(&mut self, error: E) {
        let mut l = self.shared.lock().expect("not poisoned");
        let _ready;
        let waker;
        if let SharedState::Ok { ready, .. } = &mut l.state {
            _ready = std::mem::take(ready); // drop might be slow; release lock first.
            l.state = SharedState::Err(error);
            waker = l.waker.take();
        } else {
            return;
        };
        drop(l);
        if let Some(w) = waker {
            w.wake();
        }
    }

    fn flush_helper(&mut self, dropping: bool) -> Result<(), ()> {
        if self.buf.is_empty() && !dropping {
            return Ok(());
        }
        let mut l = self.shared.lock().expect("not poisoned");
        let waker = if let SharedState::Ok {
            ready,
            ready_bytes,
            writer_dropped,
        } = &mut l.state
        {
            if !self.buf.is_empty() {
                let full_buf = mem::take(&mut self.buf);
                *ready_bytes += full_buf.len();
                ready.push_back(full_buf);
            }
            *writer_dropped = dropping;
            l.waker.take()
        } else if !self.buf.is_empty() {
            return Err(());
        } else {
            return Ok(());
        };
        drop(l);
        if let Some(w) = waker {
            w.wake();
        }
        Ok(())
    }

    /// Truncates the output buffer (for testing).
    #[cfg(test)]
    fn truncate(&mut self) {
        self.buf.clear()
    }
}

impl<D, E> Write for Writer<D, E>
where
    D: From<Vec<u8>> + Send + 'static,
    E: Send + 'static,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.buf.capacity() == 0 {
            self.buf.reserve_exact(self.cap);
        } else {
            assert!(self.buf.capacity() >= self.cap);
        }
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
        self.flush_helper(false)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "receiver was dropped"))
    }
}

impl<D, E> Drop for Writer<D, E>
where
    D: From<Vec<u8>> + Send + 'static,
    E: Send + 'static,
{
    fn drop(&mut self) {
        let _ = self.flush_helper(true);
    }
}

#[cfg(test)]
mod tests {
    use super::Writer;
    use futures_core::Stream;
    use futures_util::{stream::StreamExt, stream::TryStreamExt};
    use std::{
        io::Write,
        task::{Context, Poll},
    };

    type BoxError = Box<dyn std::error::Error + 'static + Send + Sync>;
    type Reader = super::Reader<Vec<u8>, BoxError>;

    async fn to_vec(s: Reader) -> Vec<u8> {
        s.try_concat().await.unwrap()
    }

    // Just dropping should end the stream.
    #[tokio::test]
    async fn just_drop() {
        let (w, body): (_, Reader) = Writer::with_chunk_size(4);
        assert!(!body.is_end_stream());
        drop(w);
        assert!(body.is_end_stream());
        assert_eq!(b"", &to_vec(body).await[..]);
    }

    #[test]
    fn extra_poll() {
        let (mut w, body): (_, Reader) = Writer::with_chunk_size(4);
        let mut body = std::pin::pin!(body);
        assert!(!body.is_end_stream());

        // Use a dummy waker.
        let mut cx = Context::from_waker(futures_util::task::noop_waker_ref());
        assert!(body.as_mut().poll_next(&mut cx).is_pending());
        assert!(body.as_mut().poll_next(&mut cx).is_pending());
        assert_eq!(w.write(b"1").unwrap(), 1);
        drop(w);
        let next = body.as_mut().poll_next(&mut cx);
        assert!(
            matches!(&next,  Poll::Ready(Some(Ok(chunk))) if chunk == b"1"),
            "next={next:?}"
        );
    }

    // A smaller-than-chunk-size write shouldn't be flushed on write.
    #[tokio::test]
    async fn small_no_flush() {
        let (mut w, body): (_, Reader) = Writer::with_chunk_size(4);
        assert_eq!(w.write(b"1").unwrap(), 1);
        w.truncate(); // prevents auto-flush from doing anything.
        drop(w);
        assert_eq!(b"", &to_vec(body).await[..]);
    }

    // With a flush, the content should show up.
    #[tokio::test]
    async fn small_flush() {
        let (mut w, body): (_, Reader) = Writer::with_chunk_size(4);
        assert_eq!(w.write(b"1").unwrap(), 1);
        w.flush().unwrap();
        w.truncate(); // prevents auto-flush from doing anything.
        drop(w);
        assert_eq!(b"1", &to_vec(body).await[..]);
    }

    // With a drop, the content should show up.
    #[tokio::test]
    async fn small_drop() {
        let (mut w, body): (_, Reader) = Writer::with_chunk_size(4);
        assert_eq!(w.write(b"1").unwrap(), 1);
        drop(w);
        assert_eq!(b"1", &to_vec(body).await[..]);
    }

    // A write of exactly the chunk size should be automatically flushed.
    #[tokio::test]
    async fn chunk_write() {
        let (mut w, body): (_, Reader) = Writer::with_chunk_size(4);
        assert_eq!(w.write(b"1234").unwrap(), 4);
        w.truncate();
        drop(w);
        assert_eq!(b"1234", &to_vec(body).await[..]);
    }

    // ...and everything should be set up for the next write as well.
    #[tokio::test]
    async fn chunk_double_write() {
        let (mut w, body): (_, Reader) = Writer::with_chunk_size(4);
        assert_eq!(w.write(b"1234").unwrap(), 4);
        assert_eq!(w.write(b"5678").unwrap(), 4);
        drop(w);
        assert_eq!(b"12345678", &to_vec(body).await[..]);
    }

    // A larger-than-chunk-size write should be turned into a chunk-size write.
    #[tokio::test]
    async fn large_write() {
        let (mut w, body): (_, Reader) = Writer::with_chunk_size(4);
        assert_eq!(w.write(b"123456").unwrap(), 4);
        drop(w);
        assert_eq!(b"1234", &to_vec(body).await[..]);
    }

    // ...similarly, one that uses all the remaining capacity of the chunk.
    #[tokio::test]
    async fn small_large_write() {
        let (mut w, body): (_, Reader) = Writer::with_chunk_size(4);
        assert_eq!(w.write(b"1").unwrap(), 1);
        assert_eq!(w.write(b"2345").unwrap(), 3);
        drop(w);
        assert_eq!(b"1234", &to_vec(body).await[..]);
    }

    // Aborting should cause the stream to Err, dumping anything in the queue.
    #[tokio::test]
    async fn abort() {
        let (mut w, body): (_, Reader) = Writer::with_chunk_size(4);
        w.write_all(b"12345").unwrap();
        w.truncate();
        w.abort(Box::new(std::io::Error::new(
            std::io::ErrorKind::Other,
            "asdf",
        )));
        drop(w);
        let items = body.collect::<Vec<Result<Vec<u8>, BoxError>>>().await;
        assert_eq!(items.len(), 1);
        items[0].as_ref().unwrap_err();
    }
}
