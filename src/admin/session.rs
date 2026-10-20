//! Server-side sessions for the admin console.
//!
//! Sessions live in redb, not in a cookie payload, so they can be revoked
//! instantly (logout, password change, "sign out everywhere"). The cookie
//! carries an opaque 256-bit random id; nothing about the user is encrypted
//! into it and it is meaningless without the database.
//!
//! Revocation has two layers:
//! * deleting the session row (single-device logout), and
//! * bumping `AdminUser::session_generation` (revokes every session of that
//!   user, which is what a password change must do).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::http::HeaderMap;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};

use crate::crypto::random_b64url;
use crate::error::ApiError;
use crate::meta::{ADMIN_USERS, AdminUser, Meta, SESSIONS, SessionRecord};

/// Name of the session cookie.
pub const COOKIE_NAME: &str = "hj45_session";

/// Mint a session row and return the cookie carrying its id.
pub fn create(
    meta: &Meta,
    user: &AdminUser,
    ttl: Duration,
    secure: bool,
) -> Result<Cookie<'static>, anyhow::Error> {
    let id = random_b64url(32)?;
    let now = unix_now();
    let rec = SessionRecord {
        username: user.username.clone(),
        expires_at: now + ttl.as_secs() as i64,
        generation: user.session_generation,
        created_at: now,
    };
    meta.put_json(SESSIONS, &id, &rec)?;
    Ok(build_cookie(id, ttl.as_secs() as i64, secure))
}

fn build_cookie(id: String, max_age: i64, secure: bool) -> Cookie<'static> {
    Cookie::build((COOKIE_NAME, id))
        // HttpOnly: unreadable from JavaScript, so XSS cannot steal it.
        .http_only(true)
        // SameSite=Strict: the console is same-origin only.
        .same_site(SameSite::Strict)
        .secure(secure)
        .path("/")
        .max_age(time::Duration::seconds(max_age))
        .build()
}

/// The recorded session, validated against the user's current generation.
pub fn load(meta: &Meta, jar: &CookieJar) -> Option<(String, SessionRecord)> {
    let id = jar.get(COOKIE_NAME)?.value().to_string();
    let rec: SessionRecord = meta.get_json(SESSIONS, &id).ok().flatten()?;
    if rec.expires_at < unix_now() {
        let _ = meta.delete_row(SESSIONS, &id);
        return None;
    }
    // Generation check = instant global revocation after a password change.
    let user: AdminUser = meta.get_json(ADMIN_USERS, &rec.username).ok().flatten()?;
    if user.session_generation != rec.generation {
        let _ = meta.delete_row(SESSIONS, &id);
        return None;
    }
    Some((id, rec))
}

/// Remove one session.
pub fn destroy(meta: &Meta, id: &str) {
    let _ = meta.delete_row(SESSIONS, id);
}

/// Remove every session belonging to `username` (e.g. on password change).
pub fn destroy_all_for(meta: &Meta, username: &str) -> anyhow::Result<u64> {
    let sessions: Vec<(String, SessionRecord)> = meta.scan_json(SESSIONS, None, 100_000)?;
    let mut n = 0;
    for (id, rec) in sessions {
        if rec.username == username && meta.delete_row(SESSIONS, &id)? {
            n += 1;
        }
    }
    Ok(n)
}

/// Drop expired rows; called on startup and after each login.
pub fn sweep(meta: &Meta) -> u64 {
    let Ok(sessions) = meta.scan_json::<SessionRecord>(SESSIONS, None, usize::MAX) else {
        return 0;
    };
    let now = unix_now();
    let mut n = 0;
    for (id, rec) in sessions {
        if rec.expires_at < now && meta.delete_row(SESSIONS, &id).unwrap_or(false) {
            n += 1;
        }
    }
    n
}

/// Cookie-clearing jar for logout.
pub fn clear(jar: CookieJar) -> CookieJar {
    jar.remove(Cookie::build(COOKIE_NAME).path("/").build())
}

/// Session id from a jar, without validation (for logout).
pub fn cookie_id(jar: &CookieJar) -> Option<String> {
    jar.get(COOKIE_NAME).map(|c| c.value().to_string())
}

