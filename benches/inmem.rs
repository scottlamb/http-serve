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
extern crate env_logger;
extern crate futures;
extern crate http;
extern crate http_serve;
extern crate hyper;
#[macro_use]
extern crate lazy_static;
extern crate mime;
extern crate socket2;
extern crate tokio;

use bytes::{Bytes, BytesMut};
use criterion::{Benchmark, Criterion, ParameterizedBenchmark, Throughput};
use futures::{future, stream};
use futures::{Future, Stream};
use http::header::HeaderValue;
use http::{Request, Response};
use http_serve::streaming_body;
use hyper::Body;
use std::convert::TryInto;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::ops::Range;
use std::str::FromStr;
use std::time::{Duration, SystemTime};

static WONDERLAND: &[u8] = include_bytes!("wonderland.txt");

struct BytesEntity(Bytes);

impl http_serve::Entity for BytesEntity {
    type Data = hyper::Chunk;
    type Error = Box<dyn std::error::Error + Send + Sync>;

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
            let colon = path.find(':').unwrap();
            let s = usize::from_str(&path[2..colon]).unwrap();
            let l = u32::from_str(&path[colon+1..]).unwrap();
            let (resp, w) = streaming_body(&req).with_chunk_size(s).with_gzip_level(l).build();
            if let Some(mut w) = w {
                w.write_all(WONDERLAND).unwrap();
            }
            resp
        }
        b'a' => {
            // chunked, data written after returning the Response.
            let colon = path.find(':').unwrap();
            let s = usize::from_str(&path[2..colon]).unwrap();
            let l = u32::from_str(&path[colon+1..]).unwrap();
            let (resp, w) = streaming_body(&req).with_chunk_size(s).with_gzip_level(l).build();
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
fn new_server() -> SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let addr = "127.0.0.1:0".parse().unwrap();
        let server =
            hyper::server::Server::bind(&addr)
            .tcp_nodelay(true)
            .serve(|| hyper::service::service_fn_ok(serve_req));
        let addr = server.local_addr();
        tx.send(addr).unwrap();
        tokio::run(server.map_err(|e| panic!(e)))
    });
    rx.recv().unwrap()
}

lazy_static! {
    static ref SERVER: SocketAddr = { new_server() };
}

/// Benchmarks a `GET` request for the given path using raw sockets.
///
/// This avoids using `reqwest` to keep the measurement as focused on the server-side performance
/// as possible. It uses keepalives, not only to focus the measurement on the request but also
/// to avoid errors due to ephemeral port exhaustion. This requires some parsing with `httparse`
/// to read the correct amount of data.
fn get(b: &mut criterion::Bencher, path: &str) {
    let _ = env_logger::try_init();
    let mut v = Vec::new();
    v.extend(b"GET /");
    v.extend(path.as_bytes());
    v.extend(&b" HTTP/1.1\r\nHost: localhost\r\nAccept-Encoding: gzip\r\n\r\n"[..]);

    // Add enough buffer space for the uncompressed representation and some headers.
    let mut buf = vec![0u8; WONDERLAND.len() + 8192];

    use socket2::{Domain, Socket, Type};
    let s = Socket::new(Domain::ipv4(), Type::stream(), None).unwrap();
    s.set_reuse_address(true).unwrap();
    s.set_nodelay(true).unwrap();
    s.connect(&(*SERVER).into()).unwrap();
    let mut s = s.into_tcp_stream();
    b.iter(move || {
        s.write_all(&v[..]).unwrap();

        let mut hdrs_buf = [httparse::EMPTY_HEADER; 16];
        let mut resp = httparse::Response::new(&mut hdrs_buf);
        let mut end = s.read(&mut buf[..]).unwrap();
        let mut pos = resp.parse(&buf[..end]).unwrap().unwrap();
        assert_eq!(resp.code, Some(200));
        let mut hdr_len: Option<usize> = None;
        let mut chunked = false;
        for h in resp.headers {
            if h.name.eq_ignore_ascii_case("Content-Length") {
                assert!(!chunked);
                assert!(hdr_len.is_none());
                hdr_len = Some(std::str::from_utf8(h.value).unwrap().parse().unwrap());
            } else if h.name.eq_ignore_ascii_case("Transfer-Encoding") {
                assert!(!chunked);
                assert!(hdr_len.is_none());
                assert!(h.value == b"chunked");
                chunked = true;
            }
        }
        if let Some(l) = hdr_len {
            assert!(end <= pos + l, "end={} pos={} l={}", end, pos, l);
            if end < pos + l {
                s.read_exact(&mut buf[end..pos + l]).unwrap();
            }
            return;
        } else if !chunked {
            panic!("not chunked, no length");
        }
        loop {
            let r = match httparse::parse_chunk_size(&buf[pos..end]) {
                Err(e) => panic!("error={} pos={} end={} buf[pos..end]={:?}",
                                 e, pos, end, String::from_utf8_lossy(&buf[pos..end])),
                Ok(r) => r,
            };
            match r {
                httparse::Status::Partial => {
                    end += s.read(&mut buf[end..]).unwrap();
                    continue;
                },
                httparse::Status::Complete((p, l)) => {
                    let l: usize = l.try_into().unwrap();
                    pos += p;
                    while end < pos + l + 2 {
                        end += s.read(&mut buf[end..]).unwrap();
                    }
                    assert_eq!(buf[pos + l .. pos + l + 2], b"\r\n"[..]);
                    if l == 0 {
                        assert!(end == pos + l + 2);
                        return;
                    }
                    pos += l + 2;
                },
            }
        }
    });
}

