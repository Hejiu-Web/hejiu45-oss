//! Management API for the embedded console (`/api/*`).
//!
//! ## Security model
//!
//! Console auth and S3 auth are deliberately separate paths:
//! * This module authenticates with a **server-side cookie session**
//!   (`HttpOnly; Secure; SameSite=Strict`), see [`super::session`].
//! * The data plane authenticates with **SigV4**, see [`crate::auth`].
//!
//! A stolen console session can change configuration but cannot read object
//! bytes directly (it has no access key), and an S3 key cannot log in here.
//! That split keeps the blast radius of either compromise small.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum_extra::extract::cookie::CookieJar;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::admin::session;
use crate::crypto::{hash_admin_password, verify_admin_password};
use crate::error::ApiError;
use crate::meta::{
    ACCESS_KEYS, ADMIN_USERS, BUCKETS, BucketMeta, BucketPerms, OBJECTS, ObjectMeta,
};
use crate::state::{AppState, now_rfc3339};

const MAX_LOGIN_FAILURES: u32 = 8;
const LOGIN_WINDOW_SECS: i64 = 15 * 60;

/// In-memory throttle state (per process). A single-admin console is the
/// threat model here; per-IP counters in redb would be overkill.
#[derive(Default)]
struct LoginThrottle {
    failures: std::collections::HashMap<String, (u32, i64)>,
}

static THROTTLE: parking_lot::Mutex<Option<LoginThrottle>> = parking_lot::Mutex::new(None);

fn check_throttle(ip: &str) -> Result<(), ApiError> {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let mut g = THROTTLE.lock();
    let t = g.get_or_insert_with(LoginThrottle::default);
    // Purge stale windows.
    t.failures
        .retain(|_, (_, at)| now - *at < LOGIN_WINDOW_SECS);
    match t.failures.get(ip) {
        Some((n, _)) if *n >= MAX_LOGIN_FAILURES => Err(ApiError::too_many_requests(
            "Too many failed logins from this address; try again in a few minutes.",
        )),
        _ => Ok(()),
    }
}

fn record_failure(ip: &str) {
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    let mut g = THROTTLE.lock();
    let t = g.get_or_insert_with(LoginThrottle::default);
    let e = t.failures.entry(ip.to_string()).or_insert((0, now));
    e.0 += 1;
    e.1 = now;
}

fn clear_failures(ip: &str) {
    let mut g = THROTTLE.lock();
    if let Some(t) = g.as_mut() {
        t.failures.remove(ip);
    }
}

// ---------------------------------------------------------------------------
// Public endpoints
// ---------------------------------------------------------------------------

/// `GET /api/health`
pub async fn health() -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(json!({
            "status": "ok",
            "service": crate::config::PRODUCT,
            "version": env!("CARGO_PKG_VERSION"),
        })),
    )
}

/// `GET /api/info` — flags the login page needs before a session exists.
pub async fn info(State(st): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "product": crate::config::PRODUCT,
        "version": env!("CARGO_PKG_VERSION"),
        "region": st.args.region,
        "public_url": st.args.public_url,
        "secure_cookies": st.args.secure_cookies(),
        "console_embedded": crate::web::console_embedded(),
    }))
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Serialize)]
pub struct MeResponse {
    pub username: String,
    pub created_at: String,
}

/// `POST /api/auth/login`
pub async fn login(
    State(st): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(body): Json<LoginRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let ip = session::client_ip(&headers);
    check_throttle(&ip)?;

    let user = st
        .meta
        .get_json::<crate::meta::AdminUser>(ADMIN_USERS, &body.username)
        .map_err(ApiError::internal)?
        // Same error for "no such user" and "wrong password": no user enumeration.
        .ok_or_else(|| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "InvalidCredentials",
                "Invalid username or password.",
            )
        })?;

    // Argon2id is deliberately slow; keep it off the async worker.
    let phc = user.password_phc.clone();
    let password = body.password.clone();
    let ok = tokio::task::spawn_blocking(move || verify_admin_password(&password, &phc))
        .await
        .map_err(ApiError::internal)?;
    if !ok {
        record_failure(&ip);
        st.meta.audit(
            &body.username,
            "admin.login_failed",
            &body.username,
            "",
            &ip,
        );
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "InvalidCredentials",
            "Invalid username or password.",
        ));
    }
    clear_failures(&ip);

    let cookie = session::create(
        &st.meta,
        &user,
        st.args.session_ttl(),
        st.args.secure_cookies(),
    )
    .map_err(ApiError::internal)?;
    session::sweep(&st.meta);
    st.meta
        .audit(&user.username, "admin.login", &user.username, "", &ip);
    Ok((
        jar.add(cookie),
        Json(json!({ "ok": true, "username": user.username })),
    ))
}

