#![allow(unused_imports)]

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, Content, ErrorData};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, tool, tool_router};

use super::super::QartezServer;
use super::super::helpers::{self, *};
use super::super::params::*;
use super::super::tiers;
use super::super::treesitter::*;

use crate::graph::blast;
use crate::guard;
use crate::storage::read;
use crate::storage::read::sanitize_fts_query;
use crate::toolchain;

#[tool_router(router = qartez_semantic_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_semantic",
        description = "Natural language code search. Finds symbols by meaning rather than exact keywords (e.g. 'authentication handler', 'database retry logic'). Combines vector similarity with keyword search via hybrid ranking. Two prerequisites: (1) the qartez binary must be built with `--features semantic` (clone https://github.com/kuberstar/qartez-mcp and run `cargo install --path . --features semantic`); a binary without that feature returns the rebuild command as an error. (2) Once built, run `qartez-setup` once to download the embedding model; a missing model errors with the run command.",
        annotations(
            title = "Semantic Search",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_semantic(
        &self,
        Parameters(params): Parameters<SemanticParams>,
    ) -> Result<String, String> {
        qartez_semantic_dispatch(self, params)
    }
}
#[cfg(feature = "semantic")]
fn qartez_semantic_dispatch(
    server: &QartezServer,
    params: SemanticParams,
) -> Result<String, String> {
    use std::sync::OnceLock;

    // Input validation runs before any model work so callers get a precise
    // error on empty queries instead of an encoded zero-length embedding
    // that falls through to a misleading "no matches" result.
    if params.query.trim().is_empty() {
        return Err("query must be non-empty".to_string());
    }

    // OnceLock caches the first result (success or failure) for the process
    // lifetime. If model loading fails (e.g., missing files), subsequent
    // calls return the cached error until the server is restarted.
    static MODEL: OnceLock<std::result::Result<crate::embeddings::EmbeddingModel, String>> =
        OnceLock::new();
    let result = MODEL.get_or_init(|| {
        let model_dir = match crate::embeddings::default_model_dir() {
            Some(d) => d,
            None => return Err("cannot determine home directory for model path".to_string()),
        };
        crate::embeddings::EmbeddingModel::load(&model_dir)
            .map_err(|e| format!("failed to load embedding model (run `qartez-setup`): {e}"))
    });
    let model = result.as_ref().map_err(|e| e.clone())?;

    let query_vec = model
        .encode_one(&params.query)
        .map_err(|e| format!("embedding encode failed: {e}"))?;

    let conn = server
        .db
        .lock()
        .map_err(|e| format!("DB lock error: {e}"))?;
    // `limit=0` means "no cap" project-wide convention; `None` keeps the
    // historical default of 10.
    let limit = match params.limit {
        None => 10_i64,
        Some(0) => i64::MAX,
        Some(n) => n as i64,
    };
    let concise = is_concise(&params.format);

    let results = read::hybrid_search(&conn, &params.query, &query_vec, limit)
        .map_err(|e| format!("search error: {e}"))?;

    if results.is_empty() {
        return Ok(format!(
            "No semantic matches for '{}'. Ensure embeddings are built (re-index with `semantic` feature).",
            params.query
        ));
    }

    let mut out = format!(
        "Found {} semantic match(es) for '{}':\n\n",
        results.len(),
        params.query,
    );

    for (rank, (sym, path, score)) in results.iter().enumerate() {
        if concise {
            let marker = if sym.is_exported { "+" } else { " " };
            out.push_str(&format!(
                " {marker} {} -- {} [L{}-L{}] score={:.3}\n",
                sym.name, path, sym.line_start, sym.line_end, score,
            ));
        } else {
            let sig = sym.signature.as_deref().unwrap_or("-");
            let exported = if sym.is_exported {
                "exported"
            } else {
                "private"
            };
            out.push_str(&format!(
                "  #{} {} ({}) -- score={:.3}\n  File: {} [L{}-L{}]\n  Signature: {}\n  Status: {}\n\n",
                rank + 1,
                sym.name,
                sym.kind,
                score,
                path,
                sym.line_start,
                sym.line_end,
                sig,
                exported,
            ));
        }
    }

    Ok(out)
}

#[cfg(not(feature = "semantic"))]
fn qartez_semantic_dispatch(
    _server: &QartezServer,
    params: SemanticParams,
) -> Result<String, String> {
    // Validate the query before reporting the missing-feature
    // diagnostic so an empty input surfaces the same precise error
    // the feature-enabled branch returns. Otherwise an empty query
    // in a feature-disabled build silently routes to "rebuild with
    // --features semantic", forcing the caller through a misleading
    // rebuild loop: they fix the binary, retry with the same empty
    // query, and only then see the real `query must be non-empty`
    // error.
    if params.query.trim().is_empty() {
        return Err("query must be non-empty".to_string());
    }
    // Two-step failure message so the caller knows which prerequisite
    // is missing. qartez_tools lists this tool as `[x] enabled` in the
    // tier index (the `#[tool]` attribute unconditionally registers
    // it), so the only signal a caller receives that the current
    // binary cannot actually run semantic search is this error.
    Err(
        "Semantic search is not available in this build. Rebuild with:\n  git clone https://github.com/kuberstar/qartez-mcp && cd qartez-mcp && cargo install --path . --features semantic\nAfter rebuilding, run `qartez-setup` once to download the embedding model."
            .to_string(),
    )
}