fn criterion_benchmark(c: &mut Criterion) {
    c.bench(
        "serve",
        Benchmark::new("static", |b| get(b, "s"))
            .throughput(Throughput::Bytes(WONDERLAND.len() as u32)),
    );
    c.bench(
        "serve",
        Benchmark::new("copied", |b| get(b, "c"))
            .throughput(Throughput::Bytes(WONDERLAND.len() as u32)),
    );
    c.bench(
        "streaming_body_before",
        ParameterizedBenchmark::new("gzip", |b, p| get(b, &format!("b4096:{}", p)), 0..=9)
            .throughput(|_| Throughput::Bytes(WONDERLAND.len() as u32)),
    );
    c.bench(
        "streaming_body_after",
        ParameterizedBenchmark::new("gzip", |b, p| get(b, &format!("a4096:{}", p)), 0..=9)
            .throughput(|_| Throughput::Bytes(WONDERLAND.len() as u32)),
    );

    // Also benchmark larger chunksizes, but only with gzip level 0 (disabled). The chunk size
    // difference is dwarfed by gzip overhead. When not gzipping, it makes a noticeable difference,
    // probably for two reasons:
    //
    // * more writes. hyper (as of 0.12.33) forces a flush every 16 buffers (see
    //   hyper::proto::h1::io::MAX_BUF_LIST_BUFFERS), so to write the entire request in one writev
    //   call, chunks must be at least 1/16th of the total file size.
    //
    // * more memory allocations.
    c.bench(
        "streaming_body_before",
        ParameterizedBenchmark::new("chunksize", |b, p| get(b, &format!("b{}:0", p)),
                                    &[4096, 16384, 65536, 1048576])
            .throughput(|_| Throughput::Bytes(WONDERLAND.len() as u32)),
    );
    c.bench(
        "streaming_body_after",
        ParameterizedBenchmark::new("chunksize", |b, p| get(b, &format!("a{}:0", p)),
                                    &[4096, 16384, 65536, 1048576])
            .throughput(|_| Throughput::Bytes(WONDERLAND.len() as u32)),
    );
}

criterion_group! {
    name = benches;

    // Tweak the config to run more quickly; there are a lot of bench cases here.
    config = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_millis(100))
        .measurement_time(Duration::from_secs(1));
    targets = criterion_benchmark
}
criterion_main!(benches);
