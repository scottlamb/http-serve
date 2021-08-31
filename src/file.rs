// Copyright (c) 2020 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use crate::platform::{self, FileExt};
use bytes::Buf;
use futures_core::Stream;
use futures_util::stream;
use http::header::{HeaderMap, HeaderValue};
use std::error::Error as StdError;
use std::io;
use std::ops::Range;
use std::sync::Arc;
use std::time::{self, SystemTime};

use crate::Entity;

// This stream breaks apart the file into chunks of at most CHUNK_SIZE. This size is
// a tradeoff between memory usage and thread handoffs.
static CHUNK_SIZE: u64 = 65_536;

/// HTTP entity created from a [`std::fs::File`] which reads the file chunk-by-chunk within
/// a [`tokio::task::block_in_place`] closure.
///
/// `ChunkedReadFile` is cheap to clone and reuse for many requests.
///
/// Expects to be served from a tokio threadpool.
///
/// ```
/// # use bytes::Bytes;
/// # use hyper::Body;
/// # use http::{Request, Response, header::{self, HeaderMap, HeaderValue}};
/// type BoxedError = Box<dyn std::error::Error + 'static + Send + Sync>;
/// async fn serve_dictionary(req: Request<Body>) -> Result<Response<Body>, BoxedError> {
///     let f = tokio::task::block_in_place::<_, Result<_, BoxedError>>(
///         move || {
///             let f = std::fs::File::open("/usr/share/dict/words")?;
///             let mut headers = http::header::HeaderMap::new();
///             headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
///             Ok(http_serve::ChunkedReadFile::new(f, headers)?)
///         },
///     )?;
///     Ok(http_serve::serve(f, &req))
/// }
/// ```
#[derive(Clone)]
pub struct ChunkedReadFile<
    D: 'static + Send + Buf + From<Vec<u8>> + From<&'static [u8]>,
    E: 'static + Send + Into<Box<dyn StdError + Send + Sync>> + From<Box<dyn StdError + Send + Sync>>,
> {
    inner: Arc<ChunkedReadFileInner>,
    phantom: std::marker::PhantomData<(D, E)>,
}

struct ChunkedReadFileInner {
    len: u64,
    inode: u64,
    mtime: SystemTime,
    f: std::fs::File,
    headers: HeaderMap,
}

impl<D, E> ChunkedReadFile<D, E>
where
    D: 'static + Send + Sync + Buf + From<Vec<u8>> + From<&'static [u8]>,
    E: 'static
        + Send
        + Sync
        + Into<Box<dyn StdError + Send + Sync>>
        + From<Box<dyn StdError + Send + Sync>>,
{
    /// Creates a new ChunkedReadFile.
    ///
    /// `read(2)` calls will be wrapped in [`tokio::task::block_in_place`] calls so that they don't
    /// block the tokio reactor thread on local disk I/O. Note that [`std::fs::File::open`] and
    /// this constructor (specifically, its call to `fstat(2)`) may also block, so they typically
    /// should be wrapped in [`tokio::task::block_in_place`] as well.
    pub fn new(file: std::fs::File, headers: HeaderMap) -> Result<Self, io::Error> {
        let m = file.metadata()?;
        ChunkedReadFile::new_with_metadata(file, &m, headers)
    }

    /// Creates a new ChunkedReadFile, with presupplied metadata.
    ///
    /// This is an optimization for the case where the caller has already called `fstat(2)`.
    /// Note that on Windows, this still may perform a blocking file operation, so it should
    /// still be wrapped in [`tokio::task::block_in_place`].
    pub fn new_with_metadata(
        file: ::std::fs::File,
        metadata: &::std::fs::Metadata,
        headers: HeaderMap,
    ) -> Result<Self, io::Error> {
        // `file` might represent a directory. If so, it's better to realize that now (while
        // we can still send a proper HTTP error) rather than during `get_range` (when all we can
        // do is drop the HTTP connection).
        if !metadata.is_file() {
            return Err(io::Error::new(io::ErrorKind::Other, "expected a file"));
        }

        let info = platform::file_info(&file, metadata)?;

        Ok(ChunkedReadFile {
            inner: Arc::new(ChunkedReadFileInner {
                len: info.len,
                inode: info.inode,
                mtime: info.mtime,
                headers,
                f: file,
            }),
            phantom: std::marker::PhantomData,
        })
    }
}

