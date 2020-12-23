# http-serve

[![crates.io](https://meritbadge.herokuapp.com/http-serve)](https://crates.io/crates/http-serve)
[![CI](https://github.com/scottlamb/http-serve/workflows/CI/badge.svg)](https://github.com/scottlamb/http-serve/actions?query=workflow%3ACI)

Rust helpers for serving HTTP GET and HEAD responses with
[hyper](https://crates.io/crates/hyper) 0.13.x and
[tokio](https://crates.io/crates/tokio).

This crate supplies two ways to respond to HTTP GET and HEAD requests:

*   the `serve` function can be used to serve an `Entity`, a trait representing
    reusable, byte-rangeable HTTP entities. `Entity` must be able to produce
    exactly the same data on every call, know its size in advance, and be able
    to produce portions of the data on demand.
*   the `streaming_body` function can be used to add a body to an
    otherwise-complete response.  If a body is needed, it returns a
    `BodyWriter` (which implements `std::io::Writer`). The caller should
    produce the complete body or call `BodyWriter::abort`, causing the HTTP
    stream to terminate abruptly.

## Why two ways?

They have pros and cons. This chart shows some of them:

<table>
  <tr><th><th><code>serve</code><th><code>streaming_body</code></tr>
  <tr><td>automatic byte range serving<td>yes<td>no (always sends full body)</tr>
  <tr><td>backpressure<td>yes<td>no</tr>
  <tr><td>conditional GET<td>yes<td>unimplemented (always sends body)</tr>
  <tr><td>sends first byte before length known<td>no<td>yes</tr>
  <tr><td>automatic gzip content encoding<td>no<td>yes</tr>
</table>

There's also a built-in `Entity` implementation, `ChunkedReadFile`. It serves
static files from the local filesystem, reading chunks in a separate thread
pool to avoid blocking the tokio reactor thread.

You're not limited to the built-in entity type(s), though. You could supply
your own that do anything you desire:

*   bytes built into the binary via `include_bytes!`.
*   bytes retrieved from another HTTP server or network filesystem.
*   memcached-based caching of another entity.
*   anything else for which it's cheaper to compute the etag, size, and a byte
    range than the entirety of the data. (See
    [moonfire-nvr](https://github.com/scottlamb/moonfire-nvr)'s logic for
    generating `.mp4` files to represent arbitrary time ranges.)

`http_serve::serve` is similar to golang's
[http.ServeContent](https://golang.org/pkg/net/http/#ServeContent). It was
extracted from [moonfire-nvr](https://github.com/scottlamb/moonfire-nvr)'s
`.mp4` file serving.

Try the example:

```
$ cargo run --example serve_file /usr/share/dict/words
```

## Authors

See the [AUTHORS](AUTHORS) file for details.

## License

Your choice of MIT or Apache; see [LICENSE-MIT.txt](LICENSE-MIT.txt) or
[LICENSE-APACHE](LICENSE-APACHE.txt), respectively.
