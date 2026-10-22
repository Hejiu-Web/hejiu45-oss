//! Console upload path: session-authenticated object writes, multipart, delete,
//! download and the persisted upload limits.
//!
//! These drive the real router over TCP like `s3_http.rs`, but authenticate with
//! the **console session cookie** instead of SigV4 — the path a browser uses.

use std::net::SocketAddr;
use std::sync::Arc;

use clap::Parser;
use hejiu45_oss::auth::Authenticator;
use hejiu45_oss::config::Args;
use hejiu45_oss::crypto::{SecretBox, hash_admin_password};
use hejiu45_oss::data::Data;
use hejiu45_oss::meta::{ADMIN_USERS, AdminUser, Limits, Meta, SETTINGS};
use hejiu45_oss::server::build_router;
use hejiu45_oss::state::AppState;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

struct TempDir(std::path::PathBuf);
impl TempDir {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("hj45-up-{}", uuid::Uuid::new_v4().simple()));
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

struct Ctx {
    addr: SocketAddr,
    cookie: String,
    state: AppState,
    _dir: TempDir,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

const ADMIN_PW: &str = "ConsoleTestPass-1234";

impl Ctx {
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
        ]);
        let meta = Meta::open(&data_dir.join("meta.redb")).expect("meta");
        let data = Data::open(&data_dir).expect("data");
        let mut master = [0u8; 32];
        master[0] = 11;
        let secrets = SecretBox::from_master(&master).expect("secretbox");
        let auth = Authenticator::new(meta.clone(), secrets, args.region.clone());

        // An admin account plus the limits document, as a first boot would leave.
        let user = AdminUser {
            username: "admin".into(),
            password_phc: hash_admin_password(ADMIN_PW).expect("hash"),
            created_at: "2026-01-01T00:00:00Z".into(),
            session_generation: 0,
        };
        meta.put_json(ADMIN_USERS, "admin", &user).expect("admin");
        meta.put_json(SETTINGS, Limits::KEY, &Limits::default())
            .expect("limits");

        let limits = AppState::load_limits(&meta, &args);
        let state = AppState {
            args: Arc::new(args),
            meta: meta.clone(),
            data,
            auth,
            started_at: std::time::Instant::now(),
            started_unix: time::OffsetDateTime::now_utc().unix_timestamp(),
            limits: Arc::new(parking_lot::RwLock::new(limits)),
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = listener.local_addr().expect("addr");
        // Point the state's advertised URL at the port we actually bound:
        // presigned URLs sign the Host header, so a deployment sets
        // `--public-url` and the test must do the equivalent.
        let mut args = state.args.as_ref().clone();
        args.public_url = Some(format!("http://{addr}"));
        let state = AppState {
            args: Arc::new(args),
            ..state.clone()
        };
        let router = build_router(state.clone());
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router.into_make_service())
                .with_graceful_shutdown(async move {
                    let _ = rx.await;
                })
                .await;
        });

        let mut ctx = Ctx {
            addr,
            cookie: String::new(),
            state,
            _dir: dir,
            shutdown: Some(tx),
        };
        ctx.login().await;
        ctx
    }

    async fn login(&mut self) {
        let body = format!(r#"{{"username":"admin","password":"{ADMIN_PW}"}}"#);
        let (st, headers, _) = self
            .send(
                "POST",
                "/api/auth/login",
                &[("content-type", "application/json")],
                Some(body.as_bytes()),
            )
            .await;
        assert_eq!(st, 200, "login");
        let set_cookie = headers
            .iter()
            .find(|(k, _)| k == "set-cookie")
            .map(|(_, v)| v.clone())
            .expect("session cookie");
        self.cookie = set_cookie.split(';').next().unwrap_or_default().to_string();
    }

    /// Send a request with the session cookie; returns (status, headers, body).
    async fn send(
        &self,
        method: &str,
        path: &str,
        extra: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> (u16, Vec<(String, String)>, Vec<u8>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut stream = tokio::net::TcpStream::connect(self.addr)
            .await
            .expect("connect");
        // A caller may override `Host` (custom-domain routing). Emitting the
        // default first and the override second produced two Host headers, and
        // the server read the first one.
        let host_override = extra
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("host"))
            .map(|(_, v)| *v);
        let host = host_override
            .map(str::to_string)
            .unwrap_or_else(|| self.addr.to_string());
        let mut req = format!("{method} {path} HTTP/1.1\r\nhost: {host}\r\n");
        if !self.cookie.is_empty() {
            req.push_str(&format!("cookie: {}\r\n", self.cookie));
        }
        for (k, v) in extra {
            if k.eq_ignore_ascii_case("host") {
                continue;
            }
            req.push_str(&format!("{k}: {v}\r\n"));
        }
        let len = body.map(|b| b.len()).unwrap_or(0);
        req.push_str(&format!(
            "content-length: {len}\r\nconnection: close\r\n\r\n"
        ));
        stream.write_all(req.as_bytes()).await.expect("write head");
        if let Some(b) = body {
            stream.write_all(b).await.expect("write body");
        }
        stream.flush().await.expect("flush");
        let mut raw = Vec::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            stream.read_to_end(&mut raw),
        )
        .await
        .expect("response timed out");
        parse_http(&raw)
    }

    async fn json_send(
        &self,
        method: &str,
        path: &str,
        body: &str,
    ) -> (u16, Vec<(String, String)>, Vec<u8>) {
        self.send(
            method,
            path,
            &[("content-type", "application/json")],
            Some(body.as_bytes()),
        )
        .await
    }

    async fn make_bucket(&self, name: &str) {
        let (st, _, body) = self
            .json_send("POST", "/api/buckets", &format!(r#"{{"name":"{name}"}}"#))
            .await;
        assert!(
            (200..300).contains(&st),
            "create bucket: {st} {}",
            String::from_utf8_lossy(&body)
        );
    }

    /// Upload `data` under `key` through the console endpoint.
    async fn upload(&self, bucket: &str, key: &str, data: &[u8], ctype: &str) -> (u16, Vec<u8>) {
        let path = format!("/api/buckets/{bucket}/objects?key={}", enc(key));
        let (st, _, body) = self
            .send("PUT", &path, &[("content-type", ctype)], Some(data))
            .await;
        (st, body)
    }
}

impl Drop for Ctx {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

fn enc(s: &str) -> String {
    use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
    const SET: &AsciiSet = &CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'<')
        .add(b'>')
        .add(b'?')
        .add(b'`')
        .add(b'{')
        .add(b'}')
        .add(b'/')
        .add(b'%')
        .add(b'&')
        .add(b'=');
    utf8_percent_encode(s, SET).to_string()
}

fn parse_http(raw: &[u8]) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("header terminator");
    let head = String::from_utf8_lossy(&raw[..split]);
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let payload = &raw[split + 4..];
    let chunked = headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.contains("chunked"));
    (
        status,
        headers,
        if chunked {
            dechunk(payload)
        } else {
            payload.to_vec()
        },
    )
}

fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(pos) = data.windows(2).position(|w| w == b"\r\n") {
        let size = usize::from_str_radix(
            String::from_utf8_lossy(&data[..pos])
                .split(';')
                .next()
                .unwrap_or("0")
                .trim(),
            16,
        )
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Every common file type must round-trip byte-for-byte with its type intact.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uploads_common_file_types_intact() {
    let ctx = Ctx::start().await;
    ctx.make_bucket("types").await;

    // A 1x1 PNG, a minimal PDF, SVG, JSON, plain text and a random binary blob.
    let png: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00,
    ];
    let pdf: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog >>\nendobj\ntrailer\n<<>>\n%%EOF\n";
    let svg: &[u8] = br#"<?xml version="1.0"?><svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"><rect width="4" height="4"/></svg>"#;
    let json_text = r#"{"hello":"\u4e16\u754c","n":[1,2,3]}"#;
    let json: &[u8] = json_text.as_bytes();
    let text: &[u8] = "第一行\nsecond line\n".as_bytes();
    let binary: Vec<u8> = (0..=255u8).cycle().take(8192).collect();

    let cases: Vec<(&str, &[u8], &str)> = vec![
        ("img/logo.png", png, "image/png"),
        ("docs/report.pdf", pdf, "application/pdf"),
        ("img/icon.svg", svg, "image/svg+xml"),
        ("data/config.json", json, "application/json"),
        ("notes/readme.txt", text, "text/plain;charset=utf-8"),
        ("blob/random.bin", &binary, "application/octet-stream"),
    ];

    for (key, data, ctype) in &cases {
        let (st, body) = ctx.upload("types", key, data, ctype).await;
        assert_eq!(st, 200, "upload {key}: {}", String::from_utf8_lossy(&body));

        // Fetch it back through the console download endpoint.
        let (st, headers, got) = ctx
            .send(
                "GET",
                &format!("/api/buckets/types/object?key={}", enc(key)),
                &[],
                None,
            )
            .await;
        assert_eq!(st, 200, "download {key}");
        assert_eq!(got, *data, "{key} bytes must round-trip exactly");
        let ct = headers
            .iter()
            .find(|(k, _)| k == "content-type")
            .map(|(_, v)| v.as_str())
            .unwrap_or("");
        assert_eq!(ct, *ctype, "{key} content type");
        // Stored content must never be allowed to execute on the console origin.
        assert!(
            headers.iter().any(|(k, _)| k == "x-content-type-options"),
            "nosniff header missing for {key}"
        );
    }
}

