# http-entity

http-entity is a Rust library for serving GET and HEAD requests on abstract
HTTP entities, handling conditional GETs and byte range serving. This was
extracted from [moonfire-nvr](https://github.com/scottlamb/moonfire-nvr)'s
`.mp4` file serving.

The API is unstable:

   * Currently it's based on the hyper 0.9.x interface. This is likely to
     change in the future to a future hyper async interface or to a
     higher-level abstraction such as Iron.
   * Other details may change if needed to better support serving multiple
     encodings, caching, varies, etc.

## Author

Scott Lamb, slamb@slamb.org

## License

MIT; see [LICENSE.txt](LICENSE.txt).
