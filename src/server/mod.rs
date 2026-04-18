#![allow(unused_imports)]

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    Annotated, CallToolResult, Content, ErrorData, GetPromptRequestParams, GetPromptResult,
    Implementation, ListPromptsResult, ListResourcesResult, ListToolsResult,
    PaginatedRequestParams, RawResource, ReadResourceRequestParams, ReadResourceResult,
    ResourceContents, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, prompt_handler, tool, tool_handler, tool_router};

mod cache;
mod helpers;
mod overview;
mod params;
mod prompts;
mod tiers;
mod tools;
mod treesitter;

use cache::ParseCache;
use helpers::*;
use params::*;
use treesitter::*;

use rusqlite::Connection;

use crate::graph::blast;
use crate::guard;
use crate::storage::read;
use crate::storage::read::sanitize_fts_query;
use crate::toolchain;

#[derive(Clone)]
pub struct QartezServer {
    db: Arc<Mutex<Connection>>,
    project_root: PathBuf,
    project_roots: Arc<RwLock<Vec<PathBuf>>>,
    root_aliases: Arc<RwLock<HashMap<PathBuf, String>>>,
    git_depth: u32,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
    parse_cache: Arc<Mutex<ParseCache>>,
    enabled_tools: tiers::EnabledTools,
}

impl QartezServer {
    pub fn new(conn: Connection, project_root: PathBuf, git_depth: u32) -> Self {
        let project_roots = vec![project_root.clone()];
        Self::with_roots(conn, project_root, project_roots, HashMap::new(), git_depth)
    }

    pub fn with_roots(
        conn: Connection,
        project_root: PathBuf,
        project_roots: Vec<PathBuf>,
        root_aliases: HashMap<PathBuf, String>,
        git_depth: u32,
    ) -> Self {
        // Self-heal the body FTS index. Existing `.qartez/index.db` files
        // built before the schema-migration fix have an empty
        // `symbols_body_fts` because the old migration wiped it on every
        // open. qartez_refs and qartez_rename need it populated to find call
        // sites in files with no direct import edge (external-crate `use`,
        // Rust module-form `use` resolving to `mod.rs`, child modules via
        // `use super::*;`). A one-time rebuild on startup is cheap - it
        // reads each indexed file once and inserts a row per symbol body
        // - and subsequent opens short-circuit via the count check.
        let body_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbols_body_fts", [], |row| {
                row.get(0)
            })
            .unwrap_or(0);
        let symbol_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM symbols", [], |row| row.get(0))
            .unwrap_or(0);
        if body_count == 0
            && symbol_count > 0
            && let Err(e) =
                crate::storage::write::rebuild_symbol_bodies_multi(&conn, &project_roots)
        {
            tracing::warn!("failed to rebuild symbols_body_fts on server start: {e}");
        }

        let router = Self::tool_router();
        let all_names: Vec<String> = router
            .list_all()
            .iter()
            .map(|t| t.name.to_string())
            .collect();
        let enabled_tools = tiers::initial_enabled_tools(&all_names);

        Self {
            db: Arc::new(Mutex::new(conn)),
            project_root,
            project_roots: Arc::new(RwLock::new(project_roots)),
            root_aliases: Arc::new(RwLock::new(root_aliases)),
            git_depth,
            tool_router: router,
            parse_cache: Arc::new(Mutex::new(ParseCache::default())),
            enabled_tools,
        }
    }

    /// Resolve a user-supplied relative path against the project root(s),
    /// rejecting absolute paths and directory traversal beyond the root.
    fn safe_resolve(&self, user_path: &str) -> Result<PathBuf, String> {
        let path = std::path::Path::new(user_path);
        if path.is_absolute() {
            return Err(format!(
                "Path '{user_path}' must be relative to the project root"
            ));
        }
        let mut depth: isize = 0;
        for component in path.components() {
            match component {
                std::path::Component::ParentDir => {
                    depth -= 1;
                    if depth < 0 {
                        return Err(format!("Path '{user_path}' escapes the project root"));
                    }
                }
                std::path::Component::Normal(_) => {
                    depth += 1;
                }
                std::path::Component::CurDir => {}
                _ => {
                    return Err(format!(
                        "Path '{user_path}' must be relative to the project root"
                    ));
                }
            }
        }

        let roots = self.project_roots.read().map_err(|e| e.to_string())?;
        let aliases = self.root_aliases.read().map_err(|e| e.to_string())?;

        if let Some(resolved) = helpers::resolve_prefixed_path(path, &roots, &aliases) {
            return Ok(resolved);
        }

        Ok(self.project_root.join(user_path))
    }

    /// Acquire the server's shared SQLite connection under its mutex.
    /// Panics on lock poison, matching `M-PANIC-ON-BUG`.
    #[allow(dead_code)]
    pub fn db_connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.db.lock().expect("qartez db mutex poisoned")
    }

    /// Clone the shared database handle for use by background tasks (e.g.
    /// the file watcher).
    pub fn db_arc(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.db)
    }
}

