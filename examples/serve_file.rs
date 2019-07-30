// Copyright (c) 2016-2018 The http-serve developers
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
extern crate http;
extern crate http_serve;
extern crate hyper;
extern crate leak;
extern crate mime;
extern crate tokio;
extern crate tokio_threadpool;

use futures::Future;
use http::{Request, Response};
use http_serve::ChunkedReadFile;
use hyper::Body;
use leak::Leak;

struct Context {
    path: ::std::ffi::OsString,
}

fn serve(
    ctx: &'static Context,
    req: Request<Body>,
) -> impl Future<Item = Response<Body>, Error = Box<::std::error::Error + Send + Sync + 'static>> {
    futures::future::poll_fn(move || {
        tokio_threadpool::blocking(move || {
            let f = ::std::fs::File::open(&ctx.path)?;
            let headers = http::header::HeaderMap::new();
            Ok(ChunkedReadFile::new(f, headers)?)
        })
    })
    .map_err(|_: tokio_threadpool::BlockingError| panic!("BlockingError on thread pool"))
    .and_then(::futures::future::result)
    .and_then(move |f| Ok(http_serve::serve(f, &req)))
}

fn main() {
    let mut args = ::std::env::args_os();
    if args.len() != 2 {
        eprintln!("Expected serve [FILENAME]");
        ::std::process::exit(1);
    }
    let path = args.nth(1).unwrap();

    let ctx = Box::new(Context { path: path }).leak();

    env_logger::init();
    let addr = "127.0.0.1:1337".parse().unwrap();
    let server = hyper::server::Server::bind(&addr)
        .serve(move || hyper::service::service_fn(move |req| serve(ctx, req)));
    println!(
        "Serving {} on http://{} with 1 thread.",
        ctx.path.to_string_lossy(),
        server.local_addr()
    );
    tokio::run(server.map_err(|e| eprintln!("server error: {}", e)))
}
