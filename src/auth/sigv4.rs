//! AWS Signature Version 4 *verification* (server side).
//!
//! Why this is hand-written while request *signing* uses `rusty-s3`:
//! `rusty-s3` computes signatures for outgoing requests. Verification needs the
//! opposite flow — reconstruct the canonical request from what the client
//! actually sent, re-derive the signature from the stored secret, and compare
//! in constant time. Doing it explicitly also lets us enforce the policies a
//! generic signer does not ("header auth OR presigned query, never both",
//! bounded clock skew, `UNSIGNED-PAYLOAD`, body-hash detection).
//!
//! Algorithm (AWS docs, "Signature Version 4 for the Authorization header"):
//!
//! ```text
//! CanonicalRequest = METHOD \n URI \n QUERY \n CANONICAL_HEADERS \n SIGNED_HEADERS \n PAYLOAD_HASH
//! StringToSign     = "AWS4-HMAC-SHA256" \n ISO8601(x-amz-date) \n SCOPE \n SHA256(CanonicalRequest)
//! SigningKey       = HMAC( HMAC( HMAC( HMAC("AWS4"+secret, date), region), "s3"), "aws4_request")
//! Signature        = HMAC(SigningKey, StringToSign)
//! ```
//!
//! The canonical URI is the path **exactly as sent on the wire**, URI-encoded
//! once (S3 does not double-encode). Using the axum-decoded path here would
//! break signatures for any key containing reserved characters, so the raw
//! path travels with the request in [`crate::state::RawPath`].

use std::collections::BTreeMap;

use aws_lc_rs::hmac;
use axum::http::{HeaderMap, Method};
use sha2::{Digest, Sha256};

use crate::error::S3Error;

/// One-time check that the crypto primitives produce the AWS-published
/// signature. A failure here means every signed request would 403, so the
/// server refuses to start instead of failing closed mysteriously.
pub fn crypto_self_check() -> bool {
    use std::sync::OnceLock;
    static OK: OnceLock<bool> = OnceLock::new();
    *OK.get_or_init(|| {
        let sts = "AWS4-HMAC-SHA256\n20130524T000000Z\n20130524/us-east-1/s3/aws4_request\n7344ae5b7ee6c3e7e6b0fe0640412a37625d1fbfff95c48bbb2dc43964946972";
        let key = signing_key(
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "20130524",
            "us-east-1",
            "s3",
        );
        hex::encode(hmac_sha256(&key, sts.as_bytes()))
            == "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
    })
}

/// Maximum accepted clock skew for `x-amz-date` (AWS uses 15 minutes).
pub const MAX_CLOCK_SKEW_SECS: i64 = 900;
/// Presigned URLs may live at most 7 days (AWS limit).
pub const MAX_PRESIGN_SECS: u64 = 7 * 24 * 3600;
/// How the request proved its identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStyle {
    /// `Authorization: AWS4-HMAC-SHA256 ...` header.
    Header,
    /// `X-Amz-Algorithm=...` query parameters.
    Presigned,
}

/// A parsed SigV4 credential, before cryptographic verification.
#[derive(Debug, Clone)]
pub struct SigV4Request {
    pub style: AuthStyle,
    pub access_key: String,
    /// `20261020` — date part of the credential scope.
    pub date: String,
    pub region: String,
    pub service: String,
    pub terminal: String,
    /// `;`-joined lowercase signed header names, exactly as sent.
    pub signed_headers_raw: String,
    pub signature: String,
    /// Present for header auth, or for presigned URLs with `X-Amz-Date`.
    pub amz_date: String,
    /// Present for presigned URLs.
    pub expires_secs: Option<u64>,
    /// Value of `X-Amz-Content-Sha256` if the client provided one.
    pub content_sha256: Option<String>,
}

/// Outcome of verifying a request's signature.
#[derive(Debug, Clone)]
pub struct Verified {
    pub access_key: String,
    pub style: AuthStyle,
    /// Whether the payload hash had to be computed by us (header present and
    /// not `UNSIGNED-PAYLOAD` for header auth).
    pub payload_hash_checked: bool,
}

/// Extract the declared payload hash the request references.
pub fn declared_payload_hash(req: &SigV4Request) -> &str {
    req.content_sha256.as_deref().unwrap_or("UNSIGNED-PAYLOAD")
}

