//! `/ws` WebSocket endpoint.
//!
//! Per-connection task pattern:
//! - Accept the upgrade after validating Origin (CORS does not run for upgrades).
//! - Subscribe to `AppState::events` (broadcast channel).
//! - `tokio::select!` between (a) outgoing broadcast messages, (b) incoming
//!   client frames (currently only `pong` and connection close), (c) the
//!   shutdown cancellation token.
//! - Handle `RecvError::Lagged` by continuing rather than disconnecting.

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode, header};
use tokio::sync::broadcast::error::RecvError;

use crate::auth;
use crate::state::{AppState, Event};

pub async fn handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<axum::response::Response, StatusCode> {
    if !auth::origin_is_allowed(headers.get(header::ORIGIN)) {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(ws.on_upgrade(move |socket| run(socket, state)))
}

async fn run(mut socket: WebSocket, state: AppState) {
    let mut rx = state.subscribe();
    let shutdown = state.shutdown();

    loop {
        tokio::select! {
            biased;

            _ = shutdown.cancelled() => {
                let _ = socket.send(Message::Close(None)).await;
                return;
            }

            event = rx.recv() => {
                match event {
                    Ok(ev) => {
                        let payload = match serde_json::to_string(&ev) {
                            Ok(s) => s,
                            Err(error) => {
                                tracing::warn!(?error, "ws.serialize.failed");
                                continue;
                            }
                        };
                        if socket.send(Message::Text(payload.into())).await.is_err() {
                            return;
                        }
                    }
                    Err(RecvError::Lagged(skipped)) => {
                        tracing::warn!(skipped, "ws.broadcast.lagged");
                        continue;
                    }
                    Err(RecvError::Closed) => return,
                }
            }

            client = socket.recv() => {
                match client {
                    Some(Ok(Message::Close(_))) | None => return,
                    Some(Ok(_)) => continue,
                    Some(Err(error)) => {
                        tracing::debug!(?error, "ws.recv.error");
                        return;
                    }
                }
            }
        }
    }
}

/// Helper to publish an `Event` from anywhere with access to `AppState`.
/// Drops silently when there are no subscribers; that is intentional.
pub fn broadcast(state: &AppState, event: Event) {
    let _ = state.events().send(event);
}