/// `POST /api/auth/logout`
pub async fn logout(State(st): State<AppState>, jar: CookieJar) -> impl IntoResponse {
    if let Some(id) = session::cookie_id(&jar) {
        session::destroy(&st.meta, &id);
    }
    (session::clear(jar), Json(json!({ "ok": true })))
}

/// `GET /api/auth/me`
pub async fn me(State(st): State<AppState>, jar: CookieJar) -> Result<impl IntoResponse, ApiError> {
    let auth = session::require(&st.meta, &jar)?;
    Ok(Json(MeResponse {
        username: auth.user.username.clone(),
        created_at: auth.user.created_at.clone(),
    }))
}

#[derive(Deserialize)]
pub struct ChangePassword {
    pub current_password: String,
    pub new_password: String,
}

/// `POST /api/auth/password` — changing the password revokes every session.
pub async fn change_password(
    State(st): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Json(body): Json<ChangePassword>,
) -> Result<impl IntoResponse, ApiError> {
    let auth = session::require(&st.meta, &jar)?;
    if body.new_password.chars().count() < 12 {
        return Err(ApiError::bad_request(
            "The new password must be at least 12 characters.",
        ));
    }
    let phc = auth.user.password_phc.clone();
    let cur = body.current_password.clone();
    let ok = tokio::task::spawn_blocking(move || verify_admin_password(&cur, &phc))
        .await
        .map_err(ApiError::internal)?;
    if !ok {
        return Err(ApiError::forbidden("Current password is incorrect."));
    }
    let hash = {
        let pw = body.new_password.clone();
        tokio::task::spawn_blocking(move || hash_admin_password(&pw))
            .await
            .map_err(ApiError::internal)?
            .map_err(ApiError::internal)?
    };
    let mut user = auth.user.clone();
    user.password_phc = hash;
    // Bumping the generation invalidates every existing session, including
    // this one: the UI must send the user back to the login page.
    user.session_generation += 1;
    st.meta
        .put_json(ADMIN_USERS, &user.username, &user)
        .map_err(ApiError::internal)?;
    session::destroy_all_for(&st.meta, &user.username).map_err(ApiError::internal)?;
    st.meta.audit(
        &user.username,
        "admin.password_changed",
        &user.username,
        "all sessions revoked",
        &session::client_ip(&headers),
    );
    Ok((
        session::clear(jar),
        Json(json!({ "ok": true, "reauthenticate": true })),
    ))
}

// ---------------------------------------------------------------------------
// Buckets
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct BucketView {
    pub name: String,
    pub created_at: String,
    pub visibility: String,
    pub object_count: u64,
    pub total_bytes: u64,
    pub grants: Vec<GrantView>,
    /// Hostname serving this bucket at its root, empty when unset.
    pub custom_domain: String,
    /// Permanent URL prefix for a public bucket, empty when private.
    pub public_url: String,
}

#[derive(Serialize)]
pub struct GrantView {
    pub access_key: String,
    pub label: String,
    pub read: bool,
    pub write: bool,
}

