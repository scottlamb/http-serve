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

#[macro_use] extern crate lazy_static;
#[macro_use] extern crate log;
extern crate http_entity;
extern crate hyper;
#[macro_use] extern crate mime;
extern crate smallvec;

extern crate env_logger;
extern crate reqwest;

use self::reqwest::header::{self, ByteRangeSpec, ContentRangeSpec, EntityTag};
use self::reqwest::header::Range::Bytes;
use std::io::{self, Read};
use std::ops::Range;

static BODY: &'static [u8] =
    b"01234567890123456789012345678901234567890123456789012345678901234567890123456789\
      01234567890123456789012345678901234567890123456789012345678901234567890123456789\
      01234567890123456789012345678901234567890123456789012345678901234567890123456789";

struct FakeEntity {
    etag: Option<hyper::header::EntityTag>,
    last_modified: hyper::header::HttpDate,
}

impl http_entity::Entity<io::Error> for FakeEntity {
    fn len(&self) -> u64 { BODY.len() as u64 }
    fn write_to(&self, range: Range<u64>, out: &mut io::Write) -> Result<(), io::Error> {
        out.write_all(&BODY[range.start as usize .. range.end as usize])
    }
    fn add_headers(&self, headers: &mut ::hyper::header::Headers) {
        headers.set(::hyper::header::ContentType(mime!(Application/OctetStream)));
    }
    fn etag(&self) -> Option<EntityTag> { self.etag.clone() }
    fn last_modified(&self) -> Option<header::HttpDate> { Some(self.last_modified) }
}

fn new_server() -> String {
    let mut listener = hyper::net::HttpListener::new("127.0.0.1:0").unwrap();
    use hyper::net::NetworkListener;
    let addr = listener.local_addr().unwrap();
    let server = hyper::Server::new(listener);
    use std::thread::spawn;
    spawn(move || {
        use hyper::server::{Request, Response, Fresh};
        let _ = server.handle(move |req: Request, res: Response<Fresh>| {
            use hyper::uri::RequestUri;
            let path = match req.uri {
                RequestUri::AbsolutePath(ref p) => p,
                x => panic!("unexpected uri type {:?}", x),
            };
            let entity = match path.as_str() {
                "/none" => &*ENTITY_NO_ETAG,
                "/strong" => &*ENTITY_STRONG_ETAG,
                "/weak" => &*ENTITY_WEAK_ETAG,
                p => panic!("unexpected path {}", p),
            };
            http_entity::serve(entity, &req, res).unwrap();
        });
    });
    format!("http://{}:{}", addr.ip(), addr.port())
}

const SOME_DATE_STR: &'static str =  "Sun, 06 Nov 1994 08:49:37 GMT";
const LATER_DATE_STR: &'static str = "Sun, 06 Nov 1994 09:49:37 GMT";

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
}

#[test]
fn serve_without_etag() {
    let _ = env_logger::init();
    let client = reqwest::Client::new().unwrap();
    let mut buf = Vec::new();
    let url = format!("{}/none", *SERVER);

    // Full body.
    let mut resp = client.get(&url).send().unwrap();
    assert_eq!(reqwest::StatusCode::Ok, *resp.status());
    assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
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
    assert_eq!(reqwest::StatusCode::Ok, *resp.status());
    assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
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
    assert_eq!(reqwest::StatusCode::PreconditionFailed, *resp.status());

    // If-None-Match any.
    let mut resp = client.get(&url)
                         .header(header::IfNoneMatch::Any)
                         .send()
                         .unwrap();
    assert_eq!(reqwest::StatusCode::NotModified, *resp.status());
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
    assert_eq!(reqwest::StatusCode::Ok, *resp.status());
    assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
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
    assert_eq!(reqwest::StatusCode::NotModified, *resp.status());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"", &buf[..]);

    // Range serving - basic case.
    let mut resp = client.get(&url)
                         .header(Bytes(vec![ByteRangeSpec::FromTo(1, 3)]))
                         .send()
                         .unwrap();
    assert_eq!(reqwest::StatusCode::PartialContent, *resp.status());
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
    assert_eq!(reqwest::StatusCode::PartialContent, *resp.status());
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
    assert_eq!(reqwest::StatusCode::Ok, *resp.status());
    assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
               resp.headers().get::<header::ContentType>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);

    // Range serving - not satisfiable.
    let mut resp = client.get(&url)
                         .header(Bytes(vec![ByteRangeSpec::AllFrom(500)]))
                         .send()
                         .unwrap();
    assert_eq!(reqwest::StatusCode::RangeNotSatisfiable, *resp.status());
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
    assert_eq!(reqwest::StatusCode::PartialContent, *resp.status());
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
    assert_eq!(reqwest::StatusCode::Ok, *resp.status());
    assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
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
    assert_eq!(reqwest::StatusCode::Ok, *resp.status());
    assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
               resp.headers().get::<header::ContentType>());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);
}

#[test]
fn serve_with_strong_etag() {
    let _ = env_logger::init();
    let client = reqwest::Client::new().unwrap();
    let mut buf = Vec::new();
    let url = format!("{}/strong", *SERVER);

    // If-Match any should still send the full body.
    let mut resp = client.get(&url)
                         .header(header::IfMatch::Any)
                         .send()
                         .unwrap();
    assert_eq!(reqwest::StatusCode::Ok, *resp.status());
    assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
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
    assert_eq!(reqwest::StatusCode::Ok, *resp.status());
    assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
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
    assert_eq!(reqwest::StatusCode::PreconditionFailed, *resp.status());

    // If-None-Match by etag which matches.
    let mut resp =
        client.get(&url)
              .header(header::IfNoneMatch::Items(vec![EntityTag::strong("foo".to_owned())]))
              .send()
              .unwrap();
    assert_eq!(reqwest::StatusCode::NotModified, *resp.status());
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
    assert_eq!(reqwest::StatusCode::Ok, *resp.status());
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
    assert_eq!(reqwest::StatusCode::PartialContent, *resp.status());
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
    assert_eq!(reqwest::StatusCode::Ok, *resp.status());
    assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
               resp.headers().get::<header::ContentType>());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);
}

#[test]
fn serve_with_weak_etag() {
    let _ = env_logger::init();
    let client = reqwest::Client::new().unwrap();
    let mut buf = Vec::new();
    let url = format!("{}/weak", *SERVER);

    // If-Match any should still send the full body.
    let mut resp = client.get(&url)
                         .header(header::IfMatch::Any)
                         .send()
                         .unwrap();
    assert_eq!(reqwest::StatusCode::Ok, *resp.status());
    assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
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
    assert_eq!(reqwest::StatusCode::PreconditionFailed, *resp.status());

    // If-None-Match by identical weak etag is sufficient.
    let mut resp =
        client.get(&url)
              .header(header::IfNoneMatch::Items(vec![EntityTag::weak("foo".to_owned())]))
              .send()
              .unwrap();
    assert_eq!(reqwest::StatusCode::NotModified, *resp.status());
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
    assert_eq!(reqwest::StatusCode::Ok, *resp.status());
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
    assert_eq!(reqwest::StatusCode::Ok, *resp.status());
    assert_eq!(Some(&header::ContentType(mime!(Application/OctetStream))),
               resp.headers().get::<header::ContentType>());
    assert_eq!(None, resp.headers().get::<header::ContentRange>());
    buf.clear();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(BODY, &buf[..]);
}
