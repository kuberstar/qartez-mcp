//! `POST /api/shutdown` - request graceful daemon termination.
//!
//! Cancels the shared `CancellationToken` carried by `AppState`. The
//! `serve()` future awaits this token via `with_graceful_shutdown`, so
//! cancellation drains in-flight requests and exits the process.
//!
//! Authentication and same-origin are already enforced by the router-level
//! middleware, so this handler does no extra guarding. The browser sees
//! `202 Accepted` immediately; the actual shutdown happens asynchronously.

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;

use crate::state::AppState;

/// Response body for `POST /api/shutdown`. The `ok` flag is always `true`
/// when this handler returns; failures would have already short-circuited
/// in middleware.
#[derive(Debug, Serialize)]
pub struct ShutdownAck {
    pub ok: bool,
}

/// Handle `POST /api/shutdown`.
///
/// Returns `202 Accepted` after signalling the shutdown token. Cancellation
/// is fire-and-forget; this handler does not wait for the runtime to drain.
pub async fn handler(State(state): State<AppState>) -> (StatusCode, Json<ShutdownAck>) {
    tracing::info!("shutdown.requested");
    state.shutdown().cancel();
    (StatusCode::ACCEPTED, Json(ShutdownAck { ok: true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ack_serializes_ok_true() {
        let ack = ShutdownAck { ok: true };
        let rendered = serde_json::to_string(&ack).expect("serialize");
        assert_eq!(rendered, r#"{"ok":true}"#);
    }
}
