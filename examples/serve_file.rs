// Copyright (c) 2016-2018 Scott Lamb <slamb@slamb.org>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Test program which serves a local file on `http://127.0.0.1:1337/`.
//!
//! Performs file IO on a separate thread pool from the reactor so that it doesn't block on
//! local disk. Supports HEAD, conditional GET, and byte range requests. Some commands to try:
//!
//! ```
//! $ curl --head http://127.0.0.1/
//! $ curl http://127.0.0.1:1337 > /dev/null
//! $ curl -v -H 'Range: bytes=1-10' http://127.0.0.1:1337/
//! $ curl -v -H 'Range: bytes=1-10,30-40' http://127.0.0.1:1337/
//! ```

extern crate env_logger;
extern crate futures;
extern crate futures_cpupool;
extern crate http_entity;
extern crate http_file;
extern crate hyper;
extern crate leak;
extern crate mime;

use hyper::Error;
use hyper::server::{Request, Response};
use leak::Leak;
use futures::Future;
use futures::stream::Stream;
use futures_cpupool::CpuPool;

struct Context {
    path: ::std::ffi::OsString,
    pool: CpuPool,
}

struct MyService(&'static Context);

impl hyper::server::Service for MyService {
    type Request = Request;
    type Response = Response<Box<Stream<Item = Vec<u8>, Error = Error> + Send>>;
    type Error = Error;
    type Future = Box<Future<Item = Self::Response, Error = Error>>;

    fn call(&self, req: Request) -> Self::Future {
        let ctx = self.0;
        Box::new(ctx.pool.spawn_fn(move || {
            let f = ::std::fs::File::open(&ctx.path)?;
            let f = http_file::ChunkedReadFile::new(f, Some(ctx.pool.clone()), mime::TEXT_PLAIN)?;
            Ok(http_entity::serve(f, &req))
        }))
    }
}

fn main() {
    let mut args = ::std::env::args_os();
    if args.len() != 2 {
        use std::io::Write;
        writeln!(&mut std::io::stderr(), "Expected serve [FILENAME]").unwrap();
        ::std::process::exit(1);
    }
    let path = args.nth(1).unwrap();

    let ctx = Box::new(Context {
        path: path,
        pool: CpuPool::new(1),
    }).leak();

    env_logger::init().unwrap();
    let addr = "127.0.0.1:1337".parse().unwrap();
    let server = hyper::server::Http::new()
        .bind(&addr, move || Ok(MyService(ctx)))
        .unwrap();
    println!(
        "Serving {} on http://{} with 1 thread.",
        ctx.path.to_string_lossy(),
        server.local_addr().unwrap()
    );
    server.run().unwrap();
}
