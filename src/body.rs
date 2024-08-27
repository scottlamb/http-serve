// Copyright (c) 2023 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::{pin::Pin, task::Poll};

use bytes::Buf;
use futures_core::Stream;
use sync_wrapper::SyncWrapper;

use crate::BoxError;

/// An [`http_body::Body`] implementation returned by [`crate::serve`] and
/// [`crate::StreamingBodyBuilder::build`].
#[pin_project::pin_project]
pub struct Body<D = bytes::Bytes, E = BoxError>(#[pin] pub(crate) BodyStream<D, E>);

impl<D, E> http_body::Body for Body<D, E>
where
    D: Buf + From<Vec<u8>> + From<&'static [u8]> + 'static,
    E: From<BoxError> + 'static,
{
    type Data = D;
    type Error = E;

    #[inline]
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        self.as_mut()
            .project()
            .0
            .poll_next(cx)
            .map(|p| p.map(|o| o.map(http_body::Frame::data)))
    }

    fn size_hint(&self) -> http_body::SizeHint {
        match &self.0 {
            BodyStream::Once(Some(Ok(d))) => http_body::SizeHint::with_exact(
                u64::try_from(d.remaining()).expect("usize should fit in u64"),
            ),
            BodyStream::Once(_) => http_body::SizeHint::with_exact(0),
            BodyStream::ExactLen(l) => http_body::SizeHint::with_exact(l.remaining),
            BodyStream::Multipart(s) => http_body::SizeHint::with_exact(s.remaining()),
            BodyStream::Chunker(c) => c.size_hint(),
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.0 {
            BodyStream::Once(c) => c.is_none(),
            BodyStream::ExactLen(l) => l.remaining == 0,
            BodyStream::Multipart(s) => s.remaining() == 0,
            BodyStream::Chunker(c) => c.is_end_stream(),
        }
    }
}

impl<D, E> Body<D, E> {
    /// Returns a 0-byte body.
    #[inline]
    pub fn empty() -> Self {
        Self(BodyStream::Once(None))
    }
}

impl<D, E> From<&'static [u8]> for Body<D, E>
where
    D: From<&'static [u8]>,
{
    #[inline]
    fn from(value: &'static [u8]) -> Self {
        Self(BodyStream::Once(Some(Ok(value.into()))))
    }
}

impl<D, E> From<&'static str> for Body<D, E>
where
    D: From<&'static [u8]>,
{
    #[inline]
    fn from(value: &'static str) -> Self {
        Self(BodyStream::Once(Some(Ok(value.as_bytes().into()))))
    }
}

impl<D, E> From<Vec<u8>> for Body<D, E>
where
    D: From<Vec<u8>>,
{
    #[inline]
    fn from(value: Vec<u8>) -> Self {
        Self(BodyStream::Once(Some(Ok(value.into()))))
    }
}

impl<D, E> From<String> for Body<D, E>
where
    D: From<Vec<u8>>,
{
    #[inline]
    fn from(value: String) -> Self {
        Self(BodyStream::Once(Some(Ok(value.into_bytes().into()))))
    }
}

#[pin_project::pin_project(project = BodyStreamProj)]
pub(crate) enum BodyStream<D, E> {
    Once(Option<Result<D, E>>),
    ExactLen(#[pin] ExactLenStream<D, E>),
    Multipart(#[pin] crate::serving::MultipartStream<D, E>),
    Chunker(#[pin] crate::chunker::Reader<D, E>),
}

impl<D, E> Stream for BodyStream<D, E>
where
    D: 'static + Buf + From<Vec<u8>> + From<&'static [u8]>,
    E: 'static + From<BoxError>,
{
    type Item = Result<D, E>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Result<D, E>>> {
        match self.project() {
            BodyStreamProj::Once(c) => Poll::Ready(c.take()),
            BodyStreamProj::ExactLen(s) => s.poll_next(cx),
            BodyStreamProj::Multipart(s) => s.poll_next(cx),
            BodyStreamProj::Chunker(s) => s.poll_next(cx),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct StreamTooShortError {
    remaining: u64,
}

impl std::fmt::Display for StreamTooShortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "stream ended with {} bytes still expected",
            self.remaining
        )
    }
}

impl std::error::Error for StreamTooShortError {}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct StreamTooLongError {
    extra: u64,
}

impl std::fmt::Display for StreamTooLongError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "stream returned (at least) {} bytes more than expected",
            self.extra
        )
    }
}

impl std::error::Error for StreamTooLongError {}

pub(crate) struct ExactLenStream<D, E> {
    #[allow(clippy::type_complexity)]
    stream: SyncWrapper<Pin<Box<dyn Stream<Item = Result<D, E>> + Send>>>,
    remaining: u64,
}

impl<D, E> ExactLenStream<D, E> {
    pub(crate) fn new(len: u64, stream: Pin<Box<dyn Stream<Item = Result<D, E>> + Send>>) -> Self {
        Self {
            stream: SyncWrapper::new(stream),
            remaining: len,
        }
    }
}

impl<D, E> futures_core::Stream for ExactLenStream<D, E>
where
    D: Buf,
    E: From<BoxError>,
{
    type Item = Result<D, E>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Result<D, E>>> {
        let this = Pin::into_inner(self);
        match this.stream.get_mut().as_mut().poll_next(cx) {
            Poll::Ready(Some(Ok(d))) => {
                let d_len = crate::as_u64(d.remaining());
                let new_rem = this.remaining.checked_sub(d_len);
                if let Some(new_rem) = new_rem {
                    this.remaining = new_rem;
                    Poll::Ready(Some(Ok(d)))
                } else {
                    let remaining = std::mem::take(&mut this.remaining); // fuse.
                    Poll::Ready(Some(Err(E::from(Box::new(StreamTooLongError {
                        extra: d_len - remaining,
                    })))))
                }
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => {
                if this.remaining != 0 {
                    let remaining = std::mem::take(&mut this.remaining); // fuse.
                    return Poll::Ready(Some(Err(E::from(Box::new(StreamTooShortError {
                        remaining,
                    })))));
                }
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

const _: () = {
    fn _assert() {
        fn assert_bounds<T: Sync + Send>() {}
        assert_bounds::<Body>();
    }
};

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use futures_util::StreamExt as _;

    use super::*;

    #[tokio::test]
    async fn correct_exact_len_stream() {
        let inner = futures_util::stream::iter(vec![Ok("h".into()), Ok("ello".into())]);
        let mut exact_len =
            std::pin::pin!(ExactLenStream::<Bytes, BoxError>::new(5, Box::pin(inner)));
        assert_eq!(exact_len.remaining, 5);
        let frame = exact_len.next().await.unwrap().unwrap();
        assert_eq!(frame.remaining(), 1);
        assert_eq!(exact_len.remaining, 4);
        let frame = exact_len.next().await.unwrap().unwrap();
        assert_eq!(frame.remaining(), 4);
        assert_eq!(exact_len.remaining, 0);
        assert!(exact_len.next().await.is_none()); // end of stream.
        assert!(exact_len.next().await.is_none()); // fused.
    }

    #[tokio::test]
    async fn short_exact_len_stream() {
        let inner = futures_util::stream::iter(vec![Ok("hello".into())]);
        let mut exact_len =
            std::pin::pin!(ExactLenStream::<Bytes, BoxError>::new(10, Box::pin(inner)));
        assert_eq!(exact_len.remaining, 10);
        let frame = exact_len.next().await.unwrap().unwrap();
        assert_eq!(frame.remaining(), 5);
        assert_eq!(exact_len.remaining, 5);
        let err = exact_len.next().await.unwrap().unwrap_err();
        assert_eq!(
            err.downcast_ref::<StreamTooShortError>().unwrap(),
            &StreamTooShortError { remaining: 5 }
        );
        assert!(exact_len.next().await.is_none()); // fused.
    }

    #[tokio::test]
    async fn long_exact_len_stream() {
        let inner = futures_util::stream::iter(vec![Ok("h".into()), Ok("ello".into())]);
        let mut exact_len =
            std::pin::pin!(ExactLenStream::<Bytes, BoxError>::new(3, Box::pin(inner)));
        assert_eq!(exact_len.remaining, 3);
        let frame = exact_len.next().await.unwrap().unwrap();
        assert_eq!(frame.remaining(), 1);
        assert_eq!(exact_len.remaining, 2);
        let err = exact_len.next().await.unwrap().unwrap_err();
        assert_eq!(
            err.downcast_ref::<StreamTooLongError>().unwrap(),
            &StreamTooLongError { extra: 2 }
        );
        assert!(exact_len.next().await.is_none()); // fused.
    }
}
