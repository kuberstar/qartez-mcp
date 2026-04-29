//! `GET /api/dead-code` - exported symbols with no detected importers
//! and no in-repo references.
//!
//! Reads the pre-populated `unused_exports` table written by the indexer
//! (see `populate_unused_exports` in `qartez-public/src/storage/write.rs`).
//! That table is the cross-checked output of "zero importers AND zero
//! symbol_refs", so the dashboard does not need to recompute it.
//!
//! When the table is missing - older index DB or a re-index that never
//! ran the unused-exports pass - the response is empty rather than 500.
//! Read-only: there is no delete affordance on the page.

use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

const DEFAULT_LIMIT: i64 = 1000;
const MAX_LIMIT: i64 = 5000;

#[derive(Debug, Deserialize)]
pub struct DeadCodeQuery {
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct DeadCodeItem {
    pub id: i64,
    pub name: String,
    pub kind: String,
    pub path: String,
    pub language: String,
    pub line_start: i64,
    pub is_exported: bool,
}

#[derive(Debug, Serialize)]
pub struct DeadCodeResponse {
    pub items: Vec<DeadCodeItem>,
    pub indexed: bool,
    pub available: bool,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: &'static str,
}

pub async fn handler(
    State(state): State<AppState>,
    Query(query): Query<DeadCodeQuery>,
) -> Result<Json<DeadCodeResponse>, (StatusCode, Json<ApiError>)> {
    let limit = clamp_limit(query.limit);
    let root = state.project_root().to_path_buf();

    let result = tokio::task::spawn_blocking(move || compute_dead_code_at_root(&root, limit))
        .await
        .map_err(|error| {
            tracing::error!(?error, "dead_code.join.failed");
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
            tracing::error!(?error, "dead_code.query.failed");
            Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError { error: "internal" }),
            ))
        }
    }
}

fn clamp_limit(requested: Option<i64>) -> i64 {
    match requested {
        Some(value) if (1..=MAX_LIMIT).contains(&value) => value,
        _ => DEFAULT_LIMIT,
    }
}

fn compute_dead_code_at_root(root: &Path, limit: i64) -> anyhow::Result<DeadCodeResponse> {
    let db_path = default_db_path(root);
    if !db_path.exists() {
        return Ok(DeadCodeResponse {
            items: Vec::new(),
            indexed: false,
            available: false,
        });
    }
    let conn = Connection::open(&db_path)?;
    if !table_exists(&conn, "unused_exports")? {
        return Ok(DeadCodeResponse {
            items: Vec::new(),
            indexed: true,
            available: false,
        });
    }
    let items = compute_dead_code(&conn, limit)?;
    Ok(DeadCodeResponse {
        items,
        indexed: true,
        available: true,
    })
}

pub(crate) fn compute_dead_code(
    conn: &Connection,
    limit: i64,
) -> anyhow::Result<Vec<DeadCodeItem>> {
    // The framework-convention filter applied below can drop rows from the
    // SQL page, which would leave the response with fewer than `limit` items
    // even when more genuine dead exports exist further down. Fetching one
    // batch larger than the cap and then truncating keeps the rendered list
    // close to `limit` for typical projects without forcing the dashboard to
    // page itself the way the MCP tool does.
    let fetch_cap = limit.saturating_mul(2).max(limit);
    let sql = "SELECT s.id, s.name, s.kind, f.path, f.language,
                      s.line_start, s.is_exported
               FROM unused_exports ue
               JOIN symbols s ON s.id = ue.symbol_id
               JOIN files f ON f.id = ue.file_id
               ORDER BY f.path, s.line_start
               LIMIT ?1";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![fetch_cap], |r| {
        let is_exported: i64 = r.get(6)?;
        Ok(DeadCodeItem {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: r.get(2)?,
            path: r.get(3)?,
            language: r.get(4)?,
            line_start: r.get(5)?,
            is_exported: is_exported != 0,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        let item = row?;
        if is_framework_runtime_entry_path(&item.path) {
            continue;
        }
        out.push(item);
        if out.len() as i64 >= limit {
            break;
        }
    }
    Ok(out)
}

/// Directory prefixes whose exported symbols are loaded by an external
/// runtime (plugin host, CLI extension loader, IDE extension API) via
/// string lookup rather than a static import edge. The static reference
/// graph cannot observe the dynamic caller, so the symbol survives the
/// `unused_exports` materialization even when it is a live entry point.
/// Mirrors `qartez-mcp`'s `PLUGIN_ENTRY_DIR_PREFIXES` so the dashboard view
/// matches what `qartez_unused` shows. Paths in the index are forward-slash
/// normalized, so plain `str::starts_with` works on every platform.
const PLUGIN_ENTRY_DIR_PREFIXES: &[&str] = &["scripts/", "plugins/", "extensions/"];

/// Mirror of `is_framework_runtime_entry_path` from the MCP `unused.rs`.
/// Duplicated rather than imported because the dashboard crate cannot depend
/// on `qartez-mcp` without forming a workspace cycle. Kept in lockstep by
/// hand: any new entry-point convention added there must be added here too.
fn is_framework_runtime_entry_path(path: &str) -> bool {
    if PLUGIN_ENTRY_DIR_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return true;
    }
    let basename = path.rsplit('/').next().unwrap_or(path);
    is_plugin_entry_basename(basename) || is_framework_convention_basename(basename)
}

