//! Metadata engine backed by an embedded redb database.
//!
//! Everything that is not an object body lives in one redb file:
//! buckets, object metadata, access keys, admin users, sessions and the audit
//! trail. redb provides ACID transactions and crash safety without a separate
//! database process, which is what lets this ship as a single binary.
//!
//! All tables are `str`-keyed JSON values. JSON over a schemaful encoding is a
//! deliberate trade: it makes forward-compatible upgrades trivial (unknown
//! fields are ignored, missing fields take defaults) at the cost of a few
//! bytes per record.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, bail};
use redb::{Database, ReadableDatabase, ReadableTable, TableDefinition, TableHandle};
use serde::{Deserialize, Serialize};

/// `bucket name` → [`BucketMeta`]
pub const BUCKETS: TableDefinition<&str, &str> = TableDefinition::new("buckets");
/// `bucket\0key` → [`ObjectMeta`]
pub const OBJECTS: TableDefinition<&str, &str> = TableDefinition::new("objects");
/// `access key id` → [`AccessKeyRecord`] (secret stored sealed)
pub const ACCESS_KEYS: TableDefinition<&str, &str> = TableDefinition::new("access_keys");
/// `username` → [`AdminUser`]
pub const ADMIN_USERS: TableDefinition<&str, &str> = TableDefinition::new("admin_users");
/// `session id` → [`SessionRecord`]
pub const SESSIONS: TableDefinition<&str, &str> = TableDefinition::new("sessions");
/// `timestamp-ulid` → [`AuditEntry`]
pub const AUDIT: TableDefinition<&str, &str> = TableDefinition::new("audit");
/// `upload id` → [`MultipartUpload`]
pub const MULTIPARTS: TableDefinition<&str, &str> = TableDefinition::new("multipart_uploads");

const ALL_TABLES: [&str; 7] = [
    "buckets",
    "objects",
    "access_keys",
    "admin_users",
    "sessions",
    "audit",
    "multipart_uploads",
];

/// Rights a single access key holds on a bucket.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BucketPerms {
    pub read: bool,
    pub write: bool,
}

impl BucketPerms {
    pub const NONE: Self = Self {
        read: false,
        write: false,
    };
    pub const READ: Self = Self {
        read: true,
        write: false,
    };
    pub const WRITE: Self = Self {
        read: false,
        write: true,
    };
    pub const FULL: Self = Self {
        read: true,
        write: true,
    };
    pub fn can_read(&self) -> bool {
        self.read
    }
    pub fn can_write(&self) -> bool {
        self.write
    }
}

/// Bucket record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BucketMeta {
    pub name: String,
    /// RFC3339 creation time.
    pub created_at: String,
    /// Username of the admin who created it.
    pub owner: String,
    /// Per-access-key permissions. An entry missing here means "no access".
    #[serde(default)]
    pub perms: BTreeMap<String, BucketPerms>,
    /// `private` (default) or `public-read` (anonymous GET/HEAD only).
    #[serde(default = "default_visibility")]
    pub visibility: String,
    #[serde(default)]
    pub object_count: u64,
    #[serde(default)]
    pub total_bytes: u64,
}

fn default_visibility() -> String {
    "private".to_string()
}

impl BucketMeta {
    pub fn is_public_read(&self) -> bool {
        self.visibility == "public-read"
    }
}

/// Object metadata. The body lives in the data engine at a key derived from
/// `bucket`/`key`, so renaming a bucket would be a copy operation (we don't
/// support rename for exactly this reason).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ObjectMeta {
    pub key: String,
    pub bucket: String,
    pub size: u64,
    /// Hex SHA-256 of the body.
    #[serde(default)]
    pub etag: String,
    /// MD5, base64, as used by `ETag` in S3 responses (for non-multipart
    /// objects this equals the quoted hex MD5).
    #[serde(default)]
    pub md5_hex: String,
    #[serde(default)]
    pub content_type: String,
    /// `x-amz-meta-*` headers, stored without the prefix.
    #[serde(default)]
    pub user_meta: BTreeMap<String, String>,
    /// `Cache-Control`, `Content-Disposition`, `Content-Encoding`,
    /// `Content-Language`, `Expires` — stored verbatim, values are returned on
    /// GET/HEAD.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// RFC3339.
    pub last_modified: String,
    /// True when the object was produced by a multipart complete.
    #[serde(default)]
    pub multipart: bool,
}

