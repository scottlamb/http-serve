#![no_main]
use http::header::HeaderValue;
use http_body::Body as _;
use libfuzzer_sys::fuzz_target;
use once_cell::sync::Lazy;
use std::pin::Pin;
use std::task::Poll;
use std::time::SystemTime;

static BODY: &'static [u8] =
    b"01234567890123456789012345678901234567890123456789012345678901234567890123456789\
      01234567890123456789012345678901234567890123456789012345678901234567890123456789\
      01234567890123456789012345678901234567890123456789012345678901234567890123456789";

struct FakeEntity {
    etag: Option<HeaderValue>,
    last_modified: SystemTime,
}

#[derive(arbitrary::Arbitrary, Copy, Clone, Debug)]
enum Verb {
    Get,
    Head,
    Options, // any other method
}

impl Verb {
    fn to_method(self) -> http::method::Method {
        match self {
            Verb::Get => http::method::Method::GET,
            Verb::Head => http::method::Method::HEAD,
            Verb::Options => http::method::Method::OPTIONS,
        }
    }
}

#[derive(arbitrary::Arbitrary, Debug)]
struct Req {
    verb: Verb,
    if_unmodified_since: Option<String>,
    if_modified_since: Option<String>,
    range: Option<String>,
    if_range: Option<String>,
}

impl http_serve::Entity for &'static FakeEntity {
    type Data = bytes::Bytes;
    type Error = http_serve::BoxError;

    fn len(&self) -> u64 {
        BODY.len() as u64
    }
    fn get_range(
        &self,
        range: std::ops::Range<u64>,
    ) -> Pin<Box<dyn futures::Stream<Item = Result<Self::Data, Self::Error>> + Send + Sync>> {
        Box::pin(futures::stream::once(futures::future::ok(
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

const SOME_DATE_STR: &str = "Sun, 06 Nov 1994 08:49:37 GMT";
static SOME_DATE: Lazy<SystemTime> =
    Lazy::new(|| httpdate::parse_http_date(SOME_DATE_STR).unwrap());
static ENTITY_STRONG_ETAG: Lazy<FakeEntity> = Lazy::new(|| FakeEntity {
    etag: Some(HeaderValue::from_static("\"foo\"")),
    last_modified: *SOME_DATE,
});

fuzz_target!(|data: Req| {
    let mut builder = http::request::Builder::new().method(data.verb.to_method());
    if let Some(v) = data.if_unmodified_since.as_deref() {
        builder = builder.header(http::header::IF_UNMODIFIED_SINCE, v);
    }
    if let Some(v) = data.if_modified_since.as_deref() {
        builder = builder.header(http::header::IF_MODIFIED_SINCE, v);
    }
    if let Some(v) = data.range.as_deref() {
        builder = builder.header(http::header::RANGE, v);
    }
    if let Some(v) = data.if_range.as_deref() {
        builder = builder.header(http::header::IF_RANGE, v);
    }
    let request = match builder.body(()) {
        Err(_) => return,
        Ok(r) => r,
    };

    let response = http_serve::serve(&*ENTITY_STRONG_ETAG, &request);
    let body = response.into_body();
    futures::pin_mut!(body);
    let waker = futures::task::noop_waker();
    let mut cx = futures::task::Context::from_waker(&waker);
    loop {
        match body.as_mut().poll_frame(&mut cx) {
            Poll::Pending => panic!("FakeEntity is never pending"),
            Poll::Ready(Some(Err(_))) => panic!("FakeEntity never serves error"),
            Poll::Ready(Some(Ok(_))) => {}
            Poll::Ready(None) => break,
        }
    }
});
