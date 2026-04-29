//! `GET /api/health` - liveness probe used by the CLI `qartez dashboard status`.

use axum::Json;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Health {
    pub ok: bool,
    pub version: &'static str,
}

pub async fn handler() -> Json<Health> {
    Json(Health {
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
    })
}
