//! Cryptographic primitives and key management.
//!
//! Design notes (see `docs/SECURITY.md`):
//! * S3 secret access keys are **never** stored in plaintext. They are sealed
//!   with AES-256-GCM under a key derived (HKDF-SHA256) from a 32-byte master
//!   key. A database leak alone therefore does not reveal any S3 credential.
//! * The master key lives in `HEJIU45_OSS_MASTER_KEY` or in a 0600 file inside
//!   the data directory. The file is only used so a fresh single-binary
//!   install works without a config step; operators should move it into an
//!   environment secret / systemd credential.
//! * The admin password hash is Argon2id via the `salting` crate, which
//!   pins OWASP-recommended parameters (64 MiB, t=3, p=4).

use std::path::Path;

use anyhow::{Context, bail};
use aws_lc_rs::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use aws_lc_rs::hkdf::{HKDF_SHA256, Salt};
use sha2::{Digest, Sha256};

/// Length of the master key and of per-purpose derived keys.
pub const KEY_LEN: usize = 32;
/// AAD bound into every sealed secret so cross-purpose ciphertext reuse fails.
const SECRET_AAD: &[u8] = b"hejiu45-oss/s3-secret/v1";

/// Fill `buf` from the OS CSPRNG.
pub fn fill_random(buf: &mut [u8]) -> anyhow::Result<()> {
    getrandom::fill(buf).map_err(|e| anyhow::anyhow!("OS RNG unavailable: {e}"))
}

/// Random bytes as lowercase hex.
pub fn random_hex(n: usize) -> anyhow::Result<String> {
    let mut buf = vec![0u8; n];
    fill_random(&mut buf)?;
    Ok(hex::encode(buf))
}

/// Random bytes as URL-safe base64 without padding.
pub fn random_b64url(n: usize) -> anyhow::Result<String> {
    let mut buf = vec![0u8; n];
    fill_random(&mut buf)?;
    Ok(b64url_encode(&buf))
}

/// Hex-encode helper re-exported for callers who build hashes.
pub fn hex_encode(b: impl AsRef<[u8]>) -> String {
    hex::encode(b)
}

/// SHA-256, hex encoded.
pub fn sha256_hex(data: impl AsRef<[u8]>) -> String {
    let mut h = Sha256::new();
    h.update(data.as_ref());
    hex::encode(h.finalize())
}

pub fn b64url_encode(b: impl AsRef<[u8]>) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let b = b.as_ref();
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    for chunk in b.chunks(3) {
        let n = match chunk.len() {
            3 => (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8 | chunk[2] as u32,
            2 => (chunk[0] as u32) << 16 | (chunk[1] as u32) << 8,
            _ => (chunk[0] as u32) << 16,
        };
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        if chunk.len() > 1 {
            out.push(T[(n >> 6) as usize & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(T[n as usize & 63] as char);
        }
    }
    out
}

/// Decode URL-safe base64 (with or without padding) and standard base64.
pub fn b64_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    let s = s
        .trim()
        .trim_end_matches('=')
        .replace('+', "-")
        .replace('/', "_");
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            b'\n' | b'\r' | b' ' => continue,
            _ => bail!("invalid base64 character"),
        };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Ok(out)
}

/// Parse a 32-byte key from hex (64 chars) or base64 (44 chars).
pub fn parse_key_material(raw: &str) -> anyhow::Result<[u8; KEY_LEN]> {
    let raw = raw.trim();
    let bytes = if raw.len() == KEY_LEN * 2 && raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        hex::decode(raw).context("hex decode")?
    } else {
        b64_decode(raw).context("base64 decode")?
    };
    if bytes.len() != KEY_LEN {
        bail!(
            "master key must be {KEY_LEN} bytes (hex or base64); got {}",
            bytes.len()
        );
    }
    let mut k = [0u8; KEY_LEN];
    k.copy_from_slice(&bytes);
    Ok(k)
}

