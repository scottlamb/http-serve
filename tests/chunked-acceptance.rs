// Copyright (c) 2016-2018 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use bytes::Bytes;
use hyper_util::rt::TokioIo;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Mutex;
use tokio::net::TcpListener;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

type BoxError = Box<dyn std::error::Error + Send + Sync>;

async fn serve(
    req: http::Request<hyper::body::Incoming>,
) -> Result<http::Response<http_serve::Body<Bytes, BoxError>>, BoxError> {
    let mut cmds = CMDS.lock().unwrap().remove(req.uri().path()).unwrap();
    let (resp, w) = http_serve::streaming_body(&req).build();
    let mut w = w.unwrap();
    tokio::spawn(async move {
        while let Some(cmd) = cmds.recv().await {
            match cmd {
                Cmd::WriteAll(s) => w.write_all(s).unwrap(),
                Cmd::Abort(e) => w.abort(e),
                Cmd::Flush => w.flush().unwrap(),
            }
        }
    });
    Ok(resp)
}

#[derive(Debug)]
enum Cmd {
    WriteAll(&'static [u8]),
    Flush,
    Abort(Box<dyn std::error::Error + Send + Sync>),
}

struct Server {
    addr: String,
}

fn new_server() -> Server {
    let (server_tx, server_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();
        rt.block_on(async {
            let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0));
            let listener = TcpListener::bind(addr).await.unwrap();
            let addr = listener.local_addr().unwrap();
            server_tx
                .send(Server {
                    addr: format!("http://{}:{}", addr.ip(), addr.port()),
                })
                .unwrap();
            loop {
                let (tcp, _) = listener.accept().await.unwrap();
                let io = TokioIo::new(tcp);
                tokio::task::spawn(async move {
                    hyper::server::conn::http1::Builder::new()
                        .serve_connection(io, hyper::service::service_fn(serve))
                        .await
                        .unwrap();
                });
            }
        });
    });
    server_rx.recv().unwrap()
}

static SERVER: Lazy<Server> = Lazy::new(new_server);
static CMDS: Lazy<Mutex<HashMap<&'static str, UnboundedReceiver<Cmd>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

fn setup_req(
    path: &'static str,
    auto_gzip: bool,
) -> (UnboundedSender<Cmd>, reqwest::RequestBuilder) {
    let (tx, rx) = mpsc::unbounded_channel();
    CMDS.lock().unwrap().insert(path, rx);
    let client = reqwest::Client::builder().gzip(auto_gzip).build().unwrap();
    let req = client.get(&format!("{}{}", SERVER.addr, path));
    (tx, req)
}

async fn basic(path: &'static str, auto_gzip: bool) {
    let (cmds, req) = setup_req(path, auto_gzip);
    let resp = req.send().await.unwrap();

    cmds.send(Cmd::WriteAll(b"1234")).unwrap();
    cmds.send(Cmd::WriteAll(b"5678")).unwrap();
    drop(cmds);
    assert_eq!(None, resp.headers().get(reqwest::header::CONTENT_ENCODING));
    let buf = resp.bytes().await.unwrap();
    assert_eq!(b"12345678", &buf[..]);
}

#[tokio::test]
async fn no_gzip() {
    basic("/no_gzip", false).await;
}

#[tokio::test]
async fn auto_gzip() {
    basic("/auto_gzip", true).await;
}

async fn abort(path: &'static str, auto_gzip: bool) {
    let (cmds, req) = setup_req(path, auto_gzip);
    let mut resp = req.send().await.unwrap();

    cmds.send(Cmd::WriteAll(b"1234")).unwrap();
    cmds.send(Cmd::Flush).unwrap();
    cmds.send(Cmd::WriteAll(b"5678")).unwrap();
    let buf = resp.chunk().await.unwrap().unwrap();
    assert_eq!(b"1234", &buf[..]);

    cmds.send(Cmd::Abort(Box::new(io::Error::new(
        io::ErrorKind::PermissionDenied, // note: not related to error kind below.
        "foo",
    ))))
    .unwrap();

    // This is fragile, but I don't see a better way to check that the error is as expected.
    let e = format!("{:?}", resp.chunk().await.unwrap_err());
    assert!(e.contains("UnexpectedEof"), "{}", e);
}

#[tokio::test]
async fn no_gzip_abort() {
    abort("/no_gzip_abort", false).await;
}

#[tokio::test]
async fn auto_gzip_abort() {
    abort("/auto_gzip_abort", true).await;
}

#[tokio::test]
async fn manual_gzip() {
    use reqwest::header;
    let (cmds, req) = setup_req("/manual_gzip", false);
    let resp = req.header("Accept-Encoding", "gzip").send().await.unwrap();

    cmds.send(Cmd::WriteAll(b"1234")).unwrap();
    drop(cmds);
    assert_eq!(
        resp.headers().get(header::CONTENT_ENCODING).unwrap(),
        "gzip"
    );
    let buf = resp.bytes().await.unwrap();
    assert_eq!(b"\x1f\x8b", &buf[..2]); // gzip magic number.
}
