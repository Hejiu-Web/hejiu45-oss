//! Object body storage on the local filesystem.
//!
//! Layout under `--data-dir`:
//!
//! ```text
//! data/
//! ├── meta.redb          # metadata (see meta.rs)
//! ├── master.key        # generated master key when none is provided
//! ├── objects/
//! │   └── <bucket>/<sha256(key) prefix 2>/<sha256(key) rest>
//! ├── multipart/<upload id>/<part number>
//! └── tmp/              # in-flight uploads, same filesystem as objects/
//! ```
//!
//! Two deliberate choices:
//!
//! * **Bodies are addressed by the SHA-256 of the key, not by the key itself.**
//!   S3 keys are arbitrary UTF-8 and may contain `..`, `/`, backslashes, NUL or
//!   Windows reserved names. Hashing removes that entire class of path
//!   traversal and filesystem-name bugs in one stroke. Keys in a bucket are
//!   still listed from the metadata index, so nothing is lost by it.
//! * **Writes go to `tmp/` then `rename` into place.** `tmp/` shares a mount
//!   with `objects/`, so the rename is atomic on POSIX. On Windows, `rename`
//!   fails when the destination exists, so [`atomic_replace`] falls back to a
//!   remove-then-rename under a per-key lock (documented in the design notes).

use std::collections::HashMap;
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, bail};
use parking_lot::Mutex;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter};

use crate::crypto::sha256_hex;
use crate::error::S3Error;

/// Result of a completed streaming write.
#[derive(Debug, Clone)]
pub struct StoredObject {
    /// Number of bytes received.
    pub size: u64,
    /// Hex SHA-256 of the body.
    pub sha256: String,
    /// Hex MD5 of the body (S3 `ETag` for simple uploads).
    pub md5_hex: String,
    /// The temporary path the body was written to; call [`Data::commit`] to
    /// move it into place.
    pub staged: PathBuf,
    /// MIME type guessed from the first bytes (`application/octet-stream`
    /// when unknown). Used only when the client sent no `Content-Type`.
    pub sniffed_content_type: &'static str,
}

/// Magic-number sniffing for the handful of types where it matters most.
///
/// S3 itself does not sniff, but clients that omit `Content-Type`
/// (`curl -T`, some SDKs) otherwise produce objects served as
/// `application/octet-stream`. Only unambiguous signatures are detected:
/// text is never sniffed to `text/html`, because that would turn a bucket
/// into an XSS vector.
pub fn sniff_content_type(head: &[u8]) -> &'static str {
    const RIFF: &[u8] = b"RIFF";
    let riff_sub = |tag: &[u8]| head.starts_with(RIFF) && head.len() >= 12 && &head[8..12] == tag;
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return "image/png";
    }
    if head.len() > 2 && head[0] == 0xFF && head[1] == 0xD8 && head[2] == 0xFF {
        return "image/jpeg";
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return "image/gif";
    }
    if riff_sub(b"WEBP") {
        return "image/webp";
    }
    if riff_sub(b"WAVE") {
        return "audio/wav";
    }
    if riff_sub(b"AVI ") {
        return "video/x-msvideo";
    }
    if head.starts_with(b"OggS") {
        return "audio/ogg";
    }
    if head.starts_with(b"ID3") {
        return "audio/mpeg";
    }
    if head.starts_with(b"fLaC") {
        return "audio/flac";
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        return "video/mp4";
    }
    if head.starts_with(b"PK\x03\x04") {
        return "application/zip";
    }
    if head.starts_with(&[0x1f, 0x8b]) {
        return "application/gzip";
    }
    if head.starts_with(b"7z\xbc\xaf\x27\x1c") {
        return "application/x-7z-compressed";
    }
    if head.starts_with(b"Rar!\x1a\x07") {
        return "application/vnd.rar";
    }
    if head.starts_with(b"%PDF-") {
        return "application/pdf";
    }
    if head.starts_with(b"BM") {
        return "image/bmp";
    }
    if looks_like_svg(head) {
        return "image/svg+xml";
    }
    "application/octet-stream"
}

fn looks_like_svg(head: &[u8]) -> bool {
    let s = String::from_utf8_lossy(head).to_ascii_lowercase();
    let t = s.trim_start().trim_start_matches('\u{feff}');
    (t.starts_with("<?xml") || t.starts_with("<svg")) && s.contains("<svg")
}