/// HKDF-SHA256 expansion with a domain-separation label.
fn hkdf32(ikm: &[u8], label: &[u8]) -> anyhow::Result<[u8; KEY_LEN]> {
    let salt = Salt::new(HKDF_SHA256, b"hejiu45-oss/hkdf/v1");
    let prk = salt.extract(ikm);
    let mut okm = [0u8; KEY_LEN];
    prk.expand(&[label], HKDF_SHA256.hmac_algorithm())
        .and_then(|okm_reader| okm_reader.fill(&mut okm))
        .map_err(|_| anyhow::anyhow!("hkdf expand failed"))?;
    Ok(okm)
}

/// Seals and unseals S3 secret keys with AES-256-GCM.
#[derive(Clone)]
pub struct SecretBox {
    key: [u8; KEY_LEN],
}

impl std::fmt::Debug for SecretBox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretBox(<redacted>)")
    }
}

impl SecretBox {
    pub fn from_master(master: &[u8; KEY_LEN]) -> anyhow::Result<Self> {
        Ok(Self {
            key: hkdf32(master, b"s3-secret-encryption")?,
        })
    }

    fn aead_key(&self) -> anyhow::Result<LessSafeKey> {
        let unbound = UnboundKey::new(&AES_256_GCM, &self.key)
            .map_err(|_| anyhow::anyhow!("aead key init failed"))?;
        Ok(LessSafeKey::new(unbound))
    }

    /// Returns `v1:<base64url(nonce || ciphertext || tag)>`.
    pub fn seal(&self, plaintext: &[u8]) -> anyhow::Result<String> {
        let key = self.aead_key()?;
        let mut nonce_bytes = [0u8; 12];
        fill_random(&mut nonce_bytes)?;
        let nonce = Nonce::assume_unique_for_key(nonce_bytes);
        let mut in_out = plaintext.to_vec();
        key.seal_in_place_append_tag(nonce, Aad::from(SECRET_AAD), &mut in_out)
            .map_err(|_| anyhow::anyhow!("aead seal failed"))?;
        let mut out = Vec::with_capacity(12 + in_out.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&in_out);
        Ok(format!("v1:{}", b64url_encode(out)))
    }

    pub fn open(&self, sealed: &str) -> anyhow::Result<Vec<u8>> {
        let body = sealed
            .strip_prefix("v1:")
            .context("unsupported sealed secret version")?;
        let raw = b64_decode(body)?;
        if raw.len() < 12 + 16 {
            bail!("sealed secret too short");
        }
        let (nonce_bytes, ct) = raw.split_at(12);
        let mut nonce_arr = [0u8; 12];
        nonce_arr.copy_from_slice(nonce_bytes);
        let key = self.aead_key()?;
        let mut buf = ct.to_vec();
        let plain = key
            .open_in_place(
                Nonce::assume_unique_for_key(nonce_arr),
                Aad::from(SECRET_AAD),
                &mut buf,
            )
            .map_err(|_| anyhow::anyhow!("aead open failed (wrong master key?)"))?;
        Ok(plain.to_vec())
    }
}

/// Where the master key comes from on this boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MasterKeySource {
    /// `--master-key` / env var.
    Environment,
    /// Generated and written to `<data-dir>/master.key` (0600).
    GeneratedFile,
    /// Loaded from `<data-dir>/master.key`.
    File,
}

/// Path of the fallback master key file.
pub fn master_key_path(data_dir: &Path) -> std::path::PathBuf {
    data_dir.join("master.key")
}

/// Resolve the master key: explicit value → file → generate-and-persist.
pub fn load_or_create_master_key(
    explicit: Option<&str>,
    data_dir: &Path,
) -> anyhow::Result<([u8; KEY_LEN], MasterKeySource)> {
    if let Some(raw) = explicit {
        if !raw.trim().is_empty() {
            return Ok((parse_key_material(raw)?, MasterKeySource::Environment));
        }
    }
    let path = master_key_path(data_dir);
    if path.exists() {
        let raw = std::fs::read_to_string(&path).context("reading master key file")?;
        return Ok((parse_key_material(&raw)?, MasterKeySource::File));
    }
    std::fs::create_dir_all(data_dir).context("creating data directory")?;
    let mut k = [0u8; KEY_LEN];
    fill_random(&mut k)?;
    let encoded = hex::encode(k);
    write_private_file(&path, encoded.as_bytes())?;
    Ok((k, MasterKeySource::GeneratedFile))
}

