# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [0.3.0] — 2026-10-22

Sharing a public object no longer means pasting a 300-character signed URL.

### Added

- **Public buckets get permanent, unsigned URLs.** A `public-read` bucket is
  reachable at the short path `https://oss.example.com/{bucket}/{key}` — no key,
  no signature, no expiry. The console shows the prefix with a copy button.
- **Custom domain per bucket (CNAME).** Bind `cdn.example.com` to a bucket and
  its objects are served at that hostname's root (`https://cdn.example.com/logo.svg`).
  Hostnames are validated and cannot be bound to two buckets.
- **Access guide in the console**: copy-ready snippets for aws-cli, boto3,
  JavaScript (v3) and Go, with endpoint, region and bucket pre-filled; plus
  one-click downloads of a `~/.aws` config pair and a `.env` file.
- **7-day presign lifetime** for private buckets, with the UI stating that
  permanent links require making the bucket public.

### Changed

- Presigned GET on a public bucket returns `{"url": ..., "permanent": true}`
  instead of signing, because a signature there only shortens the link's life.
  Private objects still return a signed URL.
- The share panel splits by visibility: public buckets show the permanent
  prefix and domain binding, private buckets show the signed-URL flow with a
  shortcut to make the bucket public.

### Fixed

- **Object keys were percent-encoded too aggressively in generated URLs.**
  `NON_ALPHANUMERIC` turned `.` into `%2E`, so `/img/pic.png` became
  `/img/pic%2Epng` and no longer named the stored key. Paths now use the same
  encoder as SigV4.

### Security notes

- Public short links do not widen the surface: the request runs the normal read
  handler, so the `public-read` check still applies. A private bucket's name in
  that position falls through to the SPA, so bucket names stay unguessable.
- Anonymous writes are impossible regardless of visibility.
## [0.2.1] — 2026-10-22

### Fixed

- **Presigned URLs from the console ignored the S3 mount prefix.** The signature
  was computed over `/{bucket}/{key}` while the S3 API is served under `/s3`, so
  a shared link pointed at a path the router did not recognise. The request fell
  through to the SPA fallback and answered with the console HTML (200) instead of
  the object, which looks like "the link does not work" with no error. The signed
  path now includes `--s3-prefix`, and a regression test fetches the object
  through the generated URL to prove it serves bytes rather than HTML.

### Added

- `docs/DEPLOY-LINUX.md`: a deployment guide covering manual install, custom
  paths, nginx, backup/restore, upgrades, and recovery from a lost admin
  password.
## [0.2.0] — 2026-10-20

Browser uploads, editable upload limits, and a bilingual console.

### Added

- **Browser uploads** in the console: drag and drop (files *and* folders), multi
  file selection, automatic multipart above the part size with 3 parts in
  flight, per-file progress, speed, remaining time and cancel. Works for any
  file type; the browser's `Content-Type` is preserved so previews render
  correctly.
- **Two upload modes**: as the signed-in admin (default, no access key needed),
  or through an access key's presigned `PUT` URL.
- **Object delete, download and preview** in the bucket view.
- **Editable upload limits** under Settings: maximum object size (default 5 GiB,
  optional unlimited) and multipart part size (default 8 MiB). Persisted in the
  metadata database, effective immediately, and enforced on console uploads and
  S3 `PUT`s alike. An oversized body is rejected from `Content-Length` before it
  is read.
- **Bilingual console** (English / 简体中文) with one-click switching, browser
  language detection, and a persisted choice.
- `tests/console_upload.rs`: integration coverage for the console write path
  (six file types round-trip byte-exact, 20 MiB multipart assembly, limit
  enforcement, delete semantics, and anonymous access rejection).
- `frontend/scripts/check-i18n.mjs`: fails the build when a translation key is
  missing from any language, or referenced but undefined.

### Fixed

- `aws-chunked` request bodies were stored verbatim, inflating every SDK part by
  54 bytes and corrupting multipart uploads from the AWS CLI.
- `CreateMultipartUpload` only accepted the non-standard
  `POST /{bucket}?uploads&key=…` form; SDKs use `POST /{bucket}/{key}?uploads`.
- Concurrent `UploadPart` requests lost part records (unlocked
  read-modify-write), making `CompleteMultipartUpload` fail with
  "Part N was not uploaded".
- Anonymous access to `public-read` buckets was impossible: the verifier
  rejected credential-less requests before the operation could allow them.
- Any non-chunked upload larger than one 128 KiB read chunk failed with
  `XAmzContentSHA256Mismatch` (the cumulative hash was compared per chunk).
- The Linux build was broken: a `#[cfg(unix)]` block called `flock` through
  `unsafe`, which `#![forbid(unsafe_code)]` rejects. Replaced with `fs2`.
- Eighteen files carried UTF-8 byte-order marks, which made `Cargo.toml`
  unparseable for some tooling; a CI guard now catches regressions.
- `Limit` handling: `--max-object-bytes` defaulted to unlimited and was applied
  in only one code path.

### Changed

- Opening a metadata database now materialises every table, so the new
  `settings` table appears on an existing deployment without a migration step.
- Presigned URLs may now be issued for `PUT` (write access on the bucket is
  required); previously they were read-only.

## [0.1.0] — 2026-10-20

First public release: single-binary, S3-compatible object storage with an
embedded Vue 3 console, SigV4 verification, AES-256-GCM envelope encryption for
access-key secrets, server-side sessions, per-key bucket grants, an audit log,
TLS in-process or behind a reverse proxy, and checksum-verified installers.

[0.3.0]: https://github.com/Hejiu-Web/hejiu45-oss/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/Hejiu-Web/hejiu45-oss/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/Hejiu-Web/hejiu45-oss/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Hejiu-Web/hejiu45-oss/releases/tag/v0.1.0
