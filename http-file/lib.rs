// Copyright (c) 2016-2018 Scott Lamb <slamb@slamb.org>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate futures;
extern crate futures_cpupool;
extern crate http_entity;
extern crate hyper;
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
static CHUNK_SIZE: u64 = 65_536;

/// A HTTP entity created from a `std::fs::File` which reads the file
/// chunk-by-chunk on a `CpuPool`.
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
        let stream = ::futures::stream::unfold((range, Arc::clone(&self.inner)),
                                               move |(left, inner)| {
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
                p.spawn(snd.send_all(stream.then(Ok)))
                 .forget();
                Box::new(rcv.map_err(|()| unreachable!())
                            .and_then(::futures::future::result))
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
