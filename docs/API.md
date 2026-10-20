# Console API

The management API is served under `/api`. Authentication is a **cookie
session** obtained from `POST /api/auth/login`; it is entirely separate from the
S3 data plane under `/s3`.

All bodies are JSON. Errors use one shape:

```json
{ "error": { "code": "NotFound", "message": "Bucket not found." } }
```

| Status | Meaning |
| :-- | :-- |
| 400 | Invalid input |
| 401 | No or expired session |
| 403 | Authenticated but not allowed (e.g. wrong current password) |
| 404 | Missing resource |
| 409 | Conflict (duplicate name, non-empty bucket) |
| 429 | Login throttled |

## Public

### `GET /api/health`

```json
{ "status": "ok", "service": "hejiu45-oss", "version": "0.1.0" }
```

### `GET /api/info`

Behaviour flags the login page needs before a session exists.

```json
{
  "product": "hejiu45-oss",
  "version": "0.1.0",
  "region": "us-east-1",
  "public_url": "https://oss.example.com",
  "secure_cookies": true,
  "console_embedded": true
}
```

## Authentication

### `POST /api/auth/login`

```json
{ "username": "admin", "password": "..." }
```

`200` with `Set-Cookie: hj45_session=...; HttpOnly; Secure; SameSite=Strict`.
Failures are `401 InvalidCredentials` regardless of whether the user exists.

### `POST /api/auth/logout`

Deletes the session row and clears the cookie. Always `200`.

### `GET /api/auth/me`

```json
{ "username": "admin", "created_at": "2026-10-20T04:49:24Z" }
```

### `POST /api/auth/password`

```json
{ "current_password": "...", "new_password": "at-least-12-chars" }
```

Revokes **every** session for the user (including the caller's) and clears the
cookie: the client must send the user back to the login page.

## Overview

### `GET /api/overview`

```json
{
  "buckets": 2,
  "objects": 128,
  "total_bytes": 48234012,
  "keys": 3,
  "enabled_keys": 3,
  "uptime_seconds": 3600,
  "by_type": { "image": 120, "text": 8 },
  "data_dir": "/var/lib/hejiu45-oss",
  "region": "us-east-1"
}
```

### `GET /api/audit`

Newest first, capped at 200 entries.

```json
{
  "entries": [
    {
      "at": 1760930000,
      "actor": "admin",
      "action": "key.create",
      "target": "HJ45...",
      "detail": "ci-deploy",
      "remote_ip": "203.0.113.9"
    }
  ]
}
```

## Buckets

### `GET /api/buckets`

```json
{
  "buckets": [
    {
      "name": "photos",
      "created_at": "2026-10-20T04:49:27Z",
      "visibility": "public-read",
      "object_count": 12,
      "total_bytes": 9932011,
      "grants": [
        { "access_key": "HJ45...", "label": "ci-deploy", "read": true, "write": true }
      ]
    }
  ]
}
```

### `POST /api/buckets`

```json
{ "name": "photos", "visibility": "private" }
```

`201` with the bucket view. `visibility` is `private` (default) or
`public-read`. Names follow S3 rules (3–63 chars, lowercase, no IP-looking
names).

### `PATCH /api/buckets/{name}`

```json
{ "visibility": "public-read" }
```

### `DELETE /api/buckets/{name}`

`409` while objects remain. Aborts pending multipart uploads of the bucket.

### `GET /api/buckets/{name}/objects?prefix=&delimiter=/&max_keys=500`

```json
{
  "objects": [
    {
      "key": "2026/cat.jpg",
      "size": 993201,
      "etag": "\"9f86d081884c7d659a2feaa0c55ad015\"",
      "content_type": "image/jpeg",
      "last_modified": "2026-10-20T04:50:00Z"
    }
  ],
  "common_prefixes": ["2026/"],
  "truncated": false
}
```

### `POST /api/buckets/{name}/objects/presign?key=<object key>`

```json
{ "access_key_id": "HJ45...", "ttl_seconds": 900 }
```

Mints a host-bound presigned URL. The key must hold **read** on the bucket;
the console does not bypass the ACL. Lifetime is 1 second to 7 days.

```json
{
  "url": "https://oss.example.com/s3/photos/cat.jpg?X-Amz-Algorithm=...&X-Amz-Signature=...",
  "expires_at": "2026-10-20T05:05:00Z",
  "method": "GET"
}
```

## Access keys

### `GET /api/keys`

```json
{
  "keys": [
    {
      "access_key_id": "HJ45...",
      "label": "ci-deploy",
      "created_at": "2026-10-20T04:49:27Z",
      "enabled": true,
      "expires_at": null,
      "last_used_at": "2026-10-20T05:00:01Z",
      "grants": [{ "bucket": "photos", "read": true, "write": false }]
    }
  ]
}
```

### `POST /api/keys`

```json
{ "label": "ci-deploy", "expires_at": "2027-01-01T00:00:00Z" }
```

`201`, and **this is the only time the secret is returned**:

```json
{
  "access_key_id": "HJ45...",
  "secret_access_key": "...",
  "label": "ci-deploy",
  "notice": "Store the secret now. It is sealed in the database and cannot be shown again."
}
```

### `POST /api/keys/{id}/rotate`

Creates a replacement key, revokes the old one, and **migrates every bucket
grant** to the new key. Returns the new secret once.

### `PATCH /api/keys/{id}`

```json
{ "enabled": false }
```

### `DELETE /api/keys/{id}`

Removes the key and every grant that referenced it.

### `PUT /api/keys/{id}/grants`

Replaces all grants for the key:

```json
{
  "grants": [
    { "bucket": "photos", "read": true, "write": true },
    { "bucket": "backups", "read": true, "write": false }
  ]
}
```

Buckets omitted from the list lose their grant. A grant with both flags false is
equivalent to no grant.

## S3 data plane

Not documented here in detail — it is the standard S3 wire protocol. The
differences from AWS are:

- Served under `/s3` (`--s3-prefix`, configurable).
- **Path-style addressing only**: `/{bucket}/{key}`, never `bucket.host`.
- Region must match `--region` exactly (default `us-east-1`).
- ACL documents, bucket policies, versioning, lifecycle, replication,
  notifications and `ListParts` return `NotImplemented`.
- Error responses carry `x-amz-request-id` and `x-amz-id-2` like AWS.

See [SECURITY.md](SECURITY.md) §3–§4 for the authentication and authorisation
rules that apply to every request.