/// Match `plugin.<ext>`, `extension.<ext>`, `*-plugin.<ext>`, or
/// `*-extension.<ext>` where `<ext>` is any single dotless suffix. The
/// single-extension constraint blocks accidental matches like
/// `plugin.bak.ts` that would otherwise look like a plugin module.
fn is_plugin_entry_basename(name: &str) -> bool {
    let Some((stem, ext)) = name.rsplit_once('.') else {
        return false;
    };
    if ext.is_empty() || ext.contains('.') || stem.contains('.') {
        return false;
    }
    stem == "plugin"
        || stem == "extension"
        || stem.ends_with("-plugin")
        || stem.ends_with("-extension")
}

/// Match SvelteKit and adjacent meta-framework convention entry-points.
/// Mirrors the regex set in `framework_convention_basename_patterns`:
/// `+page.<ext>`, `+page.(server|client).<ext>`, `+layout.<ext>`,
/// `+layout.(server|client).<ext>`, `+server.<ext>`, `+error.<ext>`,
/// `hooks.(server|client).<ext>`, `svelte.config.<ext>`, `vite.config.<ext>`,
/// `playwright.config.<ext>`. The two-dot variants are matched explicitly so
/// stray names like `+page.bak.ts` do not slip through.
fn is_framework_convention_basename(name: &str) -> bool {
    let parts: Vec<&str> = name.split('.').collect();
    match parts.as_slice() {
        [stem, ext] if !ext.is_empty() => {
            matches!(*stem, "+page" | "+layout" | "+server" | "+error")
        }
        [stem, mid, ext] if !ext.is_empty() => {
            let route_pair =
                matches!(*stem, "+page" | "+layout") && matches!(*mid, "server" | "client");
            let hooks_pair = *stem == "hooks" && matches!(*mid, "server" | "client");
            let config_pair = matches!(*stem, "svelte" | "vite" | "playwright") && *mid == "config";
            route_pair || hooks_pair || config_pair
        }
        _ => false,
    }
}

fn table_exists(conn: &Connection, table: &str) -> anyhow::Result<bool> {
    let exists: Option<String> = conn
        .query_row(
            "SELECT name FROM sqlite_master WHERE type='table' AND name=?1",
            params![table],
            |r| r.get(0),
        )
        .optional()?;
    Ok(exists.is_some())
}