/// Filesystem-backed data engine.
#[derive(Clone, Debug)]
pub struct Data {
    root: PathBuf,
    tmp_dir: PathBuf,
    objects_dir: PathBuf,
    multipart_dir: PathBuf,
    /// Serialises remove+rename fallbacks per destination on Windows.
    replace_locks: Arc<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>>,
}

impl Data {
    pub fn open(root: &Path) -> anyhow::Result<Self> {
        let root = root.to_path_buf();
        let objects_dir = root.join("objects");
        let multipart_dir = root.join("multipart");
        let tmp_dir = root.join("tmp");
        for d in [&root, &objects_dir, &multipart_dir, &tmp_dir] {
            std::fs::create_dir_all(d).with_context(|| format!("creating {d:?}"))?;
        }
        Ok(Self {
            root,
            tmp_dir,
            objects_dir,
            multipart_dir,
            replace_locks: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Deterministic body path for `(bucket, key)`.
    pub fn object_path(&self, bucket: &str, key: &str) -> PathBuf {
        let h = sha256_hex(format!("{bucket}\u{0}{key}"));
        self.objects_dir.join(bucket).join(&h[..2]).join(&h[2..])
    }

    pub fn multipart_part_path(&self, upload_id: &str, part: u32) -> PathBuf {
        self.multipart_dir
            .join(upload_id)
            .join(format!("{part:05}"))
    }

    pub fn multipart_dir(&self, upload_id: &str) -> PathBuf {
        self.multipart_dir.join(upload_id)
    }

    /// A fresh temporary file name (not yet created).
    pub fn new_tmp_path(&self) -> PathBuf {
        self.tmp_dir
            .join(format!("up-{}", uuid::Uuid::new_v4().simple()))
    }

    /// Stream `reader` into a new temp file, hashing as we go.
    ///
    /// At most `limit` bytes are accepted; exceeding it fails with
    /// `EntityTooLarge` and the partial temp file is removed. The first 512
    /// bytes are sniffed so callers can default `Content-Type` without
    /// buffering the upload.
    ///
    /// Security note: SigV4 signs a *declared* `x-amz-content-sha256`, not the
    /// body. Callers that receive that header must use
    /// [`Data::stage_verified`] so the bytes are bound to the signature.
    pub async fn stage<R>(&self, reader: R, limit: Option<u64>) -> Result<StoredObject, S3Error>
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        self.stage_impl(reader, limit, None).await
    }

    /// Like [`Data::stage`], but aborts as soon as the streamed SHA-256 differs
    /// from `expected_sha256`.
    pub async fn stage_verified<R>(
        &self,
        reader: R,
        limit: Option<u64>,
        expected_sha256: &str,
    ) -> Result<StoredObject, S3Error>
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        self.stage_impl(reader, limit, Some(expected_sha256)).await
    }

    async fn stage_impl<R>(
        &self,
        mut reader: R,
        limit: Option<u64>,
        expected_sha256: Option<&str>,
    ) -> Result<StoredObject, S3Error>
    where
        R: tokio::io::AsyncRead + Unpin,
    {
        let staged = self.new_tmp_path();
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staged)
            .await
            .map_err(S3Error::internal)?;
        let mut w = BufWriter::with_capacity(256 * 1024, file);
        let mut size = 0u64;
        let mut head = Vec::with_capacity(512);
        let mut live = <sha2::Sha256 as sha2::Digest>::new();
        let mut buf = vec![0u8; 128 * 1024];
        loop {
            let n = match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => {
                    drop(w);
                    let _ = tokio::fs::remove_file(&staged).await;
                    return Err(S3Error::internal(e));
                }
            };
            size += n as u64;
            if let Some(max) = limit {
                if size > max {
                    drop(w);
                    let _ = tokio::fs::remove_file(&staged).await;
                    return Err(S3Error::too_large());
                }
            }
            if head.len() < 512 {
                let take = (512 - head.len()).min(n);
                head.extend_from_slice(&buf[..take]);
            }
            if expected_sha256.is_some() {
                sha2::Digest::update(&mut live, &buf[..n]);
            }
            if let Err(e) = w.write_all(&buf[..n]).await {
                drop(w);
                let _ = tokio::fs::remove_file(&staged).await;
                return Err(S3Error::internal(e));
            }
        }
        // Compare the *complete* body hash: the declared `x-amz-content-sha256`
        // covers the whole payload, so a per-chunk comparison can only ever
        // match a single-chunk upload.
        if let Some(expected) = expected_sha256 {
            let digest = hex::encode(sha2::Digest::finalize(live));
            if digest != expected {
                drop(w);
                let _ = tokio::fs::remove_file(&staged).await;
                return Err(S3Error::new(
                    axum::http::StatusCode::BAD_REQUEST,
                    "XAmzContentSHA256Mismatch",
                    "The provided 'x-amz-content-sha256' header does not match what was computed.",
                ));
            }
        }
        if let Err(e) = w.flush().await {
            let _ = tokio::fs::remove_file(&staged).await;
            return Err(S3Error::internal(e));
        }
        let file = w.into_inner();
        if let Err(e) = file.sync_all().await {
            let _ = tokio::fs::remove_file(&staged).await;
            return Err(S3Error::internal(e));
        }
        drop(file);
        let (sha, md5) = hash_file(&staged).await.map_err(S3Error::internal)?;
        let sniffed_content_type = sniff_content_type(&head);
        Ok(StoredObject {
            size,
            sha256: sha,
            md5_hex: md5,
            staged,
            sniffed_content_type,
        })
    }

    /// Move a staged body to its final location, replacing any previous body.
    pub async fn commit(&self, staged: &Path, bucket: &str, key: &str) -> anyhow::Result<()> {
        let dest = self.object_path(bucket, key);
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        atomic_replace_with_lock(staged, &dest, self.lock_for(&dest)).await
    }

    /// Open an object body for reading.
    pub async fn open_read(&self, bucket: &str, key: &str) -> Result<File, std::io::Error> {
        File::open(self.object_path(bucket, key)).await
    }

    /// Open for reading and seek to `start`, reading at most `len` bytes.
    pub async fn open_range(
        &self,
        bucket: &str,
        key: &str,
        start: u64,
        len: u64,
    ) -> Result<tokio::io::Take<File>, std::io::Error> {
        let mut f = File::open(self.object_path(bucket, key)).await?;
        if start > 0 {
            f.seek(SeekFrom::Start(start)).await?;
        }
        Ok(f.take(len))
    }

    /// Remove a body. Missing files are not an error (idempotent deletes).
    pub async fn remove_object(&self, bucket: &str, key: &str) -> std::io::Result<bool> {
        let p = self.object_path(bucket, key);
        match tokio::fs::remove_file(&p).await {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Remove all bodies of a bucket (only valid once metadata is gone).
    pub async fn remove_bucket_dir(&self, bucket: &str) -> std::io::Result<()> {
        let p = self.objects_dir.join(bucket);
        match tokio::fs::remove_dir_all(&p).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Remove leftover temp files from a previous crash.
    pub async fn sweep_tmp(&self) -> std::io::Result<usize> {
        let mut n = 0;
        let mut rd = tokio::fs::read_dir(&self.tmp_dir).await?;
        while let Some(entry) = rd.next_entry().await? {
            if tokio::fs::remove_file(entry.path()).await.is_ok() {
                n += 1;
            }
        }
        Ok(n)
    }

    fn lock_for(&self, dest: &Path) -> Arc<tokio::sync::Mutex<()>> {
        let mut m = self.replace_locks.lock();
        m.entry(dest.to_path_buf())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }
}

/// Atomic replace used for bodies, multipart assembly and metadata snapshots.
///
/// * POSIX: `rename` replaces atomically.
/// * Windows: `rename` errors with `AlreadyExists` when the destination is
///   present, so we remove then rename. The caller must hold the per-path lock
///   ([`Data::lock_for`]) around both calls, otherwise a concurrent reader can
///   observe a momentary gap. This is the exact trap called out in the design
///   notes; the fallback is isolated here and in [`atomic_replace_with_lock`].
pub async fn atomic_replace(src: &Path, dest: &Path) -> anyhow::Result<()> {
    match tokio::fs::rename(src, dest).await {
        Ok(()) => return Ok(()),
        Err(e) if cfg!(unix) => {
            return Err(e).with_context(|| format!("rename {src:?} -> {dest:?}"));
        }
        Err(e) if cfg!(windows) && e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).with_context(|| format!("rename {src:?} -> {dest:?}")),
    }
    // Windows fallback.
    match tokio::fs::remove_file(dest).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).with_context(|| format!("removing old {dest:?}")),
    }
    tokio::fs::rename(src, dest)
        .await
        .with_context(|| format!("replacing {dest:?}"))
}

