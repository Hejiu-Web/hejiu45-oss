//! Authentication and authorisation for the S3 data plane.
//!
//! Layers, from outside in:
//!
//! 1. [`sigv4`] parses and cryptographically verifies the request. Nothing
//!    downstream runs on an unverified request.
//! 2. [`Authenticator`] resolves the access key id to a live credential record
//!    (sealed secret), checks `enabled`/expiry, and unseals the secret.
//! 3. [`Authenticator::authorize`] applies the bucket ACL — the "minimum
//!    privilege" rule from the design notes: a key can only touch buckets it
//!    was explicitly granted, with read and write tracked separately.
//!
//! Presigned URLs are produced here as well ([`Authenticator::presign`]): a
//! short-lived, cryptographically signed query string that needs no extra
//! credentials and expires on its own.

pub mod sigv4;

use axum::http::{HeaderMap, HeaderValue, Method};
use serde::Serialize;

use crate::crypto::{SecretBox, generate_access_key_id, generate_secret_access_key};
use crate::error::S3Error;
use crate::meta::{ACCESS_KEYS, AccessKeyRecord, BucketMeta, BucketPerms, Meta};
use crate::state::now_rfc3339;

/// A verified caller.
#[derive(Debug, Clone)]
pub struct Principal {
    pub access_key: String,
    pub label: String,
    pub style: sigv4::AuthStyle,
}

/// Everything needed to check a request, gathered by the HTTP layer.
pub struct AuthContext<'a> {
    pub method: &'a Method,
    pub headers: &'a HeaderMap,
    /// Raw (still percent-encoded) request path.
    pub raw_path: &'a str,
    /// Raw query string without the leading `?`.
    pub raw_query: &'a str,
    pub content_sha256: Option<String>,
    pub now: i64,
}

/// Authenticates S3 requests.
#[derive(Clone, Debug)]
pub struct Authenticator {
    meta: Meta,
    secrets: SecretBox,
    region: String,
}

impl Authenticator {
    pub fn new(meta: Meta, secrets: SecretBox, region: String) -> Self {
        Self { meta, secrets, region }
    }

    pub fn region(&self) -> &str {
        &self.region
    }

    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    /// Look up and enable-check a credential. Returns the record and the
    /// unsealed secret.
    pub fn lookup(&self, access_key: &str) -> Result<(AccessKeyRecord, String), S3Error> {
        let rec: AccessKeyRecord = self
            .meta
            .get_json(ACCESS_KEYS, access_key)
            .map_err(S3Error::internal)?
            .ok_or_else(|| S3Error::invalid_access_key(access_key))?;
        if !rec.enabled {
            return Err(S3Error::invalid_access_key(access_key));
        }
        if let Some(exp) = &rec.expires_at {
            if let Ok(dt) = time::OffsetDateTime::parse(
                exp,
                &time::format_description::well_known::Rfc3339,
            ) {
                if dt.unix_timestamp() < now_rfc3339_unix() {
                    return Err(S3Error::access_denied("The access key has expired."));
                }
            }
        }
        let secret = self
            .secrets
            .open(&rec.secret_sealed)
            .map_err(|e| S3Error::internal(e))?;
        let secret = String::from_utf8(secret).map_err(S3Error::internal)?;
        Ok((rec, secret))
    }

    /// Record first/last use (best effort: never fails a request).
    fn touch(&self, mut rec: AccessKeyRecord) {
        rec.last_used_at = Some(now_rfc3339());
        let _ = self.meta.put_json(ACCESS_KEYS, &rec.access_key_id, &rec);
    }

