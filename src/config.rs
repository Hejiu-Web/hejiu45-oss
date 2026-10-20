//! Command-line surface and runtime configuration.
//!
//! Every option has an environment-variable equivalent so the same binary
//! works for `myoss --flag`, `MYOSS_FLAG=1 myoss` and `--config file.toml`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Parser, ValueEnum};

/// The canonical product name, used for directories, service names and
/// user-agent strings. Keeping it in one place avoids the `myoss`/`hejiu45-oss`
/// drift that crept into early drafts.
pub const PRODUCT: &str = "hejiu45-oss";
/// Short name used for env-var prefixes.
pub const ENV_PREFIX: &str = "HEJIU45_OSS";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TlsMode {
    /// Off: plain HTTP. Only safe behind a TLS-terminating reverse proxy or on
    /// loopback. The server refuses to set the `Secure` cookie in this mode
    /// unless `--trust-proxy` is enabled and the proxy reports `https`.
    Off,
    /// Terminate TLS in-process with a certificate/key pair.
    Manual,
    /// Serve plain HTTP on `--listen` while trusting `X-Forwarded-Proto` from a
    /// reverse proxy. This is the recommended production mode (see
    /// `deploy/nginx/`).
    ReverseProxy,
}

#[derive(Debug, Clone, Parser)]
#[command(
    name = PRODUCT,
    version,
    about = "Single-binary S3-compatible object storage with an embedded web console",
    long_about = None,
)]
pub struct Args {
    /// Address to listen on.
    #[arg(long, env = "HEJIU45_OSS_LISTEN", default_value = "127.0.0.1:9000")]
    pub listen: SocketAddr,

    /// Data directory: all objects, metadata and the redb database live here.
    #[arg(long, env = "HEJIU45_OSS_DATA_DIR", default_value = "./data")]
    pub data_dir: PathBuf,

    /// TLS handling mode.
    #[arg(long, env = "HEJIU45_OSS_TLS", value_enum, default_value = "off")]
    pub tls: TlsMode,

    /// PEM certificate (leaf + intermediates) for `--tls manual`.
    #[arg(long, env = "HEJIU45_OSS_TLS_CERT")]
    pub tls_cert: Option<PathBuf>,

    /// PEM private key (PKCS#8, RSA or SEC1) for `--tls manual`.
    #[arg(long, env = "HEJIU45_OSS_TLS_KEY")]
    pub tls_key: Option<PathBuf>,

    /// Public base URL clients use to reach the server (used for the console
    /// snippets and presigned URL display). e.g. `https://oss.example.com`.
    #[arg(long, env = "HEJIU45_OSS_PUBLIC_URL")]
    pub public_url: Option<String>,

    /// Ignore inbound `X-Forwarded-*` headers unless they come from a loopback
    /// address. Disable only when the proxy sits on another host.
    #[arg(long, env = "HEJIU45_OSS_TRUST_PROXY", default_value_t = true)]
    pub trust_proxy: bool,

    /// Trust the `X-Forwarded-For` chain to derive the client IP for audit logs.
    #[arg(long, env = "HEJIU45_OSS_TRUST_FORWARDED_FOR", default_value_t = false)]
    pub trust_forwarded_for: bool,

    /// Initial admin username. Only used on first boot; later changes are made
    /// in the console.
    #[arg(long, env = "HEJIU45_OSS_ADMIN_USER", default_value = "admin")]
    pub admin_user: String,

    /// Initial admin password. If omitted on first boot, a strong one is
    /// generated and printed once (never re-printed).
    #[arg(long, env = "HEJIU45_OSS_ADMIN_PASSWORD")]
    pub admin_password: Option<String>,

    /// 32-byte master key (hex or base64) used to envelope-encrypt S3 secret
    /// keys. Stored secrets are unreadable if this is lost, so back it up.
    /// If omitted, a key is generated and persisted with 0600 permissions.
    #[arg(long, env = "HEJIU45_OSS_MASTER_KEY")]
    pub master_key: Option<String>,

    /// URL prefix the S3 API is served under. The console owns `/`, so S3
    /// lives under `/s3` by default. Set to empty for S3-at-root deployments
    /// (a dedicated host or port); path-style addressing is required either
    /// way.
    #[arg(long, env = "HEJIU45_OSS_S3_PREFIX", default_value = "/s3")]
    pub s3_prefix: String,

