# unreleased

* Don't panic on unparseable `Range` header values.

# 0.2.1

* Use the freshly-released reqwest 0.10.x in tests. This avoids pulling in two
  copies of the hyper/tokio/http/http-body/bytes ecosystems.

# 0.2.0

* BREAKING CHANGE: update to hyper 0.13.x, tokio 0.2.x, bytes 0.5.x, http
  0.2.x, futures 0.3.x.
* BREAKING CHANGE: use
  [`tokio::task::block_in_place`](https://docs.rs/tokio/0.2.2/tokio/task/fn.block_in_place.html)
  from `http_serve::ChunkedReadFile` rather than hand off to a thread pool.
  This simplifies the `ChunkedReadFile` interface.
* BREAKING CHANGE: bump minimum Rust version to 1.40.0.
* Convert benchmarks to [criterion](https://crates.io/crates/criterion)
  to support running with stable Rust.

# 0.1.2

* Upgrade tests to reqwest 0.9 (#13)

# 0.1.1

* Add Windows support (#10)

# 0.1.0

* Initial release
