// Copyright (c) 2020 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Test program which serves the current directory on `http://127.0.0.1:1337/`.
//! Note this requires `--features dir`.

use futures_util::future;
use http::header::{self, HeaderMap, HeaderValue};
use http_serve::dir;
use hyper::service::{make_service_fn, service_fn};
use hyper::Body;
use std::fmt::Write;
use std::io::Write as RawWrite;
use std::sync::Arc;

fn is_dir(dir: &nix::dir::Dir, ent: &nix::dir::Entry) -> Result<bool, nix::Error> {
    if let Some(t) = ent.file_type() {
        return Ok(t == nix::dir::Type::Directory);
    }
    use nix::sys::stat::{fstatat, SFlag};
    use std::os::unix::io::AsRawFd;
    let stat = fstatat(
        dir.as_raw_fd(),
        ent.file_name(),
        nix::fcntl::AtFlags::empty(),
    )?;
    let mode = SFlag::from_bits(stat.st_mode).unwrap();
    Ok(mode.contains(SFlag::S_IFDIR))
}

fn reply(
    req: http::Request<Body>,
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
                .body(Body::empty())
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
    req: http::Request<Body>,
) -> Result<http::Response<Body>, ::std::io::Error> {
    let p = if req.uri().path() == "/" {
        "."
    } else {
        &req.uri().path()[1..]
    };
    // TODO: this should go through the same unwrapping.
    let node = Arc::clone(&fs_dir).get(p, req.headers()).await?;
    let e = match reply(req, node) {
        Ok(res) => return Ok(res),
        Err(e) => e,
    };
    let status = match e.kind() {
        ::std::io::ErrorKind::NotFound => http::StatusCode::NOT_FOUND,
        _ => http::StatusCode::INTERNAL_SERVER_ERROR,
    };
    Ok(http::Response::builder()
        .status(status)
        .body(format!("I/O error: {}", e).into())
        .unwrap())
}

#[tokio::main]
async fn main() {
    let dir: &'static Arc<dir::FsDir> =
        Box::leak(Box::new(dir::FsDir::builder().for_path(".").unwrap()));
    env_logger::init();
    let addr = ([127, 0, 0, 1], 1337).into();
    let make_svc = make_service_fn(move |_conn| {
        future::ok::<_, std::convert::Infallible>(service_fn(move |req| serve(dir, req)))
    });
    let server = hyper::Server::bind(&addr).serve(make_svc);
    println!("Serving . on http://{} with 1 thread.", server.local_addr());
    server.await.unwrap();
}