/// A file larger than one 8 MiB part must arrive byte-identical.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_file_uses_multipart_and_arrives_intact() {
    let ctx = Ctx::start().await;
    ctx.make_bucket("big").await;

    // 20 MiB - 3 parts at the default 8 MiB part size.
    let size = 20 * 1024 * 1024;
    let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();

    let init = ctx
        .json_send(
            "POST",
            &format!(
                "/api/buckets/big/uploads?key={}&content_type=video/mp4",
                enc("movie.mp4")
            ),
            "{}",
        )
        .await;
    assert_eq!(init.0, 200, "init: {}", String::from_utf8_lossy(&init.2));
    let init_json: serde_json::Value = serde_json::from_slice(&init.2).expect("init json");
    let upload_id = init_json["upload_id"]
        .as_str()
        .expect("upload_id")
        .to_string();
    let part_bytes = init_json["part_bytes"].as_u64().expect("part_bytes");

    let part = part_bytes as usize;
    let total_parts = data.len().div_ceil(part);
    assert!(
        total_parts >= 3,
        "expected several parts, got {total_parts}"
    );

    let mut parts = Vec::new();
    for n in 0..total_parts {
        let start = n * part;
        let end = (start + part).min(data.len());
        let (st, _, body) = ctx
            .send(
                "PUT",
                &format!("/api/buckets/big/uploads/{upload_id}/parts/{}", n + 1),
                &[],
                Some(&data[start..end]),
            )
            .await;
        assert_eq!(
            st,
            200,
            "part {}: {}",
            n + 1,
            String::from_utf8_lossy(&body)
        );
        let j: serde_json::Value = serde_json::from_slice(&body).expect("part json");
        parts.push(format!(
            r#"{{"part_number":{},"etag":"{}"}}"#,
            n + 1,
            j["etag"].as_str().unwrap_or("")
        ));
    }

    let complete = format!(r#"{{"parts":[{}]}}"#, parts.join(","));
    let (st, _, body) = ctx
        .json_send(
            "POST",
            &format!("/api/buckets/big/uploads/{upload_id}/complete"),
            &complete,
        )
        .await;
    assert_eq!(st, 200, "complete: {}", String::from_utf8_lossy(&body));

    let (st, _, got) = ctx
        .send(
            "GET",
            &format!("/api/buckets/big/object?key={}", enc("movie.mp4")),
            &[],
            None,
        )
        .await;
    assert_eq!(st, 200);
    assert_eq!(got.len(), size, "assembled size");
    assert_eq!(got, data, "assembled bytes must match");
}