/// The permanent URL prefix a public bucket is reachable at.
///
/// Prefers the bound custom domain (shortest, most shareable), then the public
/// base URL with the short `/{bucket}/` form. Empty for private buckets, whose
/// objects need a signature.
fn public_prefix(st: &AppState, b: &BucketMeta) -> String {
    if !b.is_public_read() {
        return String::new();
    }
    if !b.custom_domain.is_empty() {
        let scheme = st
            .args
            .public_url
            .as_deref()
            .and_then(|u| u.split("://").next())
            .unwrap_or("https");
        return format!("{scheme}://{}/", b.custom_domain);
    }
    let base = st
        .args
        .public_url
        .clone()
        .unwrap_or_else(|| format!("http://{}", st.args.listen));
    format!("{}/{}/", base.trim_end_matches('/'), b.name)
}

fn bucket_view(st: &AppState, b: &BucketMeta) -> BucketView {
    let keys: Vec<(String, crate::meta::AccessKeyRecord)> = st
        .meta
        .scan_json(ACCESS_KEYS, None, 10_000)
        .unwrap_or_default();
    let mut grants = Vec::new();
    for (id, rec) in keys {
        if let Some(p) = b.perms.get(&id) {
            if p.read || p.write {
                grants.push(GrantView {
                    access_key: id,
                    label: rec.label,
                    read: p.read,
                    write: p.write,
                });
            }
        }
    }
    BucketView {
        name: b.name.clone(),
        created_at: b.created_at.clone(),
        visibility: b.visibility.clone(),
        object_count: b.object_count,
        total_bytes: b.total_bytes,
        custom_domain: b.custom_domain.clone(),
        public_url: public_prefix(st, b),
        grants,
    }
}

/// `GET /api/buckets`
pub async fn list_buckets(
    State(st): State<AppState>,
    jar: CookieJar,
) -> Result<impl IntoResponse, ApiError> {
    session::require(&st.meta, &jar)?;
    let buckets: Vec<BucketMeta> = st
        .meta
        .scan_json(BUCKETS, None, 10_000)
        .map_err(ApiError::internal)?
        .into_iter()
        .map(|(_, b)| b)
        .collect();
    let views: Vec<BucketView> = buckets.iter().map(|b| bucket_view(&st, b)).collect();
    Ok(Json(json!({ "buckets": views })))
}

#[derive(Deserialize)]
pub struct CreateBucket {
    pub name: String,
    #[serde(default)]
    pub visibility: Option<String>,
}

/// `POST /api/buckets`
pub async fn create_bucket(
    State(st): State<AppState>,
    jar: CookieJar,
    Json(body): Json<CreateBucket>,
) -> Result<impl IntoResponse, ApiError> {
    let auth = session::require(&st.meta, &jar)?;
    crate::data::validate_bucket_name(&body.name).map_err(ApiError::from)?;
    if st
        .meta
        .get_json::<BucketMeta>(BUCKETS, &body.name)
        .map_err(ApiError::internal)?
        .is_some()
    {
        return Err(ApiError::conflict(
            "A bucket with that name already exists.",
        ));
    }
    let visibility = match body.visibility.as_deref() {
        Some("public-read") => "public-read",
        _ => "private",
    };
    let b = BucketMeta {
        name: body.name.clone(),
        created_at: now_rfc3339(),
        owner: auth.user.username.clone(),
        perms: Default::default(),
        visibility: visibility.to_string(),
        object_count: 0,
        total_bytes: 0,
        custom_domain: String::new(),
    };
    st.meta
        .put_json(BUCKETS, &b.name, &b)
        .map_err(ApiError::internal)?;
    st.meta.audit(
        &auth.user.username,
        "bucket.create",
        &b.name,
        visibility,
        "",
    );
    Ok((StatusCode::CREATED, Json(bucket_view(&st, &b))))
}

#[derive(Deserialize)]
pub struct UpdateBucket {
    pub visibility: Option<String>,
    /// Empty string clears the binding.
    pub custom_domain: Option<String>,
}

