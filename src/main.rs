//! Entry point: boot sequence, single-instance guard, and admin bootstrap.

use std::sync::Arc;

use anyhow::Context;
use hejiu45_oss::config::{Args, TlsMode, PRODUCT};
use hejiu45_oss::crypto::{self, SecretBox, hash_admin_password, MasterKeySource};
use hejiu45_oss::data::Data;
use hejiu45_oss::meta::{ADMIN_USERS, AdminUser, Meta};
use hejiu45_oss::state::{AppState, now_rfc3339};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

fn main() -> anyhow::Result<()> {
    let args = Args::parse_and_validate()?;
    init_tracing(&args.log);

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;
    let result = rt.block_on(run(args));
    // `panic = "abort"` in release: nothing to unwind, just report.
    result
}

fn init_tracing(filter: &str) {
    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(filter))
        .unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

async fn run(args: Args) -> anyhow::Result<()> {
    info!(version = env!("CARGO_PKG_VERSION"), product = PRODUCT, "starting");

    // Offline maintenance modes run before anything binds a port.
    if let Some(dest) = &args.backup_to {
        let meta = Meta::open(&args.data_dir.join("meta.redb"))?;
        meta.backup_to(dest)?;
        println!("metadata backed up to {}", dest.display());
        println!("copy the `objects/` directory together with this file for a full backup.");
        return Ok(());
    }
    if let Some(src) = &args.restore_from {
        restore(src, &args.data_dir)?;
        return Ok(());
    }

    let _lock = acquire_data_lock(&args.data_dir)?;
    let (master, key_source) = crypto::load_or_create_master_key(
        args.master_key.as_deref(),
        &args.data_dir,
    )?;
    match key_source {
        MasterKeySource::GeneratedFile => warn!(
            path = %crypto::master_key_path(&args.data_dir).display(),
            "generated a master key; move it into HEJIU45_OSS_MASTER_KEY and back it up, or sealed S3 secrets become unreadable"
        ),
        MasterKeySource::File => info!("master key loaded from data directory"),
        MasterKeySource::Environment => info!("master key loaded from environment"),
    }

    let meta = Meta::open(&args.data_dir.join("meta.redb"))?;
    meta.check_schema()?;
    let data = Data::open(&args.data_dir)?;
    let swept = data.sweep_tmp().await.unwrap_or(0);
    if swept > 0 {
        info!(files = swept, "removed abandoned temp uploads");
    }

    bootstrap_admin(&meta, &args)?;

    let secrets = SecretBox::from_master(&master)?;
    let auth = hejiu45_oss::auth::Authenticator::new(meta.clone(), secrets, args.region.clone());
    let state = AppState {
        args: Arc::new(args.clone()),
        meta: meta.clone(),
        data: data.clone(),
        auth,
        started_at: std::time::Instant::now(),
        started_unix: time::OffsetDateTime::now_utc().unix_timestamp(),
    };

    if args.print_config {
        println!("{:#?}", args);
        return Ok(());
    }

    if matches!(args.tls, TlsMode::Off) && !args.allow_insecure {
        let loopback = args.listen.ip().is_loopback();
        if !loopback {
            anyhow::bail!(
                "refusing to expose plain HTTP on a non-loopback address ({}) without TLS.\n\
                 Use `--tls manual --tls-cert ... --tls-key ...`, terminate TLS at a reverse proxy \
                 with `--tls reverse-proxy` (see deploy/nginx/), or pass `--allow-insecure` if the \
                 network path is already protected (e.g. a private mesh).",
                args.listen
            );
        }
        warn!(
            "running plain HTTP on loopback without TLS: the console session cookie will NOT be \
             marked Secure. Use `--tls reverse-proxy` behind the bundled nginx config for production."
        );
    }
    if !args.secure_cookies() && !matches!(args.tls, TlsMode::Off) {
        warn!("Secure cookies are disabled because proxy trust is off; do not expose this setup");
    }

    let public_url = args
        .public_url
        .clone()
        .unwrap_or_else(|| format!("http://{}", args.listen));
    info!(url = %public_url, "console ready");

    hejiu45_oss::server::serve(state).await
}

