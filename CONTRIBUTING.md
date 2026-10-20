# Contributing

Thanks for taking a look. This project values small, reviewable changes and
explicit reasoning over clever code.

## Building

```bash
# 1. The console must be built first: rust-embed reads frontend/dist at
#    compile time. Without it the binary embeds a placeholder page.
cd frontend
npm ci
npm run build
cd ..

# 2. Then the binary
cargo build            # debug
cargo build --release  # opt-level 3 + LTO, ~6 MB, console included
```

Run it locally:

```bash
./target/debug/hejiu45-oss --data-dir ./data --listen 127.0.0.1:9000 --allow-insecure
```

`--allow-insecure` is only needed because the default listen address is
loopback and TLS is off; it silences the refusal to run plain HTTP.

Console hot reload: `npm run dev` in `frontend/` proxies `/api` to port 9000
while `cargo run` serves the API.

## Testing

```bash
cargo test --lib                 # unit tests, including AWS SigV4 vectors
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

End-to-end (needs a running server and an access key):

```bash
# Provision a key without the console, then start the server
./target/debug/hejiu45-oss --data-dir ./data --init-access-key e2e
./target/debug/hejiu45-oss --data-dir ./data --allow-insecure &

HEJIU45_OSS_AK=... HEJIU45_OSS_SK=... ./scripts/e2e-s3.ps1 -Base http://127.0.0.1:9000
```

The e2e script covers the security-relevant negative cases; please extend it
rather than adding a test that only proves the happy path.

## Code guidelines

- **No `unsafe`.** The crate has `#![forbid(unsafe_code)]`.
- **Explain security decisions in comments**, next to the code. If a check can
  be removed by a future refactor without a test failing, the comment should say
  why it exists.
- **Errors are typed**: S3 operations return `S3Error` (rendered as AWS XML),
  console operations return `ApiError` (JSON). Never leak internal paths or
  database errors to a client.
- **Streaming over buffering** for anything that can be large: object bodies,
  ranges, multipart parts.
- **Tests for parsers and crypto** are non-negotiable; use published test
  vectors where they exist.

## Console guidelines

- Plain JavaScript, Vue 3 + Vite. No TypeScript, no UI framework: the bundle is
  embedded in the binary and every kilobyte ships to every install.
- Keep the initial JS chunk under ~150 kB uncompressed.
- Every destructive action asks for confirmation; every error is surfaced, not
  swallowed.

## Commits and pull requests

- Conventional Commits (`feat:`, `fix:`, `docs:`, `refactor:`, `test:`,
  `chore:`), one logical change per commit.
- Describe the **why** in the body, especially for security-relevant changes.
- Update `docs/SECURITY.md` when a control changes, and `docs/API.md` when an
  endpoint changes.
- CI runs fmt, clippy, unit tests on Linux/Windows/macOS, and the S3 e2e suite.

## Release

1. Bump `version` in `Cargo.toml` and `frontend/package.json`.
2. Commit `chore(release): vX.Y.Z`.
3. Tag `vX.Y.Z` and push the tag; the Release workflow builds the static Linux
   (amd64/arm64) and Windows binaries, writes `checksums-hejiu45-oss.txt`, and
   publishes the release.
4. Installers pick up `releases/latest/download/...` automatically.
