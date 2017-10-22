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
extern crate http_entity;
extern crate hyper;
extern crate libc;
#[cfg(target_os="linux")] #[macro_use] extern crate log;
extern crate mime;

use futures::{Sink, Stream};
use futures_cpupool::CpuPool;
use http_entity::Entity;
use hyper::header;
use std::io;
use std::ops::Range;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::Arc;
use std::time::{self, SystemTime};

// This stream breaks apart the file into chunks of at most CHUNK_SIZE. This size is
// a tradeoff between memory usage and thread handoffs.
static CHUNK_SIZE: u64 = 65536;

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
    mtime: SystemTime,
    content_type: ::mime::Mime,
    f: std::fs::File,
    pool: Option<CpuPool>,
}

impl<B, C> ChunkedReadFile<B, C> {
    /// Creates a new ChunkedReadFile.
    ///
    /// `read(2)` calls will be performed on the supplied `pool` so that they don't block the
    /// tokio reactor thread on local disk I/O. Note that `File::open` and this constructor
    /// (specifically, its call to `fstat(2)`) may also block, so they typically shouldn't be
    /// called on the tokio reactor either.
    pub fn new(file: ::std::fs::File, pool: Option<CpuPool>, content_type: ::mime::Mime)
               -> Result<Self, io::Error> {
        let m = file.metadata()?;
        Ok(ChunkedReadFile{
            inner: Arc::new(ChunkedReadFileInner{
                len: m.len(),
                inode: m.ino(),
                mtime: m.modified()?,
                content_type: content_type,
                f: file,
                pool: pool,
            }),
            phantom: ::std::marker::PhantomData,
        })
    }
}

impl<B, C> Entity for ChunkedReadFile<B, C>
where B: 'static + Send + Stream<Item = C, Error = hyper::Error> +
         From<Box<Stream<Item = C, Error = hyper::Error> + Send>>,
      C: 'static + Send + AsRef<[u8]> + From<Vec<u8>> + From<&'static [u8]> {
    type Chunk = C;
    type Body = B;

    fn len(&self) -> u64 { self.inner.len }

    fn get_range(&self, range: Range<u64>) -> B {
        maybe_fadvise(&self.inner.f, &range);
        let stream = ::futures::stream::unfold((range, self.inner.clone()), move |(left, inner)| {
            if left.start == left.end { return None }
            let chunk_size = ::std::cmp::min(CHUNK_SIZE, left.end - left.start) as usize;
            let mut chunk = Vec::with_capacity(chunk_size);
            unsafe { chunk.set_len(chunk_size) };
            let bytes_read = match inner.f.read_at(&mut chunk, left.start) {
                Err(e) => return Some(Err(e.into())),
                Ok(b) => b,
            };
            chunk.truncate(bytes_read);
            Some(Ok((chunk.into(), (left.start + bytes_read as u64 .. left.end, inner))))
        });

        let stream: Box<Stream<Item = C, Error = hyper::Error> + Send> = match self.inner.pool {
            Some(ref p) => {
                let (snd, rcv) = ::futures::sync::mpsc::channel(0);
                p.spawn(snd.send_all(stream.then(|i| Ok(i))))
                 .forget();
                Box::new(rcv.map_err(|()| unreachable!())
                            .and_then(|r| ::futures::future::result(r)))
            },
            None => Box::new(stream),
        };
        stream.into()
    }

    fn add_headers(&self, h: &mut header::Headers) {
        h.set(header::ContentType(self.inner.content_type.clone()));
    }

    fn etag(&self) -> Option<header::EntityTag> {
        // This etag format is similar to Apache's. The etag should change if the file is modified
        // or replaced. The length is probably redundant but doesn't harm anything.
        let dur = self.inner.mtime.duration_since(time::UNIX_EPOCH)
            .expect("modification time must be after epoch");
        Some(header::EntityTag::strong(format!("{:x}:{:x}:{:x}:{:x}", self.inner.inode,
                                               self.inner.len, dur.as_secs(), dur.subsec_nanos())))
    }

    fn last_modified(&self) -> Option<header::HttpDate> { Some(self.inner.mtime.into()) }
}

#[cfg(target_os="linux")]
#[inline(always)]
fn maybe_fadvise(f: &std::fs::File, range: &Range<u64>) {
    use std::os::unix::io::AsRawFd;
    if range.end - range.start <= CHUNK_SIZE {
        return;
    }
    // Tell the operating system that this fd will be used to read the given range
    // sequentially. This may encourage it to do (more) readahead.
    let fadvise_ret = unsafe {
        ::libc::posix_fadvise(f.as_raw_fd(), range.start as i64,
                              (range.end - range.start) as i64, libc::POSIX_FADV_SEQUENTIAL)
    };
    if fadvise_ret != 0 {
        warn!("posix_fadvise failed: {}", ::std::io::Error::from_raw_os_error(fadvise_ret));
    }
}

#[cfg(not(target_os="linux"))]
#[inline(always)]
fn maybe_fadvise(_f: &std::fs::File, _range: &Range<u64>) {}
