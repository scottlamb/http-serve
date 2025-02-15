# 0.4.0-rc.2 (2025-02-15)

* Fix `http_serve::streaming_body` bug in which it would end the stream
  prematurely if the body is polled twice with the same waker before additional
  data is added. Unclear if this bug was ever hit in practice.

# 0.4.0-rc.1 (2024-08-27)

* upgrade to `http` crate version 1.0.
* bump minimum Rust to 1.79.

# 0.3.6 (2022-05-01)

* Fix [#25](https://github.com/scottlamb/http-serve/issues/25):
  unsound use of `Vec::set_len` within `ChunkedReadFile`.
* Fix infinite loop in `<ChunkedReadFile as Entity>::get_range`
  if the file ends before the specified range does. This could
  happen for example if the file is truncated between the
  `ChunkedReadFile` being created and read.

# 0.3.5 (2021-12-29)

* Remove a `println!` accidentally introduced in 0.3.3.

# 0.3.4 (2021-08-31)

* Avoid a second `fstat` call per `ChunkedReadFile` construction on Unix.

# 0.3.3 (2021-08-19)

* Fix [#23](https://github.com/scottlamb/http-serve/issues/23):
  erroneous `304 Not Modified` responses when the etag doesn't match
  `If-None-Match` but the date isn't after `If-Unmodified-Since`.

# 0.3.2 (2021-07-09)

* documentation improvements
* update deps

# 0.3.1 (2021-05-10)

* Bump minimum Rust version to 1.46.
* Remove an `unsafe` block via the `pin-project` library.
* Fix references to old hyper versions in documentation.

# 0.3.0 (2021-01-09)

* BREAKING CHANGE: update to hyper 0.14, tokio 1.0, bytes 1.0.
* Bump minimum Rust version to 1.45.
* Add new `dir` module for local filesystem directory traversal on Unix.

# 0.2.2 (2020-05-08)

* Don't panic on unparseable `Range` header values.
* Reduce code bloat, particularly when there are multiple
  implementations of `Entity` for a given `Data` and `Error` type.

# 0.2.1 (2019-12-31)

* Use the freshly-released reqwest 0.10.x in tests. This avoids pulling in two
  copies of the hyper/tokio/http/http-body/bytes ecosystems.

# 0.2.0 (2019-12-28)

* BREAKING CHANGE: update to hyper 0.13.x, tokio 0.2.x, bytes 0.5.x, http
  0.2.x, futures 0.3.x.
* BREAKING CHANGE: use
  [`tokio::task::block_in_place`](https://docs.rs/tokio/0.2.2/tokio/task/fn.block_in_place.html)
  from `http_serve::ChunkedReadFile` rather than hand off to a thread pool.
  This simplifies the `ChunkedReadFile` interface.
* BREAKING CHANGE: bump minimum Rust version to 1.40.0.
* Convert benchmarks to [criterion](https://crates.io/crates/criterion)
  to support running with stable Rust.

# 0.1.2 (2018-10-29)

* Upgrade tests to reqwest 0.9 (#13)

# 0.1.1 (2018-10-20)

* Add Windows support (#10)

# 0.1.0 (2018-08-06)

* Initial release
