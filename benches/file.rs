// Copyright (c) 2016-2018 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use criterion::{criterion_group, criterion_main, Criterion};
use http::{Request, Response};
use hyper::Body;
use once_cell::sync::Lazy;
use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::sync::Mutex;
use std::time::Duration;
use tempfile::TempDir;

type BoxedError = Box<dyn std::error::Error + Send + Sync>;

async fn serve(req: Request<Body>) -> Result<Response<Body>, BoxedError> {
    let f = tokio::task::block_in_place::<_, Result<_, BoxedError>>(move || {
        let f = std::fs::File::open(&*PATH.lock().unwrap())?;
        let headers = http::header::HeaderMap::new();
        Ok(http_serve::ChunkedReadFile::new(f, headers)?)
    })?;
    Ok(http_serve::serve(f, &req))
}

/// Returns the hostport of a newly created, never-destructed server.
fn new_server() -> String {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let make_svc = hyper::service::make_service_fn(|_conn| {
            futures_util::future::ok::<_, hyper::Error>(hyper::service::service_fn(serve))
        });
        let rt = tokio::runtime::Runtime::new().unwrap();
        let _guard = rt.enter();

        let addr = ([127, 0, 0, 1], 0).into();
        let srv = hyper::Server::bind(&addr).tcp_nodelay(true).serve(make_svc);
        let addr = srv.local_addr();
        tx.send(addr).unwrap();
        rt.block_on(srv).unwrap();
    });
    let addr = rx.recv().unwrap();
    format!("http://{}:{}", addr.ip(), addr.port())
}

static PATH: Lazy<Mutex<OsString>> = Lazy::new(|| Mutex::new(OsString::new()));
static SERVER: Lazy<String> = Lazy::new(new_server);

/// Sets up the server to serve a `kib`-KiB file, until the returned `TempDir`
/// goes out of scope and the file is deleted.
fn setup(kib: usize) -> TempDir {
    let tmpdir = tempfile::tempdir().unwrap();
    let tmppath = tmpdir.path().join("f");
    {
        let p = &mut *PATH.lock().unwrap();
        p.clear();
        p.push(&tmppath);
    }
    let mut tmpfile = File::create(tmppath).unwrap();
    for _ in 0..kib {
        tmpfile.write_all(&[0; 1024]).unwrap();
    }
    tmpdir
}

fn serve_full_entity(b: &mut criterion::Bencher, kib: &usize) {
    let _tmpdir = setup(*kib);
    let client = reqwest::Client::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    b.to_async(&rt).iter(|| async {
        let resp = client.get(&*SERVER).send().await.unwrap();
        assert_eq!(reqwest::StatusCode::OK, resp.status());
        let b = resp.bytes().await.unwrap();
        assert_eq!(1024 * *kib, b.len());
    });
}

fn serve_last_byte_1mib(b: &mut criterion::Bencher) {
    let _tmpdir = setup(1024);
    let client = reqwest::Client::new();
    let rt = tokio::runtime::Runtime::new().unwrap();
    b.to_async(&rt).iter(|| async {
        let resp = client
            .get(&*SERVER)
            .header("Range", "bytes=-1")
            .send()
            .await
            .unwrap();
        assert_eq!(reqwest::StatusCode::PARTIAL_CONTENT, resp.status());
        let b = resp.bytes().await.unwrap();
        assert_eq!(1, b.len());
    });
}

fn criterion_benchmark(c: &mut Criterion) {
    let mut g = c.benchmark_group("serve_full_entity");
    g.throughput(criterion::Throughput::Bytes(1024))
        .bench_function("1kib", |b| serve_full_entity(b, &1));
    g.throughput(criterion::Throughput::Bytes(1024 * 1024))
        .bench_function("1mib", |b| serve_full_entity(b, &1024));
    g.finish();
    c.bench_function("serve_last_byte_1mib", serve_last_byte_1mib);
}

criterion_group! {
    name = benches;

    // Tweak the config to run more quickly; http-serve has many bench cases.
    config = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_millis(100))
        .measurement_time(Duration::from_secs(1));
    targets = criterion_benchmark
}
criterion_main!(benches);