/// Dispatch a named tool call to its corresponding method.
///
/// Each entry in `infallible` routes to a method returning `String`; each
/// entry in `fallible` routes to a method returning `Result<String, String>`.
/// Wrapping the whole dispatch in a macro keeps `call_tool_by_name` free of
/// the 30-way `match`: each arm is one decision point, so inlining them
/// pushed cyclomatic complexity to 62 for what is mechanical boilerplate.
macro_rules! dispatch_tool_call {
    (
        $self:ident, $name:expr, $args:expr,
        infallible { $( $iname:literal => $imethod:ident : $iparams:ty ),* $(,)? }
        fallible   { $( $fname:literal => $fmethod:ident : $fparams:ty ),* $(,)? }
    ) => {
        match $name {
            $(
                $iname => {
                    let p: $iparams =
                        serde_json::from_value($args).map_err(|e| e.to_string())?;
                    Ok($self.$imethod(Parameters(p)))
                }
            )*
            $(
                $fname => {
                    let p: $fparams =
                        serde_json::from_value($args).map_err(|e| e.to_string())?;
                    $self.$fmethod(Parameters(p))
                }
            )*
            "qartez_tools" => {
                Err("qartez_tools is async-only (not available in benchmark mode)".to_owned())
            }
            other => Err(format!("unknown tool: {other}")),
        }
    };
}