    /// Full verification for one request.
    pub fn verify(&self, ctx: &AuthContext<'_>) -> Result<Principal, S3Error> {
        let pairs = sigv4::parse_query_pairs(ctx.raw_query);
        let query = sigv4::AuthQuery::parse(&pairs);
        // Header auth: the payload hash travels in a header, not the query.
        let parsed = match sigv4::parse(ctx.headers, &query)? {
            Some(p) => p,
            None => {
                // Last chance for anonymous GET on a public-read bucket is
                // decided by the caller; here anonymous means "no proof".
                return Err(S3Error::missing_signature());
            }
        };
        let (rec, secret) = self.lookup(&parsed.access_key)?;

        // Clock/expiry policy.
        let mut parsed = parsed;
        if parsed.amz_date.is_empty() {
            if let Some(v) = ctx.headers.get("x-amz-date").and_then(|v| v.to_str().ok()) {
                parsed.amz_date = v.to_string();
            }
        }
        if parsed.content_sha256.is_none() {
            if let Some(v) = ctx.content_sha256.clone() {
                parsed.content_sha256 = Some(v);
            }
        }
        sigv4::check_timestamp(&parsed, ctx.raw_query, ctx.now)?;

        // Region check: a wrong region cannot produce a valid signature, but
        // surface it as the specific error AWS clients look for.
        if parsed.region != self.region {
            return Err(S3Error::access_denied(format!(
                "AuthorizationHeaderMalformed: expected region `{}`.",
                self.region
            )));
        }

        let payload_hash = sigv4::declared_payload_hash(&parsed).to_string();
        // `ctx.raw_path` is derived from the request URI, which still carries
        // the client's percent-encoding, so it is exactly what was signed.
        let expected = sigv4::compute_signature(
            &parsed,
            &secret,
            ctx.method,
            ctx.raw_path,
            ctx.raw_query,
            ctx.headers,
            &payload_hash,
        )?;
        if !sigv4::signatures_equal(&expected, &parsed.signature) {
            return Err(S3Error::signature_mismatch());
        }

        // Presigned requests authenticate; the caller enforces permissions.
        if parsed.style == sigv4::AuthStyle::Presigned {
            // Presigned URLs must be GET or HEAD: a signed URL that can
            // originate writes is a privilege-escalation footgun.
            if !matches!(*ctx.method, Method::GET | Method::HEAD) {
                return Err(S3Error::access_denied(
                    "Presigned URLs may only perform GET and HEAD.",
                ));
            }
        }
        self.touch(rec.clone());
        Ok(Principal {
            access_key: rec.access_key_id,
            label: rec.label,
            style: parsed.style,
        })
    }

    /// ACL check. Returns `Ok(())` when `principal` may `need` `bucket`.
    pub fn authorize(
        &self,
        principal: &Principal,
        bucket: &BucketMeta,
        need: BucketPerms,
    ) -> Result<(), S3Error> {
        let perms = bucket.perms.get(&principal.access_key).copied().unwrap_or_default();
        if need.can_read() && !perms.can_read() {
            return Err(S3Error::access_denied(format!(
                "Access key `{}` has no read permission on bucket `{}`.",
                principal.label, bucket.name
            )));
        }
        if need.can_write() && !perms.can_write() {
            return Err(S3Error::access_denied(format!(
                "Access key `{}` has no write permission on bucket `{}`.",
                principal.label, bucket.name
            )));
        }
        Ok(())
    }

    /// Create a credential. Returns the plaintext secret **once**; only the
    /// sealed form is persisted.
    pub fn create_access_key(
        &self,
        label: &str,
        owner: &str,
        expires_at: Option<String>,
    ) -> anyhow::Result<(AccessKeyRecord, String)> {
        let access_key_id = generate_access_key_id()?;
        let secret = generate_secret_access_key()?;
        let rec = AccessKeyRecord {
            access_key_id: access_key_id.clone(),
            secret_sealed: self.secrets.seal(secret.as_bytes())?,
            label: label.to_string(),
            created_at: now_rfc3339(),
            owner: owner.to_string(),
            enabled: true,
            expires_at,
            last_used_at: None,
        };
        self.meta.put_json(ACCESS_KEYS, &access_key_id, &rec)?;
        Ok((rec, secret))
    }

