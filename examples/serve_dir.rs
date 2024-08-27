// Copyright (c) 2020 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Test program which serves the current directory on `http://127.0.0.1:1337/`.
//! Note this requires `--features dir`.

use http::header::{self, HeaderMap, HeaderValue};
use http_serve::dir;
use hyper_util::rt::TokioIo;
use std::fmt::Write;
use std::io::Write as RawWrite;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

use http_serve::Body;

fn is_dir(dir: &nix::dir::Dir, ent: &nix::dir::Entry) -> Result<bool, nix::Error> {
    // Many filesystems return file types in the directory entries.
    if let Some(t) = ent.file_type() {
        return Ok(t == nix::dir::Type::Directory);
    }

    // ...but some require an fstat call.
    use nix::sys::stat::{fstatat, SFlag};
    use std::os::unix::io::AsRawFd;
    let stat = fstatat(
        Some(dir.as_raw_fd()),
        ent.file_name(),
        nix::fcntl::AtFlags::empty(),
    )?;
    let mode = SFlag::from_bits(stat.st_mode).unwrap();
    Ok(mode.contains(SFlag::S_IFDIR))
}

fn reply(
    req: http::Request<hyper::body::Incoming>,
    node: dir::Node,
) -> Result<http::Response<Body>, ::std::io::Error> {
    if node.metadata().is_dir() {
        if !req.uri().path().ends_with("/") {
            let mut loc = ::bytes::BytesMut::with_capacity(req.uri().path().len() + 1);
            write!(loc, "{}/", req.uri().path()).unwrap();
            let loc = HeaderValue::from_maybe_shared(loc.freeze()).unwrap();
            return Ok(http::Response::builder()
                .status(http::StatusCode::MOVED_PERMANENTLY)
                .header(http::header::LOCATION, loc)
                .body(http_serve::Body::empty())
                .unwrap());
        }
        let (mut resp, stream) = http_serve::streaming_body(&req).build();
        if let Some(mut w) = stream {
            let mut dir = nix::dir::Dir::from(node.into_file()).unwrap(); // TODO: don't unwrap.
            write!(
                &mut w,
                "<!DOCTYPE html>\n<title>directory listing</title>\n<ul>\n"
            )?;
            let mut ents: Vec<_> = dir.iter().map(|e| e.unwrap()).collect();
            ents.sort_unstable_by(|a, b| a.file_name().cmp(b.file_name()));
            for ent in ents {
                let p = match ent.file_name().to_str() {
                    Err(_) => continue, // skip non-UTF-8
                    Ok(".") => continue,
                    Ok(p) => p,
                };
                if p == ".." && req.uri().path() == "/" {
                    continue;
                };
                write!(&mut w, "<li><a href=\"")?;
                htmlescape::encode_minimal_w(p, &mut w)?;
                let is_dir = is_dir(&dir, &ent).unwrap(); // TODO: don't unwrap.
                if is_dir {
                    write!(&mut w, "/")?;
                }
                write!(&mut w, "\">")?;
                htmlescape::encode_minimal_w(p, &mut w)?;
                if is_dir {
                    write!(&mut w, "/")?;
                }
                write!(&mut w, "</a>\n")?;
            }
            write!(&mut w, "</ul>\n")?;
        }
        resp.headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/html"));
        return Ok(resp);
    }
    let mut h = HeaderMap::new();
    node.add_encoding_headers(&mut h);
    if let Some(dot) = req.uri().path().rfind('.') {
        let ext = &req.uri().path()[dot + 1..];
        if let Some(mime_type) = mime_guess::from_ext(ext).first_raw() {
            h.insert(header::CONTENT_TYPE, HeaderValue::from_static(mime_type));
        }
    }
    let f = node.into_file_entity(h)?;
    Ok(http_serve::serve(f, &req))
}

async fn serve(
    fs_dir: &Arc<dir::FsDir>,
    req: http::Request<hyper::body::Incoming>,
) -> Result<http::Response<Body>, ::std::io::Error> {
    let p = if req.uri().path() == "/" {
        "."
    } else {
        &req.uri().path()[1..]
    };

    let e = match Arc::clone(&fs_dir)
        .get(p, req.headers())
        .await
        .and_then(|node| reply(req, node))
    {
        Ok(res) => return Ok(res),
        Err(e) => e,
    };
    let status = match e.kind() {
        ::std::io::ErrorKind::NotFound => http::StatusCode::NOT_FOUND,
        _ => http::StatusCode::INTERNAL_SERVER_ERROR,
    };
    Ok(http::Response::builder()
        .status(status)
        .body(http_serve::Body::from(format!("I/O error: {}", e)))
        .unwrap())
}

#[tokio::main]
async fn main() {
    let dir: &'static Arc<dir::FsDir> =
        Box::leak(Box::new(dir::FsDir::builder().for_path(".").unwrap()));
    let addr = SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 1337));
    let listener = TcpListener::bind(addr).await.unwrap();
    println!("Serving . on http://{}", listener.local_addr().unwrap());
    loop {
        let (tcp, _) = listener.accept().await.unwrap();
        tokio::spawn(async move {
            tcp.set_nodelay(true).unwrap();
            let io = TokioIo::new(tcp);
            hyper::server::conn::http1::Builder::new()
                .serve_connection(io, hyper::service::service_fn(move |req| serve(dir, req)))
                .await
                .unwrap();
        });
    }
}