/// `PATCH /api/buckets/{name}`
pub async fn update_bucket(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    Json(body): Json<UpdateBucket>,
) -> Result<impl IntoResponse, ApiError> {
    let auth = session::require(&st.meta, &jar)?;
    let mut b = st
        .meta
        .get_json::<BucketMeta>(BUCKETS, &name)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("Bucket not found."))?;
    if let Some(v) = body.visibility {
        if v != "private" && v != "public-read" {
            return Err(ApiError::bad_request(
                "visibility must be 'private' or 'public-read'.",
            ));
        }
        b.visibility = v;
    }
    if let Some(domain) = body.custom_domain {
        let domain = domain.trim().to_ascii_lowercase();
        if !domain.is_empty() {
            validate_custom_domain(&domain)?;
            // One hostname must map to exactly one bucket, or routing would be
            // ambiguous and the result would depend on iteration order.
            let buckets: Vec<(String, BucketMeta)> = st
                .meta
                .scan_json(BUCKETS, None, 10_000)
                .map_err(ApiError::internal)?;
            for (other, ob) in &buckets {
                if other != &name
                    && !ob.custom_domain.is_empty()
                    && ob.custom_domain.eq_ignore_ascii_case(&domain)
                {
                    return Err(ApiError::conflict(format!(
                        "{domain} is already bound to bucket '{other}'."
                    )));
                }
            }
        }
        b.custom_domain = domain;
    }
    st.meta
        .put_json(BUCKETS, &name, &b)
        .map_err(ApiError::internal)?;
    st.meta.audit(
        &auth.user.username,
        "bucket.update",
        &name,
        &format!("visibility={} domain={}", b.visibility, b.custom_domain),
        "",
    );
    Ok(Json(bucket_view(&st, &b)))
}

/// Reject hostnames the router could not match, and anything that is not a
/// plain DNS name (no scheme, no path, no port).
fn validate_custom_domain(domain: &str) -> Result<(), ApiError> {
    if domain.contains("://") || domain.contains('/') || domain.contains(':') {
        return Err(ApiError::bad_request(
            "custom_domain must be a bare hostname such as cdn.example.com (no scheme, path or port).",
        ));
    }
    if domain.len() > 253 {
        return Err(ApiError::bad_request("custom_domain is too long."));
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return Err(ApiError::bad_request(
            "custom_domain must contain a dot, e.g. cdn.example.com.",
        ));
    }
    for label in labels {
        let ok = !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !ok {
            return Err(ApiError::bad_request(format!(
                "custom_domain has an invalid label: '{label}'."
            )));
        }
    }
    Ok(())
}

/// `DELETE /api/buckets/{name}` — refuses non-empty buckets.
pub async fn delete_bucket(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let auth = session::require(&st.meta, &jar)?;
    let b = st
        .meta
        .get_json::<BucketMeta>(BUCKETS, &name)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("Bucket not found."))?;
    let remaining = st
        .meta
        .scan_json::<ObjectMeta>(OBJECTS, Some(&format!("{name}\u{0}")), 1)
        .map_err(ApiError::internal)?;
    if !remaining.is_empty() {
        return Err(ApiError::conflict(
            "The bucket still contains objects; empty it first.",
        ));
    }
    st.meta
        .delete_row(BUCKETS, &name)
        .map_err(ApiError::internal)?;
    st.data
        .remove_bucket_dir(&name)
        .await
        .map_err(ApiError::internal)?;
    st.meta.audit(
        &auth.user.username,
        "bucket.delete",
        &name,
        &b.visibility,
        "",
    );
    Ok(Json(json!({ "ok": true })))
}

// ---------------------------------------------------------------------------
// Access keys and grants
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct KeyView {
    pub access_key_id: String,
    pub label: String,
    pub created_at: String,
    pub enabled: bool,
    pub expires_at: Option<String>,
    pub last_used_at: Option<String>,
    /// Buckets this key can touch, with rights.
    pub grants: Vec<KeyGrantView>,
}

#[derive(Serialize)]
pub struct KeyGrantView {
    pub bucket: String,
    pub read: bool,
    pub write: bool,
}

fn all_buckets(st: &AppState) -> Vec<BucketMeta> {
    st.meta
        .scan_json(BUCKETS, None, 100_000)
        .unwrap_or_default()
        .into_iter()
        .map(|(_, b)| b)
        .collect()
}

