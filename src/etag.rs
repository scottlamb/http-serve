// Copyright (c) 2016-2018 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use http::header::{self, HeaderMap, HeaderValue};

/// Performs weak validation of two etags (such as B"W/\"foo\"" or B"\"bar\"").
pub fn weak_eq(mut a: &[u8], mut b: &[u8]) -> bool {
    if a.starts_with(b"W/") {
        a = &a[2..];
    }
    if b.starts_with(b"W/") {
        b = &b[2..];
    }
    a == b
}

/// Performs strong validation of two etags (such as B"W/\"foo\"" or B"\"bar\"").
pub fn strong_eq(a: &[u8], b: &[u8]) -> bool {
    a == b && !a.starts_with(b"W/")
}

/// Matches a `1#entity-tag`, where `#` is as specified in RFC 7230 section 7.
///
/// > A construct `#` is defined, similar to `*`, for defining
/// > comma-delimited lists of elements.  The full form is `<n>#<m>element`
/// > indicating at least `<n>` and at most `<m>` elements, each separated by a
/// > single comma (`,`) and optional whitespace (OWS).
///
/// > `OWS = *( SP / HTAB )`
struct List<'a> {
    remaining: &'a [u8],
    corrupt: bool,
}

impl<'a> List<'a> {
    fn from(l: &[u8]) -> List {
        List {
            remaining: l,
            corrupt: false,
        }
    }
}

impl<'a> Iterator for List<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining.is_empty() {
            return None;
        }

        // If on an etag, find its end. Note the '"' can't be escaped, simplifying matters.
        let end = if self.remaining.starts_with(b"W/\"") {
            self.remaining[3..]
                .iter()
                .position(|&b| b == b'"')
                .map(|p| p + 3)
        } else if self.remaining.starts_with(b"\"") {
            self.remaining[1..]
                .iter()
                .position(|&b| b == b'"')
                .map(|p| p + 1)
        } else {
            self.corrupt = true;
            None
        };
        let end = match end {
            None => {
                self.corrupt = true;
                return None;
            }
            Some(e) => e,
        };
        let (etag, mut rem) = self.remaining.split_at(end + 1);
        if rem.starts_with(b",") {
            rem = &rem[1..];
            while !rem.is_empty() && (rem[0] == b' ' || rem[0] == b'\t') {
                rem = &rem[1..];
            }
        }
        self.remaining = rem;
        Some(etag)
    }
}

/// Returns true if `req` doesn't have an `If-None-Match` header matching `req`.
pub fn none_match(etag: &Option<HeaderValue>, req_hdrs: &HeaderMap) -> Option<bool> {
    let m = match req_hdrs.get(header::IF_NONE_MATCH) {
        None => return None,
        Some(m) => m.as_bytes(),
    };
    if m == b"*" {
        return Some(false);
    }
    let mut none_match = true;
    if let Some(ref some_etag) = *etag {
        let mut items = List::from(m);
        for item in &mut items {
            // RFC 7232 section 3.2: A recipient MUST use the weak comparison function when
            // comparing entity-tags for If-None-Match
            if none_match && weak_eq(item, some_etag.as_bytes()) {
                none_match = false;
            }
        }
        if items.corrupt {
            return None; // ignore the header.
        }
    }
    Some(none_match)
}

/// Returns true if `req` has no `If-Match` header or one which matches `etag`.
pub fn any_match(etag: &Option<HeaderValue>, req_hdrs: &HeaderMap) -> Result<bool, &'static str> {
    let m = match req_hdrs.get(header::IF_MATCH) {
        None => return Ok(true),
        Some(m) => m.as_bytes(),
    };
    if m == b"*" {
        // The absent header and "If-Match: *" cases differ only when there is no entity to serve.
        // We always have an entity to serve, so consider them identical.
        return Ok(true);
    }
    let mut any_match = false;
    if let Some(ref some_etag) = *etag {
        let mut items = List::from(m);
        for item in &mut items {
            if !any_match && strong_eq(item, some_etag.as_bytes()) {
                any_match = true;
            }
        }
        if items.corrupt {
            return Err("Unparseable If-Match header");
        }
    }
    Ok(any_match)
}

#[cfg(test)]
mod tests {
    use super::List;

    #[test]
    fn weak_eq() {
        assert!(super::weak_eq(b"\"foo\"", b"\"foo\""));
        assert!(!super::weak_eq(b"\"foo\"", b"\"bar\""));
        assert!(super::weak_eq(b"W/\"foo\"", b"\"foo\""));
        assert!(super::weak_eq(b"\"foo\"", b"W/\"foo\""));
        assert!(super::weak_eq(b"W/\"foo\"", b"W/\"foo\""));
        assert!(!super::weak_eq(b"W/\"foo\"", b"W/\"bar\""));
    }

    #[test]
    fn strong_eq() {
        assert!(super::strong_eq(b"\"foo\"", b"\"foo\""));
        assert!(!super::strong_eq(b"\"foo\"", b"\"bar\""));
        assert!(!super::strong_eq(b"W/\"foo\"", b"\"foo\""));
        assert!(!super::strong_eq(b"\"foo\"", b"W/\"foo\""));
        assert!(!super::strong_eq(b"W/\"foo\"", b"W/\"foo\""));
        assert!(!super::strong_eq(b"W/\"foo\"", b"W/\"bar\""));
    }

    #[test]
    fn empty_list() {
        let mut l = List::from(b"");
        assert_eq!(l.next(), None);
        assert!(!l.corrupt);
    }

    #[test]
    fn nonempty_list() {
        let mut l = List::from(b"\"foo\", \tW/\"bar\",W/\"baz\"");
        assert_eq!(l.next(), Some(&b"\"foo\""[..]));
        assert_eq!(l.next(), Some(&b"W/\"bar\""[..]));
        assert_eq!(l.next(), Some(&b"W/\"baz\""[..]));
        assert_eq!(l.next(), None);
        assert!(!l.corrupt);
    }

    #[test]
    fn comma_in_etag() {
        let mut l = List::from(b"\"foo, bar\", \"baz\"");
        assert_eq!(l.next(), Some(&b"\"foo, bar\""[..]));
        assert_eq!(l.next(), Some(&b"\"baz\""[..]));
        assert_eq!(l.next(), None);
        assert!(!l.corrupt);
    }

    #[test]
    fn corrupt_list() {
        let mut l = List::from(b"\"foo\", bar");
        assert_eq!(l.next(), Some(&b"\"foo\""[..]));
        assert_eq!(l.next(), None);
        assert!(l.corrupt);
    }
}
