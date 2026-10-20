# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

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

[0.2.0]: https://github.com/Hejiu-Web/hejiu45-oss/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Hejiu-Web/hejiu45-oss/releases/tag/v0.1.0