    /// Rotate: new key id and secret, same label/owner/expiry; grants on
    /// buckets are migrated so rotating does not silently revoke access.
    pub fn rotate_access_key(&self, old_id: &str) -> anyhow::Result<(AccessKeyRecord, String)> {
        let old: AccessKeyRecord = self
            .meta
            .get_json(ACCESS_KEYS, old_id)?
            .ok_or_else(|| anyhow::anyhow!("access key not found"))?;
        let (fresh, secret) =
            self.create_access_key(&old.label, &old.owner, old.expires_at.clone())?;
        // Migrate bucket grants.
        let buckets: Vec<BucketMeta> = self
            .meta
            .scan_json(crate::meta::BUCKETS, None, 100_000)?
            .into_iter()
            .map(|(_, b)| b)
            .collect();
        for mut b in buckets {
            if let Some(perms) = b.perms.remove(old_id) {
                b.perms.insert(fresh.access_key_id.clone(), perms);
                self.meta.put_json(crate::meta::BUCKETS, &b.name, &b)?;
            }
        }
        self.meta.delete_row(ACCESS_KEYS, old_id)?;
        Ok((fresh, secret))
    }

    /// Generate a presigned URL.
    ///
    /// `host` must be the `Host` the URL will be served under (e.g.
    /// `oss.example.com` or `oss.example.com:8443`), because `host` is a signed
    /// header and the verifier compares against the request's own.
    /// `url_base` is the scheme+authority used in the returned absolute URL.
    #[allow(clippy::too_many_arguments)]
    pub fn presign(
        &self,
        access_key: &str,
        method: Method,
        raw_path: &str,
        extra_query: &[(String, String)],
        ttl_secs: u64,
        now: i64,
        host: &str,
        url_base: &str,
    ) -> Result<PresignedUrl, S3Error> {
        if !matches!(method, Method::GET | Method::HEAD) {
            return Err(S3Error::access_denied("Only GET and HEAD can be presigned."));
        }
        if ttl_secs == 0 || ttl_secs > sigv4::MAX_PRESIGN_SECS {
            return Err(S3Error::invalid_request(
                "Presigned URL lifetime must be between 1 and 604800 seconds.",
            ));
        }
        let (_rec, secret) = self.lookup(access_key)?;
        let when = time::OffsetDateTime::from_unix_timestamp(now)
            .map_err(|_| S3Error::internal("bad clock"))?;
        let fmt = time::macros::format_description!(
            "[year][month][day]T[hour][minute][second]Z"
        );
        let amz_date = when.format(&fmt).map_err(|_| S3Error::internal("format"))?;
        let date = amz_date[..8].to_string();

        let signed_headers = "host".to_string();
        let scope = format!("{date}/{}/{}/aws4_request", self.region, "s3");
        let query: Vec<(String, String)> = {
            let mut q = vec![
                ("X-Amz-Algorithm".to_string(), "AWS4-HMAC-SHA256".to_string()),
                (
                    "X-Amz-Credential".to_string(),
                    format!("{access_key}/{scope}"),
                ),
                ("X-Amz-Date".to_string(), amz_date.clone()),
                ("X-Amz-Expires".to_string(), ttl_secs.to_string()),
                ("X-Amz-SignedHeaders".to_string(), signed_headers),
            ];
            q.extend(extra_query.iter().filter(|(k, _)| !k.starts_with("X-Amz-")).cloned());
            q
        };

        let mut headers = HeaderMap::new();
        // `host` is a signed header: sign the exact value the verifier will
        // see on the request, otherwise every URL we mint would 403.
        headers.insert(
            "host",
            HeaderValue::from_str(host).map_err(|_| S3Error::internal("invalid host for presign"))?,
        );

        let raw_query = {
            let mut pairs: Vec<(String, String)> = query
                .iter()
                .map(|(k, v)| (pct(k), pct(v)))
                .collect();
            pairs.sort();
            pairs
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("&")
        };

        // Canonical request with presigned style: X-Amz-* params included,
        // signature absent.
        let mut req = sigv4::SigV4Request {
            style: sigv4::AuthStyle::Presigned,
            access_key: access_key.to_string(),
            date: date.to_string(),
            region: self.region.clone(),
            service: "s3".to_string(),
            terminal: "aws4_request".to_string(),
            signed_headers_raw: "host".to_string(),
            signature: String::new(),
            // Must be exactly the `X-Amz-Date` emitted in the URL below;
            // regenerating it from the clock here would sign a different
            // timestamp whenever a second boundary is crossed.
            amz_date: amz_date.clone(),
            expires_secs: Some(ttl_secs),
            content_sha256: None,
        };
        // The URL must carry exactly the path we signed, so canonicalise it
        // strictly: S3 signs the percent-encoded form. Decode first so a caller
        // passing an already-encoded path is not double-encoded.
        let signed_path =
            sigv4::uri_encode_path(&sigv4::percent_decode(raw_path));
        let expected = sigv4::compute_signature(
            &req,
            &secret,
            &method,
            &signed_path,
            &raw_query,
            &headers,
            "UNSIGNED-PAYLOAD",
        )?;

        let expires_at = time::OffsetDateTime::from_unix_timestamp(now + ttl_secs as i64)
            .map_err(|_| S3Error::internal("bad clock"))?
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        Ok(PresignedUrl {
            url: format!("{url_base}{signed_path}?{raw_query}&X-Amz-Signature={expected}"),
            expires_at,
            method: method.to_string(),
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PresignedUrl {
    /// Path + query. The console prefixes it with `--public-url`.
    pub url: String,
    pub expires_at: String,
    pub method: String,
}

fn pct(s: &str) -> String {
    use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
    const SET: &AsciiSet = &CONTROLS
        .add(b' ').add(b'"').add(b'#').add(b'%').add(b'&').add(b'+').add(b',')
        .add(b':').add(b';').add(b'<').add(b'=').add(b'>').add(b'?').add(b'@')
        .add(b'[').add(b'\\').add(b']').add(b'^').add(b'`').add(b'{').add(b'|')
        .add(b'}').add(b'/');
    utf8_percent_encode(s, SET).to_string()
}

fn now_amz_date(now: i64) -> Result<String, S3Error> {
    time::OffsetDateTime::from_unix_timestamp(now)
        .map_err(|_| S3Error::internal("bad clock"))?
        .format(&time::macros::format_description!(
            "[year][month][day]T[hour][minute][second]Z"
        ))
        .map_err(|_| S3Error::internal("format"))
}

fn now_rfc3339_unix() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta;
    use axum::http::HeaderValue;

    fn setup() -> (std::path::PathBuf, Authenticator) {
        let dir = std::env::temp_dir().join(format!("hejiu45-auth-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let m = Meta::open(&dir.join("meta.redb")).unwrap();
        let mut master = [3u8; 32];
        master[0] = 9;
        let sb = SecretBox::from_master(&master).unwrap();
        (dir, Authenticator::new(m, sb, "us-east-1".into()))
    }

    fn bucket_with(name: &str, ak: &str, perms: BucketPerms) -> BucketMeta {
        let mut b = BucketMeta {
            name: name.into(),
            created_at: now_rfc3339(),
            owner: "admin".into(),
            perms: Default::default(),
            visibility: "private".into(),
            object_count: 0,
            total_bytes: 0,
        };
        b.perms.insert(ak.into(), perms);
        b
    }

    #[test]
    fn secret_is_sealed_at_rest_and_roundtrips() {
        let (dir, auth) = setup();
        let (rec, secret) = auth.create_access_key("ci", "admin", None).unwrap();
        let raw = auth.meta().get_json::<AccessKeyRecord>(ACCESS_KEYS, &rec.access_key_id).unwrap().unwrap();
        assert!(!raw.secret_sealed.contains(&secret), "secret leaked in plaintext");
        let (_, opened) = auth.lookup(&rec.access_key_id).unwrap();
        assert_eq!(opened, secret);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn disabled_and_expired_keys_are_rejected() {
        let (dir, auth) = setup();
        let (mut rec, _) = auth.create_access_key("ci", "admin", None).unwrap();
        rec.enabled = false;
        auth.meta().put_json(ACCESS_KEYS, &rec.access_key_id, &rec).unwrap();
        assert!(auth.lookup(&rec.access_key_id).is_err());

        let (rec2, _) = auth
            .create_access_key("old", "admin", Some("2000-01-01T00:00:00Z".into()))
            .unwrap();
        assert!(auth.lookup(&rec2.access_key_id).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn acl_enforces_least_privilege() {
        let (dir, auth) = setup();
        let (ro, _) = auth.create_access_key("ro", "admin", None).unwrap();
        let (wo, _) = auth.create_access_key("wo", "admin", None).unwrap();
        let (none, _) = auth.create_access_key("none", "admin", None).unwrap();
        let b = bucket_with("photos", &ro.access_key_id, BucketPerms::READ);
        let mut b = b;
        b.perms.insert(wo.access_key_id.clone(), BucketPerms::WRITE);

        let p_ro = Principal { access_key: ro.access_key_id.clone(), label: "ro".into(), style: sigv4::AuthStyle::Header };
        let p_wo = Principal { access_key: wo.access_key_id.clone(), label: "wo".into(), style: sigv4::AuthStyle::Header };
        let p_none = Principal { access_key: none.access_key_id.clone(), label: "none".into(), style: sigv4::AuthStyle::Header };

        assert!(auth.authorize(&p_ro, &b, BucketPerms::READ).is_ok());
        assert!(auth.authorize(&p_ro, &b, BucketPerms::WRITE).is_err());
        assert!(auth.authorize(&p_wo, &b, BucketPerms::WRITE).is_ok());
        assert!(auth.authorize(&p_wo, &b, BucketPerms::READ).is_err());
        assert!(auth.authorize(&p_none, &b, BucketPerms::READ).is_err());
        assert!(auth.authorize(&p_none, &b, BucketPerms::WRITE).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotation_migrates_grants() {
        let (dir, auth) = setup();
        let (old, _) = auth.create_access_key("svc", "admin", None).unwrap();
        let mut b = bucket_with("data", &old.access_key_id, BucketPerms::FULL);
        auth.meta().put_json(meta::BUCKETS, "data", &b).unwrap();
        let (fresh, _) = auth.rotate_access_key(&old.access_key_id).unwrap();
        assert_ne!(fresh.access_key_id, old.access_key_id);
        let stored: BucketMeta = auth.meta().get_json(meta::BUCKETS, "data").unwrap().unwrap();
        assert!(stored.perms.contains_key(&fresh.access_key_id));
        assert!(!stored.perms.contains_key(&old.access_key_id));
        assert!(auth.lookup(&old.access_key_id).is_err());
        b.perms.clear();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn presigned_url_verifies_end_to_end() {
        let (dir, auth) = setup();
        let (rec, _) = auth.create_access_key("share", "admin", None).unwrap();
        let now = sigv4::parse_amz_date("20260101T120000Z").unwrap();
        let path = "/photos/a%20b.txt";

        for host in ["oss.example.com", "oss.example.com:8443"] {
            let url = auth
                .presign(
                    &rec.access_key_id,
                    Method::GET,
                    path,
                    &[],
                    300,
                    now,
                    host,
                    &format!("https://{host}"),
                )
                .unwrap();
            let rest = url.url.strip_prefix(&format!("https://{host}")).unwrap();
            let (p, query) = rest.split_once('?').unwrap();
            assert_eq!(p, path);
            assert!(query.contains("X-Amz-Signature="));

            // Re-verify exactly as the request path would: the verifier sees
            // the Host header the client actually used.
            let mut headers = HeaderMap::new();
            headers.insert("host", HeaderValue::from_str(host).unwrap());
            let pairs = sigv4::parse_query_pairs(query);
            let q = sigv4::AuthQuery::parse(&pairs);
            let parsed = sigv4::parse(&headers, &q).unwrap().unwrap();
            let (_, secret) = auth.lookup(&rec.access_key_id).unwrap();
            let expected = sigv4::compute_signature(
                &parsed, &secret, &Method::GET, p, query, &headers, "UNSIGNED-PAYLOAD",
            )
            .unwrap();
            assert_eq!(expected, parsed.signature, "host {host}");

            // A different host, path or method must not verify.
            let mut other = HeaderMap::new();
            other.insert("host", HeaderValue::from_static("evil.example.com"));
            let bad = sigv4::compute_signature(
                &parsed, &secret, &Method::GET, p, query, &other, "UNSIGNED-PAYLOAD",
            )
            .unwrap();
            assert_ne!(bad, parsed.signature);
        }

        // Expiry is enforced against X-Amz-Date + X-Amz-Expires.
        let url = auth
            .presign(&rec.access_key_id, Method::GET, path, &[], 300, now, "oss.example.com", "https://oss.example.com")
            .unwrap();
        let query = url.url.split_once('?').unwrap().1.to_string();
        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("oss.example.com"));
        let parsed = sigv4::parse(&headers, &sigv4::AuthQuery::parse(&sigv4::parse_query_pairs(&query)))
            .unwrap()
            .unwrap();
        assert_eq!(parsed.amz_date, "20260101T120000Z");
        assert!(sigv4::check_timestamp(&parsed, &query, now + 200).is_ok());
        assert!(sigv4::check_timestamp(&parsed, &query, now + 301).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Client-side encoding differs between SDKs; the server must accept a
    /// signature produced over the strictly-encoded path.
    #[test]
    fn non_standard_path_encoding_is_accepted() {
        let (dir, auth) = setup();
        let (rec, _) = auth.create_access_key("enc", "admin", None).unwrap();
        let now = sigv4::parse_amz_date("20260101T120000Z").unwrap();
        // Space, `$` and `:` — three characters SDKs disagree about.
        let strict = "/photos/a%20b%24c%3Ad.bin";
        let url = auth
            .presign(&rec.access_key_id, Method::GET, strict, &[], 60, now, "oss.example.com", "https://oss.example.com")
            .unwrap();
        let (path, query) = url
            .url
            .strip_prefix("https://oss.example.com")
            .unwrap()
            .split_once('?')
            .unwrap();
        assert_eq!(path, strict);

        let mut headers = HeaderMap::new();
        headers.insert("host", HeaderValue::from_static("oss.example.com"));
        let parsed = sigv4::parse(
            &headers,
            &sigv4::AuthQuery::parse(&sigv4::parse_query_pairs(query)),
        )
        .unwrap()
        .unwrap();
        let (_, secret) = auth.lookup(&rec.access_key_id).unwrap();
        let expected = sigv4::compute_signature(
            &parsed, &secret, &Method::GET, path, query, &headers, "UNSIGNED-PAYLOAD",
        )
        .unwrap();
        assert_eq!(expected, parsed.signature);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn presign_refuses_writes_and_bad_ttl() {
        let (dir, auth) = setup();
        let (rec, _) = auth.create_access_key("x", "admin", None).unwrap();
        let now = sigv4::parse_amz_date("20260101T120000Z").unwrap();
        assert!(auth.presign(&rec.access_key_id, Method::PUT, "/b/k", &[], 60, now, "oss.example.com", "https://oss.example.com").is_err());
        assert!(auth.presign(&rec.access_key_id, Method::GET, "/b/k", &[], 0, now, "oss.example.com", "https://oss.example.com").is_err());
        assert!(
            auth.presign(&rec.access_key_id, Method::GET, "/b/k", &[], sigv4::MAX_PRESIGN_SECS + 1, now, "oss.example.com", "https://oss.example.com")
                .is_err()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}