/// The configured maximum is enforced on the console upload path.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn size_limit_is_enforced_and_configurable() {
    let ctx = Ctx::start().await;
    ctx.make_bucket("limited").await;

    // Shrink the limit to 1 KiB through the settings API.
    let (st, _, body) = ctx
        .json_send(
            "PATCH",
            "/api/settings/limits",
            r#"{"max_object_bytes":1024}"#,
        )
        .await;
    assert_eq!(st, 200, "set limits: {}", String::from_utf8_lossy(&body));

    // Under the limit succeeds.
    let (st, _) = ctx
        .upload("limited", "small.txt", b"tiny", "text/plain")
        .await;
    assert_eq!(st, 200, "under the limit");

    // Over the limit is rejected.
    let big = vec![b'x'; 4096];
    let (st, body) = ctx.upload("limited", "big.txt", &big, "text/plain").await;
    assert_eq!(
        st,
        413,
        "over the limit must be rejected: {}",
        String::from_utf8_lossy(&body)
    );

    // And the stored limits survive a reload of the running state.
    let persisted = ctx
        .state
        .meta
        .get_json::<Limits>(SETTINGS, Limits::KEY)
        .expect("read")
        .expect("limits row");
    assert_eq!(persisted.max_object_bytes, Some(1024));

    // Clearing the limit restores unlimited uploads.
    let (st, _, _) = ctx
        .json_send(
            "PATCH",
            "/api/settings/limits",
            r#"{"max_object_bytes":null}"#,
        )
        .await;
    assert_eq!(st, 200);
    let (st, _) = ctx.upload("limited", "big2.txt", &big, "text/plain").await;
    assert_eq!(st, 200, "unlimited after clearing");
}

/// Upload, then delete; the bucket counters and listing must follow.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_removes_object_and_updates_counters() {
    let ctx = Ctx::start().await;
    ctx.make_bucket("del").await;

    let payload = b"delete me";
    let (st, _) = ctx.upload("del", "x/y.txt", payload, "text/plain").await;
    assert_eq!(st, 200);

    let (st, _, body) = ctx
        .json_send("GET", "/api/buckets/del/objects?prefix=&delimiter=%2F", "")
        .await;
    assert_eq!(st, 200);
    let listing = String::from_utf8_lossy(&body).to_string();
    assert!(
        listing.contains("y.txt") || listing.contains("x/"),
        "listing: {listing}"
    );

    let (st, _, _) = ctx
        .send(
            "DELETE",
            &format!("/api/buckets/del/objects?key={}", enc("x/y.txt")),
            &[],
            None,
        )
        .await;
    assert_eq!(st, 204, "delete");

    let (st, _, _) = ctx
        .send(
            "GET",
            &format!("/api/buckets/del/object?key={}", enc("x/y.txt")),
            &[],
            None,
        )
        .await;
    assert_eq!(st, 404, "object is gone");

    // Deleting again is idempotent.
    let (st, _, _) = ctx
        .send(
            "DELETE",
            &format!("/api/buckets/del/objects?key={}", enc("x/y.txt")),
            &[],
            None,
        )
        .await;
    assert_eq!(st, 204, "idempotent delete");
}

/// Upload endpoints must refuse an unauthenticated caller.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upload_requires_a_session() {
    let ctx = Ctx::start().await;
    ctx.make_bucket("guarded").await;

    let anon = Ctx {
        addr: ctx.addr,
        cookie: String::new(),
        // Reuse the running server; state/handles below are never touched.
        state: ctx.state.clone(),
        _dir: TempDir::new(),
        shutdown: None,
    };

    let (st, _, _) = anon
        .send(
            "PUT",
            "/api/buckets/guarded/objects?key=a.txt",
            &[("content-type", "text/plain")],
            Some(b"nope"),
        )
        .await;
    assert_eq!(st, 401, "anonymous upload must be rejected");

    let (st, _, _) = anon
        .json_send("POST", "/api/buckets/guarded/uploads?key=b.txt", "{}")
        .await;
    assert_eq!(st, 401, "anonymous multipart init must be rejected");

    let (st, _, _) = anon
        .json_send("PATCH", "/api/settings/limits", r#"{"part_bytes":8388608}"#)
        .await;
    assert_eq!(st, 401, "anonymous limit change must be rejected");
}

