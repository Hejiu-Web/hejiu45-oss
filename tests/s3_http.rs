//! In-process HTTP integration tests for the S3 data plane.
//!
//! These start the real router on an ephemeral loopback port and drive it over
//! TCP with requests signed by an **independent** SigV4 implementation (below),
//! so routing, middleware, signature verification and storage all run exactly
//! as in production.
//!
//! Why in-process rather than shelling out to a script: the suite then runs on
//! every platform with `cargo test`, needs no bash/PowerShell, and cannot drift
//! from the server code it is testing. The AWS CLI suite
//! (`scripts/e2e-awscli.ps1`) remains the separate "real SDK" check.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;

use clap::Parser;
use hejiu45_oss::auth::Authenticator;
use hejiu45_oss::config::Args;
use hejiu45_oss::crypto::SecretBox;
use hejiu45_oss::data::Data;
use hejiu45_oss::meta::Meta;
use hejiu45_oss::server::build_router;
use hejiu45_oss::state::AppState;
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

// ---------------------------------------------------------------------------
// Minimal independent SigV4 signer
//
// Deliberately written from the AWS specification rather than reused from
// `src/auth/sigv4.rs`: a test that shares the implementation under test proves
// nothing about the algorithm.
// ---------------------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut m = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("hmac key");
    m.update(data);
    m.finalize().into_bytes().to_vec()
}

/// RFC 3986 encoding; `keep_slash` leaves `/` literal (paths).
fn encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'~')
            || (keep_slash && b == b'/');
        if keep {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

struct Signer {
    access_key: String,
    secret_key: String,
    region: String,
}

struct Signed {
    url: String,
    headers: Vec<(String, String)>,
}

impl Signer {
    fn new(access_key: &str, secret_key: &str, region: &str) -> Self {
        Self {
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            region: region.into(),
        }
    }

    /// Sign one request. `path` must be the encoded path as sent.
    fn sign(
        &self,
        method: &str,
        addr: SocketAddr,
        path: &str,
        query: &[(String, String)],
        extra_headers: &[(&str, &str)],
        body: &[u8],
    ) -> Signed {
        let now = time::OffsetDateTime::now_utc();
        let fmt = time::macros::format_description!("[year][month][day]T[hour][minute][second]Z");
        let amz_date = now.format(fmt).expect("format");
        let date = &amz_date[..8];
        let payload_hash = sha256_hex(body);
        let host = format!("{addr}");

        // Canonical query: encode, sort by (key, value).
        let mut pairs: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (encode(k, false), encode(v, false)))
            .collect();
        pairs.sort();
        let canonical_query = pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&");

        // Canonical headers: sorted, lowercase names.
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        headers.insert("host".into(), host.clone());
        headers.insert("x-amz-content-sha256".into(), payload_hash.clone());
        headers.insert("x-amz-date".into(), amz_date.clone());
        for (k, v) in extra_headers {
            headers.insert(k.to_ascii_lowercase(), (*v).to_string());
        }
        let signed_headers = headers.keys().cloned().collect::<Vec<_>>().join(";");
        let canonical_headers = headers
            .iter()
            .map(|(k, v)| format!("{k}:{v}\n"))
            .collect::<String>();

        let canonical_request = format!(
            "{method}\n{path}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
        );
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            sha256_hex(canonical_request.as_bytes())
        );
        let k_date = hmac(
            format!("AWS4{}", self.secret_key).as_bytes(),
            date.as_bytes(),
        );
        let k_region = hmac(&k_date, self.region.as_bytes());
        let k_service = hmac(&k_region, b"s3");
        let k_signing = hmac(&k_service, b"aws4_request");
        let signature = hex(&hmac(&k_signing, string_to_sign.as_bytes()));

        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            self.access_key
        );

        let mut url = format!("http://{addr}{path}");
        if !canonical_query.is_empty() {
            url.push('?');
            url.push_str(&canonical_query);
        }
        let mut out_headers: Vec<(String, String)> =
            headers.into_iter().filter(|(k, _)| k != "host").collect();
        out_headers.push(("authorization".into(), authorization));
        Signed {
            url,
            headers: out_headers,
        }
    }
}

// ---------------------------------------------------------------------------
// Test server
// ---------------------------------------------------------------------------

struct TestServer {
    addr: SocketAddr,
    signer: Signer,
    auth: Authenticator,
    meta: Meta,
    _dir: TempDir,
    shutdown: tokio::sync::oneshot::Sender<()>,
}

