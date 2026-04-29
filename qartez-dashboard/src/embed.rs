// Rust guideline compliant 2026-04-26
//! Embedded SvelteKit bundle and the SPA fallback handler.

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "$CARGO_MANIFEST_DIR/web/build/"]
pub(crate) struct Web;

pub async fn static_handler(uri: Uri, headers: HeaderMap) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };

    if let Some(file) = Web::get(path) {
        return serve_embedded(path, &file, &headers);
    }

    if let Some(file) = Web::get("index.html") {
        return serve_embedded("index.html", &file, &headers);
    }

    (StatusCode::NOT_FOUND, "qartez dashboard bundle missing").into_response()
}

fn serve_embedded(
    path: &str,
    file: &rust_embed::EmbeddedFile,
    req_headers: &HeaderMap,
) -> Response {
    let etag = format!("\"{}\"", hex::encode(file.metadata.sha256_hash()));

    if let Some(if_none_match) = req_headers
        .get(header::IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        && if_none_match == etag
    {
        return Response::builder()
            .status(StatusCode::NOT_MODIFIED)
            .header(header::ETAG, etag)
            .body(Body::empty())
            .unwrap();
    }

    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let cache_control = if path.starts_with("_app/immutable/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, mime.as_ref())
        .header(header::ETAG, etag)
        .header(header::CACHE_CONTROL, cache_control)
        .body(Body::from(file.data.clone().into_owned()))
        .unwrap()
}
