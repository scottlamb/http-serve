// Copyright (c) 2016-2018 Scott Lamb <slamb@slamb.org>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate futures;
#[macro_use] extern crate lazy_static;
#[macro_use] extern crate log;
extern crate http_entity;
extern crate hyper;
extern crate smallvec;

extern crate env_logger;
extern crate reqwest;

use futures::Stream;
use futures::stream;
use reqwest::header::{self, ByteRangeSpec, ContentRangeSpec, EntityTag};
use reqwest::header::Range::Bytes;
use std::io::Read;
use std::ops::Range;

type Body = Box<Stream<Item = Vec<u8>, Error = hyper::Error> + Send>;

static BODY: &'static [u8] =
    b"01234567890123456789012345678901234567890123456789012345678901234567890123456789\
      01234567890123456789012345678901234567890123456789012345678901234567890123456789\
      01234567890123456789012345678901234567890123456789012345678901234567890123456789";

struct FakeEntity {
    etag: Option<hyper::header::EntityTag>,
    last_modified: hyper::header::HttpDate,
}

impl http_entity::Entity for &'static FakeEntity {
    type Chunk = Vec<u8>;
    type Body = Body;

    fn len(&self) -> u64 { BODY.len() as u64 }
    fn get_range(&self, range: Range<u64>)
                 -> Box<Stream<Item = Vec<u8>, Error = hyper::Error> + Send> {
        Box::new(stream::once(Ok(BODY[range.start as usize .. range.end as usize].into())))
    }
    fn add_headers(&self, headers: &mut ::hyper::header::Headers) {
        headers.set(::hyper::header::ContentType(hyper::mime::APPLICATION_OCTET_STREAM));
    }
    fn etag(&self) -> Option<hyper::header::EntityTag> { self.etag.clone() }
    fn last_modified(&self) -> Option<hyper::header::HttpDate> { Some(self.last_modified) }
}

struct MyService;

impl hyper::server::Service for MyService {
    type Request = hyper::server::Request;
    type Response = hyper::server::Response<Body>;
    type Error = hyper::Error;
    type Future = ::futures::future::FutureResult<Self::Response, hyper::Error>;

    fn call(&self, req: hyper::server::Request) -> Self::Future {
        let entity: &'static FakeEntity = match req.uri().path() {
            "/none" => &*ENTITY_NO_ETAG,
            "/strong" => &*ENTITY_STRONG_ETAG,
            "/weak" => &*ENTITY_WEAK_ETAG,
            p => panic!("unexpected path {}", p),
        };
        futures::future::ok(http_entity::serve(entity, &req))
    }
}

fn new_server() -> String {
    let (tx, rx) = ::std::sync::mpsc::channel();
    ::std::thread::spawn(move || {
        let addr = "127.0.0.1:0".parse().unwrap();
        let server = hyper::server::Http::new().bind(&addr, || {
            info!("creating a service");
            Ok(MyService)
        }).unwrap();
        let addr = server.local_addr().unwrap();
        tx.send(addr).unwrap();
        server.run().unwrap()
    });
    let addr = rx.recv().unwrap();
    format!("http://{}:{}", addr.ip(), addr.port())
}

const SOME_DATE_STR: &str =  "Sun, 06 Nov 1994 08:49:37 GMT";
const LATER_DATE_STR: &str = "Sun, 06 Nov 1994 09:49:37 GMT";

lazy_static! {
    static ref SOME_DATE: reqwest::header::HttpDate = { SOME_DATE_STR.parse().unwrap() };
    static ref LATER_DATE: reqwest::header::HttpDate = { LATER_DATE_STR.parse().unwrap() };
    static ref ENTITY_NO_ETAG: FakeEntity = FakeEntity{
        etag: None,
        last_modified: SOME_DATE_STR.parse().unwrap(),
    };
    static ref ENTITY_STRONG_ETAG: FakeEntity = FakeEntity{
        etag: Some(hyper::header::EntityTag::strong("foo".to_owned())),
        last_modified: SOME_DATE_STR.parse().unwrap(),
    };
    static ref ENTITY_WEAK_ETAG: FakeEntity = FakeEntity{
        etag: Some(hyper::header::EntityTag::strong("foo".to_owned())),
        last_modified: SOME_DATE_STR.parse().unwrap(),
    };
    static ref SERVER: String = { new_server() };
    static ref MIME: reqwest::mime::Mime = { "application/octet-stream".parse().unwrap() };
}

