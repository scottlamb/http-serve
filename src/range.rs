// Copyright (c) 2016-2018 The http-serve developers
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE.txt or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT.txt or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use http::header::HeaderValue;
use smallvec::SmallVec;
use std::cmp;
use std::ops::Range;
use std::str::FromStr;

/// Represents a `Range:` header which has been parsed and resolved to a particular entity length.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ResolvedRanges {
    /// No `Range:` header was supplied.
    None,

    /// A `Range:` header was supplied, but none of the ranges were possible to satisfy with the
    /// given entity length.
    NotSatisfiable,

    /// A `Range:` header was supplied with at least one satisfiable range, included here.
    /// Non-satisfiable ranges have been dropped. Ranges are converted from the HTTP closed
    /// interval style to the the std::ops::Range half-open interval style (start inclusive, end
    /// exclusive).
    Satisfiable(SmallVec<[Range<u64>; 1]>),
}

/// Parses the byte-range-set in the range header as described in [RFC 7233 section
/// 2.1](https://tools.ietf.org/html/rfc7233#section-2.1).
pub(crate) fn parse(range: Option<&HeaderValue>, len: u64) -> ResolvedRanges {
    let range = match range {
        None => return ResolvedRanges::None,
        Some(r) => r.to_str().unwrap(),
    };

    // byte-ranges-specifier = bytes-unit "=" byte-range-set
    if !range.starts_with("bytes=") {
        return ResolvedRanges::None;
    }

    // byte-range-set  = 1#( byte-range-spec / suffix-byte-range-spec )
    let mut ranges: SmallVec<[Range<u64>; 1]> = SmallVec::new();
    for r in range[6..].split(',') {
        // Trim OWS = *( SP / HTAB )
        let r = r.trim_start_matches(|c| c == ' ' || c == '\t');

        // Parse one of the following.
        // byte-range-spec = first-byte-pos "-" [ last-byte-pos ]
        // suffix-byte-range-spec = "-" suffix-length
        let hyphen = match r.find('-') {
            None => return ResolvedRanges::None, // unparseable.
            Some(h) => h,
        };
        if hyphen == 0 {
            // It's a suffix-byte-range-spec.
            let last = match u64::from_str(&r[1..]) {
                Err(_) => return ResolvedRanges::None, // unparseable
                Ok(l) => l,
            };
            if last >= len {
                continue; // this range is not satisfiable; skip.
            }
            ranges.push((len - last)..len);
        } else {
            let first = match u64::from_str(&r[0..hyphen]) {
                Err(_) => return ResolvedRanges::None, // unparseable
                Ok(f) => f,
            };
            let end = if r.len() > hyphen + 1 {
                cmp::min(
                    match u64::from_str(&r[hyphen + 1..]) {
                        Err(_) => return ResolvedRanges::None, // unparseable
                        Ok(l) => l,
                    } + 1,
                    len,
                )
            } else {
                len // no end specified; use EOF.
            };
            if first >= end {
                continue; // this range is not satisfiable; skip.
            }
            ranges.push(first..end);
        }
    }
    if !ranges.is_empty() {
        return ResolvedRanges::Satisfiable(ranges);
    }
    return ResolvedRanges::NotSatisfiable;
}

#[cfg(test)]
mod tests {
    use super::{parse, ResolvedRanges};
    use http::header::HeaderValue;
    use smallvec::SmallVec;

    /// Tests the specific examples enumerated in [RFC 2616 section
    /// 14.35.1](https://tools.ietf.org/html/rfc2616#section-14.35.1).
    #[test]
    fn test_resolve_ranges_rfc() {
        let mut v = SmallVec::new();

        v.push(0..500);
        assert_eq!(
            ResolvedRanges::Satisfiable(v.clone()),
            parse(Some(&HeaderValue::from_static("bytes=0-499")), 10000)
        );

        v.clear();
        v.push(500..1000);
        assert_eq!(
            ResolvedRanges::Satisfiable(v.clone()),
            parse(Some(&HeaderValue::from_static("bytes=500-999")), 10000)
        );

        v.clear();
        v.push(9500..10000);
        assert_eq!(
            ResolvedRanges::Satisfiable(v.clone()),
            parse(Some(&HeaderValue::from_static("bytes=-500")), 10000)
        );

        v.clear();
        v.push(9500..10000);
        assert_eq!(
            ResolvedRanges::Satisfiable(v.clone()),
            parse(Some(&HeaderValue::from_static("bytes=9500-")), 10000)
        );

        v.clear();
        v.push(0..1);
        v.push(9999..10000);
        assert_eq!(
            ResolvedRanges::Satisfiable(v.clone()),
            parse(Some(&HeaderValue::from_static("bytes=0-0,-1")), 10000)
        );

        // Non-canonical ranges. Possibly the point of these is that the adjacent and overlapping
        // ranges are supposed to be coalesced into one? I'm not going to do that for now.

        v.clear();
        v.push(500..601);
        v.push(601..1000);
        assert_eq!(
            ResolvedRanges::Satisfiable(v.clone()),
            parse(
                Some(&HeaderValue::from_static("bytes=500-600, 601-999")),
                10000
            )
        );

        v.clear();
        v.push(500..701);
        v.push(601..1000);
        assert_eq!(
            ResolvedRanges::Satisfiable(v.clone()),
            parse(
                Some(&HeaderValue::from_static("bytes=500-700, 601-999")),
                10000
            )
        );
    }

    #[test]
    fn test_resolve_ranges_satisfiability() {
        assert_eq!(
            ResolvedRanges::NotSatisfiable,
            parse(Some(&HeaderValue::from_static("bytes=10000-")), 10000)
        );

        let mut v = SmallVec::new();
        v.push(0..500);
        assert_eq!(
            ResolvedRanges::Satisfiable(v.clone()),
            parse(Some(&HeaderValue::from_static("bytes=0-499,10000-")), 10000)
        );

        assert_eq!(
            ResolvedRanges::NotSatisfiable,
            parse(Some(&HeaderValue::from_static("bytes=-1")), 0)
        );
        assert_eq!(
            ResolvedRanges::NotSatisfiable,
            parse(Some(&HeaderValue::from_static("bytes=0-0")), 0)
        );
        assert_eq!(
            ResolvedRanges::NotSatisfiable,
            parse(Some(&HeaderValue::from_static("bytes=0-")), 0)
        );

        v.clear();
        v.push(0..1);
        assert_eq!(
            ResolvedRanges::Satisfiable(v.clone()),
            parse(Some(&HeaderValue::from_static("bytes=0-0")), 1)
        );

        v.clear();
        v.push(0..500);
        assert_eq!(
            ResolvedRanges::Satisfiable(v.clone()),
            parse(Some(&HeaderValue::from_static("bytes=0-10000")), 500)
        );
    }

    #[test]
    fn test_resolve_ranges_absent_or_invalid() {
        assert_eq!(ResolvedRanges::None, parse(None, 10000));
    }
}
