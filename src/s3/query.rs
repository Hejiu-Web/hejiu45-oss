//! Query-string parsing for S3 operations.
//!
//! S3 uses valueless parameters as sub-resource selectors (`?acl`, `?uploads`,
//! `?delete`) and plain parameters as arguments (`?prefix=`, `?list-type=2`).
//! [`S3Query`] keeps both facts: which sub-resource was selected, and what the
//! argument values are.

/// Parsed S3 query parameters.
#[derive(Debug, Default, Clone)]
pub struct S3Query {
    /// Decoded key/value pairs in arrival order.
    pub pairs: Vec<(String, String)>,
}

impl S3Query {
    /// Parse a raw query string (percent-decoded, `+` kept literal).
    pub fn parse(raw: &str) -> Self {
        let mut pairs = crate::auth::sigv4::parse_query_pairs(raw);
        if pairs.is_empty() && !raw.is_empty() {
            // `parse_query_pairs` already handles empty strings; keep the field
            // for clarity of intent.
            pairs.clear();
        }
        // SigV4 parameters are not sub-resources; drop them from the view.
        pairs.retain(|(k, _)| !k.starts_with("X-Amz-"));
        Self { pairs }
    }

    pub fn value(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(key))
            .map(|(_, v)| v.as_str())
    }

    /// Value with an empty-string fallback (so `?prefix` equals `?prefix=`).
    pub fn value_or_empty(&self, key: &str) -> Option<&str> {
        self.value(key)
    }

    /// The sub-resource selector: the first parameter without a value whose
    /// name is not a known argument (`prefix`, `max-keys`, ...).
    pub fn subresource(&self) -> Option<&str> {
        const ARGUMENTS: &[&str] = &[
            "prefix",
            "delimiter",
            "marker",
            "max-keys",
            "continuation-token",
            "start-after",
            "encoding-type",
            "part-number",
            "list-type",
            "fetch-owner",
            "location",
            "response-content-type",
            "response-content-disposition",
            "response-cache-control",
            "response-content-encoding",
            "response-content-language",
            "response-expires",
            "upload-id",
            "partNumber",
            "versionId",
            "expires",
            "policy",
            "signature",
            "AWSAccessKeyId",
        ];
        self.pairs
            .iter()
            .find(|(k, v)| {
                v.is_empty() && !k.starts_with("X-Amz-") && !ARGUMENTS.contains(&k.as_str())
            })
            .map(|(k, _)| k.as_str())
    }

    pub fn has(&self, key: &str) -> bool {
        self.pairs.iter().any(|(k, _)| k.eq_ignore_ascii_case(key))
    }

    /// `list-type=2` selects ListObjectsV2.
    pub fn is_list_v2(&self) -> bool {
        self.value("list-type") == Some("2")
    }

    pub fn u32_value(&self, key: &str) -> Result<Option<u32>, String> {
        match self.value(key) {
            None => Ok(None),
            Some(v) if v.is_empty() => Ok(None),
            Some(v) => v
                .parse::<u32>()
                .map(Some)
                .map_err(|_| format!("{key} must be a non-negative integer")),
        }
    }
}