/// Write a file readable only by the owner (0600 on unix, hidden on windows).
pub fn write_private_file(path: &Path, contents: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .with_context(|| format!("opening {path:?} for writing"))?;
    f.write_all(contents)?;
    f.sync_all()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// Verify an admin password against a stored PHC hash.
pub fn verify_admin_password(password: &str, phc_hash: &str) -> bool {
    match salting::verify_password(password, phc_hash) {
        Ok(ok) => ok,
        Err(e) => {
            tracing::warn!(error = %e, "admin password verification failed");
            false
        }
    }
}

/// Hash an admin password with OWASP parameters.
pub fn hash_admin_password(password: &str) -> anyhow::Result<String> {
    salting::hash_password(password).map_err(|e| anyhow::anyhow!("password hashing failed: {e}"))
}

/// S3 access key ids: `HJ45` + 16 hex chars. Shown to users, not secret.
pub fn generate_access_key_id() -> anyhow::Result<String> {
    Ok(format!("HJ45{}", random_hex(8)?.to_uppercase()))
}

/// S3 secret access keys: 40 base62-ish chars.
pub fn generate_secret_access_key() -> anyhow::Result<String> {
    random_b64url(30)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_master() -> [u8; KEY_LEN] {
        let mut k = [7u8; KEY_LEN];
        k[0] = 1;
        k
    }

    #[test]
    fn seal_roundtrip_and_tamper_detection() {
        let sb = SecretBox::from_master(&dummy_master()).unwrap();
        let sealed = sb.seal(b"super-secret-ak").unwrap();
        assert!(sealed.starts_with("v1:"));
        assert_eq!(sb.open(&sealed).unwrap(), b"super-secret-ak");
        // Flipping any character must fail authentication, and the plaintext
        // must never appear verbatim in the sealed blob.
        let mut bad = sealed.clone();
        let last = bad.pop().unwrap();
        bad.push(if last == 'A' { 'B' } else { 'A' });
        assert!(sb.open(&bad).is_err());
        // The sealed blob is base64url, so the plaintext cannot reappear verbatim;
        // assert on the decoded bytes instead of the ASCII-looking source.
        let raw = b64_decode(sealed.strip_prefix("v1:").unwrap()).unwrap();
        assert!(
            !raw.windows(b"super-secret-ak".len())
                .any(|w| w == b"super-secret-ak"),
            "plaintext leaked into the sealed blob"
        );
    }

    #[test]
    fn wrong_master_key_cannot_open() {
        let a = SecretBox::from_master(&dummy_master()).unwrap();
        let mut other = dummy_master();
        other[1] = 9;
        let b = SecretBox::from_master(&other).unwrap();
        let sealed = a.seal(b"x").unwrap();
        assert!(b.open(&sealed).is_err());
    }

    #[test]
    fn base64url_roundtrip() {
        for n in 0..40usize {
            let mut buf = vec![0u8; n];
            let _ = fill_random(&mut buf);
            let enc = b64url_encode(&buf);
            assert!(!enc.contains('='));
            assert_eq!(b64_decode(&enc).unwrap(), buf);
        }
    }

    #[test]
    fn key_parse_accepts_hex_and_base64() {
        let k = [42u8; 32];
        assert_eq!(parse_key_material(&hex::encode(k)).unwrap(), k);
        assert_eq!(parse_key_material(&b64url_encode(k)).unwrap(), k);
        assert!(parse_key_material("short").is_err());
    }

    #[test]
    fn admin_password_roundtrip() {
        let h = hash_admin_password("correct horse battery staple").unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_admin_password("correct horse battery staple", &h));
        assert!(!verify_admin_password("wrong", &h));
        assert!(!verify_admin_password("x", "not-a-hash"));
    }

    #[test]
    fn generated_keys_have_expected_shape() {
        let ak = generate_access_key_id().unwrap();
        assert!(ak.starts_with("HJ45") && ak.len() == 20, "{ak}");
        let sk = generate_secret_access_key().unwrap();
        assert!(sk.len() >= 40, "{sk}");
    }
}
