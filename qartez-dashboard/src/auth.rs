//! Authentication and CSRF defense for the local dashboard.
//!
//! Three-layer defense:
//! 1. Bind to 127.0.0.1 only (handled in `daemon::serve`).
//! 2. Random session token written to `~/.qartez/auth.token` (mode 0600).
//!    Browser exchanges it for an `HttpOnly; Secure; SameSite=Strict` cookie
//!    on `/auth?token=...`, then redirects to `/`.
//! 3. Strict Origin allow-list on every request, including the WebSocket
//!    upgrade (CORS preflight does not run for `Upgrade: websocket`).

use std::path::Path;

use anyhow::{Context, Result};
use axum::extract::State;
use axum_extra::extract::cookie::CookieJar;
use http::{HeaderValue, Request, StatusCode, header};
use rand::TryRngCore;

use crate::paths;
use crate::state::AppState;

/// Session token length in bytes before hex encoding.
///
/// 32 bytes = 256 bits, matching common bearer-token strength
/// (RFC 6750, Jupyter `--NotebookApp.token`). Hex-encoded length is 64 chars.
pub const TOKEN_BYTES: usize = 32;

/// Cookie name for the validated session.
pub const SESSION_COOKIE: &str = "qartez_session";

/// Origin allow-list for browser-issued requests.
///
/// Browsers route `*.localhost` to 127.0.0.1 per RFC 6761 (Chrome >= 78,
/// Firefox >= 84, Edge). Safari delegates to mDNSResponder and may not
/// resolve `qartez.localhost`, so we also accept the literal loopback origins.
const ALLOWED_ORIGIN_HOSTS: &[&str] = &["qartez.localhost", "localhost", "127.0.0.1"];

/// Generate a fresh 32-byte hex token. Source: `OsRng`.
pub fn generate_token() -> String {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rngs::OsRng
        .try_fill_bytes(&mut bytes)
        .expect("OsRng should never fail on a healthy system");
    hex::encode(bytes)
}

