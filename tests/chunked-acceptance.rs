// Copyright (c) 2018 Scott Lamb <slamb@slamb.org>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

extern crate env_logger;
extern crate futures;
extern crate http_serve;
extern crate hyper;
#[macro_use]
extern crate lazy_static;
extern crate reqwest;
extern crate tokio_core as tokio;

use futures::{Future, Stream};
use futures::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::Mutex;

type Body = Box<Stream<Item = Vec<u8>, Error = hyper::Error> + Send + 'static>;

struct MyService(tokio::reactor::Handle);

impl hyper::server::Service for MyService {
    type Request = hyper::server::Request;
    type Response = hyper::server::Response<Body>;
    type Error = hyper::Error;
    type Future = ::futures::future::FutureResult<Self::Response, hyper::Error>;

    fn call(&self, req: hyper::server::Request) -> Self::Future {
        let cmds = CMDS.lock().unwrap().remove(req.uri().path()).unwrap();
        let mut resp: Self::Response = Self::Response::new();
        let mut w = http_serve::streaming_body(&req, &mut resp).build().unwrap();
        self.0.spawn(cmds.for_each(move |cmd| {
            match cmd {
                Cmd::WriteAll(s) => w.write_all(s).unwrap(),
                Cmd::Abort => w.abort(),
                Cmd::Flush => w.flush().unwrap(),
            }
            futures::future::ok(())
        }));
        futures::future::ok(resp)
    }
}

enum Cmd {
    WriteAll(&'static [u8]),
    Flush,
    Abort,
}

struct Server {
    addr: String,
}

fn new_server() -> Server {
    let (server_tx, server_rx) = ::std::sync::mpsc::channel();
    ::std::thread::spawn(move || {
        let addr = "127.0.0.1:0".parse().unwrap();
        let mut core = tokio::reactor::Core::new().unwrap();
        let h = core.handle();
        let srv = hyper::server::Http::new()
            .serve_addr_handle(&addr, &core.handle(), move || Ok(MyService(h.clone())))
            .unwrap();
        let h = core.handle();
        let addr = srv.incoming_ref().local_addr();
        core.handle().spawn(srv.for_each(move |conn| {
            h.spawn(
                conn.map(|_| ())
                    .map_err(|err| println!("srv error: {:?}", err)),
            );
            Ok(())
        }).map_err(|_| ()));
        server_tx
            .send(Server {
                addr: format!("http://{}:{}", addr.ip(), addr.port()),
            })
            .unwrap();
        core.run(futures::future::empty::<(), ()>()).unwrap();
    });
    server_rx.recv().unwrap()
}

lazy_static! {
    static ref CMDS: Mutex<HashMap<&'static str, UnboundedReceiver<Cmd>>> = {
        Mutex::new(HashMap::new())
    };

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
    let _ = env_logger::init();
    let (cmds, mut req) = setup_req(path, auto_gzip);
    let mut resp = req.send().unwrap();

    cmds.unbounded_send(Cmd::WriteAll(b"1234")).unwrap();
    cmds.unbounded_send(Cmd::WriteAll(b"5678")).unwrap();
    drop(cmds);
    let mut buf = Vec::new();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"12345678", &buf[..]);
    assert_eq!(
        None,
        resp.headers().get::<reqwest::header::ContentEncoding>()
    );
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
    let _ = env_logger::init();
    let (cmds, mut req) = setup_req(path, auto_gzip);
    let mut resp = req.send().unwrap();

    cmds.unbounded_send(Cmd::WriteAll(b"1234")).unwrap();
    cmds.unbounded_send(Cmd::Flush).unwrap();
    cmds.unbounded_send(Cmd::WriteAll(b"5678")).unwrap();
    let mut buf = [0u8; 4];
    resp.read_exact(&mut buf).unwrap();
    assert_eq!(b"1234", &buf);

    cmds.unbounded_send(Cmd::Abort).unwrap();
    assert_eq!(
        io::ErrorKind::UnexpectedEof,
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
    let _ = env_logger::init();
    let (cmds, mut req) = setup_req("/manual_gzip", false);
    let mut resp = req.header(header::AcceptEncoding(vec![
        header::qitem(header::Encoding::Gzip),
    ])).send()
        .unwrap();

    cmds.unbounded_send(Cmd::WriteAll(b"1234")).unwrap();
    drop(cmds);
    let mut buf = Vec::new();
    resp.read_to_end(&mut buf).unwrap();
    assert_eq!(b"\x1f\x8b", &buf[..2]); // gzip magic number.
    assert_eq!(
        Some(&header::ContentEncoding(vec![
            header::Encoding::Gzip,
            header::Encoding::Chunked,
        ])),
        resp.headers().get()
    );
}
