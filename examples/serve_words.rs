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

//! Test program which serves `/usr/share/dict/words` on `http://127.0.0.1:1337/`.
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

extern crate futures;
extern crate futures_cpupool;
extern crate http_entity;
extern crate http_entity_static;
extern crate hyper;
#[macro_use] extern crate lazy_static;
#[macro_use] extern crate mime;
extern crate tokio_core;

use hyper::server::{Request, Response};
use futures::Future;
use futures_cpupool::CpuPool;
use std::sync::Mutex;
use tokio_core::reactor::Remote;

lazy_static! {
    static ref POOL: CpuPool = CpuPool::new(1);
    static ref REACTOR: Mutex<Option<Remote>> = { Mutex::new(None) };
}

static FILE: &'static str = "/usr/share/dict/words";

struct MyService;

impl hyper::server::Service for MyService {
    type Request = Request;
    type Response = Response;
    type Error = hyper::Error;
    type Future = ::futures::BoxFuture<Response, hyper::Error>;

    fn call(&self, req: Request) -> Self::Future {
        if req.path() != "/" {
            let resp = Response::new().with_status(hyper::StatusCode::NotFound);
            return futures::future::ok(resp).boxed();
        }
        POOL.spawn_fn(move || {
            let f = ::std::fs::File::open(FILE)?;
            let l = REACTOR.lock().unwrap();
            let r: &Remote = l.as_ref().unwrap();
            let f = http_entity_static::File::new(f, POOL.clone(), r.clone(), mime!(Text/Plain))?;
            Ok(http_entity::serve(r, f, &req))
        }).boxed()
    }
}

fn main() {
    let addr = "127.0.0.1:1337".parse().unwrap();
    let server = hyper::server::Http::new().bind(&addr, || Ok(MyService)).unwrap();
    let handle = server.handle();
    *REACTOR.lock().unwrap() = Some(handle.remote().clone());
    println!("Serving {} on http://{} with 1 thread.", FILE, server.local_addr().unwrap());
    server.run().unwrap();
}