fn key_view(st: &AppState, rec: &crate::meta::AccessKeyRecord) -> KeyView {
    let mut grants = Vec::new();
    for b in all_buckets(st) {
        if let Some(p) = b.perms.get(&rec.access_key_id) {
            if p.read || p.write {
                grants.push(KeyGrantView {
                    bucket: b.name.clone(),
                    read: p.read,
                    write: p.write,
                });
            }
        }
    }
    KeyView {
        access_key_id: rec.access_key_id.clone(),
        label: rec.label.clone(),
        created_at: rec.created_at.clone(),
        enabled: rec.enabled,
        expires_at: rec.expires_at.clone(),
        last_used_at: rec.last_used_at.clone(),
        grants,
    }
}

/// `GET /api/keys`
pub async fn list_keys(
    State(st): State<AppState>,
    jar: CookieJar,
) -> Result<impl IntoResponse, ApiError> {
    session::require(&st.meta, &jar)?;
    let recs: Vec<crate::meta::AccessKeyRecord> = st
        .meta
        .scan_json(ACCESS_KEYS, None, 10_000)
        .map_err(ApiError::internal)?
        .into_iter()
        .map(|(_, r)| r)
        .collect();
    let views: Vec<KeyView> = recs.iter().map(|r| key_view(&st, r)).collect();
    Ok(Json(json!({ "keys": views })))
}

#[derive(Deserialize)]
pub struct CreateKey {
    pub label: String,
    #[serde(default)]
    pub expires_at: Option<String>,
}

/// `POST /api/keys` — the plaintext secret is returned exactly once.
pub async fn create_key(
    State(st): State<AppState>,
    jar: CookieJar,
    Json(body): Json<CreateKey>,
) -> Result<impl IntoResponse, ApiError> {
    let auth = session::require(&st.meta, &jar)?;
    let label = body.label.trim();
    if label.is_empty() {
        return Err(ApiError::bad_request("A label is required."));
    }
    if let Some(exp) = &body.expires_at {
        if time::OffsetDateTime::parse(exp, &time::format_description::well_known::Rfc3339).is_err()
        {
            return Err(ApiError::bad_request("expires_at must be RFC 3339."));
        }
    }
    let (rec, secret) = st
        .auth
        .create_access_key(label, &auth.user.username, body.expires_at.clone())
        .map_err(ApiError::internal)?;
    st.meta.audit(
        &auth.user.username,
        "key.create",
        &rec.access_key_id,
        label,
        "",
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "access_key_id": rec.access_key_id,
            "secret_access_key": secret,
            "label": rec.label,
            "notice": "Store the secret now. It is sealed in the database and cannot be shown again.",
        })),
    ))
}

/// `POST /api/keys/{id}/rotate` — new key, grants preserved.
pub async fn rotate_key(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let auth = session::require(&st.meta, &jar)?;
    let (rec, secret) = st.auth.rotate_access_key(&id).map_err(ApiError::internal)?;
    st.meta.audit(
        &auth.user.username,
        "key.rotate",
        &rec.access_key_id,
        &id,
        "",
    );
    Ok(Json(json!({
        "access_key_id": rec.access_key_id,
        "secret_access_key": secret,
        "notice": "The old key is revoked. Grants were migrated to the new key.",
    })))
}

#[derive(Deserialize)]
pub struct ToggleKey {
    pub enabled: bool,
}

/// `PATCH /api/keys/{id}` — enable/disable without deleting the record.
pub async fn toggle_key(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(id): Path<String>,
    Json(body): Json<ToggleKey>,
) -> Result<impl IntoResponse, ApiError> {
    let auth = session::require(&st.meta, &jar)?;
    let mut rec = st
        .meta
        .get_json::<crate::meta::AccessKeyRecord>(ACCESS_KEYS, &id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("Access key not found."))?;
    rec.enabled = body.enabled;
    st.meta
        .put_json(ACCESS_KEYS, &id, &rec)
        .map_err(ApiError::internal)?;
    st.meta.audit(
        &auth.user.username,
        if body.enabled {
            "key.enable"
        } else {
            "key.disable"
        },
        &id,
        "",
        "",
    );
    Ok(Json(key_view(&st, &rec)))
}

