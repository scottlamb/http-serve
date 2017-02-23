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

#[macro_use] extern crate log;
extern crate http_entity;
extern crate hyper;
extern crate libc;
extern crate mime;
extern crate time;

use http_entity::Entity;
use hyper::header;
use std::io;
use std::ops::Range;
use std::os::unix::io::AsRawFd;
use std::os::unix::fs::{FileExt, MetadataExt};

/// A HTTP entity created from a `std::fs::File` which reads and copies the file chunk-by-chunk.
pub struct ChunkedReadFile {
    len: u64,
    inode: u64,
    mtime: i64,
    mtime_nsec: i64,
    content_type: ::mime::Mime,
    f: std::fs::File,
}

impl ChunkedReadFile {
    pub fn new(file: ::std::fs::File, content_type: ::mime::Mime) -> Result<Self, io::Error> {
        let m = file.metadata()?;
        Ok(ChunkedReadFile{
            len: m.len(),
            inode: m.ino(),
            mtime: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
            content_type: content_type,
            f: file,
        })
    }
}

impl Entity<io::Error> for ChunkedReadFile {
    fn len(&self) -> u64 { self.len }

    fn write_to(&self, range: Range<u64>, out: &mut io::Write) -> Result<(), io::Error> {
        // This stream breaks apart the file into chunks of at most CHUNK_SIZE.
        static CHUNK_SIZE: u64 = 65536;

        if range.end - range.start > CHUNK_SIZE {
            // Tell the operating system that this fd will be used to read the given range
            // sequentially. This may encourage it to do (more) readahead.
            let fadvise_ret = unsafe {
                ::libc::posix_fadvise(self.f.as_raw_fd(), range.start as i64,
                                      (range.end - range.start) as i64, libc::POSIX_FADV_SEQUENTIAL)
            };
            if fadvise_ret != 0 {
                warn!("posix_fadvise failed: {}", ::std::io::Error::from_raw_os_error(fadvise_ret));
            }
        }

        let mut left = range;
        let mut chunk = Vec::new();
        while left.start < left.end {
            let chunk_size = ::std::cmp::min(CHUNK_SIZE, left.end - left.start) as usize;
            if chunk.capacity() < chunk_size {
                let needed = chunk_size - chunk.capacity();
                chunk.reserve(needed);
            }
            unsafe { chunk.set_len(chunk_size) };
            let bytes_read = self.f.read_at(&mut chunk, left.start)?;
            out.write_all(&chunk[0 .. bytes_read])?;
            left.start += bytes_read as u64;
        }
        Ok(())
    }

    fn add_headers(&self, h: &mut header::Headers) {
        h.set(header::ContentType(self.content_type.clone()));
    }

    fn etag(&self) -> Option<header::EntityTag> {
        // This etag format is similar to Apache's, with more mtime precision.
        // The etag should change if the file is modified or replaced. The length is probably
        // redundant but doesn't harm anything.
        Some(header::EntityTag::strong(format!("{:x}:{:x}:{:x}:{:x}", self.inode,
                                               self.len, self.mtime,
                                               self.mtime_nsec)))
    }

    fn last_modified(&self) -> Option<header::HttpDate> {
        Some(header::HttpDate(time::at_utc(time::Timespec{
            sec: self.mtime,
            nsec: self.mtime_nsec as i32,
        })))
    }
}
