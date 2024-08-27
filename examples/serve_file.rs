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
//! ```text
//! $ curl --head http://127.0.0.1:1337/
//! $ curl http://127.0.0.1:1337 > /dev/null
//! $ curl -v -H 'Range: bytes=1-10' http://127.0.0.1:1337/
//! $ curl -v -H 'Range: bytes=1-10,30-40' http://127.0.0.1:1337/
//! ```

use std::net::{Ipv4Addr, SocketAddr};

use bytes::Bytes;
use http::{
    header::{self, HeaderMap, HeaderValue},
    Request, Response,
};
use http_serve::ChunkedReadFile;
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

struct Context {
    path: std::ffi::OsString,
}

type BoxError = Box<dyn std::error::Error + Send + Sync>;

async fn serve(
    ctx: &'static Context,
    req: Request<hyper::body::Incoming>,
) -> Result<Response<http_serve::Body>, BoxError> {
    let f = tokio::task::block_in_place::<_, Result<ChunkedReadFile<Bytes, BoxError>, BoxError>>(
        move || {
            let f = std::fs::File::open(&ctx.path)?;
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            Ok(ChunkedReadFile::new(f, headers)?)
        },
    )?;
    Ok(http_serve::serve(f, &req))
}

#[tokio::main]
async fn main() -> Result<(), BoxError> {
    let mut args = std::env::args_os();
    if args.len() != 2 {
        eprintln!("Expected serve [FILENAME]");
        std::process::exit(1);
    }
    let path = args.nth(1).unwrap();

    let ctx: &'static Context = Box::leak(Box::new(Context { path: path }));

    let addr: SocketAddr = (Ipv4Addr::LOCALHOST, 1337).into();
    let listener = TcpListener::bind(addr).await?;
    println!("Serving {} on http://{}", ctx.path.to_string_lossy(), addr,);
    loop {
        let (tcp, _) = listener.accept().await?;
        let io = TokioIo::new(tcp);
        tokio::task::spawn(async move {
            if let Err(e) = http1::Builder::new()
                .serve_connection(io, service_fn(move |req| serve(ctx, req)))
                .await
            {
                eprintln!("Error serving connection: {}", e);
            }
        });
    }
}