impl QartezServer {
    /// Dispatch a tool call by name with JSON arguments.
    ///
    /// Provides a single in-process entry point so the CLI and benchmark
    /// harness can invoke any tool without going through the rmcp stdio
    /// transport.
    pub fn call_tool_by_name(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> std::result::Result<String, String> {
        let args = if args.is_null() {
            serde_json::json!({})
        } else {
            args
        };
        dispatch_tool_call!(self, name, args,
            infallible {
                "qartez_map" => qartez_map: QartezParams,
            }
            fallible {
                "qartez_find"        => qartez_find:        SoulFindParams,
                "qartez_workspace"   => qartez_workspace:   SoulWorkspaceParams,
                "qartez_read"        => qartez_read:        SoulReadParams,
                "qartez_impact"      => qartez_impact:      SoulImpactParams,
                "qartez_diff_impact" => qartez_diff_impact: SoulDiffImpactParams,
                "qartez_cochange"    => qartez_cochange:    SoulCochangeParams,
                "qartez_grep"        => qartez_grep:        SoulGrepParams,
                "qartez_unused"      => qartez_unused:      SoulUnusedParams,
                "qartez_refs"        => qartez_refs:        SoulRefsParams,
                "qartez_rename"      => qartez_rename:      SoulRenameParams,
                "qartez_project"     => qartez_project:     SoulProjectParams,
                "qartez_move"        => qartez_move:        SoulMoveParams,
                "qartez_rename_file" => qartez_rename_file: SoulRenameFileParams,
                "qartez_outline"     => qartez_outline:     SoulOutlineParams,
                "qartez_deps"        => qartez_deps:        SoulDepsParams,
                "qartez_stats"       => qartez_stats:       SoulStatsParams,
                "qartez_calls"       => qartez_calls:       SoulCallsParams,
                "qartez_context"     => qartez_context:     SoulContextParams,
                "qartez_wiki"        => qartez_wiki:        SoulWikiParams,
                "qartez_hotspots"    => qartez_hotspots:    SoulHotspotsParams,
                "qartez_clones"      => qartez_clones:      SoulClonesParams,
                "qartez_smells"      => qartez_smells:      SoulSmellsParams,
                "qartez_test_gaps"   => qartez_test_gaps:   SoulTestGapsParams,
                "qartez_boundaries"  => qartez_boundaries:  SoulBoundariesParams,
                "qartez_hierarchy"   => qartez_hierarchy:   SoulHierarchyParams,
                "qartez_trend"       => qartez_trend:       SoulTrendParams,
                "qartez_security"    => qartez_security:    SoulSecurityParams,
                "qartez_semantic"    => qartez_semantic:    SemanticParams,
                "qartez_knowledge"   => qartez_knowledge:   SoulKnowledgeParams,
            }
        )
    }
}

#[tool_handler]
#[prompt_handler]
impl ServerHandler for QartezServer {
    fn get_info(&self) -> ServerInfo {
        let mut caps = ServerCapabilities::builder().enable_tools();
        if tiers::is_progressive_mode() {
            caps = caps.enable_tool_list_changed();
        }
        let caps = caps.enable_prompts().enable_resources().build();
        ServerInfo::new(caps)
            .with_server_info(Implementation::new("qartez-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions(include_str!("mcp_instructions.md"))
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListToolsResult, ErrorData>> + Send + '_ {
        let enabled = self
            .enabled_tools
            .read()
            .expect("enabled_tools lock poisoned");
        let tools = self
            .tool_router
            .list_all()
            .into_iter()
            .filter(|t| enabled.contains(t.name.as_ref()))
            .collect();
        std::future::ready(Ok(ListToolsResult {
            tools,
            meta: None,
            next_cursor: None,
        }))
    }

    fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ListResourcesResult, ErrorData>> + Send + '_ {
        let overview = Annotated {
            raw: RawResource {
                uri: "qartez://overview".to_string(),
                name: "Codebase Overview".to_string(),
                title: Some("Qartez Codebase Overview".to_string()),
                description: Some(
                    "Ranked overview of the most important files, symbols, and dependency structure"
                        .to_string(),
                ),
                mime_type: Some("text/plain".to_string()),
                size: None,
                icons: None,
                meta: None,
            },
            annotations: None,
        };
        let hotspots = Annotated {
            raw: RawResource {
                uri: "qartez://hotspots".to_string(),
                name: "Hotspots".to_string(),
                title: Some("Code Hotspots".to_string()),
                description: Some(
                    "Top files ranked by composite score (complexity x coupling x change frequency)"
                        .to_string(),
                ),
                mime_type: Some("text/plain".to_string()),
                size: None,
                icons: None,
                meta: None,
            },
            annotations: None,
        };
        let stats = Annotated {
            raw: RawResource {
                uri: "qartez://stats".to_string(),
                name: "Project Stats".to_string(),
                title: Some("Project Statistics".to_string()),
                description: Some(
                    "File counts, LOC, symbol counts, language breakdown, and top imported files"
                        .to_string(),
                ),
                mime_type: Some("text/plain".to_string()),
                size: None,
                icons: None,
                meta: None,
            },
            annotations: None,
        };
        std::future::ready(Ok(ListResourcesResult {
            meta: None,
            resources: vec![overview, hotspots, stats],
            next_cursor: None,
        }))
    }

    fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: rmcp::service::RequestContext<rmcp::RoleServer>,
    ) -> impl std::future::Future<Output = Result<ReadResourceResult, ErrorData>> + Send + '_ {
        let result = match request.uri.as_str() {
            "qartez://overview" => {
                let text = self.build_overview(20, 4000, None, None, false, false);
                Ok(ReadResourceResult::new(vec![ResourceContents::text(
                    text,
                    "qartez://overview",
                )]))
            }
            "qartez://hotspots" => {
                let params = SoulHotspotsParams {
                    limit: Some(15),
                    level: Some(HotspotLevel::File),
                    format: Some(Format::Concise),
                    ..Default::default()
                };
                let text = self
                    .qartez_hotspots(Parameters(params))
                    .unwrap_or_else(|e| format!("Error: {e}"));
                Ok(ReadResourceResult::new(vec![ResourceContents::text(
                    text,
                    "qartez://hotspots",
                )]))
            }
            "qartez://stats" => {
                let params = SoulStatsParams { file_path: None };
                let text = self
                    .qartez_stats(Parameters(params))
                    .unwrap_or_else(|e| format!("Error: {e}"));
                Ok(ReadResourceResult::new(vec![ResourceContents::text(
                    text,
                    "qartez://stats",
                )]))
            }
            _ => Err(ErrorData::resource_not_found(
                format!("Unknown resource URI: {}", request.uri),
                None,
            )),
        };
        std::future::ready(result)
    }
}

