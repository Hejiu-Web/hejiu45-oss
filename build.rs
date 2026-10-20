//! Build script: embeds the console bundle at compile time.
//!
//! `rust-embed` reads `frontend/dist/` during compilation, so the bundle must
//! exist before `cargo build`. To keep plain `cargo build` working in CI and on
//! a fresh clone (where `dist/` is gitignored), we materialise a placeholder
//! page when the directory is absent and print a loud warning. Release builds
//! in CI build the frontend first, so the placeholder never ships.

use std::path::Path;

fn main() {
    let dist = Path::new("frontend/dist");
    if !dist.join("index.html").exists() {
        println!(
            "cargo:warning=frontend/dist/index.html not found — embedding a placeholder page. \
             Run `npm --prefix frontend ci && npm --prefix frontend run build` for the real console."
        );
        let _ = std::fs::create_dir_all(dist);
        let _ = std::fs::write(
            dist.join("index.html"),
            PLACEHOLDER,
        );
    }

    // Rebuild whenever the bundle changes.
    for entry in walkdir::WalkDir::new("frontend/dist").into_iter().flatten() {
        if entry.file_type().is_file() {
            println!("cargo:rerun-if-changed={}", entry.path().display());
        }
    }
    println!("cargo:rerun-if-changed=frontend/dist");
}

const PLACEHOLDER: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>hejiu45-oss</title>
<style>body{font:16px/1.6 system-ui,sans-serif;max-width:44rem;margin:12vh auto;padding:0 1.5rem;color:#111}
code{background:#f2f2f2;padding:.15em .4em;border-radius:4px}</style></head>
<body><h1>hejiu45-oss</h1>
<p>The server is running, but the web console bundle was not embedded in this build.</p>
<p>Build it with <code>npm --prefix frontend ci &amp;&amp; npm --prefix frontend run build</code>, then rebuild the binary.</p>
<p>The S3 API and the management API at <code>/api</code> are fully functional.</p>
</body></html>
"#;
