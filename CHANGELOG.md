# 0.2.0 (unreleased)

* BREAKING CHANGE: use
  [`tokio-threadpool::blocking`](https://docs.rs/tokio-threadpool/0.1.15/tokio_threadpool/fn.blocking.html)
  from `http_serve::ChunkedReadFile` rather than hand off to a
  [futures-cpupool](https://crates.io/crates/futures-cpupool). This simplifies
  the `ChunkedReadFile` interface.
* BREAKING CHANGE: bump minimum Rust version to 1.34.0.
* Convert benchmarks to [criterion](https://crates.io/crates/criterion)
  to support running with stable Rust.

# 0.1.2

* Upgrade tests to reqwest 0.9 (#13)

# 0.1.1

* Add Windows support (#10)

# 0.1.0

* Initial release