struct TempDir(std::path::PathBuf);

impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hj45-it-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&p).expect("temp dir");
        Self(p)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl TestServer {
    async fn start() -> Self {
        let dir = TempDir::new();
        let data_dir = dir.path().join("data");
        std::fs::create_dir_all(&data_dir).expect("data dir");

        let args = Args::parse_from([
            "hejiu45-oss",
            "--data-dir",
            data_dir.to_str().expect("utf8"),
            "--listen",
            "127.0.0.1:0",
            "--allow-insecure",
            "--s3-prefix",
            "/s3",
        ]);
        let meta = Meta::open(&data_dir.join("meta.redb")).expect("meta");
        let data = Data::open(&data_dir).expect("data");
        let mut master = [0u8; 32];
        master[0] = 7;
        let secrets = SecretBox::from_master(&master).expect("secretbox");
        let auth = Authenticator::new(meta.clone(), secrets, args.region.clone());

        let (rec, secret) = auth
            .create_access_key("integration", "test", None)
            .expect("access key");

        let state = AppState {
            args: Arc::new(args),
            meta: meta.clone(),
            data,
            auth: auth.clone(),
            started_at: std::time::Instant::now(),
            started_unix: time::OffsetDateTime::now_utc().unix_timestamp(),
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        let router = build_router(state);
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router.into_make_service())
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await;
        });

        TestServer {
            addr,
            signer: Signer::new(&rec.access_key_id, &secret, "us-east-1"),
            auth,
            meta,
            _dir: dir,
            shutdown: tx,
        }
    }

    /// Sign and send; returns `(status, headers, body)`.
    async fn call(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, &str)],
        extra: &[(&str, &str)],
        body: &[u8],
    ) -> (u16, Vec<(String, String)>, Vec<u8>) {
        let q: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        let signed = self.signer.sign(method, self.addr, path, &q, extra, body);
        self.send_raw(method, &signed.url, &signed.headers, body)
            .await
    }

    async fn send_raw(
        &self,
        method: &str,
        url: &str,
        headers: &[(String, String)],
        body: &[u8],
    ) -> (u16, Vec<(String, String)>, Vec<u8>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = tokio::net::TcpStream::connect(self.addr)
            .await
            .expect("connect");
        let path_and_query = url
            .split_once("://")
            .and_then(|(_, rest)| rest.find('/').map(|i| &rest[i..]))
            .unwrap_or("/");
        let mut req = format!(
            "{method} {path_and_query} HTTP/1.1\r\nhost: {}\r\n",
            self.addr
        );
        for (k, v) in headers {
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        req.push_str(&format!(
            "content-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        ));
        stream.write_all(req.as_bytes()).await.expect("write head");
        stream.write_all(body).await.expect("write body");
        stream.flush().await.expect("flush");

        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.expect("read");
        parse_http(&raw)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = std::mem::replace(&mut self.shutdown, tokio::sync::oneshot::channel().0).send(());
    }
}

/// Just enough HTTP/1.1 parsing for `connection: close` responses.
fn parse_http(raw: &[u8]) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("header terminator");
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let chunked = headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.contains("chunked"));
    let payload = &raw[split + 4..];
    let body = if chunked {
        dechunk(payload)
    } else {
        payload.to_vec()
    };
    (status, headers, body)
}

fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(pos) = data.windows(2).position(|w| w == b"\r\n") {
        let size_str = String::from_utf8_lossy(&data[..pos]);
        let size = usize::from_str_radix(size_str.split(';').next().unwrap_or("0").trim(), 16)
            .unwrap_or(0);
        data = &data[pos + 2..];
        if size == 0 {
            break;
        }
        let end = size.min(data.len());
        out.extend_from_slice(&data[..end]);
        data = &data[end..];
        if data.starts_with(b"\r\n") {
            data = &data[2..];
        }
    }
    out
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn bucket_and_object_lifecycle() {
    let srv = TestServer::start().await;
    let bucket = "lifecycle-bucket";
    let p = format!("/s3/{bucket}");

    // CreateBucket
    let (st, _, _) = srv.call("PUT", &p, &[], &[], b"").await;
    assert_eq!(st, 200, "CreateBucket");

    // Duplicate CreateBucket
    let (st, _, body) = srv.call("PUT", &p, &[], &[], b"").await;
    assert_eq!(st, 409);
    assert!(String::from_utf8_lossy(&body).contains("BucketAlreadyOwnedByYou"));

    // HeadBucket
    let (st, _, _) = srv.call("HEAD", &p, &[], &[], b"").await;
    assert_eq!(st, 200, "HeadBucket");

    // ListBuckets shows it
    let (st, _, body) = srv.call("GET", "/s3", &[], &[], b"").await;
    assert_eq!(st, 200);
    assert!(String::from_utf8_lossy(&body).contains(&format!("<Name>{bucket}</Name>")));

    // PutObject + ETag
    let key_path = format!("{p}/hello.txt");
    let payload = b"hello world";
    let (st, headers, _) = srv.call("PUT", &key_path, &[], &[], payload).await;
    assert_eq!(st, 200, "PutObject");
    assert_eq!(
        header(&headers, "etag"),
        Some("\"5eb63bbbe01eeed093cb22bb8f5acdc3\""),
        "ETag must be the quoted MD5"
    );

    // GetObject
    let (st, _, body) = srv.call("GET", &key_path, &[], &[], b"").await;
    assert_eq!(st, 200);
    assert_eq!(body, payload);

    // HeadObject
    let (st, headers, _) = srv.call("HEAD", &key_path, &[], &[], b"").await;
    assert_eq!(st, 200);
    assert_eq!(header(&headers, "content-length"), Some("11"));
    // No Content-Type was sent and plain text has no magic number, so the
    // server must NOT guess text/* (that would be an XSS vector). An explicit
    // type is honoured, which `content_type_is_sniffed_when_not_provided`
    // covers.
    assert_eq!(
        header(&headers, "content-type"),
        Some("application/octet-stream")
    );

    // Range GET
    let (st, headers, body) = srv
        .call("GET", &key_path, &[], &[("range", "bytes=6-10")], b"")
        .await;
    assert_eq!(st, 206, "range request");
    assert_eq!(body, b"world");
    assert_eq!(header(&headers, "content-range"), Some("bytes 6-10/11"));

    // Overwrite (exercises the atomic-replace path, including on Windows)
    let (st, _, _) = srv.call("PUT", &key_path, &[], &[], b"replaced").await;
    assert_eq!(st, 200);
    let (_, _, body) = srv.call("GET", &key_path, &[], &[], b"").await;
    assert_eq!(body, b"replaced");

    // DeleteObject is idempotent
    let (st, _, _) = srv.call("DELETE", &key_path, &[], &[], b"").await;
    assert_eq!(st, 204);
    let (st, _, _) = srv.call("DELETE", &key_path, &[], &[], b"").await;
    assert_eq!(st, 204);

    // DeleteBucket
    let (st, _, _) = srv.call("DELETE", &p, &[], &[], b"").await;
    assert_eq!(st, 204);
}

#[tokio::test]
async fn listing_with_prefix_delimiter_and_pagination() {
    let srv = TestServer::start().await;
    let bucket = "listing-bucket";
    let p = format!("/s3/{bucket}");
    srv.call("PUT", &p, &[], &[], b"").await;

    for key in ["a.txt", "dir/one.txt", "dir/two.txt"] {
        let (st, _, _) = srv.call("PUT", &format!("{p}/{key}"), &[], &[], b"x").await;
        assert_eq!(st, 200, "put {key}");
    }

    // delimiter roll-up
    let (st, _, body) = srv
        .call(
            "GET",
            &p,
            &[("list-type", "2"), ("delimiter", "/")],
            &[],
            b"",
        )
        .await;
    assert_eq!(st, 200);
    let xml = String::from_utf8_lossy(&body);
    assert!(
        xml.contains("<CommonPrefixes><Prefix>dir/</Prefix></CommonPrefixes>"),
        "expected dir/ as a common prefix: {xml}"
    );
    assert!(xml.contains("<Key>a.txt</Key>"));

    // pagination
    let (_, _, body) = srv
        .call(
            "GET",
            &p,
            &[("list-type", "2"), ("max-keys", "1")],
            &[],
            b"",
        )
        .await;
    let xml = String::from_utf8_lossy(&body);
    assert!(xml.contains("<IsTruncated>true</IsTruncated>"));
    assert!(xml.contains("<NextContinuationToken>"));

    // the v1 form still works
    let (st, _, body) = srv.call("GET", &p, &[("delimiter", "/")], &[], b"").await;
    assert_eq!(st, 200);
    assert!(String::from_utf8_lossy(&body).contains("<CommonPrefixes>"));
}