/// Parse `Authorization: AWS4-HMAC-SHA256 Credential=.../date/region/s3/aws4_request, SignedHeaders=a;b, Signature=hex`
///
/// `headers` supplies `x-amz-date` and `x-amz-content-sha256`, which are part
/// of the string-to-sign and the canonical request respectively. They are
/// read here rather than in [`parse`] so that a parsed [`SigV4Request`] is
/// always complete enough to compute a signature from.
fn parse_authorization(value: &str, headers: &HeaderMap) -> Result<SigV4Request, S3Error> {
    let rest = value
        .strip_prefix("AWS4-HMAC-SHA256 ")
        .ok_or_else(|| S3Error::invalid_request("Unsupported authorization algorithm; expected AWS4-HMAC-SHA256."))?;
    let mut credential = None;
    let mut signed_headers = None;
    let mut signature = None;
    for part in rest.split(',') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("Credential=") {
            credential = Some(v.to_string());
        } else if let Some(v) = part.strip_prefix("SignedHeaders=") {
            signed_headers = Some(v.to_string());
        } else if let Some(v) = part.strip_prefix("Signature=") {
            signature = Some(v.to_string());
        }
    }
    let credential_str = credential.ok_or_else(|| S3Error::invalid_request("Missing Credential in Authorization."))?;
    let mut scope = credential_str.split('/');
    let access_key = scope.next().unwrap_or_default().to_string();
    let date = scope.next().unwrap_or_default().to_string();
    let region = scope.next().unwrap_or_default().to_string();
    let service = scope.next().unwrap_or_default().to_string();
    let terminal = scope.next().unwrap_or_default().to_string();
    if access_key.is_empty() || date.len() != 8 || terminal != "aws4_request" || service != "s3" {
        return Err(S3Error::invalid_request(
            "Malformed Credential scope; expected <key>/<yyyymmdd>/<region>/s3/aws4_request.",
        ));
    }
    let signed_headers_raw =
        signed_headers.ok_or_else(|| S3Error::invalid_request("Missing SignedHeaders."))?;
    if signed_headers_raw != signed_headers_raw.to_ascii_lowercase() {
        return Err(S3Error::invalid_request("SignedHeaders must be lowercase."));
    }
    let signature = signature.ok_or_else(|| S3Error::invalid_request("Missing Signature."))?;
    if signature.len() != 64 || !signature.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(S3Error::invalid_request("Signature must be 64 hex characters."));
    }
    Ok(SigV4Request {
        style: AuthStyle::Header,
        access_key,
        date,
        region,
        service,
        terminal,
        signed_headers_raw,
        signature: signature.to_ascii_lowercase(),
        amz_date: headers
            .get("x-amz-date")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .unwrap_or_default(),
        expires_secs: None,
        content_sha256: headers
            .get("x-amz-content-sha256")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
    })
}

/// Query parameters relevant to SigV4, taken from the raw query string.
#[derive(Debug, Default, Clone)]
pub struct AuthQuery {
    pub algorithm: Option<String>,
    pub credential: Option<String>,
    pub date: Option<String>,
    pub expires: Option<String>,
    pub signed_headers: Option<String>,
    pub signature: Option<String>,
    pub security_token: Option<String>,
}

impl AuthQuery {
    pub fn parse(pairs: &[(String, String)]) -> Self {
        let mut q = Self::default();
        for (k, v) in pairs {
            match k.as_str() {
                "X-Amz-Algorithm" => q.algorithm = Some(v.clone()),
                "X-Amz-Credential" => q.credential = Some(v.clone()),
                "X-Amz-Date" => q.date = Some(v.clone()),
                "X-Amz-Expires" => q.expires = Some(v.clone()),
                "X-Amz-SignedHeaders" => q.signed_headers = Some(v.clone()),
                "X-Amz-Signature" => q.signature = Some(v.clone()),
                "X-Amz-Security-Token" => q.security_token = Some(v.clone()),
                _ => {}
            }
        }
        q
    }

    pub fn is_presigned(&self) -> bool {
        self.algorithm.is_some() || self.credential.is_some() || self.signature.is_some()
    }
}