#[cfg(test)]
mod progressive_tests {
    use super::*;
    use rusqlite::Connection;

    fn test_server() -> QartezServer {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::schema::create_schema(&conn).unwrap();
        QartezServer::new(conn, std::path::PathBuf::from("/tmp/test"), 0)
    }

    #[test]
    fn tool_router_includes_qartez_tools() {
        let server = test_server();
        let all = server.tool_router.list_all();
        let names: Vec<&str> = all.iter().map(|t| t.name.as_ref()).collect();
        assert!(
            names.contains(&"qartez_tools"),
            "qartez_tools not in router: {names:?}"
        );
    }

    #[test]
    fn default_mode_enables_all_tools() {
        let server = test_server();
        let enabled = server.enabled_tools.read().unwrap();
        let all = server.tool_router.list_all();
        for tool in &all {
            assert!(
                enabled.contains(tool.name.as_ref()),
                "{} not enabled in default mode",
                tool.name
            );
        }
    }

    #[test]
    fn enabled_tools_always_include_meta_tool() {
        let server = test_server();
        let enabled = server.enabled_tools.read().unwrap();
        assert!(enabled.contains("qartez_tools"));
    }

    #[test]
    fn tier_constants_cover_all_router_tools() {
        let server = test_server();
        let all = server.tool_router.list_all();
        let all_names: HashSet<&str> = all.iter().map(|t| t.name.as_ref()).collect();

        let mut tiered: HashSet<&str> = HashSet::new();
        for &name in tiers::TIER_CORE {
            tiered.insert(name);
        }
        for &name in tiers::TIER_ANALYSIS {
            tiered.insert(name);
        }
        for &name in tiers::TIER_REFACTOR {
            tiered.insert(name);
        }
        for &name in tiers::TIER_META {
            tiered.insert(name);
        }
        tiered.insert(tiers::META_TOOL_NAME);

        for name in &all_names {
            assert!(tiered.contains(name), "tool {name} is not in any tier");
        }
        for name in &tiered {
            assert!(
                all_names.contains(name),
                "tiered tool {name} is not in router"
            );
        }
    }

    #[test]
    fn total_tool_count_is_31() {
        let server = test_server();
        let all = server.tool_router.list_all();
        assert_eq!(all.len(), 31, "expected 31 tools, got {}", all.len());
    }

    #[test]
    fn tier_sizes_are_correct() {
        assert_eq!(tiers::TIER_CORE.len(), 9, "core tier");
        assert_eq!(tiers::TIER_ANALYSIS.len(), 16, "analysis tier");
        assert_eq!(tiers::TIER_REFACTOR.len(), 3, "refactor tier");
        assert_eq!(tiers::TIER_META.len(), 2, "meta tier");
    }

    #[cfg(feature = "benchmark")]
    #[test]
    fn call_tool_by_name_knows_qartez_tools() {
        let server = test_server();
        let result = server.call_tool_by_name("qartez_tools", serde_json::json!({}));
        assert!(result.is_err());
        assert!(
            result.unwrap_err().contains("async-only"),
            "should return async-only error, not unknown-tool"
        );
    }

    #[test]
    fn call_tool_by_name_handles_every_router_tool() {
        // Guards against drift between the per-tool router (which #[tool_handler]
        // dispatches) and the call_tool_by_name match (which the benchmark
        // harness and CLI use). If a new tool is added to a per-tool module
        // but its arm is missing from call_tool_by_name, this test fails:
        // an unhandled tool reaches the `_ => Err("unknown tool: ...")`
        // catch-all instead of its real handler.
        let server = test_server();
        for tool in server.tool_router.list_all() {
            let result = server.call_tool_by_name(tool.name.as_ref(), serde_json::json!({}));
            // Tools may fail on empty args (missing required params) but must
            // never return the catch-all "unknown tool" error.
            if let Err(msg) = &result {
                assert!(
                    !msg.starts_with("unknown tool:"),
                    "{} reached the unknown-tool catch-all in call_tool_by_name",
                    tool.name,
                );
            }
        }
    }