#[test]
fn serve_without_etag() {
    let _ = env_logger::init();
    let client = reqwest::Client::new();
    let mut buf = Vec::new();
    let url = format!("{}/none", *SERVER);

    // Full body.
    let mut resp = client.get(&url).send().unwrap();
    assert_eq!(reqwest::StatusCode::Ok, resp.status());
    assert_eq!(Some(&header::ContentType(MIME.clone())),
               resp.headers().get::<header::ContentType>());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // If-Match any should still send the full body.
    let mut resp = client.get(&url)
                         .header(header::IfMatch::Any)
                         .send()
                         .unwrap();
    assert_eq!(reqwest::StatusCode::Ok, resp.status());
    assert_eq!(Some(&header::ContentType(MIME.clone())),
               resp.headers().get::<header::ContentType>());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // If-Match by etag doesn't match (as this request has no etag).
    let resp =
        client.get(&url)
              .header(header::IfMatch::Items(vec![EntityTag::strong("foo".to_owned())]))
              .send()
              .unwrap();
    assert_eq!(reqwest::StatusCode::PreconditionFailed, resp.status());

    // If-None-Match any.
    let mut resp = client.get(&url)
                         .header(header::IfNoneMatch::Any)
                         .send()
                         .unwrap();
    assert_eq!(reqwest::StatusCode::NotModified, resp.status());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"", &buf[..]);

    // If-None-Match by etag doesn't match (as this request has no etag).
    let mut resp =
        client.get(&url)
              .header(header::IfNoneMatch::Items(vec![EntityTag::strong("foo".to_owned())]))
              .send()
              .unwrap();
    assert_eq!(reqwest::StatusCode::Ok, resp.status());
    assert_eq!(Some(&header::ContentType(MIME.clone())),
               resp.headers().get::<header::ContentType>());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // Unmodified since supplied date.
    let mut resp = client.get(&url)
                         .header(header::IfModifiedSince(*SOME_DATE))
                         .send()
                         .unwrap();
    assert_eq!(reqwest::StatusCode::NotModified, resp.status());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"", &buf[..]);

    // Range serving - basic case.
    let mut resp = client.get(&url)
                         .header(Bytes(vec![ByteRangeSpec::FromTo(1, 3)]))
                         .send()
                         .unwrap();
    assert_eq!(reqwest::StatusCode::PartialContent, resp.status());
    assert_eq!(Some(&header::ContentRange(ContentRangeSpec::Bytes{
        range: Some((1, 3)),
        instance_length: Some(BODY.len() as u64),
    })), resp.headers().get());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"123", &buf[..]);

    // Range serving - multiple ranges.
    let mut resp = client.get(&url)
                         .header(Bytes(vec![ByteRangeSpec::FromTo(0, 1),
                                            ByteRangeSpec::FromTo(3, 4)]))
                         .send()
                         .unwrap();
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    assert_eq!(reqwest::StatusCode::PartialContent, resp.status());
    assert_eq!(Some(&header::ContentType("multipart/byteranges; boundary=B".parse().unwrap())),
               resp.headers().get::<header::ContentType>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!("\
               \r\n--B\r\n\
               Content-Range: bytes 0-1/240\r\n\
               Content-Type: application/octet-stream\r\n\
               \r\n\
               01\r\n\
               --B\r\n\
               Content-Range: bytes 3-4/240\r\n\
               Content-Type: application/octet-stream\r\n\
               \r\n\
               34\r\n\
               --B--\r\n"[..], String::from_utf8(buf.clone()).unwrap());

    // Range serving - multiple ranges which are less efficient than sending the whole.
    let mut resp = client.get(&url)
                         .header(Bytes(vec![ByteRangeSpec::FromTo(0, 100),
                                            ByteRangeSpec::FromTo(120, 240)]))
                         .send()
                         .unwrap();
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    assert_eq!(reqwest::StatusCode::Ok, resp.status());
    assert_eq!(Some(&header::ContentType(MIME.clone())),
               resp.headers().get::<header::ContentType>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // Range serving - not satisfiable.
    let mut resp = client.get(&url)
                         .header(Bytes(vec![ByteRangeSpec::AllFrom(500)]))
                         .send()
                         .unwrap();
    assert_eq!(reqwest::StatusCode::RangeNotSatisfiable, resp.status());
    assert_eq!(Some(&header::ContentRange(ContentRangeSpec::Bytes{
        range: None,
        instance_length: Some(BODY.len() as u64),
    })), resp.headers().get());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"", &buf[..]);

    // Range serving - matching If-Range by date honors the range.
    let mut resp = client.get(&url)
                         .header(Bytes(vec![ByteRangeSpec::FromTo(1, 3)]))
                         .header(header::IfRange::Date(*SOME_DATE))
                         .send()
                         .unwrap();
    assert_eq!(reqwest::StatusCode::PartialContent, resp.status());
    assert_eq!(Some(&header::ContentRange(ContentRangeSpec::Bytes{
        range: Some((1, 3)),
        instance_length: Some(BODY.len() as u64),
    })), resp.headers().get());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"123", &buf[..]);

    // Range serving - non-matching If-Range by date ignores the range.
    let mut resp = client.get(&url)
                         .header(Bytes(vec![ByteRangeSpec::FromTo(1, 3)]))
                         .header(header::IfRange::Date(*LATER_DATE))
                         .send()
                         .unwrap();
    assert_eq!(reqwest::StatusCode::Ok, resp.status());
    assert_eq!(Some(&header::ContentType(MIME.clone())),
               resp.headers().get::<header::ContentType>());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // Range serving - this resource has no etag, so any If-Range by etag ignores the range.
    let mut resp =
        client.get(&url)
              .header(Bytes(vec![ByteRangeSpec::FromTo(1, 3)]))
              .header(header::IfRange::EntityTag(EntityTag::strong("foo".to_owned())))
              .send()
              .unwrap();
    assert_eq!(reqwest::StatusCode::Ok, resp.status());
    assert_eq!(Some(&header::ContentType(MIME.clone())),
               resp.headers().get::<header::ContentType>());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);
}