/// `DELETE /api/keys/{id}` — remove the key and every grant referring to it.
pub async fn delete_key(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let auth = session::require(&st.meta, &jar)?;
    if st
        .meta
        .get_json::<crate::meta::AccessKeyRecord>(ACCESS_KEYS, &id)
        .map_err(ApiError::internal)?
        .is_none()
    {
        return Err(ApiError::not_found("Access key not found."));
    }
    for mut b in all_buckets(&st) {
        if b.perms.remove(&id).is_some() {
            st.meta
                .put_json(BUCKETS, &b.name, &b)
                .map_err(ApiError::internal)?;
        }
    }
    st.meta
        .delete_row(ACCESS_KEYS, &id)
        .map_err(ApiError::internal)?;
    st.meta
        .audit(&auth.user.username, "key.delete", &id, "", "");
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct GrantRequest {
    /// Grant entries: `[{"bucket": "...", "read": true, "write": false}]`
    pub grants: Vec<GrantEntry>,
}

#[derive(Deserialize, Clone)]
pub struct GrantEntry {
    pub bucket: String,
    #[serde(default)]
    pub read: bool,
    #[serde(default)]
    pub write: bool,
}

/// `PUT /api/keys/{id}/grants` — replace every grant for this key atomically
/// enough for a single-admin console (per-bucket writes, audited).
pub async fn set_grants(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(id): Path<String>,
    Json(body): Json<GrantRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let auth = session::require(&st.meta, &jar)?;
    if st
        .meta
        .get_json::<crate::meta::AccessKeyRecord>(ACCESS_KEYS, &id)
        .map_err(ApiError::internal)?
        .is_none()
    {
        return Err(ApiError::not_found("Access key not found."));
    }
    let wanted: std::collections::HashMap<String, GrantEntry> = body
        .grants
        .into_iter()
        .map(|g| (g.bucket.clone(), g))
        .collect();
    for mut b in all_buckets(&st) {
        let existing = b.perms.get(&id).copied();
        let next = match wanted.get(&b.name) {
            Some(g) if g.read || g.write => BucketPerms {
                read: g.read,
                write: g.write,
            },
            _ => BucketPerms::NONE,
        };
        if existing != Some(next) {
            if next.read || next.write {
                b.perms.insert(id.clone(), next);
            } else {
                b.perms.remove(&id);
            }
            st.meta
                .put_json(BUCKETS, &b.name, &b)
                .map_err(ApiError::internal)?;
        }
    }
    st.meta.audit(
        &auth.user.username,
        "key.grants",
        &id,
        &format!("{} buckets", wanted.len()),
        "",
    );
    let rec = st
        .meta
        .get_json::<crate::meta::AccessKeyRecord>(ACCESS_KEYS, &id)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("Access key not found."))?;
    Ok(Json(key_view(&st, &rec)))
}

// ---------------------------------------------------------------------------
// Objects
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ListObjectsQuery {
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub delimiter: String,
    #[serde(default)]
    pub max_keys: Option<usize>,
}

#[derive(Serialize)]
pub struct ObjectView {
    pub key: String,
    pub size: u64,
    pub etag: String,
    pub content_type: String,
    pub last_modified: String,
}

