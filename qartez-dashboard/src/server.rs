//! axum router assembly and the bind/serve loop.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::Redirect;
use axum::routing::{get, post};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use serde::Deserialize;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower_http::trace::TraceLayer;

use crate::api;
use crate::auth;
use crate::state::AppState;
use crate::ws;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(api::health::handler))
        .route("/api/project", get(api::project::handler))
        .route("/api/focused-file", get(api::focused_file::handler))
        .route("/api/graph", get(api::graph::handler))
        .route("/api/symbol-graph", get(api::symbol_graph::handler))
        .route("/api/focused-symbol", get(api::focused_symbol::handler))
        .route("/api/symbol-search", get(api::symbol_search::handler))
        .route("/api/symbol-cochanges", get(api::symbol_cochanges::handler))
        .route("/api/graph-diff", get(api::graph_diff::handler))
        .route("/api/hotspots", get(api::hotspots::handler))
        .route("/api/smells", get(api::smells::handler))
        .route("/api/clones", get(api::clones::handler))
        .route("/api/dead-code", get(api::dead_code::handler))
        .route("/api/project-health", get(api::project_health::handler))
        .route("/api/shutdown", post(api::shutdown::handler))
        .route("/api/reindex", post(api::reindex::handler))
        .route("/auth", get(auth_handshake))
        .route("/ws", get(ws::handler))
        .fallback(get(crate::embed::static_handler))
        .layer(middleware::from_fn(auth::require_loopback_origin))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[derive(Debug, Deserialize)]
struct AuthQuery {
    token: String,
}

async fn auth_handshake(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<AuthQuery>,
) -> Result<(CookieJar, Redirect), StatusCode> {
    if !constant_time_eq(query.token.as_bytes(), state.auth_token().as_bytes()) {
        return Err(StatusCode::FORBIDDEN);
    }
    let cookie = Cookie::build((auth::SESSION_COOKIE, state.auth_token().to_string()))
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Strict)
        .path("/")
        .build();
    Ok((jar.add(cookie), Redirect::to("/")))
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Bind to `127.0.0.1:<port>` and return the listener. When `port` is `None`
/// or the requested port is already in use, falls back to an OS-assigned
/// ephemeral port and emits a `tracing::warn!` so callers can persist the
/// actual port before serving.
pub async fn bind(port: Option<u16>) -> Result<TcpListener> {
    if let Some(requested) = port {
        match TcpListener::bind(("127.0.0.1", requested)).await {
            Ok(listener) => return Ok(listener),
            Err(error) => {
                tracing::warn!(
                    requested,
                    %error,
                    "dashboard.bind.fallback_to_ephemeral"
                );
            }
        }
    }
    TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("binding 127.0.0.1:0")
}

/// Serve the dashboard until `shutdown` is cancelled.
pub async fn serve(
    listener: TcpListener,
    state: AppState,
    shutdown: CancellationToken,
) -> Result<()> {
    let local_addr: SocketAddr = listener.local_addr()?;
    tracing::info!(%local_addr, "dashboard.serve.start");
    let app = router(state);
    let cancel = shutdown.clone();
    axum::serve(listener, app)
        .with_graceful_shutdown(async move { cancel.cancelled().await })
        .await
        .context("axum::serve")?;
    tracing::info!("dashboard.serve.stopped");
    Ok(())
}