impl<D, E> Entity for ChunkedReadFile<D, E>
where
    D: 'static + Send + Sync + Buf + From<Vec<u8>> + From<&'static [u8]>,
    E: 'static
        + Send
        + Sync
        + Into<Box<dyn StdError + Send + Sync>>
        + From<Box<dyn StdError + Send + Sync>>,
{
    type Data = D;
    type Error = E;

    fn len(&self) -> u64 {
        self.inner.len
    }

    fn get_range(
        &self,
        range: Range<u64>,
    ) -> Box<dyn Stream<Item = Result<Self::Data, Self::Error>> + Send + Sync> {
        let stream = stream::unfold(
            (range, Arc::clone(&self.inner)),
            move |(left, inner)| async {
                if left.start == left.end {
                    return None;
                }
                let chunk_size = std::cmp::min(CHUNK_SIZE, left.end - left.start) as usize;
                Some(tokio::task::block_in_place(move || {
                    // Read directly into an uninitialized buffer. By a strict reading of the
                    // Vec::set_len docs, this is unsound (the buffer must be initialized first),
                    // but tokio::io::BufReader does something similar (at least these two calls in
                    // a row), so I'll assume for now it doesn't cause problems in practice.
                    // It looks like there's work to avoid this problem here:
                    // https://github.com/rust-lang/rust/issues/42788
                    let mut chunk = Vec::with_capacity(chunk_size);
                    unsafe { chunk.set_len(chunk_size) };
                    let bytes_read = match inner.f.read_at(&mut chunk, left.start) {
                        Err(e) => {
                            return (
                                Err(Box::<dyn StdError + Send + Sync + 'static>::from(e).into()),
                                (left, inner),
                            )
                        }
                        Ok(b) => b,
                    };
                    chunk.truncate(bytes_read);
                    (
                        Ok(chunk.into()),
                        (left.start + bytes_read as u64..left.end, inner),
                    )
                }))
            },
        );
        let _: &dyn Stream<Item = Result<Self::Data, Self::Error>> = &stream;
        Box::new(stream)
    }

    fn add_headers(&self, h: &mut HeaderMap) {
        h.extend(
            self.inner
                .headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone())),
        );
    }

    fn etag(&self) -> Option<HeaderValue> {
        // This etag format is similar to Apache's. The etag should change if the file is modified
        // or replaced. The length is probably redundant but doesn't harm anything.
        let dur = self
            .inner
            .mtime
            .duration_since(time::UNIX_EPOCH)
            .expect("modification time must be after epoch");

        static HEX_U64_LEN: usize = 16;
        static HEX_U32_LEN: usize = 8;
        Some(unsafe_fmt_ascii_val!(
            HEX_U64_LEN * 3 + HEX_U32_LEN + 5,
            "\"{:x}:{:x}:{:x}:{:x}\"",
            self.inner.inode,
            self.inner.len,
            dur.as_secs(),
            dur.subsec_nanos()
        ))
    }

    fn last_modified(&self) -> Option<SystemTime> {
        Some(self.inner.mtime)
    }
}

#[cfg(test)]
mod tests {
    use super::ChunkedReadFile;
    use super::Entity;
    use bytes::Bytes;
    use futures_core::Stream;
    use http::header::HeaderMap;
    use std::fs::File;
    use std::io::Write;

    type BoxedError = Box<dyn std::error::Error + Sync + Send>;
    type CRF = ChunkedReadFile<Bytes, BoxedError>;

    async fn to_bytes(s: Box<dyn Stream<Item = Result<Bytes, BoxedError>> + Send>) -> Bytes {
        hyper::body::to_bytes(hyper::Body::from(s)).await.unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn basic() {
        tokio::spawn(async move {
            let tmp = tempfile::tempdir().unwrap();
            let p = tmp.path().join("f");
            let mut f = File::create(&p).unwrap();
            f.write_all(b"asdf").unwrap();

            let crf = CRF::new(File::open(&p).unwrap(), HeaderMap::new()).unwrap();
            assert_eq!(4, crf.len());
            let etag1 = crf.etag();

            // Test returning part/all of the stream.
            assert_eq!(&to_bytes(crf.get_range(0..4)).await.as_ref(), b"asdf");
            assert_eq!(&to_bytes(crf.get_range(1..3)).await.as_ref(), b"sd");

            // A ChunkedReadFile constructed from a modified file should have a different etag.
            f.write_all(b"jkl;").unwrap();
            let crf = CRF::new(File::open(&p).unwrap(), HeaderMap::new()).unwrap();
            assert_eq!(8, crf.len());
            let etag2 = crf.etag();
            assert_ne!(etag1, etag2);
        })
        .await
        .unwrap();
    }
}