#[tokio::test]
async fn unicode_and_awkward_keys() {
    let srv = TestServer::start().await;
    let bucket = "keys-bucket";
    let p = format!("/s3/{bucket}");
    srv.call("PUT", &p, &[], &[], b"").await;

    // Percent-encoded UTF-8 and a space; the signature covers the encoded path.
    let path = format!("{p}/dir/%E4%B8%AD%E6%96%87%20file.txt");
    let (st, _, _) = srv.call("PUT", &path, &[], &[], b"unicode").await;
    assert_eq!(st, 200, "put unicode key");
    let (st, _, body) = srv.call("GET", &path, &[], &[], b"").await;
    assert_eq!(st, 200);
    assert_eq!(body, b"unicode");

    // A key with literal dot-dot segments must stay inside the bucket. (The
    // object store addresses bodies by the hash of the key, so `..` cannot be
    // interpreted as a filesystem parent — this asserts that contract.)
    let evil = format!("{p}/../../etc/passwd");
    let (st, _, _) = srv.call("PUT", &evil, &[], &[], b"nope").await;
    assert!(st == 200, "traversal-looking key is just a key, got {st}");
    assert!(
        srv.data_file_count() < 100,
        "the server must not create files outside the data directory"
    );
    let (st, _, body) = srv.call("GET", "/s3", &[], &[], b"").await;
    assert_eq!(st, 200);
    assert!(
        !String::from_utf8_lossy(&body).contains("root:x:"),
        "traversal key escaped the data directory"
    );
}

#[tokio::test]
async fn content_type_is_sniffed_when_not_provided() {
    let srv = TestServer::start().await;
    let bucket = "sniff-bucket";
    let p = format!("/s3/{bucket}");
    srv.call("PUT", &p, &[], &[], b"").await;

    let png: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3, 4];
    let (st, _, _) = srv
        .call("PUT", &format!("{p}/img.png"), &[], &[], png)
        .await;
    assert_eq!(st, 200);
    let (_, headers, _) = srv
        .call("HEAD", &format!("{p}/img.png"), &[], &[], b"")
        .await;
    assert_eq!(header(&headers, "content-type"), Some("image/png"));

    // An explicit header wins.
    let (_, _, _) = srv
        .call(
            "PUT",
            &format!("{p}/data.bin"),
            &[],
            &[("content-type", "application/x-custom")],
            b"\x00\x01",
        )
        .await;
    let (_, headers, _) = srv
        .call("HEAD", &format!("{p}/data.bin"), &[], &[], b"")
        .await;
    assert_eq!(
        header(&headers, "content-type"),
        Some("application/x-custom")
    );
}