/// Write the token to disk with mode 0600 on Unix.
pub fn write_token(path: &Path, token: &str) -> Result<()> {
    std::fs::write(path, token).with_context(|| format!("writing {}", path.display()))?;
    set_owner_only_permissions(path)?;
    Ok(())
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o600);
    std::fs::set_permissions(path, perms)
        .with_context(|| format!("setting 0600 on {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

/// Read the token currently on disk, used by `qartez dashboard open`
/// to construct the handshake URL.
pub fn read_token() -> Result<String> {
    let path = paths::token_file()?;
    let token =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    Ok(token.trim().to_string())
}

/// Validate that an `Origin` header is loopback. Returns true when allowed.
///
/// Returns false when:
/// - the header is missing (non-browser callers should not use the cookie path)
/// - the header is malformed
/// - the host is not in `ALLOWED_ORIGIN_HOSTS`
pub fn origin_is_allowed(origin: Option<&HeaderValue>) -> bool {
    let Some(value) = origin else {
        return false;
    };
    let Ok(text) = value.to_str() else {
        return false;
    };
    let Ok(parsed) = url::Url::parse(text) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    ALLOWED_ORIGIN_HOSTS.contains(&host)
}

/// `tower::Service` extractor used as middleware for HTTP routes that must
/// originate from a browser tab on a loopback origin. WebSocket upgrades
/// run the same check inline because CORS does not protect the upgrade.
pub async fn require_loopback_origin(
    req: Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, StatusCode> {
    let origin = req.headers().get(header::ORIGIN);
    if origin.is_none() {
        // Same-origin GETs from typing the URL into the address bar omit
        // Origin. Allow them only for safe, idempotent methods.
        if req.method().is_safe() {
            return Ok(next.run(req).await);
        }
        return Err(StatusCode::FORBIDDEN);
    }
    if origin_is_allowed(origin) {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::FORBIDDEN)
    }
}

/// Middleware enforcing a valid session cookie on protected routes.
///
/// The loopback-origin check alone is trivially spoofable by any local
/// process (`curl -H 'Origin: http://localhost'`), so it cannot stand in for
/// authentication. This layer requires the `qartez_session` cookie to
/// constant-time-equal the daemon's session token (the same value the browser
/// received from the 0600 token file via the `/auth` handshake).
///
/// It gates every `/api/*`, `/ws`, and data route. The `/auth` handshake and
/// the static-asset fallback are intentionally left ungated: the browser must
/// be able to reach `/auth` to obtain the cookie and to load the SPA shell
/// before it holds one. Chain this AFTER [`require_loopback_origin`].
///
/// Returns `401 Unauthorized` when the cookie is absent or does not match.
pub async fn require_session_cookie(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, StatusCode> {
    let jar = CookieJar::from_headers(req.headers());
    let Some(cookie) = jar.get(SESSION_COOKIE) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    if constant_time_eq(cookie.value().as_bytes(), state.auth_token().as_bytes()) {
        Ok(next.run(req).await)
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

/// Constant-time byte-slice equality.
///
/// Compares every byte regardless of where the first mismatch occurs, so the
/// running time does not leak how many leading bytes matched. Unequal lengths
/// return `false` immediately - the token length is fixed and not secret.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_hex_encoded_and_64_chars() {
        let token = generate_token();
        assert_eq!(token.len(), TOKEN_BYTES * 2);
        assert!(token.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn allowed_origins_match_loopback_hosts() {
        let cases = &[
            ("http://qartez.localhost:47821", true),
            ("http://localhost:47821", true),
            ("http://127.0.0.1:47821", true),
            ("https://evil.com", false),
            ("http://qartez.localhost.evil.com", false),
        ];
        for (origin, expected) in cases {
            let header = HeaderValue::from_str(origin).unwrap();
            assert_eq!(
                origin_is_allowed(Some(&header)),
                *expected,
                "origin={origin}"
            );
        }
    }

    #[test]
    fn missing_origin_header_rejected() {
        assert!(!origin_is_allowed(None));
    }

    #[test]
    fn constant_time_eq_matches_and_rejects() {
        assert!(constant_time_eq(b"same-token", b"same-token"));
        assert!(!constant_time_eq(b"same-token", b"diff-token"));
        // Length mismatch is rejected without panicking on the shorter slice.
        assert!(!constant_time_eq(b"short", b"short-and-then-some"));
        assert!(constant_time_eq(b"", b""));
    }

    #[tokio::test]
    async fn session_cookie_gate_rejects_and_accepts() {
        use std::future::poll_fn;

        use axum::Router;
        use axum::body::Body;
        use axum::routing::get;
        use tokio_util::sync::CancellationToken;
        use tower::Service;

        let token = "s3cr3t-session-token-value";
        let state = AppState::new(
            std::path::PathBuf::from("."),
            token.to_string(),
            CancellationToken::new(),
        );
        let build = || {
            Router::new()
                .route("/protected", get(|| async { StatusCode::OK }))
                .layer(axum::middleware::from_fn_with_state(
                    state.clone(),
                    require_session_cookie,
                ))
                .with_state(state.clone())
        };

        let status_for = |cookie: Option<String>| {
            let mut app = build();
            async move {
                let mut builder = Request::builder().uri("/protected");
                if let Some(cookie) = cookie {
                    builder = builder.header(header::COOKIE, cookie);
                }
                let req = builder.body(Body::empty()).unwrap();
                poll_fn(|cx| <axum::Router as Service<Request<Body>>>::poll_ready(&mut app, cx))
                    .await
                    .unwrap();
                app.call(req).await.unwrap().status()
            }
        };

        // No cookie at all: rejected before the handler runs.
        assert_eq!(status_for(None).await, StatusCode::UNAUTHORIZED);
        // Wrong cookie value: rejected.
        assert_eq!(
            status_for(Some(format!("{SESSION_COOKIE}=not-the-token"))).await,
            StatusCode::UNAUTHORIZED
        );
        // Correct cookie value: passes through to the handler.
        assert_eq!(
            status_for(Some(format!("{SESSION_COOKIE}={token}"))).await,
            StatusCode::OK
        );
    }

    // End-to-end gate check against the REAL production router (not a synthetic
    // one): proves /api/* are session-gated while /auth and the SPA fallback
    // stay reachable so the browser can bootstrap the cookie and load the app.
    #[tokio::test]
    async fn real_router_gates_api_but_not_auth_or_fallback() {
        use std::future::poll_fn;

        use axum::body::Body;
        use axum::http::{Request, StatusCode, header};
        use tokio_util::sync::CancellationToken;
        use tower::Service;

        let token = "real-router-session-token";
        let state = AppState::new(
            std::path::PathBuf::from("."),
            token.to_string(),
            CancellationToken::new(),
        );

        let status_of = |req: Request<Body>| {
            let mut app = crate::server::router(state.clone());
            async move {
                poll_fn(|cx| <axum::Router as Service<Request<Body>>>::poll_ready(&mut app, cx))
                    .await
                    .unwrap();
                app.call(req).await.unwrap().status()
            }
        };

        // Protected data route, no cookie -> rejected by the session gate.
        let no_cookie = Request::builder()
            .uri("/api/health")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            status_of(no_cookie).await,
            StatusCode::UNAUTHORIZED,
            "/api/* must require the session cookie"
        );

        // Same route with the correct cookie -> gate passes (health needs no
        // index, so the handler returns 200).
        let with_cookie = Request::builder()
            .uri("/api/health")
            .header(header::COOKIE, format!("{SESSION_COOKIE}={token}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            status_of(with_cookie).await,
            StatusCode::OK,
            "/api/* must pass with the correct session cookie"
        );

        // /auth handshake is reachable WITHOUT a cookie (it mints one). Correct
        // token -> redirect + Set-Cookie, never a session 401.
        let auth_ok = Request::builder()
            .uri(format!("/auth?token={token}"))
            .body(Body::empty())
            .unwrap();
        let auth_ok_status = status_of(auth_ok).await;
        assert!(
            auth_ok_status.is_redirection(),
            "/auth with the correct token should redirect to mint the cookie, got {auth_ok_status}"
        );

        // Wrong token at /auth -> rejected by the handshake (403), not the gate.
        let auth_bad = Request::builder()
            .uri("/auth?token=wrong")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            status_of(auth_bad).await,
            StatusCode::FORBIDDEN,
            "/auth with a wrong token must be rejected by the handshake"
        );

        // The SPA fallback must load WITHOUT a cookie so the browser can reach
        // /auth. It is 200 when the bundle is embedded or 404 in a bare test
        // build, but must never be a session 401.
        let spa = Request::builder().uri("/").body(Body::empty()).unwrap();
        let spa_status = status_of(spa).await;
        assert_ne!(
            spa_status,
            StatusCode::UNAUTHORIZED,
            "the SPA fallback must not be session-gated, got {spa_status}"
        );
    }
}