    #[test]
    fn call_tool_by_name_rejects_unknown_tool_with_name() {
        // The dispatch macro's catch-all formats "unknown tool: {name}". Verify
        // the literal name reaches the error so users can debug typos.
        let server = test_server();
        let result = server.call_tool_by_name("qartez_nonexistent_tool", serde_json::json!({}));
        let err = result.expect_err("unknown tool must error");
        assert_eq!(err, "unknown tool: qartez_nonexistent_tool");
    }

    #[test]
    fn call_tool_by_name_treats_null_args_as_empty_object() {
        // Pre-refactor a closure normalised null -> {}. Post-refactor a let
        // binding does the same. Verify both paths reach a real handler
        // instead of bombing in serde_json::from_value with a null error.
        let server = test_server();
        let null_result = server.call_tool_by_name("qartez_map", serde_json::Value::Null);
        let empty_result = server.call_tool_by_name("qartez_map", serde_json::json!({}));
        // qartez_map is infallible (returns String). Both should produce Ok
        // with identical content given the same inputs.
        assert!(
            null_result.is_ok(),
            "null args should normalise: {null_result:?}"
        );
        assert!(empty_result.is_ok());
        assert_eq!(null_result.unwrap(), empty_result.unwrap());
    }

    #[test]
    fn call_tool_by_name_propagates_deserialization_error() {
        // When args have the right shape but wrong types, serde_json::from_value
        // fails. The dispatch macro converts that error to a string via .map_err.
        let server = test_server();
        // qartez_find requires `name: String`. Pass an integer to force a
        // serde_json type error.
        let bad_args = serde_json::json!({ "name": 12345 });
        let result = server.call_tool_by_name("qartez_find", bad_args);
        let err = result.expect_err("type-mismatched args must error");
        // Must NOT hit the unknown-tool catch-all and must NOT panic. The
        // serde_json error mentions the offending field shape.
        assert!(!err.starts_with("unknown tool:"), "got catch-all: {err}");
    }

    #[test]
    fn call_tool_by_name_qartez_map_returns_ok() {
        // qartez_map is the only `infallible` arm in the dispatch macro: its
        // tool method returns String, and the macro wraps it in Ok. Verify
        // the wrapping is correct - i.e. we get Ok(String), not Err.
        let server = test_server();
        let result = server.call_tool_by_name("qartez_map", serde_json::json!({}));
        assert!(
            result.is_ok(),
            "qartez_map must succeed with empty params: {result:?}"
        );
    }
}

#[cfg(test)]
mod safe_resolve_tests {
    use super::*;
    use rusqlite::Connection;

    fn dummy_server(root: &std::path::Path) -> QartezServer {
        let conn = Connection::open_in_memory().unwrap();
        crate::storage::schema::create_schema(&conn).unwrap();
        QartezServer::new(conn, root.to_path_buf(), 0)
    }

    #[test]
    fn accepts_plain_relative_path() {
        let server = dummy_server(std::path::Path::new("/tmp/project"));
        let result = server.safe_resolve("src/main.rs");
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            std::path::PathBuf::from("/tmp/project/src/main.rs")
        );
    }

    #[test]
    fn rejects_absolute_path() {
        let server = dummy_server(std::path::Path::new("/tmp/project"));
        let result = server.safe_resolve("/etc/passwd");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must be relative"));
    }

    #[test]
    fn rejects_traversal_beyond_root() {
        let server = dummy_server(std::path::Path::new("/tmp/project"));
        let result = server.safe_resolve("../../../etc/passwd");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("escapes"));
    }

    #[test]
    fn rejects_sneaky_traversal() {
        let server = dummy_server(std::path::Path::new("/tmp/project"));
        let result = server.safe_resolve("src/../../secret");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("escapes"));
    }

    #[test]
    fn allows_internal_parent_within_root() {
        let server = dummy_server(std::path::Path::new("/tmp/project"));
        let result = server.safe_resolve("src/../lib/mod.rs");
        assert!(result.is_ok());
    }

    #[test]
    fn rejects_single_parent_dir() {
        let server = dummy_server(std::path::Path::new("/tmp/project"));
        let result = server.safe_resolve("../sibling/file.rs");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("escapes"));
    }
}

#[cfg(test)]
#[allow(
    clippy::needless_update,
    reason = "test constructions use ..Default::default() uniformly so future field additions don't require touching every site"
)]
mod quality_tests;

#[cfg(test)]
mod prompt_tests;