    /// Region reported by the S3 API.
    #[arg(long, env = "HEJIU45_OSS_REGION", default_value = "us-east-1")]
    pub region: String,

    /// Maximum accepted body size for object uploads, in bytes. Ignored for
    /// multipart part uploads, which are bounded per part.
    #[arg(long, env = "HEJIU45_OSS_MAX_OBJECT_BYTES")]
    pub max_object_bytes: Option<u64>,

    /// Session lifetime for the admin console.
    #[arg(long, env = "HEJIU45_OSS_SESSION_HOURS", default_value_t = 12)]
    pub session_hours: u64,

    /// Enable mTLS-less plaintext S3 access on loopback only. When false and
    /// `--tls off`, the server refuses to start unless explicitly overridden.
    #[arg(long, env = "HEJIU45_OSS_ALLOW_INSECURE", default_value_t = false)]
    pub allow_insecure: bool,

    /// Log filter, e.g. `info`, `hejiu45_oss=debug,tower_http=debug`.
    #[arg(long, env = "HEJIU45_OSS_LOG", default_value = "info")]
    pub log: String,

    /// Write an **updated** redb file to this path and exit (online backup via
    /// redb's savepoint semantics: always consistent).
    #[arg(long, env = "HEJIU45_OSS_BACKUP_TO")]
    pub backup_to: Option<PathBuf>,

    /// Restore a backup created by `--backup-to` into `--data-dir` and exit.
    /// Refuses to overwrite a non-empty data directory.
    #[arg(long, env = "HEJIU45_OSS_RESTORE_FROM")]
    pub restore_from: Option<PathBuf>,

    /// Print the effective configuration and exit.
    #[arg(long)]
    pub print_config: bool,

    /// Create (or re-label) one S3 access key and print its access key id and
    /// secret, then exit. The secret is printed exactly once; only the sealed
    /// form is stored.
    #[arg(long, env = "HEJIU45_OSS_INIT_ACCESS_KEY")]
    pub init_access_key: Option<String>,

    /// Create a bucket and grant the key above full access to it.
    #[arg(long, env = "HEJIU45_OSS_INIT_BUCKET")]
    pub init_bucket: Option<String>,

    /// Make the `--init-bucket` publicly readable.
    #[arg(long, default_value_t = false)]
    pub init_bucket_public_read: bool,
}

impl Args {
    pub fn parse_and_validate() -> anyhow::Result<Self> {
        let mut args = Self::parse();
        args.data_dir = absolutize(&args.data_dir);
        if let Some(p) = &args.public_url {
            args.public_url = Some(p.trim_end_matches('/').to_string());
        }
        // Normalise: "" or "/" means root, otherwise "/prefix" without a
        // trailing slash.
        let p = args.s3_prefix.trim().trim_end_matches('/').to_string();
        args.s3_prefix = if p == "/" { String::new() } else { p };
        if args.tls == TlsMode::Manual && (args.tls_cert.is_none() || args.tls_key.is_none()) {
            anyhow::bail!("`--tls manual` requires both --tls-cert and --tls-key");
        }
        Ok(args)
    }

    /// Whether cookies may be marked `Secure`.
    pub fn secure_cookies(&self) -> bool {
        match self.tls {
            TlsMode::Manual => true,
            TlsMode::ReverseProxy => self.trust_proxy,
            TlsMode::Off => false,
        }
    }

    pub fn session_ttl(&self) -> Duration {
        Duration::from_secs(self.session_hours.max(1) * 3600)
    }
}

fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(p))
            .unwrap_or_else(|_| p.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Args::command().debug_assert();
    }

    #[test]
    fn secure_cookie_policy() {
        let mut a = Args::parse_from(["x"]);
        a.tls = TlsMode::Off;
        assert!(!a.secure_cookies());
        a.tls = TlsMode::Manual;
        assert!(a.secure_cookies());
        a.tls = TlsMode::ReverseProxy;
        a.trust_proxy = true;
        assert!(a.secure_cookies());
        a.trust_proxy = false;
        assert!(!a.secure_cookies());
    }
}
