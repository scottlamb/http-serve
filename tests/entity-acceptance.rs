// Copyright (c) 2016-2018 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate env_logger;
extern crate futures;
extern crate http;
extern crate http_serve;
extern crate httpdate;
extern crate hyper;
#[macro_use]
extern crate lazy_static;
extern crate reqwest;
extern crate smallvec;
extern crate tokio;

use futures::stream;
use futures::{Future, Stream};
use http::header::{self, HeaderValue};
use http::{Request, Response};
use hyper::body::Body;
use std::io::Read;
use std::ops::Range;
use std::time::SystemTime;

static BODY: &'static [u8] =
    b"01234567890123456789012345678901234567890123456789012345678901234567890123456789\
      01234567890123456789012345678901234567890123456789012345678901234567890123456789\
      01234567890123456789012345678901234567890123456789012345678901234567890123456789";

struct FakeEntity {
    etag: Option<HeaderValue>,
    last_modified: SystemTime,
}

impl http_serve::Entity for &'static FakeEntity {
    type Data = hyper::Chunk;
    type Error = Box<::std::error::Error + Send + Sync>;

    fn len(&self) -> u64 {
        BODY.len() as u64
    }
    fn get_range(
        &self,
        range: Range<u64>,
    ) -> Box<Stream<Item = Self::Data, Error = Self::Error> + Send> {
        Box::new(stream::once(Ok(BODY
            [range.start as usize..range.end as usize]
            .into())))
    }
    fn add_headers(&self, headers: &mut http::header::HeaderMap) {
        headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        );
    }
    fn etag(&self) -> Option<HeaderValue> {
        self.etag.clone()
    }
    fn last_modified(&self) -> Option<SystemTime> {
        Some(self.last_modified)
    }
}

fn serve(req: Request<Body>) -> Response<Body> {
    let entity: &'static FakeEntity = match req.uri().path() {
        "/none" => &*ENTITY_NO_ETAG,
        "/strong" => &*ENTITY_STRONG_ETAG,
        "/weak" => &*ENTITY_WEAK_ETAG,
        p => panic!("unexpected path {}", p),
    };
    http_serve::serve(entity, &req)
}

fn new_server() -> String {
    let (tx, rx) = ::std::sync::mpsc::channel();
    ::std::thread::spawn(move || {
        let addr = "127.0.0.1:0".parse().unwrap();
        let server =
            hyper::server::Server::bind(&addr).serve(|| hyper::service::service_fn_ok(serve));
        tx.send(server.local_addr()).unwrap();
        tokio::run(server.map_err(|e| eprintln!("server error: {}", e)))
    });
    let addr = rx.recv().unwrap();
    format!("http://{}:{}", addr.ip(), addr.port())
}

const SOME_DATE_STR: &str = "Sun, 06 Nov 1994 08:49:37 GMT";
const LATER_DATE_STR: &str = "Sun, 06 Nov 1994 09:49:37 GMT";
const MIME: &str = "application/octet-stream";

lazy_static! {
    static ref SOME_DATE: SystemTime = httpdate::parse_http_date(SOME_DATE_STR).unwrap();
    static ref ENTITY_NO_ETAG: FakeEntity = FakeEntity {
        etag: None,
        last_modified: *SOME_DATE,
    };
    static ref ENTITY_STRONG_ETAG: FakeEntity = FakeEntity {
        etag: Some(HeaderValue::from_static("\"foo\"")),
        last_modified: *SOME_DATE,
    };
    static ref ENTITY_WEAK_ETAG: FakeEntity = FakeEntity {
        etag: Some(HeaderValue::from_static("W/\"foo\"")),
        last_modified: *SOME_DATE,
    };
    static ref SERVER: String = { new_server() };
}

