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

use hyper::{Error, StatusCode};
use hyper::server::{Request, Response};
use leak::Leak;
use futures::Future;
use futures::future;
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
        let (pool_constructor, pool_stream) = match req.path() {
            "/" | "/inline-inline" => (false, false),
            "/pool-inline" => (true, false),
            "/inline-pool" => (false, true),
            "/pool-pool" => (true, true),
            _ => return Box::new(future::ok(Response::new().with_status(StatusCode::NotFound))),
        };
        let ctx = self.0;
        let construction = move || {
            let f = ::std::fs::File::open(&ctx.path)?;
            let p = if pool_stream { Some(ctx.pool.clone()) } else { None };
            let f = http_file::ChunkedReadFile::new(f, p, mime::TEXT_PLAIN)?;
            Ok(http_entity::serve(f, &req))
        };
        if pool_constructor {
            Box::new(ctx.pool.spawn_fn(construction))
        } else {
            Box::new(future::result(construction()))
        }
    }
}

fn main() {
    let mut args = ::std::env::args_os();
    if args.len() != 2 {
        use ::std::io::Write;
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
    let server = hyper::server::Http::new().bind(&addr, move || Ok(MyService(ctx))).unwrap();
    println!("Serving {} on http://{} with 1 thread.",
             ctx.path.to_string_lossy(), server.local_addr().unwrap());
    server.run().unwrap();
}
