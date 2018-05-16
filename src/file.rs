// Copyright (c) 2016-2018 Scott Lamb <slamb@slamb.org>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use futures::{Sink, Stream};
use futures_cpupool::CpuPool;
use http::header::{HeaderMap, HeaderValue};
use std::io;
use std::ops::Range;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::Arc;
use std::time::{self, SystemTime};
use Entity;

// This stream breaks apart the file into chunks of CHUNK_SIZE. This size is
// a tradeoff between memory usage and thread handoffs. This value might be
// overridden by the file system if it reports a larger IO block size.
static CHUNK_SIZE: u64 = 65_536;

/// A HTTP entity created from a `std::fs::File` which reads the file
/// chunk-by-chunk on a `CpuPool`.
#[derive(Clone)]
pub struct ChunkedReadFile<B, C> {
    inner: Arc<ChunkedReadFileInner>,
    phantom: ::std::marker::PhantomData<(B, C)>,
}

struct ChunkedReadFileInner {
    chunk_size: u64,
    len: u64,
    inode: u64,
    mtime: SystemTime,
    f: ::std::fs::File,
    pool: Option<CpuPool>,
    headers: HeaderMap,
}

impl<B, C> ChunkedReadFile<B, C> {
    /// Creates a new ChunkedReadFile.
    ///
    /// `read(2)` calls will be performed on the supplied `pool` so that they don't block the
    /// tokio reactor thread on local disk I/O. Note that `File::open` and this constructor
    /// (specifically, its call to `fstat(2)`) may also block, so they typically shouldn't be
    /// called on the tokio reactor either.
    pub fn new(
        file: ::std::fs::File,
        pool: Option<CpuPool>,
        headers: HeaderMap,
    ) -> Result<Self, io::Error> {
        let m = file.metadata()?;
        let chunk_size = ::std::cmp::max(m.blksize(), CHUNK_SIZE);
        let chunk_size = ::std::cmp::min(chunk_size, m.len());
        Ok(ChunkedReadFile {
            inner: Arc::new(ChunkedReadFileInner {
                chunk_size: chunk_size,
                len: m.len(),
                inode: m.ino(),
                mtime: m.modified()?,
                headers,
                f: file,
                pool: pool,
            }),
            phantom: ::std::marker::PhantomData,
        })
    }
}

impl<B, C> Entity for ChunkedReadFile<B, C>
where
    B: 'static
        + Send
        + Stream<Item = C, Error = ::hyper::Error>
        + From<Box<Stream<Item = C, Error = ::hyper::Error> + Send>>,
    C: 'static + Send + AsRef<[u8]> + From<Vec<u8>> + From<&'static [u8]>,
{
    type Chunk = C;
    type Body = B;

    fn len(&self) -> u64 {
        self.inner.len
    }

    fn get_range(&self, range: Range<u64>) -> B {
        let stream =
            ::futures::stream::unfold((range, Arc::clone(&self.inner)), move |(left, inner)| {
                if left.start == left.end {
                    return None;
                }
                let chunk_size = ::std::cmp::min(inner.chunk_size, left.end - left.start) as usize;
                let mut chunk = Vec::with_capacity(chunk_size);
                unsafe { chunk.set_len(chunk_size) };
                let bytes_read = match inner.f.read_at(&mut chunk, left.start) {
                    Err(e) => return Some(Err(e.into())),
                    Ok(b) => b,
                };
                chunk.truncate(bytes_read);
                Some(Ok((
                    chunk.into(),
                    (left.start + bytes_read as u64..left.end, inner),
                )))
            });

        let stream: Box<Stream<Item = C, Error = ::hyper::Error> + Send> = match self.inner.pool {
            Some(ref p) => {
                let (snd, rcv) = ::futures::sync::mpsc::channel(0);
                p.spawn(snd.send_all(stream.then(Ok))).forget();
                Box::new(
                    rcv.map_err(|()| unreachable!())
                        .and_then(::futures::future::result),
                )
            }
            None => Box::new(stream),
        };
        stream.into()
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
        let dur = self.inner
            .mtime
            .duration_since(time::UNIX_EPOCH)
            .expect("modification time must be after epoch");

        // Rust doesn't seem to understand these lengths are used in the macro invocation.
        #[allow(dead_code)]
        static HEX_U64_LEN: usize = 16;
        #[allow(dead_code)]
        static HEX_U32_LEN: usize = 16;
        Some(fmt_ascii_val!(
            HEX_U64_LEN * 3 + HEX_U64_LEN + 5,
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
    extern crate tempdir;

    use futures::{Future, Stream};
    use futures_cpupool::CpuPool;
    use super::Entity;
    use http::header::HeaderMap;
    use hyper::Error;
    use self::tempdir::TempDir;
    use std::io::Write;
    use std::fs::File;
    use super::ChunkedReadFile;

    type Body = Box<Stream<Item = Vec<u8>, Error = Error> + Send>;

    fn basic_tests(pool: Option<CpuPool>) {
        let tmp = TempDir::new("http-file").unwrap();
        let p = tmp.path().join("f");
        let mut f = File::create(&p).unwrap();
        f.write_all(b"asdf").unwrap();

        let crf = ChunkedReadFile::<Body, _>::new(
            File::open(&p).unwrap(),
            pool.clone(),
            HeaderMap::new(),
        ).unwrap();
        assert_eq!(4, crf.len());
        let etag1 = crf.etag();

        // Test returning part/all of the stream.
        assert_eq!(&crf.get_range(0..4).concat2().wait().unwrap(), b"asdf");
        assert_eq!(&crf.get_range(1..3).concat2().wait().unwrap(), b"sd");

        // A ChunkedReadFile constructed from a modified file should have a different etag.
        f.write_all(b"jkl;").unwrap();
        let crf = ChunkedReadFile::<Body, _>::new(File::open(&p).unwrap(), pool, HeaderMap::new())
            .unwrap();
        assert_eq!(8, crf.len());
        let etag2 = crf.etag();
        assert_ne!(etag1, etag2);
    }

    #[test]
    fn with_pool() {
        basic_tests(Some(CpuPool::new(1)));
    }

    #[test]
    fn without_pool() {
        basic_tests(None);
    }
}