#[test]
fn serve_with_strong_etag() {
    let _ = env_logger::init();
    let client = reqwest::Client::new();
    let mut buf = Vec::new();
    let url = format!("{}/strong", *SERVER);

    // If-Match any should still send the full body.
    let mut resp = client.get(&url)
                         .header(header::IfMatch::Any)
                         .send()
                         .unwrap();
    assert_eq!(reqwest::StatusCode::Ok, resp.status());
    assert_eq!(Some(&header::ContentType(MIME.clone())),
               resp.headers().get::<header::ContentType>());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // If-Match by matching etag should send the full body.
    let mut resp =
        client.get(&url)
              .header(header::IfMatch::Items(vec![EntityTag::strong("foo".to_owned())]))
              .send()
              .unwrap();
    assert_eq!(reqwest::StatusCode::Ok, resp.status());
    assert_eq!(Some(&header::ContentType(MIME.clone())),
               resp.headers().get::<header::ContentType>());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // If-Match by etag which doesn't match.
    let resp =
        client.get(&url)
              .header(header::IfMatch::Items(vec![EntityTag::strong("bar".to_owned())]))
              .send()
              .unwrap();
    assert_eq!(reqwest::StatusCode::PreconditionFailed, resp.status());

    // If-None-Match by etag which matches.
    let mut resp =
        client.get(&url)
              .header(header::IfNoneMatch::Items(vec![EntityTag::strong("foo".to_owned())]))
              .send()
              .unwrap();
    assert_eq!(reqwest::StatusCode::NotModified, resp.status());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"", &buf[..]);

    // If-None-Match by etag which doesn't match.
    let mut resp =
        client.get(&url)
              .header(header::IfNoneMatch::Items(vec![EntityTag::strong("bar".to_owned())]))
              .send()
              .unwrap();
    assert_eq!(reqwest::StatusCode::Ok, resp.status());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // Range serving - If-Range matching by etag.
    let mut resp =
        client.get(&url)
              .header(Bytes(vec![ByteRangeSpec::FromTo(1, 3)]))
              .header(header::IfRange::EntityTag(EntityTag::strong("foo".to_owned())))
              .send()
              .unwrap();
    assert_eq!(reqwest::StatusCode::PartialContent, resp.status());
    assert_eq!(None, resp.headers().get::<header::ContentType>());
    assert_eq!(Some(&header::ContentRange(ContentRangeSpec::Bytes{
        range: Some((1, 3)),
        instance_length: Some(BODY.len() as u64),
    })), resp.headers().get());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"123", &buf[..]);

    // Range serving - If-Range not matching by etag.
    let mut resp =
        client.get(&url)
              .header(Bytes(vec![ByteRangeSpec::FromTo(1, 3)]))
              .header(header::IfRange::EntityTag(EntityTag::strong("bar".to_owned())))
              .send()
              .unwrap();
    assert_eq!(reqwest::StatusCode::Ok, resp.status());
    assert_eq!(Some(&header::ContentType(MIME.clone())),
               resp.headers().get::<header::ContentType>());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);
}