/// Parse credentials for a presigned request.
fn parse_presigned(q: &AuthQuery, headers: &HeaderMap) -> Result<SigV4Request, S3Error> {
    let alg = q
        .algorithm
        .as_deref()
        .ok_or_else(|| S3Error::invalid_request("Missing X-Amz-Algorithm."))?;
    if alg != "AWS4-HMAC-SHA256" {
        return Err(S3Error::invalid_request("Unsupported X-Amz-Algorithm."));
    }
    let credential = q
        .credential
        .as_deref()
        .ok_or_else(|| S3Error::invalid_request("Missing X-Amz-Credential."))?;
    let mut scope = credential.split('/');
    let access_key = scope.next().unwrap_or_default().to_string();
    let date = scope.next().unwrap_or_default().to_string();
    let region = scope.next().unwrap_or_default().to_string();
    let service = scope.next().unwrap_or_default().to_string();
    let terminal = scope.next().unwrap_or_default().to_string();
    if access_key.is_empty() || date.len() != 8 || terminal != "aws4_request" || service != "s3" {
        return Err(S3Error::invalid_request("Malformed X-Amz-Credential scope."));
    }
    let expires: u64 = q
        .expires
        .as_deref()
        .ok_or_else(|| S3Error::invalid_request("Missing X-Amz-Expires."))?
        .parse()
        .map_err(|_| S3Error::invalid_request("X-Amz-Expires must be an integer."))?;
    if expires == 0 || expires > MAX_PRESIGN_SECS {
        return Err(S3Error::invalid_request(
            "X-Amz-Expires must be between 1 and 604800 seconds.",
        ));
    }
    let signature = q
        .signature
        .as_deref()
        .ok_or_else(|| S3Error::invalid_request("Missing X-Amz-Signature."))?
        .to_ascii_lowercase();
    if signature.len() != 64 || !signature.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(S3Error::invalid_request("X-Amz-Signature must be 64 hex characters."));
    }
    Ok(SigV4Request {
        style: AuthStyle::Presigned,
        access_key,
        date,
        region,
        service,
        terminal,
        signed_headers_raw: q.signed_headers.clone().unwrap_or_else(|| "host".to_string()),
        signature,
        amz_date: q.date.clone().unwrap_or_default(),
        expires_secs: Some(expires),
        content_sha256: headers
            .get("x-amz-content-sha256")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string),
    })
}

/// Determine the authentication style and parse credentials.
///
/// A request carrying both an `Authorization` header and presigned query
/// parameters is rejected: two conflicting proofs of identity are a classic
/// request-smuggling/confusion vector.
pub fn parse(
    headers: &HeaderMap,
    query: &AuthQuery,
) -> Result<Option<SigV4Request>, S3Error> {
    let auth = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    match (auth, query.is_presigned()) {
        (Some(_), true) => Err(S3Error::invalid_request(
            "Request must use either the Authorization header or presigned query parameters, not both.",
        )),
        (Some(a), false) => Ok(Some(parse_authorization(a, headers)?)),
        (None, true) => Ok(Some(parse_presigned(query, headers)?)),
        (None, false) => Ok(None),
    }
}

/// RFC 3986 encoding for a query key or value (space → `%20`, `/` → `%2F`).
fn uri_encode(s: &str, encode_slash: bool) -> String {
    use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
    const QSAFE: &AsciiSet = &CONTROLS
        .add(b' ').add(b'"').add(b'#').add(b'%').add(b'&').add(b'+').add(b',')
        .add(b':').add(b';').add(b'<').add(b'=').add(b'>').add(b'?').add(b'@')
        .add(b'[').add(b'\\').add(b']').add(b'^').add(b'`').add(b'{').add(b'|').add(b'}');
    const QSAFE_NO_SLASH: &AsciiSet = &QSAFE.add(b'/');
    let set = if encode_slash { QSAFE_NO_SLASH } else { QSAFE };
    utf8_percent_encode(s, set).to_string()
}

/// Canonical query string: key/value pairs, RFC3986-encoded, sorted by key then
/// value, `X-Amz-Signature` excluded (it is what we are verifying).
pub fn canonical_query(raw_pairs: &[(String, String)], style: AuthStyle) -> String {
    let mut pairs: Vec<(String, String)> = raw_pairs
        .iter()
        .filter(|(k, _)| !(style == AuthStyle::Presigned && k == "X-Amz-Signature"))
        .map(|(k, v)| (uri_encode(k, true), uri_encode(v, true)))
        .collect();
    pairs.sort();
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(k);
        out.push('=');
        out.push_str(v);
    }
    out
}