/// `GET /api/buckets/{name}/objects`
pub async fn list_objects(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    Query(q): Query<ListObjectsQuery>,
) -> Result<impl IntoResponse, ApiError> {
    session::require(&st.meta, &jar)?;
    st.meta
        .get_json::<BucketMeta>(BUCKETS, &name)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("Bucket not found."))?;
    let max = q.max_keys.unwrap_or(500).clamp(1, 5000);
    let mut objs: Vec<ObjectMeta> = st
        .meta
        .scan_json::<ObjectMeta>(OBJECTS, Some(&format!("{name}\u{0}")), usize::MAX)
        .map_err(ApiError::internal)?
        .into_iter()
        .map(|(_, o)| o)
        .filter(|o: &ObjectMeta| o.key.starts_with(&q.prefix))
        .collect();
    objs.sort_by(|a, b| a.key.cmp(&b.key));

    let mut prefixes: Vec<String> = Vec::new();
    let mut out: Vec<ObjectView> = Vec::new();
    for o in objs {
        if !q.delimiter.is_empty() {
            let rest = &o.key[q.prefix.len()..];
            if let Some(pos) = rest.find(&q.delimiter) {
                let p = format!("{}{}", q.prefix, &rest[..pos + q.delimiter.len()]);
                if prefixes.last() != Some(&p) {
                    prefixes.push(p);
                }
                continue;
            }
        }
        if out.len() + prefixes.len() >= max {
            break;
        }
        out.push(ObjectView {
            key: o.key,
            size: o.size,
            etag: crate::s3::handlers::etag_quoted(&o.md5_hex),
            content_type: o.content_type,
            last_modified: o.last_modified,
        });
    }
    Ok(Json(json!({
        "objects": out,
        "common_prefixes": prefixes,
        "truncated": out.len() + prefixes.len() >= max,
    })))
}

#[derive(Deserialize)]
pub struct PresignRequest {
    pub access_key_id: String,
    #[serde(default = "default_ttl")]
    pub ttl_seconds: u64,
    /// `"GET"` (default, for sharing) or `"PUT"` (for browser direct upload).
    #[serde(default)]
    pub method: Option<String>,
}

fn default_ttl() -> u64 {
    900
}

/// `POST /api/buckets/{name}/objects/presign` (key in the body as `key`)
///
/// Generates a short-lived, host-bound URL. Requires an access key that holds
/// read permission on the bucket — the console does not bypass the ACL.
/// Sign a URL for `GET` (share) or `PUT` (browser direct upload).
///
/// The method decides which grant the key needs: read for GET, write for PUT.
/// The security policy that presigned URLs cannot originate writes was relaxed
/// deliberately here — the alternative (a console proxy for every byte) means
/// the server buffers data it does not need to see, and an operator who can
/// grant write access can already upload through the console.
pub async fn presign_object(
    State(st): State<AppState>,
    jar: CookieJar,
    Path(name): Path<String>,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
    Json(body): Json<PresignRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let auth = session::require(&st.meta, &jar)?;
    let key = params
        .get("key")
        .cloned()
        .ok_or_else(|| ApiError::bad_request("Missing `key` query parameter."))?;
    let b = st
        .meta
        .get_json::<BucketMeta>(BUCKETS, &name)
        .map_err(ApiError::internal)?
        .ok_or_else(|| ApiError::not_found("Bucket not found."))?;
    let principal = crate::auth::Principal {
        access_key: body.access_key_id.clone(),
        label: body.access_key_id.clone(),
        style: crate::auth::sigv4::AuthStyle::Presigned,
    };
    let need = if body.method.as_deref() == Some("PUT") {
        BucketPerms::WRITE
    } else {
        BucketPerms::READ
    };
    st.auth
        .authorize(&principal, &b, need)
        .map_err(ApiError::from)?;

    let public = st
        .args
        .public_url
        .clone()
        .unwrap_or_else(|| format!("http://{}", st.args.listen));
    let host = public.split("://").nth(1).unwrap_or(&public).to_string();
    // Encode exactly the way SigV4 does for paths: `.` stays literal (encoding
    // it as `%2E` produces a URL that does not name the stored key).
    let encoded_key = key
        .split('/')
        .map(crate::auth::sigv4::uri_encode_path)
        .collect::<Vec<_>>()
        .join("/");
    // The signed path must be the path the client will actually request,
    // including the S3 mount prefix. Signing `/{bucket}/{key}` while the router
    // serves S3 under `/s3` produced URLs that fell through to the SPA fallback
    // and returned the console HTML instead of the object.
    let path = if st.args.s3_prefix.is_empty() {
        format!("/{}/{}", name, encoded_key)
    } else {
        format!(
            "{}/{}/{}",
            st.args.s3_prefix.trim_end_matches('/'),
            name,
            encoded_key
        )
    };
    // A public bucket needs no signature at all: hand back the permanent URL.
    //
    // This is the difference between "share this image with a friend" (a short
    // stable link) and "authorize this request" (a signed, expiring URL). Only
    // `public-read` buckets, and only for reads, can answer with a plain URL —
    // a private object still requires a signature, and writes always do.
    let is_get = body.method.as_deref() != Some("PUT");
    if is_get && b.is_public_read() {
        let encoded = key
            .split('/')
            .map(crate::auth::sigv4::uri_encode_path)
            .collect::<Vec<_>>()
            .join("/");
        let public_link = format!("{}/{}/{}", public.trim_end_matches('/'), name, encoded);
        st.meta.audit(
            &auth.user.username,
            "object.public_link",
            &format!("{name}/{key}"),
            &public_link,
            "",
        );
        return Ok(Json(json!({
            "url": public_link,
            // No expiry: the URL is derived from bucket visibility, not a key.
            "expires_at": serde_json::Value::Null,
            "permanent": true,
            "method": "GET",
        })));
    }

    let url = st
        .auth
        .presign(
            &body.access_key_id,
            if body.method.as_deref() == Some("PUT") {
                axum::http::Method::PUT
            } else {
                axum::http::Method::GET
            },
            &path,
            &[],
            body.ttl_seconds.min(7 * 24 * 3600),
            time::OffsetDateTime::now_utc().unix_timestamp(),
            &host,
            &public,
        )
        .map_err(ApiError::from)?;
    st.meta.audit(
        &auth.user.username,
        if body.method.as_deref() == Some("PUT") {
            "object.presign.put"
        } else {
            "object.presign"
        },
        &format!("{name}/{key}"),
        &url.expires_at,
        "",
    );
    Ok(Json(json!({
        "url": url.url,
        "expires_at": url.expires_at,
        "permanent": false,
        "method": if body.method.as_deref() == Some("PUT") { "PUT" } else { "GET" },
    })))
}