/// Authenticated admin resolved from the cookie jar.
pub struct Authn {
    pub session_id: String,
    pub user: AdminUser,
}

/// Require a valid session; `Err` maps to 401.
pub fn require(meta: &Meta, jar: &CookieJar) -> Result<Authn, ApiError> {
    let (session_id, rec) = load(meta, jar).ok_or_else(ApiError::unauthorized)?;
    let user = meta
        .get_json::<AdminUser>(ADMIN_USERS, &rec.username)
        .ok()
        .flatten()
        .ok_or_else(ApiError::unauthorized)?;
    Ok(Authn { session_id, user })
}

/// Best-effort client address for the audit log.
pub fn client_ip(headers: &HeaderMap) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .unwrap_or("unknown")
        .to_string()
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Cookie name constant for tests/docs.
pub fn cookie_name() -> &'static str {
    COOKIE_NAME
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hash_admin_password;

    fn setup() -> (std::path::PathBuf, Meta, AdminUser) {
        let dir =
            std::env::temp_dir().join(format!("hejiu45-sess-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let meta = Meta::open(&dir.join("meta.redb")).unwrap();
        let user = AdminUser {
            username: "admin".into(),
            password_phc: hash_admin_password("pw").unwrap(),
            created_at: "2026-01-01T00:00:00Z".into(),
            session_generation: 1,
        };
        meta.put_json(ADMIN_USERS, "admin", &user).unwrap();
        (dir, meta, user)
    }

    #[test]
    fn session_roundtrip_and_cookie_attributes() {
        let (dir, meta, user) = setup();
        let cookie = create(&meta, &user, Duration::from_secs(3600), true).unwrap();
        assert_eq!(cookie.name(), COOKIE_NAME);
        assert!(cookie.http_only().unwrap_or(false));
        assert_eq!(cookie.secure(), Some(true));
        assert_eq!(cookie.same_site(), Some(SameSite::Strict));
        assert_eq!(cookie.path(), Some("/"));

        let jar = CookieJar::new().add(cookie);
        let (id, rec) = load(&meta, &jar).expect("session valid");
        assert_eq!(rec.username, "admin");
        destroy(&meta, &id);
        assert!(load(&meta, &jar).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn insecure_cookie_when_tls_terminated_somewhere_untrusted() {
        let (dir, meta, user) = setup();
        let c = create(&meta, &user, Duration::from_secs(60), false).unwrap();
        assert_eq!(c.secure(), Some(false));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn generation_bump_revokes_every_session() {
        let (dir, meta, user) = setup();
        let c1 = create(&meta, &user, Duration::from_secs(3600), true).unwrap();
        let c2 = create(&meta, &user, Duration::from_secs(3600), true).unwrap();
        let jar1 = CookieJar::new().add(c1);
        let jar2 = CookieJar::new().add(c2);
        assert!(load(&meta, &jar1).is_some());
        assert!(load(&meta, &jar2).is_some());

        let mut bumped = user.clone();
        bumped.session_generation += 1;
        meta.put_json(ADMIN_USERS, "admin", &bumped).unwrap();
        assert!(load(&meta, &jar1).is_none());
        assert!(load(&meta, &jar2).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expired_sessions_are_swept() {
        let (dir, meta, user) = setup();
        let _ = create(&meta, &user, Duration::from_secs(3600), true).unwrap();
        meta.put_json(
            SESSIONS,
            "stale",
            &SessionRecord {
                username: "admin".into(),
                expires_at: 1,
                generation: 1,
                created_at: 1,
            },
        )
        .unwrap();
        assert_eq!(sweep(&meta), 1);
        assert!(
            meta.get_json::<SessionRecord>(SESSIONS, "stale")
                .unwrap()
                .is_none()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn destroy_all_for_user() {
        let (dir, meta, user) = setup();
        for _ in 0..3 {
            let _ = create(&meta, &user, Duration::from_secs(3600), true).unwrap();
        }
        assert_eq!(destroy_all_for(&meta, "admin").unwrap(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