#[test]
fn serve_with_weak_etag() {
    let _ = env_logger::init();
    let client = reqwest::Client::new();
    let mut buf = Vec::new();
    let url = format!("{}/weak", *SERVER);

    // If-Match any should still send the full body.
    let mut resp = client.get(&url)
                         .header(header::IfMatch::Any)
                         .send()
                         .unwrap();
    assert_eq!(reqwest::StatusCode::Ok, resp.status());
    assert_eq!(Some(&header::ContentType(MIME.clone())),
               resp.headers().get::<header::ContentType>());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // If-Match by etag doesn't match because matches use the strong comparison function.
    let resp =
        client.get(&url)
              .header(header::IfMatch::Items(vec![EntityTag::weak("foo".to_owned())]))
              .send()
              .unwrap();
    assert_eq!(reqwest::StatusCode::PreconditionFailed, resp.status());

    // If-None-Match by identical weak etag is sufficient.
    let mut resp =
        client.get(&url)
              .header(header::IfNoneMatch::Items(vec![EntityTag::weak("foo".to_owned())]))
              .send()
              .unwrap();
    assert_eq!(reqwest::StatusCode::NotModified, resp.status());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"", &buf[..]);

    // If-None-Match by etag which doesn't match.
    let mut resp =
        client.get(&url)
              .header(header::IfNoneMatch::Items(vec![EntityTag::weak("bar".to_owned())]))
              .send()
              .unwrap();
    assert_eq!(reqwest::StatusCode::Ok, resp.status());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // Range serving - If-Range matching by weak etag isn't sufficient.
    let mut resp =
        client.get(&url)
              .header(Bytes(vec![ByteRangeSpec::FromTo(1, 3)]))
              .header(header::IfRange::EntityTag(EntityTag::weak("foo".to_owned())))
              .send()
              .unwrap();
    assert_eq!(reqwest::StatusCode::Ok, resp.status());
    assert_eq!(Some(&header::ContentType(MIME.clone())),
               resp.headers().get::<header::ContentType>());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);
}