// ---------------------------------------------------------------------------
// Audit and stats
// ---------------------------------------------------------------------------

/// `GET /api/audit` — recent security-relevant events, newest first.
pub async fn audit_log(
    State(st): State<AppState>,
    jar: CookieJar,
) -> Result<impl IntoResponse, ApiError> {
    session::require(&st.meta, &jar)?;
    let rows = st.meta.recent_audit(200).map_err(ApiError::internal)?;
    Ok(Json(json!({ "entries": rows })))
}

/// `GET /api/overview` — dashboard counters.
pub async fn overview(
    State(st): State<AppState>,
    jar: CookieJar,
) -> Result<impl IntoResponse, ApiError> {
    session::require(&st.meta, &jar)?;
    let buckets = all_buckets(&st);
    let keys: Vec<crate::meta::AccessKeyRecord> = st
        .meta
        .scan_json(ACCESS_KEYS, None, 100_000)
        .map_err(ApiError::internal)?
        .into_iter()
        .map(|(_, r)| r)
        .collect();
    let objects: Vec<ObjectMeta> = st
        .meta
        .scan_json(OBJECTS, None, 200_000)
        .map_err(ApiError::internal)?
        .into_iter()
        .map(|(_, o)| o)
        .collect();
    let total_bytes: u64 = objects.iter().map(|o| o.size).sum();
    let mut by_type: std::collections::BTreeMap<String, usize> = Default::default();
    for o in &objects {
        let top = o
            .content_type
            .split('/')
            .next()
            .unwrap_or("other")
            .to_string();
        *by_type.entry(top).or_default() += 1;
    }
    Ok(Json(json!({
        "buckets": buckets.len(),
        "keys": keys.len(),
        "enabled_keys": keys.iter().filter(|k| k.enabled).count(),
        "objects": objects.len(),
        "total_bytes": total_bytes,
        "uptime_seconds": st.started_at.elapsed().as_secs(),
        "by_type": by_type,
        "data_dir": st.args.data_dir.display().to_string(),
        "region": st.args.region,
    })))
}