/// [`atomic_replace`] with a caller-held lock (see docs above).
pub async fn atomic_replace_with_lock(
    src: &Path,
    dest: &Path,
    lock: Arc<tokio::sync::Mutex<()>>,
) -> anyhow::Result<()> {
    let _g = lock.lock().await;
    atomic_replace(src, dest).await
}

/// Hex SHA-256 and MD5 of a file, streamed.
pub async fn hash_file(path: &Path) -> anyhow::Result<(String, String)> {
    use md5::Digest as Md5Digest;
    use sha2::Digest as Sha256Digest;
    let mut f = File::open(path).await?;
    let mut sha = <sha2::Sha256 as Sha256Digest>::new();
    let mut md5 = <md5::Md5 as Md5Digest>::new();
    let mut buf = vec![0u8; 128 * 1024];
    loop {
        let n = f.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
        md5.update(&buf[..n]);
    }
    Ok((hex::encode(sha.finalize()), hex::encode(md5.finalize())))
}

// ---------------------------------------------------------------------------
// MD5 for S3 `ETag` compatibility only.
//
// Integrity/identity use SHA-256; MD5 exists because S3 clients compare
// `ETag` against the MD5 of the body they uploaded.
// ---------------------------------------------------------------------------

/// Convenience: MD5 of a byte slice, hex encoded.
pub fn md5_hex(data: &[u8]) -> String {
    use md5::Digest as _;
    let mut h = md5::Md5::new();
    h.update(data);
    hex::encode(h.finalize())
}