#[test]
fn serve_without_etag() {
    let _ = env_logger::try_init();
    let client = reqwest::Client::new();
    let mut buf = Vec::new();
    let url = format!("{}/none", *SERVER);

    // Full body.
    let mut resp = client.get(&url).send().unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), MIME);
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // If-Match any should still send the full body.
    let mut resp = client
        .get(&url)
        .header("If-Match", "*")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), MIME);
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // If-Match by etag doesn't match (as this request has no etag).
    let resp = client
        .get(&url)
        .header("If-Match", "\"foo\"")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::PRECONDITION_FAILED, resp.status());

    // If-None-Match any.
    let mut resp = client
        .get(&url)
        .header("If-None-Match", "*")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::NOT_MODIFIED, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"", &buf[..]);

    // If-None-Match by etag doesn't match (as this request has no etag).
    let mut resp = client
        .get(&url)
        .header("If-None-Match", "\"foo\"")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), MIME);
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // Unmodified since supplied date.
    let mut resp = client
        .get(&url)
        .header("If-Modified-Since", SOME_DATE_STR)
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::NOT_MODIFIED, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"", &buf[..]);

    // Range serving - basic case.
    let mut resp = client
        .get(&url)
        .header("Range", "bytes=1-3")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::PARTIAL_CONTENT, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_RANGE).unwrap(),
               &format!("bytes 1-3/{}", BODY.len()));
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"123", &buf[..]);

    // Range serving - multiple ranges.
    let mut resp = client
        .get(&url)
        .header("Range", "bytes=0-1, 3-4")
        .send()
        .unwrap();
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    assert_eq!(reqwest::StatusCode::PARTIAL_CONTENT, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(),
               &"multipart/byteranges; boundary=B");
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(
        "\
         \r\n--B\r\n\
         Content-Range: bytes 0-1/240\r\n\
         content-type: application/octet-stream\r\n\
         \r\n\
         01\r\n\
         --B\r\n\
         Content-Range: bytes 3-4/240\r\n\
         content-type: application/octet-stream\r\n\
         \r\n\
         34\r\n\
         --B--\r\n"[..],
        String::from_utf8(buf.clone()).unwrap()
    );

    // Range serving - multiple ranges which are less efficient than sending the whole.
    let mut resp = client
        .get(&url)
        .header("Range", "bytes=0-100, 120-240")
        .send()
        .unwrap();
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), MIME);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // Range serving - not satisfiable.
    let mut resp = client
        .get(&url)
        .header("Range", "bytes=500-")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::RANGE_NOT_SATISFIABLE, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_RANGE).unwrap(),
               &format!("bytes */{}", BODY.len()));
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"", &buf[..]);

    // Range serving - matching If-Range by date doesn't honor the range.
    let mut resp = client
        .get(&url)
        .header("Range", "bytes=1-3")
        .header("If-Range", SOME_DATE_STR)
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), MIME);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // Range serving - non-matching If-Range by date ignores the range.
    let mut resp = client
        .get(&url)
        .header("Range", "bytes=1-3")
        .header("If-Range", LATER_DATE_STR)
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), MIME);
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // Range serving - this resource has no etag, so any If-Range by etag ignores the range.
    let mut resp = client
        .get(&url)
        .header("Range", "bytes=1-3")
        .header("If-Range", "\"foo\"")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), MIME);
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);
}

#[test]
fn serve_with_strong_etag() {
    let _ = env_logger::try_init();
    let client = reqwest::Client::new();
    let mut buf = Vec::new();
    let url = format!("{}/strong", *SERVER);

    // If-Match any should still send the full body.
    let mut resp = client
        .get(&url)
        .header("If-Match", "*")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), MIME);
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // If-Match by matching etag should send the full body.
    let mut resp = client
        .get(&url)
        .header("If-Match", "\"foo\"")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), MIME);
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // If-Match by etag which doesn't match.
    let resp = client
        .get(&url)
        .header("If-Match", "\"bar\"")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::PRECONDITION_FAILED, resp.status());

    // If-None-Match by etag which matches.
    let mut resp = client
        .get(&url)
        .header("If-None-Match", "\"foo\"")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::NOT_MODIFIED, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"", &buf[..]);

    // If-None-Match by etag which doesn't match.
    let mut resp = client
        .get(&url)
        .header("If-None-Match", "\"bar\"")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // Range serving - If-Range matching by etag.
    let mut resp = client
        .get(&url)
        .header("Range", "bytes=1-3")
        .header("If-Range", "\"foo\"")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::PARTIAL_CONTENT, resp.status());
    assert_eq!(None, resp.headers().get(header::CONTENT_TYPE));
    assert_eq!(resp.headers().get(header::CONTENT_RANGE).unwrap(),
               &format!("bytes 1-3/{}", BODY.len()));
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"123", &buf[..]);

    // Range serving - If-Range not matching by etag.
    let mut resp = client
        .get(&url)
        .header("Range", "bytes=1-3")
        .header("If-Range", "\"bar\"")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), MIME);
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);
}

#[test]
fn serve_with_weak_etag() {
    let _ = env_logger::try_init();
    let client = reqwest::Client::new();
    let mut buf = Vec::new();
    let url = format!("{}/weak", *SERVER);

    // If-Match any should still send the full body.
    let mut resp = client
        .get(&url)
        .header("If-Match", "*")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), MIME);
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // If-Match by etag doesn't match because matches use the strong comparison function.
    let resp = client
        .get(&url)
        .header("If-Match", "W/\"foo\"")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::PRECONDITION_FAILED, resp.status());

    // If-None-Match by identical weak etag is sufficient.
    let mut resp = client
        .get(&url)
        .header("If-None-Match", "W/\"foo\"")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::NOT_MODIFIED, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"", &buf[..]);

    // If-None-Match by etag which doesn't match.
    let mut resp = client
        .get(&url)
        .header("If-None-Match", "W/\"bar\"")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // Range serving - If-Range matching by weak etag isn't sufficient.
    let mut resp = client
        .get(&url)
        .header("Range", "bytes=1-3")
        .header("If-Range", "\"foo\"")
        .send()
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(resp.headers().get(header::CONTENT_TYPE).unwrap(), MIME);
    assert_eq!(resp.headers().get(header::CONTENT_RANGE), None);
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);
}
