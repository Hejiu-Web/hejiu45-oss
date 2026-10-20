//! Generate a self-signed TLS certificate for local testing:
//! `cargo run --bin gen-cert -- cert.pem key.pem`
//!
//! Ships as a developer convenience only; production certificates come from
//! Let's Encrypt (certbot) or an internal CA.
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cert_path = args.get(1).cloned().unwrap_or_else(|| "cert.pem".into());
    let key_path = args.get(2).cloned().unwrap_or_else(|| "key.pem".into());

    let cert =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])?;
    std::fs::write(&cert_path, cert.cert.pem())?;
    std::fs::write(&key_path, cert.signing_key.serialize_pem())?;
    // Private keys must not be world-readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
    }
    println!("wrote {cert_path} and {key_path} (valid for localhost / 127.0.0.1)");
    Ok(())
}