/// Reject bucket names that could confuse routing or the filesystem.
pub fn validate_bucket_name(name: &str) -> Result<(), S3Error> {
    let ok_len = (3..=63).contains(&name.len());
    let ok_chars = name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.');
    let ok_edges = !name.starts_with('-')
        && !name.starts_with('.')
        && !name.ends_with('-')
        && !name.ends_with('.');
    let ok_ip = name.parse::<std::net::IpAddr>().is_err();
    let ok_dots = !name.contains("..");
    if ok_len && ok_chars && ok_edges && ok_ip && ok_dots {
        Ok(())
    } else {
        Err(S3Error::invalid_bucket_name(name))
    }
}

/// Validate an object key. S3 keys are nearly arbitrary; we only reject the
/// NUL byte (cannot round-trip through the metadata index) and cap the length
/// at the S3 maximum of 1024 bytes.
pub fn validate_object_key(key: &str) -> Result<(), S3Error> {
    if key.is_empty() {
        return Err(S3Error::invalid_request("Object key must not be empty."));
    }
    if key.len() > 1024 {
        return Err(S3Error::invalid_request("Object key exceeds 1024 bytes."));
    }
    if key.contains('\u{0}') {
        return Err(S3Error::invalid_request("Object key must not contain NUL."));
    }
    Ok(())
}

/// True when `path` stays inside `root` after canonicalisation attempts.
/// Defence in depth: body paths are hashed so this should never trigger.
pub fn path_is_inside(root: &Path, candidate: &Path) -> bool {
    let norm = |p: &Path| {
        p.components().fold(PathBuf::new(), |mut acc, c| {
            match c {
                std::path::Component::ParentDir => {
                    acc.pop();
                }
                std::path::Component::CurDir => {}
                other => acc.push(other.as_os_str()),
            }
            acc
        })
    };
    norm(candidate).starts_with(norm(root))
}

