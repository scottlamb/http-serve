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

use bytes::Bytes;
use http::{Request, Response};
use http_serve::ChunkedReadFile;
use hyper::service::{make_service_fn, service_fn};
use hyper::Body;

struct Context {
    path: std::ffi::OsString,
}

type BoxedError = Box<dyn std::error::Error + Send + Sync>;

async fn serve(ctx: &'static Context, req: Request<Body>) -> Result<Response<Body>, BoxedError> {
    let f = tokio::task::block_in_place::<_, Result<ChunkedReadFile<Bytes, BoxedError>, BoxedError>>(
        move || {
            let f = std::fs::File::open(&ctx.path)?;
            let headers = http::header::HeaderMap::new();
            Ok(ChunkedReadFile::new(f, headers)?)
        },
    )?;
    Ok(http_serve::serve(f, &req))
}

#[tokio::main]
async fn main() -> Result<(), BoxedError> {
    let mut args = std::env::args_os();
    if args.len() != 2 {
        eprintln!("Expected serve [FILENAME]");
        std::process::exit(1);
    }
    let path = args.nth(1).unwrap();

    let ctx: &'static Context = Box::leak(Box::new(Context { path: path }));

    env_logger::init();
    let addr = ([127, 0, 0, 1], 1337).into();
    let make_svc = make_service_fn(move |_conn| {
        futures::future::ok::<_, std::convert::Infallible>(service_fn(move |req| serve(ctx, req)))
    });
    let server = hyper::server::Server::bind(&addr).serve(make_svc);
    println!(
        "Serving {} on http://{} with 1 thread.",
        ctx.path.to_string_lossy(),
        server.local_addr()
    );
    server.await?;

    Ok(())
}
