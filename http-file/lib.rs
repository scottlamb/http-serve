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
extern crate tokio_core;

use futures::{Future, Sink, Stream};
use futures_cpupool::CpuPool;
use http_entity::Entity;
use hyper::Error;
use hyper::header;
use std::ops::Range;
use std::os::unix::io::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::Arc;
use tokio_core::reactor;

/// A HTTP entity created from a `std::fs::File`.
#[derive(Clone)]
pub struct File(Arc<FileInner>);

struct FileInner {
    len: u64,
    inode: u64,
    mtime: i64,
    mtime_nsec: i64,
    f: std::fs::File,
    pool: CpuPool,
    remote: reactor::Remote,
    content_type: ::mime::Mime,
}

impl File {
    /// Creates a new File.
    ///
    /// `read(2)` calls will be performed on the supplied `pool` so that they don't block the
    /// tokio reactor thread on local disk I/O. Note that `File::open` and this constructor
    /// (specifically, its call to `fstat(2)`) may also block, so they typically shouldn't be
    /// called on the tokio reactor either.
    pub fn new(file: ::std::fs::File, pool: CpuPool, remote: reactor::Remote,
               content_type: ::mime::Mime) -> Result<Self, Error> {
        let m = file.metadata()?;
        Ok(File(Arc::new(FileInner{
            len: m.len(),
            inode: m.ino(),
            mtime: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
            f: file,
            pool: pool,
            remote: remote,
            content_type: content_type,
        })))
    }
}

impl Entity for File {
    fn len(&self) -> u64 { self.0.len }

    fn get_range(&self, range: Range<u64>) -> hyper::Body {
        // Tell the operating system that this fd will be used to read the given range sequentially.
        // This may encourage it to do readahead.
        let fadvise_ret = unsafe {
            ::libc::posix_fadvise(self.0.f.as_raw_fd(), range.start as i64,
                                  (range.end - range.start) as i64, libc::POSIX_FADV_SEQUENTIAL)
        };
        if fadvise_ret != 0 {
            warn!("posix_fadvise failed: {}", ::std::io::Error::from_raw_os_error(fadvise_ret));
        }
        let (sender, body) = hyper::Body::pair();

        // This stream breaks apart the file into chunks of at most CHUNK_SIZE. This size is
        // a tradeoff between memory usage and thread handoffs.
        static CHUNK_SIZE: u64 = 65536;
        let stream = ::futures::stream::unfold((range, self.0.clone()), move |(left, inner)| {
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
        });

        self.0.remote.spawn(|_h| {
            sender.send_all(stream.then(|i| ::futures::future::ok(i)))
                  .map(|_| ())
                  .map_err(|_| ())
        });
        body
    }

    fn add_headers(&self, h: &mut header::Headers) {
        h.set(header::ContentType(self.0.content_type.clone()));
    }

    fn etag(&self) -> Option<header::EntityTag> {
        // This etag format is similar to Apache's, with more mtime precision.
        // The etag should change if the file is modified or replaced. The length is probably
        // redundant but doesn't harm anything.
        Some(header::EntityTag::strong(format!("{:x}:{:x}:{:x}:{:x}", self.0.inode, self.0.len,
                                               self.0.mtime, self.0.mtime_nsec)))
    }

    fn last_modified(&self) -> Option<header::HttpDate> {
        Some(header::HttpDate(time::at_utc(time::Timespec{
            sec: self.0.mtime,
            nsec: self.0.mtime_nsec as i32,
        })))
    }
}
