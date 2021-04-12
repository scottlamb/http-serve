// Copyright (c) 2020 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Support for directory traversal from local filesystem.
//! Currently Unix-only.

use http::header::{self, HeaderMap, HeaderValue};
use memchr::memchr;
use std::convert::TryInto;
use std::ffi::CStr;
use std::fs::File;
use std::io::{Error, ErrorKind};
use std::os::unix::{ffi::OsStrExt, io::FromRawFd};
use std::path::Path;
use std::sync::Arc;

/// A builder for a `FsDir`.
pub struct FsDirBuilder {
    auto_gzip: bool,
}

impl FsDirBuilder {
    /// Enables or disables automatic gzipping based on `Accept-Encoding` request headers.
    ///
    /// Default is `true`.
    pub fn auto_gzip(mut self, auto_gzip: bool) -> Self {
        self.auto_gzip = auto_gzip;
        self
    }

    /// Returns a `FsDir` for the given path.
    pub fn for_path<P: AsRef<Path>>(&self, path: P) -> Result<Arc<FsDir>, Error> {
        FsDir::open(path.as_ref(), self.auto_gzip)
    }
}

/// A base directory for local filesystem traversal.
pub struct FsDir {
    auto_gzip: bool,
    fd: std::os::unix::io::RawFd,
}

impl FsDir {
    pub fn builder() -> FsDirBuilder {
        FsDirBuilder { auto_gzip: true }
    }

