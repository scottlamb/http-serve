// Copyright (c) 2016-2018 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Benchmarks of serving data built in to the binary via `include_bytes!`, using both the
//! `serve` function on an `Entity` and the `streaming_body` method.

#[macro_use]
extern crate criterion;
extern crate bytes;
extern crate futures;
extern crate http;
extern crate http_serve;
extern crate hyper;
#[macro_use]
extern crate lazy_static;
extern crate mime;
extern crate reqwest;
extern crate tokio;

use bytes::{Bytes, BytesMut};
use criterion::Criterion;
use futures::{future, stream};
use futures::{Future, Stream};
use http::header::HeaderValue;
use http::{Request, Response};
use http_serve::streaming_body;
use hyper::Body;
use std::io::{Read, Write};
use std::ops::Range;
use std::str::FromStr;
use std::time::SystemTime;

static WONDERLAND: &[u8] = include_bytes!("wonderland.txt");

struct BytesEntity(Bytes);

impl http_serve::Entity for BytesEntity {
    type Data = hyper::Chunk;
    type Error = Box<::std::error::Error + Send + Sync>;

    fn len(&self) -> u64 {
        self.0.len() as u64
    }
    fn get_range(
        &self,
        range: Range<u64>,
    ) -> Box<Stream<Item = Self::Data, Error = Self::Error> + Send> {
        Box::new(stream::once(Ok(self
            .0
            .slice(range.start as usize, range.end as usize)
            .into())))
    }
    fn add_headers(&self, headers: &mut http::header::HeaderMap) {
        headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain"),
        );
    }
    fn etag(&self) -> Option<HeaderValue> {
        None
    }
    fn last_modified(&self) -> Option<SystemTime> {
        None
    }
}

fn serve_req(req: Request<Body>) -> Response<Body> {
    let path = req.uri().path();
    match path.as_bytes()[1] {
        b's' => {
            // static entity
            http_serve::serve(BytesEntity(Bytes::from_static(WONDERLAND)), &req)
        }
        b'c' => {
            // copied entity
            let mut b = BytesMut::with_capacity(WONDERLAND.len());
            b.extend_from_slice(WONDERLAND);
            http_serve::serve(BytesEntity(b.freeze()), &req)
        }
        b'b' => {
            // chunked, data written before returning the Response.
            let l = u32::from_str(&path[2..]).unwrap();
            let (resp, w) = streaming_body(&req).with_gzip_level(l).build();
            if let Some(mut w) = w {
                w.write_all(WONDERLAND).unwrap();
            }
            resp
        }
        b'a' => {
            // chunked, data written after returning the Response.
            let l = u32::from_str(&path[2..]).unwrap();
            let (resp, w) = streaming_body(&req).with_gzip_level(l).build();
            tokio::spawn(future::lazy(|| {
                if let Some(mut w) = w {
                    w.write_all(WONDERLAND).unwrap();
                }
                Ok(())
            }));
            resp
        }
        _ => unreachable!(),
    }
}

/// Returns the hostport of a newly-created, never-destructed server.
fn new_server() -> String {
    let (tx, rx) = ::std::sync::mpsc::channel();
    ::std::thread::spawn(move || {
        let addr = "127.0.0.1:0".parse().unwrap();
        let server =
            hyper::server::Server::bind(&addr).serve(|| hyper::service::service_fn_ok(serve_req));
        let addr = server.local_addr();
        tx.send(addr).unwrap();
        tokio::run(server.map_err(|e| panic!(e)))
    });
    let addr = rx.recv().unwrap();
    format!("http://{}:{}", addr.ip(), addr.port())
}

lazy_static! {
    static ref SERVER: String = { new_server() };
}

fn serve(b: &mut criterion::Bencher, path: &str) {
    let client = reqwest::Client::new();

    // Add enough buffer space for the uncompressed representation and some extra header stuff.
    // Should be plenty for effective or ineffective compression.
    let mut buf = Vec::with_capacity(WONDERLAND.len());
    let mut run = || {
        let mut resp = client
            .get(&format!("{}/{}", &*SERVER, path))
            .send()
            .unwrap();
        buf.clear();
        let size = resp.read_to_end(&mut buf).unwrap();
        assert_eq!(http::StatusCode::OK, resp.status());
        assert_eq!(size, WONDERLAND.len());
    };
    run(); // warm.
    b.iter(run);
}

fn criterion_benchmark(c: &mut Criterion) {
    c.bench("serve",
            criterion::Benchmark::new("static", |b| serve(b, "s"))
            .throughput(criterion::Throughput::Bytes(WONDERLAND.len() as u32)));
    c.bench("serve",
            criterion::Benchmark::new("copied", |b| serve(b, "c"))
            .throughput(criterion::Throughput::Bytes(WONDERLAND.len() as u32)));
    c.bench("serve_chunked_before_gzip",
            criterion::ParameterizedBenchmark::new("level",
                                                   |b, p| serve(b, &format!("b{}", p)),
                                                   0..=9)
            .throughput(|_| criterion::Throughput::Bytes(WONDERLAND.len() as u32)));
    c.bench("serve_chunked_after_gzip",
            criterion::ParameterizedBenchmark::new("level",
                                                   |b, p| serve(b, &format!("a{}", p)),
                                                   0..=9)
            .throughput(|_| criterion::Throughput::Bytes(WONDERLAND.len() as u32)));
}

criterion_group!{
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = criterion_benchmark
}
criterion_main!(benches);
