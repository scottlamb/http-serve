// Copyright (c) 2016-2018 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use futures_core::Stream;
use futures_util::{future, stream};
use http::header::HeaderValue;
use http::{Request, Response};
use hyper::body::Body;
use once_cell::sync::Lazy;
use std::ops::Range;
use std::time::SystemTime;

type BoxedError = Box<dyn std::error::Error + Send + Sync>;

static BODY: &'static [u8] =
    b"01234567890123456789012345678901234567890123456789012345678901234567890123456789\
      01234567890123456789012345678901234567890123456789012345678901234567890123456789\
      01234567890123456789012345678901234567890123456789012345678901234567890123456789";

struct FakeEntity {
    etag: Option<HeaderValue>,
    last_modified: SystemTime,
}

impl http_serve::Entity for &'static FakeEntity {
    type Data = bytes::Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn len(&self) -> u64 {
        BODY.len() as u64
    }
    fn get_range(
        &self,
        range: Range<u64>,
    ) -> Box<dyn Stream<Item = Result<Self::Data, Self::Error>> + Send + Sync> {
        Box::new(stream::once(future::ok(
            BODY[range.start as usize..range.end as usize].into(),
        )))
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

async fn serve(req: Request<Body>) -> Result<Response<Body>, BoxedError> {
    let entity: &'static FakeEntity = match req.uri().path() {
        "/none" => &*ENTITY_NO_ETAG,
        "/strong" => &*ENTITY_STRONG_ETAG,
        "/weak" => &*ENTITY_WEAK_ETAG,
        p => panic!("unexpected path {}", p),
    };
    Ok(http_serve::serve(entity, &req))
}

fn new_server() -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let make_svc = hyper::service::make_service_fn(|_conn| {
            future::ok::<_, hyper::Error>(hyper::service::service_fn(serve))
        });
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();

        let addr = ([127, 0, 0, 1], 0).into();
        let srv = hyper::Server::bind(&addr).serve(make_svc);
        tx.send(srv.local_addr()).unwrap();
        rt.block_on(srv).unwrap();
    });
    let addr = rx.recv().unwrap();
    format!("http://{}:{}", addr.ip(), addr.port())
}

const SOME_DATE_STR: &str = "Sun, 06 Nov 1994 08:49:37 GMT";
const LATER_DATE_STR: &str = "Sun, 06 Nov 1994 09:49:37 GMT";
const MIME: &str = "application/octet-stream";

static SOME_DATE: Lazy<SystemTime> =
    Lazy::new(|| httpdate::parse_http_date(SOME_DATE_STR).unwrap());
static ENTITY_NO_ETAG: Lazy<FakeEntity> = Lazy::new(|| FakeEntity {
    etag: None,
    last_modified: *SOME_DATE,
});
static ENTITY_STRONG_ETAG: Lazy<FakeEntity> = Lazy::new(|| FakeEntity {
    etag: Some(HeaderValue::from_static("\"foo\"")),
    last_modified: *SOME_DATE,
});
static ENTITY_WEAK_ETAG: Lazy<FakeEntity> = Lazy::new(|| FakeEntity {
    etag: Some(HeaderValue::from_static("W/\"foo\"")),
    last_modified: *SOME_DATE,
});
static SERVER: Lazy<String> = Lazy::new(new_server);

#[tokio::test]
async fn serve_without_etag() {
    let _ = env_logger::try_init();
    let client = reqwest::Client::new();
    let url = format!("{}/none", *SERVER);

    // Full body.
    let resp = client.get(&url).send().await.unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        MIME
    );
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    let buf = resp.bytes().await.unwrap();
    assert_eq!(BODY, &buf[..]);

    // If-Match any should still send the full body.
    let resp = client
        .get(&url)
        .header("If-Match", "*")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        MIME
    );
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    let buf = resp.bytes().await.unwrap();
    assert_eq!(BODY, &buf[..]);

    // If-Match by etag doesn't match (as this request has no etag).
    let resp = client
        .get(&url)
        .header("If-Match", "\"foo\"")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::PRECONDITION_FAILED, resp.status());

    // If-None-Match any.
    let resp = client
        .get(&url)
        .header("If-None-Match", "*")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::NOT_MODIFIED, resp.status());
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    let buf = resp.bytes().await.unwrap();
    assert_eq!(b"", &buf[..]);

    // If-None-Match by etag doesn't match (as this request has no etag).
    let resp = client
        .get(&url)
        .header("If-None-Match", "\"foo\"")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        MIME
    );
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    let buf = resp.bytes().await.unwrap();
    assert_eq!(BODY, &buf[..]);

    // Unmodified since supplied date.
    let resp = client
        .get(&url)
        .header("If-Modified-Since", SOME_DATE_STR)
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::NOT_MODIFIED, resp.status());
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    let buf = resp.bytes().await.unwrap();
    assert_eq!(b"", &buf[..]);

    // Range serving - basic case.
    let resp = client
        .get(&url)
        .header("Range", "bytes=1-3")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::PARTIAL_CONTENT, resp.status());
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_RANGE).unwrap(),
        &format!("bytes 1-3/{}", BODY.len())
    );
    let buf = resp.bytes().await.unwrap();
    assert_eq!(b"123", &buf[..]);

    // Range serving - multiple ranges.
    let resp = client
        .get(&url)
        .header("Range", "bytes=0-1, 3-4")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    assert_eq!(reqwest::StatusCode::PARTIAL_CONTENT, resp.status());
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        &"multipart/byteranges; boundary=B"
    );
    let buf = resp.bytes().await.unwrap();
    assert_eq!(
        &b"\
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
        &buf[..]
    );

    // Range serving - multiple ranges which are less efficient than sending the whole.
    let resp = client
        .get(&url)
        .header("Range", "bytes=0-100, 120-240")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        MIME
    );
    let buf = resp.bytes().await.unwrap();
    assert_eq!(BODY, &buf[..]);

    // Range serving - not satisfiable.
    let resp = client
        .get(&url)
        .header("Range", "bytes=500-")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::RANGE_NOT_SATISFIABLE, resp.status());
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_RANGE).unwrap(),
        &format!("bytes */{}", BODY.len())
    );
    let buf = resp.bytes().await.unwrap();
    assert_eq!(b"", &buf[..]);

    // Range serving - matching If-Range by date doesn't honor the range.
    let resp = client
        .get(&url)
        .header("Range", "bytes=1-3")
        .header("If-Range", SOME_DATE_STR)
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        MIME
    );
    let buf = resp.bytes().await.unwrap();
    assert_eq!(BODY, &buf[..]);

    // Range serving - non-matching If-Range by date ignores the range.
    let resp = client
        .get(&url)
        .header("Range", "bytes=1-3")
        .header("If-Range", LATER_DATE_STR)
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        MIME
    );
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    let buf = resp.bytes().await.unwrap();
    assert_eq!(BODY, &buf[..]);

    // Range serving - this resource has no etag, so any If-Range by etag ignores the range.
    let resp = client
        .get(&url)
        .header("Range", "bytes=1-3")
        .header("If-Range", "\"foo\"")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        MIME
    );
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    let buf = resp.bytes().await.unwrap();
    assert_eq!(BODY, &buf[..]);
}