    fn open(path: &Path, auto_gzip: bool) -> Result<Arc<Self>, Error> {
        let path = path.as_os_str().as_bytes();
        if memchr(0, path).is_some() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "path contains NUL byte",
            ));
        }
        if path.len() >= libc::PATH_MAX.try_into().unwrap() {
            return Err(Error::new(ErrorKind::InvalidInput, "path is too long"));
        }
        let mut buf = [0u8; libc::PATH_MAX as usize];
        unsafe { std::ptr::copy_nonoverlapping(path.as_ptr(), buf.as_mut_ptr(), path.len()) };
        let fd = unsafe {
            libc::open(
                buf.as_ptr() as *const libc::c_char,
                libc::O_DIRECTORY | libc::O_CLOEXEC,
                0,
            )
        };
        if fd < 0 {
            return Err(Error::last_os_error());
        }
        Ok(Arc::new(FsDir { auto_gzip, fd }))
    }

    /// Opens a path within this base directory.
    ///
    /// If using `auto_gzip` (the default) and `req_hdrs` indicate the client supports `gzip`, will
    /// look for a `.gz`-suffixed version of this path first and note that in the returned `Node`.
    /// `.gz`-suffixed directories are ignored.
    ///
    /// Validates that `path` has no `..` segments or interior NULs. Currently doesn't check for
    /// symlinks, however. That may eventually be configurable via the builder.
    pub async fn get(self: Arc<Self>, path: &str, req_hdrs: &HeaderMap) -> Result<Node, Error> {
        if let Err(e) = validate_path(path) {
            return Err(Error::new(ErrorKind::InvalidInput, e));
        }
        let mut buf = Vec::with_capacity(path.len() + b".gz\0".len());
        buf.extend_from_slice(path.as_bytes());
        let should_gzip = self.auto_gzip && super::should_gzip(req_hdrs);
        tokio::task::spawn_blocking(move || -> Result<Node, Error> {
            if should_gzip {
                let path_len = buf.len();
                buf.extend_from_slice(&b".gz\0"[..]);
                match self.open_file(
                    // This is safe because we've ensured in validate_path that there are no
                    // interior NULs, and we've just appended a NUL.
                    unsafe { CStr::from_bytes_with_nul_unchecked(&buf[..]) },
                ) {
                    Ok(file) => {
                        let metadata = file.metadata()?;
                        if !metadata.is_dir() {
                            return Ok(Node {
                                file,
                                metadata,
                                auto_gzip: self.auto_gzip,
                                is_gzipped: true,
                            });
                        }
                    }
                    Err(ref e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                };
                buf.truncate(path_len);
            }

            buf.push(b'\0');

            // As in the case above, we've ensured buf contains exactly one NUL, at the end.
            let p = unsafe { CStr::from_bytes_with_nul_unchecked(&buf[..]) };
            let file = self.open_file(p)?;
            let metadata = file.metadata()?;
            Ok(Node {
                file,
                metadata,
                auto_gzip: self.auto_gzip,
                is_gzipped: false,
            })
        })
        .await
        .unwrap_or_else(|e: tokio::task::JoinError| Err(Error::new(ErrorKind::Other, e)))
    }

    /// Opens the given file with a path relative to this directory.
    /// Performs the blocking I/O directly from this thread.
    fn open_file(&self, path: &CStr) -> Result<File, Error> {
        let fd =
            unsafe { libc::openat(self.fd, path.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC, 0) };
        if fd < 0 {
            return Err(Error::last_os_error());
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

impl Drop for FsDir {
    fn drop(&mut self) {
        unsafe { libc::close(self.fd) };
    }
}

/// An opened path (aka inode on Unix) as returned by `FsDir::open`.
///
/// This is not necessarily a plain file; it could also be a directory, for example.
///
/// The caller can inspect it as desired. If it is a directory, the caller might pass the result of
/// `into_file()` to `nix::dir::Dir::from`. If it is a plain file, the caller might create an
/// `http_serve::Entity` with `into_file_entity()`.
pub struct Node {
    file: std::fs::File,
    metadata: std::fs::Metadata,
    auto_gzip: bool,
    is_gzipped: bool,
}

impl Node {
    /// Converts this node to a `std::fs::File`.
    pub fn into_file(self) -> std::fs::File {
        self.file
    }

    /// Converts this node (which must represent a plain file) into a `ChunkedReadFile`.
    /// The caller is expected to supply all headers. The function `add_encoding_headers`
    /// may be useful.
    pub fn into_file_entity<D, E>(
        self,
        headers: HeaderMap,
    ) -> Result<crate::file::ChunkedReadFile<D, E>, Error>
    where
        D: 'static + Send + Sync + bytes::Buf + From<Vec<u8>> + From<&'static [u8]>,
        E: 'static
            + Send
            + Sync
            + Into<Box<dyn std::error::Error + Send + Sync>>
            + From<Box<dyn std::error::Error + Send + Sync>>,
    {
        crate::file::ChunkedReadFile::new_with_metadata(self.file, &self.metadata, headers)
    }

    /// Returns the (already fetched) metadata for this node.
    pub fn metadata(&self) -> &std::fs::Metadata {
        &self.metadata
    }

    /// Returns the encoding this file is assumed to have applied to the caller's request.
    /// E.g., if automatic gzip compression is enabled and `index.html.gz` was found when the
    /// caller requested `index.html`, this will return `Some("gzip")`. If the caller requests
    /// `index.html.gz`, this will return `None` because the gzip encoding is built in to the
    /// caller's request.
    pub fn encoding(&self) -> Option<&'static str> {
        if self.is_gzipped {
            Some("gzip")
        } else {
            None
        }
    }

    /// Returns true iff the content varies with the request's `Accept-Encoding` header value.
    pub fn encoding_varies(&self) -> bool {
        self.auto_gzip
    }

    /// Adds `Content-Encoding` and `Vary` headers for the encoding to `hdrs`.
    ///
    /// Note if there are other `Vary` header components known to the caller, this method is
    /// inappropriate.
    pub fn add_encoding_headers(&self, hdrs: &mut HeaderMap) {
        if let Some(e) = self.encoding() {
            hdrs.insert(header::CONTENT_ENCODING, HeaderValue::from_static(e));
        }
        if self.auto_gzip {
            hdrs.insert(header::VARY, HeaderValue::from_static("accept-encoding"));
        }
    }
}

/// Ensures path is safe: no NUL bytes, not absolute, no `..` segments.
fn validate_path(path: &str) -> Result<(), &'static str> {
    if memchr::memchr(0, path.as_bytes()).is_some() {
        return Err("path contains NUL byte");
    }
    if path.as_bytes().first() == Some(&b'/') {
        return Err("path is absolute");
    }
    let mut left = path.as_bytes();
    loop {
        let next = memchr::memchr(b'/', left);
        let seg = &left[0..next.unwrap_or(left.len())];
        if seg == b".." {
            return Err("path contains .. segment");
        }
        match next {
            None => break,
            Some(n) => left = &left[n + 1..],
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn path_with_interior_nul() {
        let tmp = tempfile::tempdir().unwrap();
        let fsdir = FsDir::builder().for_path(tmp.path()).unwrap();
        let e = match fsdir.get("foo\0bar", &HeaderMap::new()).await {
            Ok(_) => panic!("should have failed"),
            Err(e) => e,
        };
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(e.to_string(), "path contains NUL byte");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn path_with_parent_dir_segment() {
        let tmp = tempfile::tempdir().unwrap();
        let fsdir = FsDir::builder().for_path(tmp.path()).unwrap();

        for p in &["..", "../foo", "foo/../bar", "foo/.."] {
            let e = match Arc::clone(&fsdir).get(p, &HeaderMap::new()).await {
                Ok(_) => panic!("should have failed"),
                Err(e) => e,
            };
            assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
            assert_eq!(e.to_string(), "path contains .. segment");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn absolute_path() {
        let tmp = tempfile::tempdir().unwrap();
        let fsdir = FsDir::builder().for_path(tmp.path()).unwrap();
        let e = match fsdir.get("/etc/passwd", &HeaderMap::new()).await {
            Ok(_) => panic!("should have failed"),
            Err(e) => e,
        };
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
        assert_eq!(e.to_string(), "path is absolute");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn regular_file() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::spawn(async move {
            let fsdir = FsDir::builder().for_path(tmp.path()).unwrap();
            let p = "foo.txt";
            let contents = b"1234";
            {
                use std::io::Write;
                let mut f = File::create(tmp.path().join(p)).unwrap();
                f.write_all(contents).unwrap();
            }
            let f = fsdir.get("foo.txt", &HeaderMap::new()).await.unwrap();
            assert_eq!(f.metadata.len(), contents.len() as u64);
        })
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::spawn(async move {
            let fsdir = FsDir::builder().for_path(tmp.path()).unwrap();
            match fsdir.get("nonexistent.txt", &HeaderMap::new()).await {
                Ok(_) => panic!("nonexistent file found?!?"),
                Err(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            };
        })
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn symlink_allowed_in_last_path_component() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::spawn(async move {
            let fsdir = FsDir::builder().for_path(tmp.path()).unwrap();
            ::std::os::unix::fs::symlink("/etc/passwd", tmp.path().join("foo.txt")).unwrap();
            fsdir.get("foo.txt", &HeaderMap::new()).await.unwrap();
        })
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn symlink_allowed_in_earlier_path_component() {
        let tmp = tempfile::tempdir().unwrap();
        tokio::spawn(async move {
            let fsdir = FsDir::builder().for_path(tmp.path()).unwrap();
            ::std::os::unix::fs::symlink("/etc", tmp.path().join("etc")).unwrap();
            fsdir.get("etc/passwd", &HeaderMap::new()).await.unwrap();
        })
        .await
        .unwrap()
    }
}