/// Parse `Range: bytes=start-end` (single range only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    pub fn len(&self) -> u64 {
        self.end - self.start + 1
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// Parse a single-range `Range` header against a known object size.
    ///
    /// Returns `Ok(None)` when the header is absent, `Err(())` when it is
    /// syntactically invalid, and a range that may be unsatisfiable (start past
    /// EOF) so the caller can emit `416` with the right `Content-Range`.
    pub fn parse(header: &str, size: u64) -> Result<Option<Result<ByteRange, ()>>, ()> {
        let Some(spec) = header.trim().strip_prefix("bytes=") else {
            return Ok(None);
        };
        // Multi-range requests are legal HTTP but S3 clients do not use them;
        // serving a single range is the compatible subset.
        let spec = spec.split(',').next().unwrap_or("").trim();
        if spec.is_empty() {
            return Err(());
        }
        let (start_s, end_s) = spec.split_once('-').ok_or(())?;
        if size == 0 {
            // Any range on an empty object is unsatisfiable.
            return Ok(Some(Err(())));
        }
        let (start, end) = match (start_s.trim(), end_s.trim()) {
            ("", "") => return Err(()),
            ("", suffix) => {
                // Suffix range: last N bytes.
                let n: u64 = suffix.parse().map_err(|_| ())?;
                if n == 0 {
                    return Ok(Some(Err(())));
                }
                let n = n.min(size);
                (size - n, size - 1)
            }
            (start, "") => {
                let s: u64 = start.parse().map_err(|_| ())?;
                (s, size - 1)
            }
            (start, end) => {
                let s: u64 = start.parse().map_err(|_| ())?;
                let e: u64 = end.parse().map_err(|_| ())?;
                if s > e {
                    return Err(());
                }
                (s, e.min(size - 1))
            }
        };
        if start >= size {
            return Ok(Some(Err(())));
        }
        Ok(Some(Ok(ByteRange { start, end })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_subresources_vs_arguments() {
        let q = S3Query::parse("acl");
        assert_eq!(q.subresource(), Some("acl"));

        let q = S3Query::parse("list-type=2&prefix=dir%2F&delimiter=%2F&max-keys=100");
        assert_eq!(q.subresource(), None);
        assert!(q.is_list_v2());
        assert_eq!(q.value("prefix"), Some("dir/"));
        assert_eq!(q.value("delimiter"), Some("/"));
        assert_eq!(q.u32_value("max-keys").unwrap(), Some(100));

        let q = S3Query::parse("uploads&prefix=x");
        assert_eq!(q.subresource(), Some("uploads"));

        let q = S3Query::parse("delete");
        assert_eq!(q.subresource(), Some("delete"));

        // SigV4 parameters disappear from the operation view.
        let q = S3Query::parse("X-Amz-Signature=abc&uploadId=u1&partNumber=3");
        assert_eq!(q.subresource(), None);
        assert_eq!(q.value("uploadId"), Some("u1"));
        assert_eq!(q.u32_value("partNumber").unwrap(), Some(3));
    }

    #[test]
    fn range_parsing() {
        assert_eq!(ByteRange::parse("bytes=0-9", 100), Ok(Some(Ok(ByteRange { start: 0, end: 9 }))));
        assert_eq!(ByteRange::parse("bytes=90-", 100), Ok(Some(Ok(ByteRange { start: 90, end: 99 }))));
        assert_eq!(ByteRange::parse("bytes=-10", 100), Ok(Some(Ok(ByteRange { start: 90, end: 99 }))));
        // End beyond EOF is clamped.
        assert_eq!(ByteRange::parse("bytes=95-200", 100), Ok(Some(Ok(ByteRange { start: 95, end: 99 }))));
        // Suffix larger than the object returns the whole object.
        assert_eq!(ByteRange::parse("bytes=-500", 100), Ok(Some(Ok(ByteRange { start: 0, end: 99 }))));
        // Unsatisfiable.
        assert_eq!(ByteRange::parse("bytes=100-", 100), Ok(Some(Err(()))));
        assert_eq!(ByteRange::parse("bytes=0-0", 0), Ok(Some(Err(()))));
        // Malformed.
        assert_eq!(ByteRange::parse("bytes=abc", 100), Err(()));
        assert_eq!(ByteRange::parse("bytes=5-1", 100), Err(()));
        // Absent / non-byte units.
        assert_eq!(ByteRange::parse("items=0-5", 100), Ok(None));
    }

    #[test]
    fn range_len() {
        assert_eq!(ByteRange { start: 0, end: 9 }.len(), 10);
        assert_eq!(ByteRange { start: 5, end: 5 }.len(), 1);
    }
}