/// Credential record. The secret is sealed by [`crate::crypto::SecretBox`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessKeyRecord {
    pub access_key_id: String,
    /// `v1:<base64url(nonce||ct||tag)>`
    pub secret_sealed: String,
    /// Human label, e.g. "ci-deploy".
    pub label: String,
    /// RFC3339.
    pub created_at: String,
    pub owner: String,
    pub enabled: bool,
    /// RFC3339; absent means no expiry.
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub last_used_at: Option<String>,
}

/// Console administrator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminUser {
    pub username: String,
    pub password_phc: String,
    pub created_at: String,
    /// Bumped on password change / forced logout: sessions created under an
    /// older generation are rejected, which gives instant revocation even if
    /// a session store row is missed.
    #[serde(default)]
    pub session_generation: u64,
}

/// Server-side session row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    pub username: String,
    /// Unix seconds.
    pub expires_at: i64,
    pub generation: u64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub at: i64,
    pub actor: String,
    pub action: String,
    pub target: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub remote_ip: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultipartUpload {
    pub upload_id: String,
    pub bucket: String,
    pub key: String,
    pub initiated_at: i64,
    #[serde(default)]
    pub content_type: String,
    #[serde(default)]
    pub user_meta: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// part number → `etag` (md5 hex of that part).
    #[serde(default)]
    pub parts: BTreeMap<u32, String>,
}

/// The metadata engine handle. Cheap to clone (`Arc` inside).
///
/// The inner `Option` exists so a backup can briefly take the database down:
/// redb holds an exclusive OS file lock on Windows, which rules out copying
/// the file of a live handle. [`Meta::backup_to`] closes, copies and reopens.
#[derive(Clone)]
pub struct Meta {
    db: Arc<parking_lot::Mutex<Option<Database>>>,
    path: Arc<std::path::PathBuf>,
}

impl std::fmt::Debug for Meta {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Meta")
    }
}

pub fn composite_key(bucket: &str, key: &str) -> String {
    format!("{bucket}\u{0}{key}")
}

pub fn split_composite_key(k: &str) -> Option<(&str, &str)> {
    k.split_once('\u{0}')
}

