# Security model

This document describes what `hejiu45-oss` protects, what it does not, and the
reasoning behind the controls. It is the reference for operators and auditors.

## 1. Trust boundaries

```text
   Browser ──(cookie session)──┐
                               ├──►  hejiu45-oss ──► redb (metadata) ──► disk
   S3 client ──(SigV4)────────┘         │
                                        └──► objects/ (bodies) ──► disk
```

Two independent authentication paths share one process:

| | Console (`/`, `/api/*`) | Data plane (`/s3/*`) |
| :-- | :-- | :-- |
| Who | A human with a browser | SDKs, CLIs, CI, applications |
| Proof | Username + password → **session cookie** | **AWS Signature Version 4** |
| Secret at rest | Argon2id PHC string | AES-256-GCM sealed blob |
| Failure mode | 401, redirect to login | 403 with an S3 XML error |

**Why the split matters:** the two paths cannot be leveraged into each other.
A stolen console cookie can change configuration (issue keys) but cannot read
object bytes without an access key. A leaked S3 key cannot reach the console.

## 2. Credentials

### 2.1 Console passwords

- Hashed with **Argon2id** through the `salting` crate, which pins the OWASP
  Password Storage Cheat Sheet parameters (64 MiB memory, 3 iterations, 4
  lanes) and enforces them when verifying a stored hash.
- Verification runs on a blocking thread pool so a login cannot stall the
  runtime.
- **Login throttling**: eight failures from one address in a 15-minute window
  produce `429`. Successful logins clear the counter.
- **No user enumeration**: an unknown username and a wrong password produce the
  same error and the same timing path.
- Minimum length for a new password is 12 characters; the generated initial
  password is 24 base64url characters (144 bits).

### 2.2 Console sessions

- A session is an opaque **256-bit random identifier** (`getrandom`), stored as
  a row in redb. Nothing about the user is encoded in it.
- Cookie attributes, all mandatory:
  - `HttpOnly` — unreadable from JavaScript, so XSS cannot exfiltrate it.
  - `Secure` — only sent over HTTPS. The server sets this when TLS is
    terminated in-process, or when `--tls reverse-proxy` is configured and the
    trusted proxy reports `X-Forwarded-Proto: https`.
  - `SameSite=Strict` — the console is same-origin only, so cross-site requests
    carry no session.
  - `Path=/`, `Max-Age` = `--session-hours` (12 h default).
- **Instant revocation** works at two levels: deleting the row (one device) and
  incrementing `AdminUser::session_generation` (every device, used on password
  change). Expired rows are swept on login and at startup.

### 2.3 S3 access keys

- Format: `HJ45` + 16 hex characters. Secret: 30 random bytes, base64url.
- **Only the sealed form is stored.** The plaintext secret is returned exactly
  once, in the response that creates or rotates it.
- Sealing: HKDF-SHA256 derives a 256-bit key from the master key with the
  label `s3-secret-encryption`; AES-256-GCM seals the secret with a random
  96-bit nonce and the associated data `hejiu45-oss/s3-secret/v1` (so a
  ciphertext cannot be replayed as another kind of secret).
- The **master key** comes from `--master-key` / `HEJIU45_OSS_MASTER_KEY`. If
  absent, one is generated into `<data-dir>/master.key` with mode `0600` so a
  fresh install works, and a warning is logged explaining that it should be
  moved into a secret store.
- **Consequence:** losing the master key means every access key must be
  reissued. Back it up separately from the data directory.

## 3. AWS Signature Version 4 verification

Implemented in `src/auth/sigv4.rs`.

1. **Exactly one mechanism.** An `Authorization` header together with
   `X-Amz-Algorithm` query parameters is rejected (two conflicting proofs of
   identity are a request-smuggling pattern).
2. **Canonical request** reconstructed from the request as sent:
   - the URI is the **raw request target** (percent-encoded bytes), captured
     before the router rewrote the path — necessary because SigV4 signs the
     encoded path, and because the S3 mount prefix is stripped by the router;
   - the query string is parsed without form-decoding (`+` stays `+`), RFC 3986
     encoded, sorted, and `X-Amz-Signature` is excluded;
   - signed headers are lowercased and values collapsed per AWS rules;
   - the payload line is the declared `x-amz-content-sha256`, `UNSIGNED-PAYLOAD`
     or the streaming marker.
3. **Path-encoding tolerance.** SDKs disagree on whether `$`, `:`, `@`, `'`,
   `!`, `=`, `+`, `,`, `;` must be encoded in the path. Verification tries the
   strict form, the raw form as sent, and the `$`-literal form. All candidates
   derive from the same signed bytes, so this broadens client compatibility
   without broadening what an attacker can get signed.
4. **Clock policy.** Header auth: `x-amz-date` within ±15 minutes. Presigned:
   not in the future beyond the skew and not older than `X-Amz-Expires`
   (maximum 7 days).
5. **Region is checked** and reported as `AuthorizationHeaderMalformed`, which
   is what AWS SDKs look for.
6. **Constant-time comparison** of the computed and presented signatures
   (`subtle::ConstantTimeEq`).
7. **Body binding.** When a client declares a concrete `x-amz-content-sha256`,
   `Data::stage_verified` compares the streamed digest and rejects the upload
   with `XAmzContentSHA256Mismatch` as soon as it diverges — no need to buffer
   the body.
8. **Presigned URLs are read-only.** Only `GET` and `HEAD` may be presigned, so
   a shared link cannot originate a write.

