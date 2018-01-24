// Copyright (c) 2016-2018 Scott Lamb <slamb@slamb.org>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![feature(test)]

extern crate futures;
extern crate http_serve;
extern crate hyper;
#[macro_use]
extern crate lazy_static;
extern crate mime;
extern crate reqwest;
extern crate test;

use http_serve::streaming_body;
use hyper::Error;
use hyper::server::{Request, Response};
use futures::stream::Stream;
use std::io::{Read, Write};
use std::str::FromStr;

static WONDERLAND: &[u8] = include_bytes!("wonderland.txt");

struct MyService;

impl hyper::server::Service for MyService {
    type Request = Request;
    type Response = Response<Box<Stream<Item = Vec<u8>, Error = Error> + Send>>;
    type Error = Error;
    type Future = futures::future::FutureResult<Self::Response, Error>;

    fn call(&self, req: Request) -> Self::Future {
        let mut resp = Response::new();
        let l = u32::from_str(&req.uri().path()[1..]).unwrap();
        if let Some(mut w) = streaming_body(&req, &mut resp).with_gzip_level(l).build() {
            w.write_all(WONDERLAND).unwrap();
        }
        futures::future::ok(resp)
    }
}

/// Returns the hostport of a newly created, never-destructed server.
fn new_server() -> String {
    let (tx, rx) = ::std::sync::mpsc::channel();
    ::std::thread::spawn(move || {
        let addr = "127.0.0.1:0".parse().unwrap();
        let server = hyper::server::Http::new()
            .bind(&addr, || Ok(MyService))
            .unwrap();
        let addr = server.local_addr().unwrap();
        tx.send(addr).unwrap();
        server.run().unwrap()
    });
    let addr = rx.recv().unwrap();
    format!("http://{}:{}", addr.ip(), addr.port())
}

lazy_static! {
    static ref SERVER: String = { new_server() };
}

fn serve(b: &mut test::Bencher, level: u32) {
    let client = reqwest::Client::new();

    // Add enough buffer space for the uncompressed representation and some extra header stuff.
    // Should be plenty for effective or ineffective compression.
    let mut buf = Vec::with_capacity(WONDERLAND.len());
    let mut run = || {
        let mut resp = client.get(&format!("{}/{}", &*SERVER, level)).send().unwrap();
        buf.clear();
        let size = resp.read_to_end(&mut buf).unwrap();
        assert_eq!(reqwest::StatusCode::Ok, resp.status());
        assert_eq!(size, WONDERLAND.len());
    };
    run(); // warm.
    b.iter(run);
    b.bytes = WONDERLAND.len() as u64;
}

#[bench]
fn serve_gzip_level_0(b: &mut test::Bencher) { serve(b, 0); }

#[bench]
fn serve_gzip_level_1(b: &mut test::Bencher) { serve(b, 1); }

#[bench]
fn serve_gzip_level_2(b: &mut test::Bencher) { serve(b, 2); }

#[bench]
fn serve_gzip_level_3(b: &mut test::Bencher) { serve(b, 3); }

#[bench]
fn serve_gzip_level_4(b: &mut test::Bencher) { serve(b, 4); }

#[bench]
fn serve_gzip_level_5(b: &mut test::Bencher) { serve(b, 5); }

#[bench]
fn serve_gzip_level_6(b: &mut test::Bencher) { serve(b, 6); }

#[bench]
fn serve_gzip_level_7(b: &mut test::Bencher) { serve(b, 7); }

#[bench]
fn serve_gzip_level_8(b: &mut test::Bencher) { serve(b, 8); }

#[bench]
fn serve_gzip_level_9(b: &mut test::Bencher) { serve(b, 9); }
