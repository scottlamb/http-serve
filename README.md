# http-entity

`http-entity` is a Rust crate for serving GET and HEAD requests on abstract
HTTP entities, handling conditional GETs and byte range serving. This was
extracted from [moonfire-nvr](https://github.com/scottlamb/moonfire-nvr)'s
`.mp4` file serving.

Based on hyper 0.11.x and tokio.

# http-file

`http-file` is a Rust crate which provides a `http_entity::Entity`
for serving static files from the local filesystem.

Try the example:

```
$ cargo run --example serve_file /usr/share/dict/words
```

## Author

Scott Lamb, slamb@slamb.org

## License

MIT; see [LICENSE.txt](LICENSE.txt).
