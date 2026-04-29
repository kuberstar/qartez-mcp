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
use http::{HeaderValue, Request, StatusCode, header};
use rand::TryRngCore;

use crate::paths;

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
}
