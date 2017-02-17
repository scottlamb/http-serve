// The MIT License (MIT)
// Copyright (c) 2016 Scott Lamb <slamb@slamb.org>
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

extern crate futures;
extern crate futures_cpupool;
#[macro_use] extern crate log;
extern crate http_entity;
extern crate hyper;
extern crate libc;
extern crate mime;
extern crate time;

use futures::Stream;
use futures::stream::BoxStream;
use futures_cpupool::CpuPool;
use http_entity::Entity;
use hyper::Error;
use hyper::header;
use std::ops::Range;
use std::os::unix::io::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::Arc;

/// A HTTP entity created from a `std::fs::File` which reads the file
/// chunk-by-chunk on a CpuPool.
#[derive(Clone)]
pub struct ChunkedReadFile<B, C> {
    inner: Arc<ChunkedReadFileInner>,
    phantom: ::std::marker::PhantomData<(B, C)>,
}

struct ChunkedReadFileInner {
    len: u64,
    inode: u64,
    mtime: i64,
    mtime_nsec: i64,
    f: std::fs::File,
    pool: CpuPool,
    content_type: ::mime::Mime,
}

impl<B, C> ChunkedReadFile<B, C> {
    /// Creates a new ChunkedReadFile.
    ///
    /// `read(2)` calls will be performed on the supplied `pool` so that they don't block the
    /// tokio reactor thread on local disk I/O. Note that `File::open` and this constructor
    /// (specifically, its call to `fstat(2)`) may also block, so they typically shouldn't be
    /// called on the tokio reactor either.
    pub fn new(file: ::std::fs::File, pool: CpuPool, content_type: ::mime::Mime)
               -> Result<Self, Error> {
        let m = file.metadata()?;
        Ok(ChunkedReadFile{
            inner: Arc::new(ChunkedReadFileInner{
                len: m.len(),
                inode: m.ino(),
                mtime: m.mtime(),
                mtime_nsec: m.mtime_nsec(),
                f: file,
                pool: pool,
                content_type: content_type,
            }),
            phantom: ::std::marker::PhantomData,
        })
    }
}

impl<B, C> Entity<B> for ChunkedReadFile<B, C>
where B: 'static + Send + From<BoxStream<C, Error>>,
      C: 'static + Send + From<Vec<u8>> {
    fn len(&self) -> u64 { self.inner.len }

    fn get_range(&self, range: Range<u64>) -> B {
        // Tell the operating system that this fd will be used to read the given range sequentially.
        // This may encourage it to do readahead.
        let fadvise_ret = unsafe {
            ::libc::posix_fadvise(self.inner.f.as_raw_fd(), range.start as i64,
                                  (range.end - range.start) as i64, libc::POSIX_FADV_SEQUENTIAL)
        };
        if fadvise_ret != 0 {
            warn!("posix_fadvise failed: {}", ::std::io::Error::from_raw_os_error(fadvise_ret));
        }

        // This stream breaks apart the file into chunks of at most CHUNK_SIZE. This size is
        // a tradeoff between memory usage and thread handoffs.
        static CHUNK_SIZE: u64 = 65536;
        ::futures::stream::unfold((range, self.inner.clone()), move |(left, inner)| {
            if left.start == left.end { return None };
            let p = inner.pool.clone();
            Some(p.spawn_fn(move || {
                let chunk_size = ::std::cmp::min(CHUNK_SIZE, left.end - left.start) as usize;
                let mut chunk = Vec::with_capacity(chunk_size);
                unsafe { chunk.set_len(chunk_size) };
                let bytes_read = inner.f.read_at(&mut chunk, left.start)?;
                chunk.truncate(bytes_read);
                Ok((chunk.into(), (left.start + bytes_read as u64 .. left.end, inner)))
            }))
        }).boxed().into()
    }

    fn add_headers(&self, h: &mut header::Headers) {
        h.set(header::ContentType(self.inner.content_type.clone()));
    }

    fn etag(&self) -> Option<header::EntityTag> {
        // This etag format is similar to Apache's, with more mtime precision.
        // The etag should change if the file is modified or replaced. The length is probably
        // redundant but doesn't harm anything.
        Some(header::EntityTag::strong(format!("{:x}:{:x}:{:x}:{:x}", self.inner.inode,
                                               self.inner.len, self.inner.mtime,
                                               self.inner.mtime_nsec))) }

    fn last_modified(&self) -> Option<header::HttpDate> {
        Some(header::HttpDate(time::at_utc(time::Timespec{
            sec: self.inner.mtime,
            nsec: self.inner.mtime_nsec as i32,
        })))
    }
}