/// Re-export so tests can build a bail error easily.
#[allow(dead_code)]
fn _bail_unused() -> anyhow::Result<()> {
    bail!("unused")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    fn tmpdir() -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("hejiu45-data-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn md5_matches_rfc1321_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"a"), "0cc175b9c0f1b6a831c399e269772661");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"message digest"),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            md5_hex(b"abcdefghijklmnopqrstuvwxyz"),
            "c3fcd3d76192e4007dfb496cca67e13b"
        );
        // Chunked updates over 1 MiB of 'a' must match the same single-shot
        // implementation (they share the crate, but this pins our call site).
        let big = vec![b'a'; 1024 * 1024];
        assert_eq!(
            md5_hex(&big),
            "7202826a7791073fe2787f0c94603278",
            "1 MiB of 'a'"
        );
    }

    #[test]
    fn hashed_paths_are_traversal_proof() {
        let root = tmpdir();
        let d = Data::open(&root).unwrap();
        for evil in [
            "../../../etc/passwd",
            "..\\..\\windows\\system32",
            "a/\u{0}/b",
            "CON",
            "aux.txt",
            "very/deep/nested/../still.txt",
        ] {
            let p = d.object_path("bucket", evil);
            assert!(path_is_inside(&root, &p), "{evil} escaped: {p:?}");
            assert_eq!(
                p.parent().unwrap().parent().unwrap(),
                root.join("objects").join("bucket"),
                "unexpected layout for {evil}"
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn stage_commit_read_range_and_overwrite() {
        let root = tmpdir();
        let d = Data::open(&root).unwrap();

        let s = d.stage(&b"hello world"[..], None).await.unwrap();
        assert_eq!(s.size, 11);
        assert_eq!(s.md5_hex, md5_hex(b"hello world"));
        d.commit(&s.staged, "bkt", "dir/hello.txt").await.unwrap();

        let mut f = d.open_read("bkt", "dir/hello.txt").await.unwrap();
        let mut buf = String::new();
        f.read_to_string(&mut buf).await.unwrap();
        assert_eq!(buf, "hello world");

        let mut r = d.open_range("bkt", "dir/hello.txt", 6, 5).await.unwrap();
        let mut part = String::new();
        r.read_to_string(&mut part).await.unwrap();
        assert_eq!(part, "world");

        // Overwrite must replace the body (Windows rename trap).
        let s2 = d.stage(&b"bye"[..], None).await.unwrap();
        d.commit(&s2.staged, "bkt", "dir/hello.txt").await.unwrap();
        let mut f = d.open_read("bkt", "dir/hello.txt").await.unwrap();
        let mut buf = String::new();
        f.read_to_string(&mut buf).await.unwrap();
        assert_eq!(buf, "bye");

        assert!(d.remove_object("bkt", "dir/hello.txt").await.unwrap());
        assert!(!d.remove_object("bkt", "dir/hello.txt").await.unwrap());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn stage_enforces_size_limit_and_cleans_up() {
        let root = tmpdir();
        let d = Data::open(&root).unwrap();
        let err = d.stage(&vec![0u8; 4096][..], Some(1024)).await.unwrap_err();
        assert_eq!(err.code, "EntityTooLarge");
        let mut rd = tokio::fs::read_dir(&root.join("tmp")).await.unwrap();
        assert!(rd.next_entry().await.unwrap().is_none(), "temp file leaked");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn content_type_sniffing() {
        assert_eq!(
            sniff_content_type(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]),
            "image/png"
        );
        assert_eq!(sniff_content_type(&[0xFF, 0xD8, 0xFF, 0xE0]), "image/jpeg");
        assert_eq!(sniff_content_type(b"GIF89a..."), "image/gif");
        assert_eq!(sniff_content_type(b"%PDF-1.7"), "application/pdf");
        assert_eq!(sniff_content_type(b"PK\x03\x04rest"), "application/zip");
        assert_eq!(sniff_content_type(b"\x1f\x8b\x08"), "application/gzip");
        assert_eq!(sniff_content_type(b"RIFF\0\0\0\0WEBPVP8 "), "image/webp");
        assert_eq!(sniff_content_type(b"\0\0\0\x18ftypmp42"), "video/mp4");
        assert_eq!(
            sniff_content_type(b"<?xml version=\"1.0\"?><svg xmlns=\"...\">"),
            "image/svg+xml"
        );
        // Text is deliberately not sniffed: an uploaded .html must stay
        // generic so it can never be served as a script-bearing document by
        // content sniffing.
        assert_eq!(
            sniff_content_type(b"<html><body>hi</body></html>"),
            "application/octet-stream"
        );
        assert_eq!(
            sniff_content_type(b"hello world"),
            "application/octet-stream"
        );
        assert_eq!(sniff_content_type(b""), "application/octet-stream");
    }

    #[test]
    fn bucket_name_rules() {
        for good in ["abc", "my-bucket", "a.b.c", "logs-2026"] {
            assert!(validate_bucket_name(good).is_ok(), "{good}");
        }
        for bad in [
            "ab",
            "MyBucket",
            "-lead",
            "trail-",
            "a..b",
            "192.168.1.1",
            "a_b",
        ] {
            assert!(validate_bucket_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn object_key_rules() {
        assert!(validate_object_key("normal/key.txt").is_ok());
        assert!(validate_object_key("").is_err());
        assert!(validate_object_key("a\u{0}b").is_err());
        assert!(validate_object_key(&"k".repeat(1025)).is_err());
    }
}
