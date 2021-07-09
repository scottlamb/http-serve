# http-serve

[![crates.io](https://meritbadge.herokuapp.com/http-serve)](https://crates.io/crates/http-serve)
[![Released API docs](https://docs.rs/http-serve/badge.svg)](https://docs.rs/http-serve/)
[![CI](https://github.com/scottlamb/http-serve/workflows/CI/badge.svg)](https://github.com/scottlamb/http-serve/actions?query=workflow%3ACI)

Rust helpers for serving HTTP GET and HEAD responses with
[hyper](https://crates.io/crates/hyper) 0.14.x and
[tokio](https://crates.io/crates/tokio).

This crate supplies two ways to respond to HTTP GET and HEAD requests:

*   the `serve` function can be used to serve an `Entity`, a trait representing
    reusable, byte-rangeable HTTP entities. `Entity` must be able to produce
    exactly the same data on every call, know its size in advance, and be able
    to produce portions of the data on demand.
*   the `streaming_body` function can be used to add a body to an
    otherwise-complete response.  If a body is needed (on `GET` rather than `HEAD`
    requests, it returns a `BodyWriter` (which implements `std::io::Writer`).
    The caller should produce the complete body or call `BodyWriter::abort`,
    causing the HTTP stream to terminate abruptly.

It supplies a static file `Entity` implementation and a (currently Unix-only)
helper for serving a full directory tree from the local filesystem, including
automatically looking for `.gz`-suffixed files when the client advertises
`Accept-Encoding: gzip`.

## Why two ways?

They have pros and cons. This table shows some of them:

<table>
  <tr><th><th><code>serve</code><th><code>streaming_body</code></tr>
  <tr><td>automatic byte range serving<td>yes<td>no [<a href="#range">1</a>]</tr>
  <tr><td>backpressure<td>yes<td>no [<a href="#backpressure">2</a>]</tr>
  <tr><td>conditional GET<td>yes<td>no [<a href="#conditional_get">3</a>]</tr>
  <tr><td>sends first byte before length known<td>no<td>yes</tr>
  <tr><td>automatic gzip content encoding<td>no [<a href="#gzip">4</a>]<td>yes</tr>
</table>

<a name="range">\[1\]</a>: `streaming_body` always sends the full body. Byte range serving
wouldn't make much sense with its interface. The application will generate all the bytes
every time anyway, and `http-serve`'s buffering logic would have to be complex
to handle multiple ranges well.

<a name="backpressure">\[2\]</a>: `streaming_body` is often appended to while holding
a lock or open database transaction, where backpressure is undesired. It'd be
possible to add support for "wait points" where the caller explicitly wants backpressure. This
would make it more suitable for large streams, even infinite streams like
[Server-sent events](https://developer.mozilla.org/en-US/docs/Web/API/Server-sent_events).

<a name="conditional_get">\[3\]</a>: `streaming_body` doesn't yet support
generating etags or honoring conditional GET requests. PRs welcome!

<a name="gzip">\[4\]</a>: `serve` doesn't automatically apply `Content-Encoding:
gzip` because the content encoding is a property of the entity you supply. The
entity's etag, length, and byte range boundaries must match the encoding. You
can use the `http_serve::should_gzip` helper to decide between supplying a plain
or gzipped entity. `serve` could automatically apply the related
`Transfer-Encoding: gzip` where the browser requests it via `TE: gzip`, but
common browsers have
[chosen](https://bugs.chromium.org/p/chromium/issues/detail?id=94730) to avoid
requesting or handling `Transfer-Encoding`.

See the [documentation](https://docs.rs/http-serve/) for more.

There's a built-in `Entity` implementation, `ChunkedReadFile`. It serves
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

Examples:

*   Serve a single file:
    ```
    $ cargo run --example serve_file /usr/share/dict/words
    ```
*   Serve a directory tree:
    ```
    $ cargo run --features dir --example serve_dir .
    ```

## Authors

See the [AUTHORS](AUTHORS) file for details.

## License

Your choice of MIT or Apache; see [LICENSE-MIT.txt](LICENSE-MIT.txt) or
[LICENSE-APACHE](LICENSE-APACHE.txt), respectively.