/// A presigned URL minted by the console must point at the path the router
/// actually serves S3 from.
///
/// Regression: the signature was computed over `/{bucket}/{key}` while S3 is
/// mounted under `/s3`, so the returned URL fell through to the SPA fallback and
/// answered with the console HTML instead of the object.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn console_presign_url_includes_the_s3_prefix() {
    let ctx = Ctx::start().await;
    ctx.make_bucket("share").await;

    let payload = b"share me";
    let (st, _) = ctx.upload("share", "pic.png", payload, "image/png").await;
    assert_eq!(st, 200);

    // The default prefix is `/s3`; the test harness does not override it.
    let assert_prefix = ctx.state.args.s3_prefix.clone();
    assert_eq!(assert_prefix, "/s3", "test assumes the default prefix");

    // Create an access key with read access, then presign through the console.
    let (st, _, body) = ctx
        .json_send("POST", "/api/keys", r#"{"label":"sharer"}"#)
        .await;
    assert!((200..300).contains(&st), "create key: {st}");
    let key: serde_json::Value = serde_json::from_slice(&body).expect("key json");
    let ak = key["access_key_id"].as_str().expect("ak").to_string();

    let (st, _, _) = ctx
        .json_send(
            "PUT",
            &format!("/api/keys/{ak}/grants"),
            r#"{"grants":[{"bucket":"share","read":true,"write":false}]}"#,
        )
        .await;
    assert_eq!(st, 200, "grant read");

    let (st, _, body) = ctx
        .json_send(
            "POST",
            "/api/buckets/share/objects/presign?key=pic.png",
            &format!(r#"{{"access_key_id":"{ak}","ttl_seconds":300}}"#),
        )
        .await;
    assert_eq!(st, 200, "presign: {}", String::from_utf8_lossy(&body));
    let signed: serde_json::Value = serde_json::from_slice(&body).expect("presign json");
    let url = signed["url"].as_str().expect("url").to_string();

    // The signed path must carry the mount prefix, not the bare bucket path.
    assert!(
        url.contains("/s3/share/pic.png"),
        "presigned URL must be signed for the mounted S3 path, got: {url}"
    );
    assert!(
        !url.contains("//share/pic.png"),
        "presigned URL must not sign the bare bucket path, got: {url}"
    );

    // And the object must actually be fetchable through that URL.
    let path_and_query = url
        .split_once("://")
        .and_then(|(_, rest)| rest.find('/').map(|i| &rest[i..]))
        .expect("path in url");
    let (st, _, got) = ctx.send("GET", path_and_query, &[], None).await;
    assert_eq!(
        st,
        200,
        "presigned GET must serve the object, not the SPA; url={url} body={}",
        String::from_utf8_lossy(&got)
    );
    assert_eq!(got, payload, "presigned GET must return the object bytes");
}

// ---------------------------------------------------------------------------
// Public access: short links, permanent URLs, custom domains
// ---------------------------------------------------------------------------

/// A `public-read` bucket is shareable at the short path, and the console hands
/// back a permanent link instead of an expiring signature.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_bucket_serves_short_links_and_permanent_urls() {
    let ctx = Ctx::start().await;
    ctx.make_bucket("cdn").await;

    let payload = b"public bytes";
    let (st, _) = ctx.upload("cdn", "img/pic.png", payload, "image/png").await;
    assert_eq!(st, 200);

    // Private by default: the short path must NOT serve the object.
    //
    // It falls through to the SPA and answers 200 with the console HTML, which
    // is deliberate: an anonymous caller cannot tell a private bucket from an
    // unknown path, so bucket names are not enumerable.
    let (_, _, body) = ctx.send("GET", "/cdn/img/pic.png", &[], None).await;
    assert!(
        !body.starts_with(payload),
        "a private bucket must not leak object bytes on the short path"
    );

    // Flip to public-read.
    let (st, _, body) = ctx
        .json_send(
            "PATCH",
            "/api/buckets/cdn",
            r#"{"visibility":"public-read"}"#,
        )
        .await;
    assert_eq!(st, 200, "set public: {}", String::from_utf8_lossy(&body));

    // Now the short link works, with no credentials and no signature.
    let (st, headers, got) = ctx.send("GET", "/cdn/img/pic.png", &[], None).await;
    assert_eq!(st, 200, "short link must serve a public object");
    assert_eq!(got, payload, "short link returns the object bytes");
    let ct = headers
        .iter()
        .find(|(k, _)| k == "content-type")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    assert_eq!(ct, "image/png");

    // The console reports the permanent prefix for the bucket.
    let (st, _, body) = ctx.json_send("GET", "/api/buckets", "").await;
    assert_eq!(st, 200);
    let listing: serde_json::Value = serde_json::from_slice(&body).expect("buckets json");
    let bucket = listing["buckets"]
        .as_array()
        .and_then(|a| a.iter().find(|b| b["name"] == "cdn"))
        .expect("cdn bucket in listing");
    let prefix = bucket["public_url"].as_str().unwrap_or("");
    assert!(
        prefix.ends_with("/cdn/"),
        "public_url must point at the short prefix, got: {prefix}"
    );

    // A presigned GET on a public bucket is answered with the permanent link.
    let (st, _, body) = ctx
        .json_send("POST", "/api/keys", r#"{"label":"k1"}"#)
        .await;
    assert!((200..300).contains(&st));
    let key: serde_json::Value = serde_json::from_slice(&body).expect("key json");
    let ak = key["access_key_id"].as_str().expect("ak").to_string();
    // The key still needs read permission: making a bucket public does not
    // authorize every key to act on its behalf.
    let (st, _, _) = ctx
        .json_send(
            "PUT",
            &format!("/api/keys/{ak}/grants"),
            r#"{"grants":[{"bucket":"cdn","read":true,"write":false}]}"#,
        )
        .await;
    assert_eq!(st, 200, "grant read");

    let (st, _, body) = ctx
        .json_send(
            "POST",
            "/api/buckets/cdn/objects/presign?key=img/pic.png",
            &format!(r#"{{"access_key_id":"{ak}","ttl_seconds":300}}"#),
        )
        .await;
    assert_eq!(st, 200, "presign: {}", String::from_utf8_lossy(&body));
    let signed: serde_json::Value = serde_json::from_slice(&body).expect("presign json");
    assert_eq!(
        signed["permanent"].as_bool(),
        Some(true),
        "a public bucket must yield a permanent link: {signed}"
    );
    let url = signed["url"].as_str().expect("url");
    assert!(
        !url.contains("X-Amz-Signature"),
        "permanent link must carry no signature: {url}"
    );
    assert!(url.contains("/cdn/img/pic.png"), "url: {url}");
}

/// A hostname bound to a bucket serves that bucket's objects at its root.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn custom_domain_serves_bucket_at_root() {
    let ctx = Ctx::start().await;
    ctx.make_bucket("assets").await;

    let payload = b"domain bytes";
    let (st, _) = ctx
        .upload("assets", "logo.svg", payload, "image/svg+xml")
        .await;
    assert_eq!(st, 200);

    // Bind a hostname.
    let (st, _, body) = ctx
        .json_send(
            "PATCH",
            "/api/buckets/assets",
            r#"{"visibility":"public-read","custom_domain":"cdn.example.com"}"#,
        )
        .await;
    assert_eq!(st, 200, "bind domain: {}", String::from_utf8_lossy(&body));

    // A request arriving with that Host resolves the whole path as the key,
    // so `/logo.svg` on the domain is object `logo.svg` in the bucket.
    let (st, _, got) = ctx
        .send("GET", "/logo.svg", &[("host", "cdn.example.com")], None)
        .await;
    assert_eq!(
        st,
        200,
        "custom domain must serve the object: {}",
        String::from_utf8_lossy(&got)
    );
    assert_eq!(got, payload);

    // A port in the Host header must not break matching.
    let (st, _, _) = ctx
        .send("GET", "/logo.svg", &[("host", "cdn.example.com:443")], None)
        .await;
    assert_eq!(st, 200, "Host with a port must still match");

    // An unbound hostname falls through to the SPA, not to the object. It
    // answers 200 with the console shell by design, so assert on the bytes.
    let (_, _, body) = ctx
        .send("GET", "/logo.svg", &[("host", "other.example.com")], None)
        .await;
    assert!(
        !body.starts_with(payload),
        "unbound host must not leak object bytes"
    );

    // A second bucket cannot claim the same hostname.
    ctx.make_bucket("other").await;
    let (st, _, body) = ctx
        .json_send(
            "PATCH",
            "/api/buckets/other",
            r#"{"custom_domain":"cdn.example.com"}"#,
        )
        .await;
    assert_eq!(
        st,
        409,
        "duplicate domain must be refused: {}",
        String::from_utf8_lossy(&body)
    );

    // Malformed hostnames are rejected.
    for bad in [
        r#"{"custom_domain":"ftp://x.com"}"#,
        r#"{"custom_domain":"localhost"}"#,
    ] {
        let (st, _, _) = ctx.json_send("PATCH", "/api/buckets/other", bad).await;
        assert_eq!(st, 400, "must reject {bad}");
    }
}