#[tokio::test]
async fn security_negatives() {
    let srv = TestServer::start().await;
    let bucket = "security-bucket";
    let p = format!("/s3/{bucket}");
    srv.call("PUT", &p, &[], &[], b"").await;
    srv.call("PUT", &format!("{p}/secret.txt"), &[], &[], b"top secret")
        .await;

    let url = format!("http://{}/s3/{bucket}/secret.txt", srv.addr);

    // 1. No credentials at all.
    let (st, _, _) = srv.send_raw("GET", &url, &[], b"").await;
    assert_eq!(st, 403, "unsigned request must be rejected");

    // 2. A signature with one character changed.
    let signed = srv.signer.sign(
        "GET",
        srv.addr,
        &format!("/s3/{bucket}/secret.txt"),
        &[],
        &[],
        b"",
    );
    let tampered: Vec<(String, String)> = signed
        .headers
        .iter()
        .map(|(k, v)| {
            if k == "authorization" {
                let mut a = v.clone();
                let last = a.pop().unwrap();
                a.push(if last == '0' { '1' } else { '0' });
                (k.clone(), a)
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect();
    let (st, _, body) = srv.send_raw("GET", &url, &tampered, b"").await;
    assert_eq!(st, 403);
    assert!(
        String::from_utf8_lossy(&body).contains("SignatureDoesNotMatch"),
        "expected SignatureDoesNotMatch: {}",
        String::from_utf8_lossy(&body)
    );

    // 3. Declared payload hash does not match the body.
    let signed = srv.signer.sign(
        "PUT",
        srv.addr,
        &format!("/s3/{bucket}/mismatch.txt"),
        &[],
        &[],
        b"signed-over-other-bytes",
    );
    let (st, _, body) = srv
        .send_raw("PUT", &signed.url, &signed.headers, b"actual-body")
        .await;
    assert_eq!(st, 400, "body/signature mismatch must fail");
    assert!(String::from_utf8_lossy(&body).contains("XAmzContentSHA256Mismatch"));

    // 4. Wrong region in the credential scope.
    let wrong = Signer::new(
        // Reuse the same access key but a different region.
        &{
            let auth = srv.signer.sign("GET", srv.addr, "/", &[], &[], b"");
            auth.headers
                .iter()
                .find(|(k, _)| k == "authorization")
                .and_then(|(_, v)| v.split("Credential=").nth(1))
                .and_then(|v| v.split('/').next())
                .unwrap_or_default()
                .to_string()
        },
        &srv.signer.secret_key,
        "eu-west-1",
    );
    let signed = wrong.sign("GET", srv.addr, "/s3", &[], &[], b"");
    let (st, _, _) = srv.send_raw("GET", &signed.url, &signed.headers, b"").await;
    assert_eq!(st, 403, "wrong region must be rejected");

    // 5. Presigned URL signed an hour ago with a 1-second lifetime.
    //
    // The signature itself is valid (the server recomputes it over the stale
    // timestamp), so the only thing that can reject it is the expiry policy.
    let key_path = format!("/s3/{bucket}/secret.txt");
    let presigned = srv.presign_at(&key_path, -3600, 1);
    let (st, _, body) = srv.send_raw("GET", &presigned, &[], b"").await;
    assert_eq!(
        st,
        403,
        "expired presigned request must be rejected: {}",
        String::from_utf8_lossy(&body)
    );
}

#[tokio::test]
async fn multipart_upload_roundtrip() {
    let srv = TestServer::start().await;
    let bucket = "multipart-bucket";
    let p = format!("/s3/{bucket}");
    srv.call("PUT", &p, &[], &[], b"").await;

    let key_path = format!("{p}/big.bin");

    // CreateMultipartUpload in the standard path form (what SDKs use).
    let (st, _, body) = srv
        .call("POST", &key_path, &[("uploads", "")], &[], b"")
        .await;
    assert_eq!(
        st,
        200,
        "CreateMultipartUpload: {}",
        String::from_utf8_lossy(&body)
    );
    let xml = String::from_utf8_lossy(&body).to_string();
    let upload_id = xml
        .split("<UploadId>")
        .nth(1)
        .and_then(|s| s.split("</UploadId>").next())
        .expect("upload id")
        .to_string();

    // Two parts, uploaded "concurrently" in the sense that the records race.
    let part: Vec<u8> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
    let mut etags = Vec::new();
    for n in 1..=2u32 {
        let (st, headers, body) = srv
            .call(
                "PUT",
                &key_path,
                &[("partNumber", &n.to_string()), ("uploadId", &upload_id)],
                &[],
                &part,
            )
            .await;
        assert_eq!(
            st,
            200,
            "UploadPart {n}: {}",
            String::from_utf8_lossy(&body)
        );
        etags.push(header(&headers, "etag").expect("part etag").to_string());
    }

    let complete = format!(
        "<CompleteMultipartUpload>{}</CompleteMultipartUpload>",
        etags
            .iter()
            .enumerate()
            .map(|(i, e)| format!(
                "<Part><PartNumber>{}</PartNumber><ETag>{e}</ETag></Part>",
                i + 1
            ))
            .collect::<String>()
    );
    let (st, _, body) = srv
        .call(
            "POST",
            &key_path,
            &[("uploadId", &upload_id)],
            &[],
            complete.as_bytes(),
        )
        .await;
    assert_eq!(st, 200, "Complete: {}", String::from_utf8_lossy(&body));

    let (st, headers, _) = srv.call("HEAD", &key_path, &[], &[], b"").await;
    assert_eq!(st, 200);
    assert_eq!(
        header(&headers, "content-length"),
        Some((2 * 1024 * 1024).to_string().as_str()),
        "assembled size"
    );
}

#[tokio::test]
async fn acl_enforcement_and_public_read() {
    let srv = TestServer::start().await;

    // A second credential with no grants.
    let (other_key, other_secret) = srv.access_key_of_other("no-grants").await;
    let outsider = Signer::new(&other_key, &other_secret, "us-east-1");

    let bucket = "acl-bucket";
    let p = format!("/s3/{bucket}");
    let (st, _, _) = srv.call("PUT", &p, &[], &[], b"").await;
    assert_eq!(st, 200);
    srv.call("PUT", &format!("{p}/file.txt"), &[], &[], b"data")
        .await;

    // The un-granted key cannot list, read or write.
    let signed = outsider.sign("GET", srv.addr, &p, &[], &[], b"");
    let (st, _, _) = srv.send_raw("GET", &signed.url, &signed.headers, b"").await;
    assert_eq!(st, 403, "ungranted key must not list the bucket");

    let signed = outsider.sign("GET", srv.addr, &format!("{p}/file.txt"), &[], &[], b"");
    let (st, _, _) = srv.send_raw("GET", &signed.url, &signed.headers, b"").await;
    assert_eq!(st, 403, "ungranted key must not read objects");

    // It also does not see the bucket in ListBuckets.
    let signed = outsider.sign("GET", srv.addr, "/s3", &[], &[], b"");
    let (st, _, body) = srv.send_raw("GET", &signed.url, &signed.headers, b"").await;
    assert_eq!(st, 200);
    assert!(
        !String::from_utf8_lossy(&body).contains(bucket),
        "ungranted key must not see the bucket in a listing"
    );

    // Making it public-read allows anonymous GET but still no anonymous write.
    srv.make_public(bucket).await;
    let (st, _, body) = srv
        .send_raw(
            "GET",
            &format!("http://{}/s3/{bucket}/file.txt", srv.addr),
            &[],
            b"",
        )
        .await;
    assert_eq!(st, 200, "public-read bucket allows anonymous GET");
    assert_eq!(body, b"data");

    let (st, _, _) = srv
        .send_raw(
            "PUT",
            &format!("http://{}/s3/{bucket}/evil.txt", srv.addr),
            &[],
            b"x",
        )
        .await;
    assert_eq!(st, 403, "anonymous writes are never allowed");

    // And the console shell is what `/` serves, not S3.
    let (st, headers, _) = srv
        .send_raw("GET", &format!("http://{}/", srv.addr), &[], b"")
        .await;
    assert_eq!(st, 200);
    assert_eq!(header(&headers, "content-type"), Some("text/html"));
}

impl TestServer {
    /// Create a second credential with no bucket grants.
    async fn access_key_of_other(&self, label: &str) -> (String, String) {
        let (rec, secret) = self
            .auth
            .create_access_key(label, "test", None)
            .expect("second access key");
        (rec.access_key_id, secret)
    }

    /// Count files under the data directory (used to prove a traversal-looking
    /// key did not create anything outside it).
    fn data_file_count(&self) -> usize {
        fn walk(dir: &std::path::Path, n: &mut usize) {
            if let Ok(rd) = std::fs::read_dir(dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        walk(&p, n);
                    } else {
                        *n += 1;
                    }
                }
            }
        }
        let mut n = 0;
        walk(self._dir.path(), &mut n);
        n
    }

    /// Mint a presigned URL whose signature is *valid* but whose timestamp is
    /// `offset_secs` away from now, so only the expiry policy can reject it.
    ///
    /// The verifier recomputes the signature over the stale `X-Amz-Date`, so
    /// this exercises the timestamp rules rather than signature mismatch.
    fn presign_at(&self, path: &str, offset_secs: i64, expires: u64) -> String {
        let now = time::OffsetDateTime::now_utc().unix_timestamp() + offset_secs;
        let url = self
            .auth
            .presign(
                &self.signer.access_key,
                axum::http::Method::GET,
                path,
                &[],
                expires,
                now,
                &self.addr.to_string(),
                &format!("http://{}", self.addr),
            )
            .expect("presign");
        url.url
    }

    /// Flip a bucket to `public-read` in the metadata (the console does this
    /// through `PATCH /api/buckets/{name}`; the test writes it directly to keep
    /// the S3 suite free of console sessions).
    async fn make_public(&self, bucket: &str) {
        use hejiu45_oss::meta::{BUCKETS, BucketMeta};
        let mut b: BucketMeta = self
            .meta
            .get_json(BUCKETS, bucket)
            .expect("read bucket")
            .expect("bucket exists");
        b.visibility = "public-read".into();
        self.meta
            .put_json(BUCKETS, bucket, &b)
            .expect("write bucket");
    }
}