/// Split a raw query string into *decoded* pairs without form-encoding
/// semantics (`+` stays `+`, which SigV4 requires).
pub fn parse_query_pairs(raw: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if raw.is_empty() {
        return out;
    }
    for part in raw.split('&') {
        if part.is_empty() {
            continue;
        }
        let (k, v) = match part.split_once('=') {
            Some((k, v)) => (k, v),
            None => (part, ""),
        };
        out.push((percent_decode(k), percent_decode(v)));
    }
    out
}

/// Percent-decode `%XX` sequences (UTF-8), leaving `+` untouched.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hi = (b[i + 1] as char).to_digit(16);
            let lo = (b[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Normalised header value per SigV4: trim and collapse sequential spaces.
fn normalize_header_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut prev_space = false;
    for c in v.trim().chars() {
        if c == ' ' || c == '\t' {
            if !prev_space {
                out.push(' ');
            }
            prev_space = true;
        } else {
            out.push(c);
            prev_space = false;
        }
    }
    out
}

/// Build the canonical request. `raw_path` must be the on-the-wire path
/// including its leading `/` and its original percent-encoding.
pub fn canonical_request(
    method: &Method,
    raw_path: &str,
    raw_query: &str,
    headers: &HeaderMap,
    signed_headers_raw: &str,
    payload_hash: &str,
    style: AuthStyle,
) -> Result<String, S3Error> {
    let pairs = parse_query_pairs(raw_query);
    let cq = canonical_query(&pairs, style);

    let mut ch = String::new();
    for name in signed_headers_raw.split(';') {
        // Some clients send "host;x-amz-date" where the separator carries a
        // stale escape, yielding entries like "host;x-amz-date". Tolerate a
        // leading ';' and any surrounding whitespace rather than rejecting a
        // request the client itself considers well-formed.
        let name = name.trim().trim_start_matches(';').trim();
        if name.is_empty() {
            continue;
        }
        if name.chars().any(|c| c.is_uppercase()) {
            return Err(S3Error::invalid_request("SignedHeaders must be lowercase, without spaces."));
        }
        let values = if name == "host" {
            // `host` is mandatory and always present on HTTP/1.1.
            headers
                .get(name)
                .map(|v| vec![v.to_str().unwrap_or_default()])
                .unwrap_or_default()
        } else {
            headers
                .get_all(name)
                .iter()
                .map(|v| v.to_str().unwrap_or_default())
                .collect::<Vec<_>>()
        };
        if values.is_empty() {
            return Err(S3Error::access_denied(format!(
                "Signed header `{name}` is missing from the request."
            )));
        }
        let joined = values
            .iter()
            .map(|v| normalize_header_value(v))
            .collect::<Vec<_>>()
            .join(",");
        ch.push_str(name);
        ch.push(':');
        ch.push_str(&joined);
        ch.push('\n');
    }

    Ok(format!(
        "{}\n{}\n{}\n{}\n{}\n{}",
        method.as_str(),
        raw_path,
        cq,
        ch,
        signed_headers_raw,
        payload_hash
    ))
}

fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// HMAC-SHA256 helper.
pub fn hmac_sha256(key: &[u8], data: &[u8]) -> [u8; 32] {
    let k = hmac::Key::new(hmac::HMAC_SHA256, key);
    let tag = hmac::sign(&k, data);
    let mut out = [0u8; 32];
    out.copy_from_slice(tag.as_ref());
    out
}

/// Derive the SigV4 signing key.
pub fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> [u8; 32] {
    let k_date = hmac_sha256(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}

/// Compute the expected hex signature for a request, trying each
/// [`path_candidates`] encoding of `raw_path`.
///
/// Returns the first candidate that matches the signature the client sent;
/// when none match it returns the canonical (first) candidate's value, which
/// is what [`signatures_equal`] will then reject.
pub fn compute_signature(
    req: &SigV4Request,
    secret: &str,
    method: &Method,
    raw_path: &str,
    raw_query: &str,
    headers: &HeaderMap,
    payload_hash: &str,
) -> Result<String, S3Error> {
    let key = signing_key(secret, &req.date, &req.region, &req.service);
    let scope = format!("{}/{}/{}/{}", req.date, req.region, req.service, req.terminal);
    let mut first: Option<String> = None;
    for candidate in path_candidates(raw_path) {
        let creq = canonical_request(
            method,
            &candidate,
            raw_query,
            headers,
            &req.signed_headers_raw,
            payload_hash,
            req.style,
        )?;
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            req.amz_date,
            scope,
            sha256_hex(creq.as_bytes())
        );
        let sig = hex::encode(hmac_sha256(&key, string_to_sign.as_bytes()));
        if req.style == AuthStyle::Presigned && signatures_equal(&sig, &req.signature) {
            return Ok(sig);
        }
        first.get_or_insert(sig);
    }
    Ok(first.unwrap_or_default())
}

/// Constant-time comparison of two hex signatures.
pub fn signatures_equal(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.ct_eq(b).into()
}

/// Parse `x-amz-date` (`YYYYMMDDTHHMMSSZ`) into unix seconds.
pub fn parse_amz_date(s: &str) -> Option<i64> {
    let fmt = time::macros::format_description!("[year][month][day]T[hour][minute][second]Z");
    let dt = time::PrimitiveDateTime::parse(s, fmt).ok()?;
    Some(dt.assume_utc().unix_timestamp())
}

/// Check the timestamp policy for the request style.
///
/// Header auth: `x-amz-date` must be within [`MAX_CLOCK_SKEW_SECS`].
/// Presigned: must not be in the future beyond skew and not older than
/// `X-Amz-Expires`.
pub fn check_timestamp(req: &SigV4Request, raw_query: &str, now: i64) -> Result<(), S3Error> {
    let date_str = if req.amz_date.is_empty() {
        // Presigned URLs without X-Amz-Date: must be present per spec.
        return Err(S3Error::invalid_request("Missing x-amz-date / X-Amz-Date."));
    } else {
        &req.amz_date
    };
    let ts = parse_amz_date(date_str)
        .ok_or_else(|| S3Error::invalid_request("x-amz-date must be ISO8601 basic format."))?;
    if ts - now > MAX_CLOCK_SKEW_SECS {
        return Err(S3Error::access_denied(
            "Request time is too far in the future; check your clock.",
        ));
    }
    match req.style {
        AuthStyle::Header => {
            if now - ts > MAX_CLOCK_SKEW_SECS {
                return Err(S3Error::access_denied(
                    "Request has expired: x-amz-date is older than 15 minutes.",
                ));
            }
        }
        AuthStyle::Presigned => {
            let expires = req
                .expires_secs
                .or_else(|| {
                    parse_query_pairs(raw_query)
                        .iter()
                        .find(|(k, _)| k == "X-Amz-Expires")
                        .and_then(|(_, v)| v.parse().ok())
                })
                .unwrap_or(0);
            if now - ts > expires as i64 {
                return Err(S3Error::expired_presigned());
            }
        }
    }
    Ok(())
}

/// Extract the raw (still percent-encoded) path from a request target or an
/// absolute URI, exactly as it appeared on the wire.
///
/// SigV4 signs the encoded path, so this must **not** be percent-decoded.
pub fn raw_path_from_target(target: &str) -> &str {
    let after_authority = match target.find("://") {
        // Skip "scheme://authority"; the path starts at the next '/'.
        Some(i) => i + 3,
        None => 0,
    };
    let rest = if after_authority > 0 {
        match target[after_authority..].find('/') {
            Some(i) => &target[after_authority + i..],
            None => "/",
        }
    } else {
        target
    };
    let path = match rest.find('?') {
        Some(i) => &rest[..i],
        None => rest,
    };
    if path.is_empty() { "/" } else { path }
}

/// RFC 3986 path encoding: everything except unreserved characters and `/` is
/// encoded, including `$`.
pub fn uri_encode_path(s: &str) -> String {
    use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
    const PATH: &AsciiSet = &CONTROLS
        .add(b' ').add(b'"').add(b'#').add(b'%').add(b'&').add(b'+').add(b',')
        .add(b':').add(b';').add(b'<').add(b'=').add(b'>').add(b'?').add(b'@')
        .add(b'[').add(b'\\').add(b']').add(b'^').add(b'`').add(b'{').add(b'|')
        .add(b'}').add(b'$');
    utf8_percent_encode(s, PATH).to_string()
}

/// Encoded-path variants to try when verifying.
///
/// Clients disagree on which characters must be encoded in the path (`$`,
/// `:`, `@`, `'`, `!`, `=`, `+`, `,`, `;`). A request is accepted if **any**
/// of these canonicalisations matches the client's signature. Every candidate
/// derives from the same raw bytes, so this widens client compatibility
/// without widening what an attacker can get signed.
pub fn path_candidates(raw_path: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(3);
    let strict = uri_encode_path(&percent_decode(raw_path));
    // Strict first: that is what we sign and what the AWS reference uses.
    // Then the raw bytes as sent (a canonicalising proxy may have changed
    // percent-hex casing), then the `$`-literal SDK variant.
    out.push(strict.clone());
    if !out.contains(&raw_path.to_string()) {
        out.push(raw_path.to_string());
    }
    let dollar_literal = strict.replace("%24", "$");
    if !out.contains(&dollar_literal) {
        out.push(dollar_literal);
    }
    out
}

/// The only payload hashes accepted without recomputation for header auth.
pub fn payload_hash_is_literal(h: &str) -> bool {
    h == "UNSIGNED-PAYLOAD" || h == "STREAMING-AWS4-HMAC-SHA256-PAYLOAD"
}

/// Sorted, lowercase map of request headers (useful for debugging).
pub fn header_map_debug(h: &HeaderMap) -> BTreeMap<String, String> {
    h.iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("<binary>").to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn hm(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }
    // ---------------------------------------------------------------------
    // ---------------------------------------------------------------------
    // Vectors from the AWS S3 documentation (sig-v4-header-based-auth),
    // cross-checked against the independent `s3s` implementation
    // (crates/s3s-sigv4/src/methods.rs). Secret key:
    //   wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY
    // ---------------------------------------------------------------------

    const EXAMPLE_SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn authorize(access: &str, signed: &str, sig: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential={access}/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders={signed}, Signature={sig}"
        )
    }

    fn verify_vector(
        method: Method,
        raw_path: &str,
        raw_query: &str,
        headers: &HeaderMap,
        signed: &str,
        payload_hash: &str,
        expected: &str,
    ) {
        let auth = authorize("AKIAIOSFODNN7EXAMPLE", signed, expected);
        let req = parse_authorization(&auth, headers).expect("authorization parses");
        let sig = compute_signature(
            &req, EXAMPLE_SECRET, &method, raw_path, raw_query, headers, payload_hash,
        )
        .expect("signature computes");
        assert_eq!(sig, expected, "path={raw_path} query={raw_query}");
        assert!(signatures_equal(&sig, &req.signature));
    }


    /// "Example: GET Object"
    #[test]
    fn pr_self_check() {
        assert!(crypto_self_check(), "crypto primitives are broken");
    }

    /// "Example: GET Object"
    #[test]
    #[allow(duplicate_macro_attributes)]
    fn aws_s3_get_object_authorization_vector() {
        let headers = hm(&[
            ("host", "examplebucket.s3.amazonaws.com"),
            ("range", "bytes=0-9"),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", "20130524T000000Z"),
        ]);
        verify_vector(
            Method::GET,
            "/test.txt",
            "",
            &headers,
            "host;range;x-amz-content-sha256;x-amz-date",
            EMPTY_SHA256,
            "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41",
        );
    }

    /// "Example: PUT Object" — note the literal `$` in the key.
    #[test]
    #[test]
    fn aws_s3_put_object_vector() {
        let headers = hm(&[
            ("date", "Fri, 24 May 2013 00:00:00 GMT"),
            ("host", "examplebucket.s3.amazonaws.com"),
            (
                "x-amz-content-sha256",
                "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072",
            ),
            ("x-amz-date", "20130524T000000Z"),
            ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
        ]);
        verify_vector(
            Method::PUT,
            "/test%24file.text",
            "",
            &headers,
            "date;host;x-amz-content-sha256;x-amz-date;x-amz-storage-class",
            "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072",
            "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd",
        );
    }

    /// "Example: GET Bucket Lifecycle" — valueless query parameter.
    #[test]
    fn aws_s3_get_bucket_lifecycle_vector() {
        let headers = hm(&[
            ("host", "examplebucket.s3.amazonaws.com"),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", "20130524T000000Z"),
        ]);
        verify_vector(
            Method::GET,
            "/",
            "lifecycle=",
            &headers,
            "host;x-amz-content-sha256;x-amz-date",
            EMPTY_SHA256,
            "fea454ca298b7da1c68078a5d1bdbfbbe0d65c699e0f91ac7a200a0136783543",
        );
    }

    /// "Example: List Objects" — query parameters sorted by key.
    #[test]
    fn aws_s3_list_objects_vector() {
        let headers = hm(&[
            ("host", "examplebucket.s3.amazonaws.com"),
            ("x-amz-content-sha256", EMPTY_SHA256),
            ("x-amz-date", "20130524T000000Z"),
        ]);
        verify_vector(
            Method::GET,
            "/",
            "max-keys=2&prefix=J",
            &headers,
            "host;x-amz-content-sha256;x-amz-date",
            EMPTY_SHA256,
            "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7",
        );
    }

    /// "Example: Generate a presigned URL to share an object".
    #[test]
    fn aws_s3_presigned_url_vector() {
        let raw_query = "X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request&X-Amz-Date=20130524T000000Z&X-Amz-Expires=86400&X-Amz-SignedHeaders=host&X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404";
        let pairs = parse_query_pairs(raw_query);
        let q = AuthQuery::parse(&pairs);
        let headers = hm(&[("host", "examplebucket.s3.amazonaws.com")]);
        let req = parse(&headers, &q).unwrap().unwrap();
        assert_eq!(req.style, AuthStyle::Presigned);
        assert_eq!(req.access_key, "AKIAIOSFODNN7EXAMPLE");
        assert_eq!(req.expires_secs, Some(86400));
        let sig = compute_signature(
            &req,
            EXAMPLE_SECRET,
            &Method::GET,
            "/test.txt",
            raw_query,
            &headers,
            "UNSIGNED-PAYLOAD",
        )
        .unwrap();
        assert_eq!(sig, req.signature, "AWS presigned vector must verify");
    }

    #[test]
    fn double_auth_is_rejected() {
        let headers = hm(&[
            ("host", "h"),
            (
                "authorization",
                "AWS4-HMAC-SHA256 Credential=A/20130524/us-east-1/s3/aws4_request,SignedHeaders=host,Signature=00",
            ),
        ]);
        let pairs = parse_query_pairs("X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Signature=aa");
        let q = AuthQuery::parse(&pairs);
        assert!(parse(&headers, &q).is_err());
    }

    #[test]
    fn query_canonicalisation_matches_aws_rules() {
        let pairs = vec![
            ("prefix".to_string(), "J".to_string()),
            ("max-keys".to_string(), "2".to_string()),
        ];
        assert_eq!(canonical_query(&pairs, AuthStyle::Header), "max-keys=2&prefix=J");
        // Space must become %20, not '+'.
        let pairs = vec![("prefix".to_string(), "a b".to_string())];
        assert_eq!(canonical_query(&pairs, AuthStyle::Header), "prefix=a%20b");
    }

    #[test]
    fn timestamps_and_expiry_policy() {
        let now = parse_amz_date("20130524T000500Z").unwrap();
        let mut req = SigV4Request {
            style: AuthStyle::Header,
            access_key: "A".into(),
            date: "20130524".into(),
            region: "us-east-1".into(),
            service: "s3".into(),
            terminal: "aws4_request".into(),
            signed_headers_raw: "host".into(),
            signature: "a".repeat(64),
            amz_date: "20130524T000000Z".into(),
            expires_secs: None,
            content_sha256: None,
        };
        assert!(check_timestamp(&req, "", now).is_ok());
        // 16 minutes in the past is stale for header auth.
        req.amz_date = "20130424T000000Z".into();
        assert!(check_timestamp(&req, "", now).is_err());
        // Presigned: inside the window.
        req.style = AuthStyle::Presigned;
        req.amz_date = "20130524T000000Z".into();
        req.expires_secs = Some(3600);
        assert!(check_timestamp(&req, "", now).is_ok());
        req.expires_secs = Some(60);
        assert!(check_timestamp(&req, "", now).is_err());
        // Future timestamps are always rejected.
        req.amz_date = "20130525T000000Z".into();
        assert!(check_timestamp(&req, "", now).is_err());
    }

    #[test]
    fn header_value_normalisation() {
        assert_eq!(normalize_header_value("  a   b  "), "a b");
        assert_eq!(normalize_header_value("a\t\tb"), "a b");
        assert_eq!(normalize_header_value("x"), "x");
    }

    #[test]
    fn percent_decoding_leaves_plus() {
        assert_eq!(percent_decode("a%2Fb+c"), "a/b+c");
        assert_eq!(percent_decode("%E4%B8%AD"), "中");
        assert_eq!(percent_decode("bad%zz"), "bad%zz");
    }
}