#[tokio::test]
async fn serve_with_strong_etag() {
    let _ = env_logger::try_init();
    let client = reqwest::Client::new();
    let url = format!("{}/strong", *SERVER);

    // If-Match any should still send the full body.
    let resp = client
        .get(&url)
        .header("If-Match", "*")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        MIME
    );
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    assert_eq!(BODY, &resp.bytes().await.unwrap()[..]);

    // If-Match by matching etag should send the full body.
    let resp = client
        .get(&url)
        .header("If-Match", "\"foo\"")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        MIME
    );
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    assert_eq!(BODY, &resp.bytes().await.unwrap()[..]);

    // If-Match by etag which doesn't match.
    let resp = client
        .get(&url)
        .header("If-Match", "\"bar\"")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::PRECONDITION_FAILED, resp.status());

    // If-None-Match by etag which matches.
    let resp = client
        .get(&url)
        .header("If-None-Match", "\"foo\"")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::NOT_MODIFIED, resp.status());
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    assert_eq!(b"", &resp.bytes().await.unwrap()[..]);

    // If-None-Match by etag which doesn't match.
    let resp = client
        .get(&url)
        .header("If-None-Match", "\"bar\"")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    assert_eq!(BODY, &resp.bytes().await.unwrap()[..]);

    // Range serving - If-Range matching by etag.
    let resp = client
        .get(&url)
        .header("Range", "bytes=1-3")
        .header("If-Range", "\"foo\"")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::PARTIAL_CONTENT, resp.status());
    assert_eq!(None, resp.headers().get(reqwest::header::CONTENT_TYPE));
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_RANGE).unwrap(),
        &format!("bytes 1-3/{}", BODY.len())
    );
    assert_eq!(b"123", &resp.bytes().await.unwrap()[..]);

    // Range serving - If-Range not matching by etag.
    let resp = client
        .get(&url)
        .header("Range", "bytes=1-3")
        .header("If-Range", "\"bar\"")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        MIME
    );
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    assert_eq!(BODY, &resp.bytes().await.unwrap()[..]);
}

#[tokio::test]
async fn serve_with_weak_etag() {
    let _ = env_logger::try_init();
    let client = reqwest::Client::new();
    let url = format!("{}/weak", *SERVER);

    // If-Match any should still send the full body.
    let resp = client
        .get(&url)
        .header("If-Match", "*")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        MIME
    );
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    assert_eq!(BODY, &resp.bytes().await.unwrap()[..]);

    // If-Match by etag doesn't match because matches use the strong comparison function.
    let resp = client
        .get(&url)
        .header("If-Match", "W/\"foo\"")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::PRECONDITION_FAILED, resp.status());

    // If-None-Match by identical weak etag is sufficient.
    let resp = client
        .get(&url)
        .header("If-None-Match", "W/\"foo\"")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::NOT_MODIFIED, resp.status());
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    assert_eq!(b"", &resp.bytes().await.unwrap()[..]);

    // If-None-Match by etag which doesn't match.
    let resp = client
        .get(&url)
        .header("If-None-Match", "W/\"bar\"")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    assert_eq!(BODY, &resp.bytes().await.unwrap()[..]);

    // Range serving - If-Range matching by weak etag isn't sufficient.
    let resp = client
        .get(&url)
        .header("Range", "bytes=1-3")
        .header("If-Range", "\"foo\"")
        .send()
        .await
        .unwrap();
    assert_eq!(reqwest::StatusCode::OK, resp.status());
    assert_eq!(
        resp.headers().get(reqwest::header::CONTENT_TYPE).unwrap(),
        MIME
    );
    assert_eq!(resp.headers().get(reqwest::header::CONTENT_RANGE), None);
    assert_eq!(BODY, &resp.bytes().await.unwrap()[..]);
}
