# http-entity

http-entity is a Rust library for serving GET and HEAD requests on abstract
HTTP entities, handling conditional GETs and byte range serving. This was
extracted from [moonfire-nvr](https://github.com/scottlamb/moonfire-nvr)'s
`.mp4` file serving.

The API is in flight:

   * The `master` branch is based on hyper 0.10.x.
   * The `hyper-0.11.x` branch is based on hyper's master branch, which is
     intended to become hyper 0.11.x. The hyper API, and the http-entity API,
     are still unstable.

## Author

Scott Lamb, slamb@slamb.org

## License

MIT; see [LICENSE.txt](LICENSE.txt).