/// First boot: create the initial admin. Never re-prints or resets an
/// existing password.
fn bootstrap_admin(meta: &Meta, args: &Args) -> anyhow::Result<()> {
    let existing: Vec<(String, AdminUser)> = meta.scan_json(ADMIN_USERS, None, 1)?;
    if !existing.is_empty() {
        return Ok(());
    }
    let (password, generated) = match &args.admin_password {
        Some(p) if !p.is_empty() => (p.clone(), false),
        _ => (crypto::random_b64url(18)?, true),
    };
    if password.len() < 12 {
        warn!("the supplied admin password is shorter than 12 characters; consider lengthening it");
    }
    let user = AdminUser {
        username: args.admin_user.clone(),
        password_phc: hash_admin_password(&password)?,
        created_at: now_rfc3339(),
        session_generation: 1,
    };
    meta.put_json(ADMIN_USERS, &user.username, &user)?;
    if generated {
        // Printed exactly once, to stdout, so it is visible where operators
        // look first. It is never written to disk in plaintext.
        println!(
            "\n──────────────────────────────────────────────────────────────\n\
             \u{1F510} Initial admin credentials for {PRODUCT}\n\
             ┌──────────────────────────────────────────┐\n\
             │ username: {:<30} │\n\
             │ password: {:<30} │\n\
             └──────────────────────────────────────────┘\n\
             This password is shown once. Change it after your first login.\n\
             ──────────────────────────────────────────────────────────────\n",
            user.username, password
        );
    } else {
        info!(username = %user.username, "created initial admin account");
    }
    Ok(())
}

/// Best-effort single-instance guard: two processes sharing one data
/// directory would corrupt the index, so hold an advisory lock file.
fn acquire_data_lock(dir: &std::path::Path) -> anyhow::Result<DataLock> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("server.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // flock(LOCK_EX | LOCK_NB)
        let r = unsafe_flock(file.as_raw_fd());
        if !r {
            anyhow::bail!(
                "another {PRODUCT} instance is already using {}",
                dir.display()
            );
        }
    }
    #[cfg(not(unix))]
    {
        use std::io::Read;
        let mut existing = String::new();
        let _ = std::fs::File::open(&path).and_then(|mut f| f.read_to_string(&mut existing));
        if let Some(pid) = existing.trim().parse::<u32>().ok().filter(|p| pid_alive(*p)) {
            anyhow::bail!("another {PRODUCT} instance (pid {pid}) is already using {}", dir.display());
        }
        std::fs::write(&path, std::process::id().to_string())?;
    }
    Ok(DataLock { path, file: Some(file) })
}

#[cfg(unix)]
fn unsafe_flock(fd: i32) -> bool {
    extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    // SAFETY: `fd` is a valid open file descriptor for the lifetime of the call.
    unsafe { flock(fd, LOCK_EX | LOCK_NB) == 0 }
}

#[cfg(not(unix))]
fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

struct DataLock {
    path: std::path::PathBuf,
    file: Option<std::fs::File>,
}

impl Drop for DataLock {
    fn drop(&mut self) {
        #[cfg(not(unix))]
        {
            let _ = std::fs::remove_file(&self.path);
        }
        drop(self.file.take());
    }
}

fn restore(src: &std::path::Path, data_dir: &std::path::Path) -> anyhow::Result<()> {
    if !src.exists() {
        anyhow::bail!("backup file not found: {}", src.display());
    }
    let target = data_dir.join("meta.redb");
    if target.exists() {
        anyhow::bail!(
            "refusing to overwrite existing metadata at {}; move it aside first",
            target.display()
        );
    }
    std::fs::create_dir_all(data_dir)?;
    std::fs::copy(src, &target)?;
    // Verify the copy opens before declaring success.
    let meta = Meta::open(&target)?;
    meta.check_schema()?;
    println!("restored metadata into {}", target.display());
    println!("restore the `objects/` directory from your backup as well.");
    Ok(())
}
