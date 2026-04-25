//! Per-tool MCP handlers split from `server/mod.rs`. Each module contributes
//! one `#[tool_router(router = qartez_<name>_router)]` impl block. The
//! master `tool_router()` below merges them into a single `ToolRouter<Self>`
//! that `#[tool_handler]` picks up via `Self::tool_router()`.

use rmcp::handler::server::router::tool::ToolRouter;

use super::QartezServer;

mod boundaries;
mod calls;
mod clones;
mod cochange;
mod context;
mod deps;
mod diff_impact;
mod find;
mod grep;
mod health;
mod hierarchy;
mod hotspots;
mod impact;
mod insert;
mod knowledge;
mod maintenance;
mod map;
mod mv;
mod outline;
mod project;
mod read;
mod refactor_common;
mod refactor_plan;
mod refs;
mod rename;
mod rename_file;
mod replace;
mod safe_delete;
mod security;
mod semantic;
pub(in crate::server) mod smells;
mod stats;
pub(super) mod test_gaps;
mod tools_meta;
mod trend;
mod understand;
mod unused;
mod wiki;
mod workspace;

impl QartezServer {
    pub(super) fn tool_router() -> ToolRouter<Self> {
        Self::qartez_map_router()
            + Self::qartez_workspace_router()
            + Self::qartez_add_root_router()
            + Self::qartez_list_roots_router()
            + Self::qartez_find_router()
            + Self::qartez_read_router()
            + Self::qartez_impact_router()
            + Self::qartez_diff_impact_router()
            + Self::qartez_cochange_router()
            + Self::qartez_grep_router()
            + Self::qartez_unused_router()
            + Self::qartez_refs_router()
            + Self::qartez_rename_router()
            + Self::qartez_project_router()
            + Self::qartez_move_router()
            + Self::qartez_rename_file_router()
            + Self::qartez_outline_router()
            + Self::qartez_deps_router()
            + Self::qartez_stats_router()
            + Self::qartez_calls_router()
            + Self::qartez_context_router()
            + Self::qartez_hotspots_router()
            + Self::qartez_clones_router()
            + Self::qartez_smells_router()
            + Self::qartez_health_router()
            + Self::qartez_refactor_plan_router()
            + Self::qartez_test_gaps_router()
            + Self::qartez_wiki_router()
            + Self::qartez_boundaries_router()
            + Self::qartez_hierarchy_router()
            + Self::qartez_trend_router()
            + Self::qartez_security_router()
            + Self::qartez_knowledge_router()
            + Self::qartez_tools_router()
            + Self::qartez_semantic_router()
            + Self::qartez_replace_symbol_router()
            + Self::qartez_insert_before_symbol_router()
            + Self::qartez_insert_after_symbol_router()
            + Self::qartez_safe_delete_router()
            + Self::qartez_maintenance_router()
            + Self::qartez_understand_router()
    }
}
