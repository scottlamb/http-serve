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
extern crate hyper;
#[macro_use]
extern crate lazy_static;
extern crate reqwest;
extern crate tokio;

use futures::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use futures::{Future, Stream};
use http::header;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::Mutex;

fn serve(req: http::Request<hyper::Body>) -> http::Response<hyper::Body> {
    let cmds = CMDS.lock().unwrap().remove(req.uri().path()).unwrap();
    let (resp, w) = http_serve::streaming_body(&req).build();
    let mut w = w.unwrap();
    tokio::spawn(cmds.for_each(move |cmd| {
        match cmd {
            Cmd::WriteAll(s) => w.write_all(s).unwrap(),
            Cmd::Abort(e) => w.abort(e),
            Cmd::Flush => w.flush().unwrap(),
        }
        futures::future::ok(())
    }));
    resp
}

#[derive(Debug)]
enum Cmd {
    WriteAll(&'static [u8]),
    Flush,
    Abort(Box<::std::error::Error + Send + Sync>),
}

struct Server {
    addr: String,
}

fn new_server() -> Server {
    let (server_tx, server_rx) = ::std::sync::mpsc::channel();
    ::std::thread::spawn(move || {
        let addr = "127.0.0.1:0".parse().unwrap();
        let srv = hyper::server::Server::bind(&addr).serve(|| hyper::service::service_fn_ok(serve));
        let addr = srv.local_addr();
        server_tx
            .send(Server {
                addr: format!("http://{}:{}", addr.ip(), addr.port()),
            })
            .unwrap();
        tokio::run(srv.map_err(|e| eprintln!("server error: {}", e)))
    });
    server_rx.recv().unwrap()
}

lazy_static! {
    static ref CMDS: Mutex<HashMap<&'static str, UnboundedReceiver<Cmd>>> =
        { Mutex::new(HashMap::new()) };
    static ref SERVER: Server = { new_server() };
}

fn setup_req(
    path: &'static str,
    auto_gzip: bool,
) -> (UnboundedSender<Cmd>, reqwest::RequestBuilder) {
    let (tx, rx) = mpsc::unbounded();
    CMDS.lock().unwrap().insert(path, rx);
    let client = reqwest::Client::builder().gzip(auto_gzip).build().unwrap();
    let req = client.get(&format!("{}{}", SERVER.addr, path));
    (tx, req)
}

fn basic(path: &'static str, auto_gzip: bool) {
    let _ = env_logger::try_init();
    let (cmds, req) = setup_req(path, auto_gzip);
    let mut resp = req.send().unwrap();

    cmds.unbounded_send(Cmd::WriteAll(b"1234")).unwrap();
    cmds.unbounded_send(Cmd::WriteAll(b"5678")).unwrap();
    drop(cmds);
    let mut buf = Vec::new();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"12345678", &buf[..]);
    assert_eq!(None, resp.headers().get(header::CONTENT_ENCODING));
}

#[test]
fn no_gzip() {
    basic("/no_gzip", false);
}

#[test]
fn auto_gzip() {
    basic("/auto_gzip", true);
}

fn abort(path: &'static str, auto_gzip: bool) {
    let _ = env_logger::try_init();
    let (cmds, req) = setup_req(path, auto_gzip);
    let mut resp = req.send().unwrap();

    cmds.unbounded_send(Cmd::WriteAll(b"1234")).unwrap();
    cmds.unbounded_send(Cmd::Flush).unwrap();
    cmds.unbounded_send(Cmd::WriteAll(b"5678")).unwrap();
    let mut buf = [0u8; 4];
    resp.read_exact(&mut buf).unwrap();
    assert_eq!(b"1234", &buf);

    cmds.unbounded_send(Cmd::Abort(Box::new(io::Error::new(
        io::ErrorKind::PermissionDenied,  // note: not related to error kind below.
        "foo",
    )))).unwrap();
    assert_eq!(
        io::ErrorKind::Other,
        resp.read(&mut buf).unwrap_err().kind()
    );
}

#[test]
fn no_gzip_abort() {
    abort("/no_gzip_abort", false);
}

#[test]
fn auto_gzip_abort() {
    abort("/auto_gzip_abort", true);
}

#[test]
fn manual_gzip() {
    use reqwest::header;
    let _ = env_logger::try_init();
    let (cmds, req) = setup_req("/manual_gzip", false);
    let mut resp = req.header("Accept-Encoding", "gzip").send().unwrap();

    cmds.unbounded_send(Cmd::WriteAll(b"1234")).unwrap();
    drop(cmds);
    let mut buf = Vec::new();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"\x1f\x8b", &buf[..2]); // gzip magic number.
    assert_eq!(resp.headers().get(header::CONTENT_ENCODING).unwrap(), "gzip");
}
