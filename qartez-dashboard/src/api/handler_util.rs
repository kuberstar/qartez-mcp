//! Shared handler plumbing for the paged `/api/*` endpoints.
//!
//! Several endpoints share the exact same shape: clamp the limit, snapshot
//! the project root, run a blocking SQLite computation off the async runtime,
//! and map both the join panic and the query error onto a `500` with a JSON
//! envelope. Only the response type, the compute function, and the tracing
//! label differ. `run_blocking` captures that boilerplate so each handler is
//! a single delegating call.

use std::path::{Path, PathBuf};

use axum::Json;
use axum::http::StatusCode;
use serde::Serialize;

/// JSON error envelope returned on `500`.
#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: &'static str,
}

/// Run `f` on a blocking thread and map its outcome to an HTTP result.
///
/// The `root` snapshot and clamped `limit` are handed to `f`, which performs
/// the (synchronous, SQLite-backed) computation. A join panic and a query
/// error both become `500 Internal Server Error` with a JSON `ApiError`;
/// `label` is interpolated into the tracing events (`<label>.join.failed`
/// and `<label>.query.failed`) so logs still identify the endpoint.
///
/// # Errors
///
/// Returns `500` when the blocking task panics or when `f` returns an error.
pub async fn run_blocking<T, F>(
    root: PathBuf,
    limit: i64,
    label: &'static str,
    f: F,
) -> Result<Json<T>, (StatusCode, Json<ApiError>)>
where
    T: Serialize + Send + 'static,
    F: FnOnce(&Path, i64) -> anyhow::Result<T> + Send + 'static,
{
    let result = tokio::task::spawn_blocking(move || f(&root, limit))
        .await
        .map_err(|error| {
            tracing::error!(?error, "{label}.join.failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError {
                    error: "join error",
                }),
            )
        })?;

    match result {
        Ok(response) => Ok(Json(response)),
        Err(error) => {
            tracing::error!(?error, "{label}.query.failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError { error: "internal" }),
            ))
        }
    }
}