fn default_db_path(root: &Path) -> PathBuf {
    root.join(".qartez").join("index.db")
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_SCHEMA: &str = "
        CREATE TABLE files (
            id       INTEGER PRIMARY KEY AUTOINCREMENT,
            path     TEXT    NOT NULL UNIQUE,
            language TEXT    NOT NULL DEFAULT 'rust'
        );
        CREATE TABLE symbols (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            name        TEXT    NOT NULL,
            kind        TEXT    NOT NULL,
            line_start  INTEGER NOT NULL,
            line_end    INTEGER NOT NULL,
            is_exported INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE unused_exports (
            symbol_id INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
            file_id   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
            PRIMARY KEY (symbol_id)
        );
    ";

    fn fresh_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory sqlite");
        conn.execute_batch(TEST_SCHEMA).expect("create schema");
        conn
    }

    #[test]
    fn returns_unused_exports_in_path_order() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO files (id, path) VALUES (1, 'b.rs'), (2, 'a.rs')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols (id, file_id, name, kind, line_start, line_end, is_exported)
             VALUES (1, 1, 'used', 'function', 10, 15, 1),
                    (2, 1, 'unused1', 'function', 20, 25, 1),
                    (3, 2, 'unused2', 'function', 5, 10, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO unused_exports (symbol_id, file_id) VALUES (2, 1), (3, 2)",
            [],
        )
        .unwrap();

        let items = compute_dead_code(&conn, 100).expect("query ok");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].path, "a.rs");
        assert_eq!(items[0].name, "unused2");
        assert_eq!(items[1].path, "b.rs");
        assert_eq!(items[1].name, "unused1");
        assert!(items[0].is_exported);
    }

    #[test]
    fn missing_table_is_handled_at_root_layer() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path TEXT, language TEXT);
             CREATE TABLE symbols (id INTEGER PRIMARY KEY);",
        )
        .unwrap();
        assert!(!table_exists(&conn, "unused_exports").unwrap());
    }

    #[test]
    fn drops_framework_convention_entry_points() {
        let conn = fresh_db();
        conn.execute(
            "INSERT INTO files (id, path) VALUES
              (1, 'src/routes/+page.ts'),
              (2, 'src/routes/+page.server.ts'),
              (3, 'plugins/foo.ts'),
              (4, 'src/lib/dead.ts'),
              (5, 'vite.config.ts'),
              (6, 'hooks.server.ts')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO symbols (id, file_id, name, kind, line_start, line_end, is_exported)
             VALUES (1, 1, 'load', 'function', 1, 5, 1),
                    (2, 2, 'actions', 'const', 1, 5, 1),
                    (3, 3, 'plugin_export', 'function', 1, 5, 1),
                    (4, 4, 'really_dead', 'function', 1, 5, 1),
                    (5, 5, 'default', 'const', 1, 5, 1),
                    (6, 6, 'handle', 'function', 1, 5, 1)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO unused_exports (symbol_id, file_id)
             VALUES (1, 1), (2, 2), (3, 3), (4, 4), (5, 5), (6, 6)",
            [],
        )
        .unwrap();

        let items = compute_dead_code(&conn, 100).expect("query ok");
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["really_dead"],
            "framework-convention and plugin entry points must be hidden",
        );
    }

    #[test]
    fn framework_convention_basenames_match_or_skip() {
        // Hits.
        assert!(is_framework_convention_basename("+page.ts"));
        assert!(is_framework_convention_basename("+page.svelte"));
        assert!(is_framework_convention_basename("+page.server.ts"));
        assert!(is_framework_convention_basename("+page.client.ts"));
        assert!(is_framework_convention_basename("+layout.ts"));
        assert!(is_framework_convention_basename("+layout.server.ts"));
        assert!(is_framework_convention_basename("+server.ts"));
        assert!(is_framework_convention_basename("+error.ts"));
        assert!(is_framework_convention_basename("hooks.server.ts"));
        assert!(is_framework_convention_basename("hooks.client.ts"));
        assert!(is_framework_convention_basename("svelte.config.js"));
        assert!(is_framework_convention_basename("vite.config.ts"));
        assert!(is_framework_convention_basename("playwright.config.ts"));

        // Misses.
        assert!(!is_framework_convention_basename("hooks.ts"));
        assert!(!is_framework_convention_basename("page.ts"));
        assert!(!is_framework_convention_basename("+page.bak.ts"));
        assert!(!is_framework_convention_basename("regular.ts"));
        assert!(!is_framework_convention_basename(""));
        assert!(!is_framework_convention_basename("+page"));
    }

    #[test]
    fn plugin_entry_basenames_match_or_skip() {
        assert!(is_plugin_entry_basename("plugin.ts"));
        assert!(is_plugin_entry_basename("extension.ts"));
        assert!(is_plugin_entry_basename("foo-plugin.ts"));
        assert!(is_plugin_entry_basename("foo-extension.ts"));

        assert!(!is_plugin_entry_basename("plugin.bak.ts"));
        assert!(!is_plugin_entry_basename("plugins.ts"));
        assert!(!is_plugin_entry_basename("plugin"));
        assert!(!is_plugin_entry_basename(".ts"));
    }

    #[test]
    fn directory_prefixes_match() {
        assert!(is_framework_runtime_entry_path("scripts/install.ts"));
        assert!(is_framework_runtime_entry_path("plugins/foo.ts"));
        assert!(is_framework_runtime_entry_path("extensions/bar.ts"));
        assert!(!is_framework_runtime_entry_path("src/scripts/foo.ts"));
        assert!(!is_framework_runtime_entry_path("src/main.ts"));
    }
}