The implementation is validated in unit tests against the AWS documentation
vectors (GET Object, PUT Object, GET Bucket Lifecycle, List Objects, presigned
URL), and a process-level self-check verifies HMAC derivation at startup.

## 4. Authorisation

There is no IAM policy engine. Permissions are a **grant table**:

```
Bucket.perms: { access_key_id → { read: bool, write: bool } }
```

- A bucket created through the console starts with **no grants**.
- A bucket created through the S3 API grants **full** access to the key that
  created it (otherwise the creator could not use it).
- Every S3 operation checks the grant for the action it performs:
  `ListBuckets` only shows granted buckets (a listing is information too),
  `GetObject` requires `read`, `PutObject` requires `write`.
- `public-read` buckets allow anonymous `GET`/`HEAD` only; anonymous access is
  never possible for writes or listings. Anonymity is decided at the operation
  (the verifier returns "no principal" rather than an error), so an anonymous
  request can never reach an operation that requires a caller.
- Rotating a key **migrates its grants** to the new key, so rotation is not a
  silent outage. Deleting a key removes its grants from every bucket.

## 5. Storage safety

- **Path traversal is structurally impossible.** Body paths are
  `objects/<bucket>/<sha256(bucket NUL key)[0:2]>/<rest of the hash>`. Object
  keys never become filesystem names, so `..`, backslashes, NUL bytes, Windows
  reserved names (`CON`, `AUX`) and Unicode normalisation cannot escape.
- **Atomic replacement.** Uploads are written to `tmp/` (same filesystem) and
  renamed into place. On Windows, where `rename` fails when the destination
  exists, a remove-then-rename fallback runs under a per-path lock.
- **Bucket names are validated** (3–63 characters, lowercase, no leading or
  trailing separators, not an IP address) before touching the filesystem.
- **Data directory ACLs**: the installer restricts `ProgramData\hejiu45-oss`
  to Administrators/SYSTEM on Windows and `0750 user:group` on Linux.
- The **redb file** contains no plaintext secrets (only sealed ones) and no
  object bytes.

## 6. Web hardening

| Control | Where |
| :-- | :-- |
| `X-Content-Type-Options: nosniff` | every response |
| `X-Frame-Options: DENY` | every response |
| `Referrer-Policy: strict-origin-when-cross-origin` | every response |
| `Content-Security-Policy: sandbox; ...` on object downloads | `s3::object::get_object` |
| Cache policy: `no-cache` for the shell, `immutable` for hashed assets | `web.rs` |
| Console assets served from memory (embedded), never from a path | `web.rs` |
| Path isolation: S3 under `/s3`, console under `/` | `server.rs` |

Uploaded content is never sniffed into an executable type: text files stay
`application/octet-stream` unless the client said otherwise, and a `sandbox`
CSP makes even an uploaded HTML file inert if it is opened directly.

## 7. Denial-of-service considerations

- Object bodies stream in fixed 128 KiB chunks; the declared hash is checked
  incrementally, so a hostile upload is rejected at the first mismatching
  chunk.
- `--max-object-bytes` caps a single request; multipart parts are capped at
  512 MiB each.
- Login attempts are throttled per address.
- Request bodies for XML operations (`DeleteObjects`, `CompleteMultipartUpload`)
  are read with a hard 1–2 MiB limit.
- The reverse-proxy templates set `client_max_body_size`, streaming (no
  buffering) and generous timeouts for large transfers.

## 8. Known limitations

These are conscious trade-offs, not oversights:

1. **No HA and no replication.** A single node with a local filesystem. Back up
   the data directory and the master key.
2. **No bucket policies, ACL documents, versioning or lifecycle.** Access is the
   grant table described above. `?acl`, `?policy`, `?versioning` and friends
   return `NotImplemented` rather than pretending to work.
3. **Anonymity is limited** to `public-read` buckets and `GET`/`HEAD`.
4. **Single-node rate limiting** for login is in memory; a restart resets the
   counters.
5. **No multi-factor authentication** for the console. Put it behind a VPN or a
   proxy with additional auth if that is in your threat model.
6. **`salting` verification cost is bounded** by design (memory ≤ 64 MiB,
   iterations ≤ 16, parallelism ≤ 8), so a corrupted or attacker-supplied hash
   string cannot cause an unbounded allocation.
7. **Windows service management** relies on WinSW (pinned version, checksum
   verified when available) or a Scheduled Task; the binary itself is a console
   program.

## 9. Reporting a vulnerability

Please **do not open a public issue** for a security problem. Instead:

1. Use GitHub's [private vulnerability reporting](https://github.com/Hejiu-Web/hejiu45-oss/security/advisories/new)
   on this repository, or
2. Email the maintainer listed in the repository profile.

Include a minimal reproduction (request, configuration, version). We aim to
acknowledge within 72 hours and to ship a fix or mitigation before disclosing.
Credit will be given unless you ask otherwise.

## 10. Operator checklist

- [ ] TLS is terminated (`--tls manual` or `--tls reverse-proxy` with a trusted
      proxy) and the console is **not** reachable over plain HTTP.
- [ ] `Secure` appears on `Set-Cookie` after login (check the browser inspector).
- [ ] The master key is backed up somewhere other than the data directory.
- [ ] `objects/` and the metadata file are in your backup rotation, and a
      restore has been tested once.
- [ ] The initial generated admin password has been changed.
- [ ] Every access key has exactly the buckets it needs; nothing else.
- [ ] `--public-url` matches the URL users and SDKs actually use (presigned URLs
      are host-bound).
- [ ] The data directory is not world-readable.