impl Meta {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = Database::create(path)
            .with_context(|| format!("opening metadata database {path:?}"))?;
        // Create every table eagerly so read transactions never fail on a
        // fresh database.
        let tx = db.begin_write()?;
        {
            tx.open_table(BUCKETS)?;
            tx.open_table(OBJECTS)?;
            tx.open_table(ACCESS_KEYS)?;
            tx.open_table(ADMIN_USERS)?;
            tx.open_table(SESSIONS)?;
            tx.open_table(AUDIT)?;
            tx.open_table(MULTIPARTS)?;
        }
        tx.commit()?;
        Ok(Self {
            db: Arc::new(parking_lot::Mutex::new(Some(db))),
            path: Arc::new(path.to_path_buf()),
        })
    }

    pub fn path_checkpoint(&self) -> anyhow::Result<()> {
        // Force a durable checkpoint of the WAL so a copied `.redb` file is
        // complete even if the process is killed right after.
        self.write()?.commit()?;
        Ok(())
    }

    /// Write a consistent copy of the database to `dest`.
    ///
    /// redb holds an exclusive OS file lock while a `Database` is open (on
    /// Windows this blocks even a read handle), and the header is double
    /// buffered. The robust cross-platform sequence is therefore:
    /// 1. take the write lock and **drop the handle**, which flushes and
    ///    releases the file;
    /// 2. copy the (now quiescent) file;
    /// 3. reopen the database and restore the handle.
    ///
    /// The caller's `Meta` clone keeps the lock during the copy, so no
    /// concurrent transaction can slip in; operations that lose the race get a
    /// descriptive error instead of touching a half-copied file.
    pub fn backup_to(&self, dest: &Path) -> anyhow::Result<()> {
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let path = self.db_path();
        let mut guard = self.db.lock();
        let db = guard
            .take()
            .ok_or_else(|| anyhow::anyhow!("database is already closed for backup"))?;
        drop(db);
        let result = (|| -> anyhow::Result<()> {
            std::fs::copy(&path, dest).context("copying database file")?;
            // Prove the copy opens and has the expected schema.
            let probe = Meta::open_storage_only(dest)?;
            probe.check_schema()?;
            Ok(())
        })();
        // Reopen the live database regardless of the copy outcome.
        let reopened = Database::create(&path)
            .with_context(|| format!("reopening metadata database {path:?}"));
        *guard = Some(reopened?);
        result
    }

    /// Open a database file without creating tables (used to validate copies).
    fn open_storage_only(path: &Path) -> anyhow::Result<Self> {
        let db = Database::open(path).with_context(|| format!("opening {path:?}"))?;
        Ok(Self {
            db: Arc::new(parking_lot::Mutex::new(Some(db))),
            path: Arc::new(path.to_path_buf()),
        })
    }

    fn db_path(&self) -> std::path::PathBuf {
        self.path.as_ref().clone()
    }

    /// Borrow the live database. Returns an error only if a concurrent backup
    /// has momentarily closed the handle (backups are taken from the CLI path).
    fn db(&self) -> anyhow::Result<parking_lot::MutexGuard<'_, Option<Database>>> {
        let guard = self.db.lock();
        if guard.is_none() {
            bail!("metadata database is temporarily closed for backup");
        }
        Ok(guard)
    }

    /// Begin a read transaction.
    fn read(&self) -> anyhow::Result<redb::ReadTransaction> {
        Ok(self.db()?.as_ref().expect("checked").begin_read()?)
    }

    /// Begin a write transaction.
    fn write(&self) -> anyhow::Result<redb::WriteTransaction> {
        Ok(self.db()?.as_ref().expect("checked").begin_write()?)
    }
    /// Generic read of one JSON row.
    pub fn get_json<T: for<'de> Deserialize<'de>>(
        &self,
        table: TableDefinition<&str, &str>,
        key: &str,
    ) -> anyhow::Result<Option<T>> {
        let tx = self.read()?;
        let t = tx.open_table(table)?;
        match t.get(key)? {
            Some(v) => {
                let parsed = serde_json::from_str(v.value())
                    .with_context(|| format!("decoding metadata row {key:?}"))?;
                Ok(Some(parsed))
            }
            None => Ok(None),
        }
    }

    /// Generic read inside an existing read transaction.
    pub fn tx_get_json<T: for<'de> Deserialize<'de>>(
        tx: &redb::ReadTransaction,
        table: TableDefinition<&str, &str>,
        key: &str,
    ) -> anyhow::Result<Option<T>> {
        let t = tx.open_table(table)?;
        match t.get(key)? {
            Some(v) => Ok(Some(serde_json::from_str(v.value())?)),
            None => Ok(None),
        }
    }

    /// Generic write of one JSON row.
    pub fn put_json<T: Serialize>(
        &self,
        table: TableDefinition<&str, &str>,
        key: &str,
        value: &T,
    ) -> anyhow::Result<()> {
        let tx = self.write()?;
        {
            let mut t = tx.open_table(table)?;
            let json = serde_json::to_string(value)?;
            t.insert(key, json.as_str())?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Delete a row; returns whether it existed.
    pub fn delete_row(
        &self,
        table: TableDefinition<&str, &str>,
        key: &str,
    ) -> anyhow::Result<bool> {
        let tx = self.write()?;
        let existed = {
            let mut t = tx.open_table(table)?;
            t.remove(key)?.is_some()
        };
        tx.commit()?;
        Ok(existed)
    }

    /// Snapshot all rows of a table (bounded by `limit`; audit/logs use this).
    pub fn scan_json<T: for<'de> Deserialize<'de>>(
        &self,
        table: TableDefinition<&str, &str>,
        prefix: Option<&str>,
        limit: usize,
    ) -> anyhow::Result<Vec<(String, T)>> {
        let tx = self.read()?;
        let t = tx.open_table(table)?;
        let mut out = Vec::new();
        for row in t.iter()? {
            let (k, v) = row?;
            let k = k.value();
            if let Some(p) = prefix {
                if !k.starts_with(p) {
                    continue;
                }
            }
            let parsed: T = serde_json::from_str(v.value())?;
            out.push((k.to_string(), parsed));
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Delete every row with a given prefix in one transaction.
    pub fn delete_prefix(
        &self,
        table: TableDefinition<&str, &str>,
        prefix: &str,
    ) -> anyhow::Result<usize> {
        let tx = self.write()?;
        let mut removed = 0usize;
        {
            let mut t = tx.open_table(table)?;
            let keys: Vec<String> = t
                .iter()?
                .filter_map(Result::ok)
                .filter(|(k, _)| k.value().starts_with(prefix))
                .map(|(k, _)| k.value().to_string())
                .collect();
            for k in keys {
                if t.remove(k.as_str())?.is_some() {
                    removed += 1;
                }
            }
        }
        tx.commit()?;
        Ok(removed)
    }

    /// Append an audit row. Failures are logged, never surfaced: auditing must
    /// not break a successful write.
    pub fn audit(&self, actor: &str, action: &str, target: &str, detail: &str, remote_ip: &str) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let at = time::OffsetDateTime::now_utc().unix_timestamp();
        // Key = zero-padded seconds + monotonic sequence, so lexicographic
        // order equals write order even for entries in the same second.
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let key = format!("{at:013}-{seq:010}",);
        let entry = AuditEntry {
            at,
            actor: actor.to_string(),
            action: action.to_string(),
            target: target.to_string(),
            detail: detail.to_string(),
            remote_ip: remote_ip.to_string(),
        };
        if let Err(e) = self.put_json(AUDIT, &key, &entry) {
            tracing::warn!(error = %e, "failed to write audit entry");
        }
    }

    /// Recent audit rows, newest first.
    pub fn recent_audit(&self, limit: usize) -> anyhow::Result<Vec<AuditEntry>> {
        let tx = self.read()?;
        let t = tx.open_table(AUDIT)?;
        let mut out: Vec<AuditEntry> = Vec::new();
        for row in t.iter()?.rev() {
            let (_, v) = row?;
            out.push(serde_json::from_str(v.value())?);
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Verify the table set on disk matches what this build expects.
    pub fn check_schema(&self) -> anyhow::Result<()> {
        let tx = self.read()?;
        let mut present: Vec<String> = Vec::new();
        let tables = tx.list_tables()?;
        for t in tables {
            present.push(t.name().to_string());
        }
        for name in ALL_TABLES {
            if !present.iter().any(|n| n == name) {
                bail!(
                    "metadata database is missing table {name}; was it created by a newer version?"
                );
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_meta() -> (tempdir::Tmp, Meta) {
        let t = tempdir::Tmp::new();
        let m = Meta::open(&t.path().join("meta.redb")).unwrap();
        (t, m)
    }

    /// Tiny self-contained tempdir so the crate has no dev-dependency.
    mod tempdir {
        pub struct Tmp(std::path::PathBuf);
        impl Tmp {
            pub fn new() -> Self {
                let p = std::env::temp_dir().join(format!(
                    "hejiu45-oss-test-{}",
                    uuid::Uuid::new_v4().simple()
                ));
                std::fs::create_dir_all(&p).unwrap();
                Self(p)
            }
            pub fn path(&self) -> &std::path::Path {
                &self.0
            }
        }
        impl Drop for Tmp {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }

    fn sample_bucket(name: &str) -> BucketMeta {
        BucketMeta {
            name: name.into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            owner: "admin".into(),
            perms: BTreeMap::new(),
            visibility: default_visibility(),
            object_count: 0,
            total_bytes: 0,
        }
    }

    #[test]
    fn roundtrip_bucket_and_schema() {
        let (_t, m) = temp_meta();
        m.check_schema().unwrap();
        m.put_json(BUCKETS, "photos", &sample_bucket("photos"))
            .unwrap();
        let got: BucketMeta = m.get_json(BUCKETS, "photos").unwrap().unwrap();
        assert_eq!(got.name, "photos");
        assert!(!got.is_public_read());
        assert!(m.get_json::<BucketMeta>(BUCKETS, "nope").unwrap().is_none());
        assert!(m.delete_row(BUCKETS, "photos").unwrap());
        assert!(!m.delete_row(BUCKETS, "photos").unwrap());
    }

    #[test]
    fn delete_prefix_only_touches_matching_rows() {
        let (_t, m) = temp_meta();
        for k in ["a\u{0}x", "a\u{0}y", "ab\u{0}z"] {
            m.put_json(
                OBJECTS,
                k,
                &ObjectMeta {
                    key: k.into(),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let removed = m.delete_prefix(OBJECTS, "a\u{0}").unwrap();
        assert_eq!(removed, 2);
        let left: Vec<ObjectMeta> = m
            .scan_json(OBJECTS, None, 10)
            .unwrap()
            .into_iter()
            .map(|x| x.1)
            .collect();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].key, "ab\u{0}z");
    }

    #[test]
    fn audit_is_newest_first() {
        let (_t, m) = temp_meta();
        for i in 0..3 {
            m.audit("admin", "test", &format!("t{i}"), "", "127.0.0.1");
        }
        let rows = m.recent_audit(10).unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].target, "t2");
    }

    #[test]
    fn backup_produces_readable_copy() {
        let (t, m) = temp_meta();
        m.put_json(BUCKETS, "b1", &sample_bucket("b1")).unwrap();
        let dest = t.path().join("backup.redb");
        m.backup_to(&dest).unwrap();
        assert!(dest.exists());
        let reopened = Meta::open(&dest).unwrap();
        assert!(
            reopened
                .get_json::<BucketMeta>(BUCKETS, "b1")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn composite_keys_roundtrip() {
        let k = composite_key("bucket", "dir/file name.txt");
        let (b, o) = split_composite_key(&k).unwrap();
        assert_eq!((b, o), ("bucket", "dir/file name.txt"));
    }
}
