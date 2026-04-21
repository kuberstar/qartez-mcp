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

#[tool_router(router = qartez_map_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_map",
        description = "Start here. Returns the codebase skeleton: files ranked by importance (PageRank), their exports, and blast radii. Use boost_files/boost_terms to focus on areas relevant to your current task.",
        annotations(
            title = "Project Map",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    pub(in crate::server) fn qartez_map(
        &self,
        Parameters(params): Parameters<QartezParams>,
    ) -> String {
        let requested_top = params.top_n.unwrap_or(20);
        let all_files = params.all_files.unwrap_or(false) || requested_top == 0;
        let top_n = if all_files {
            i64::MAX
        } else {
            requested_top as i64
        };
        let token_budget = params.token_budget.unwrap_or(DEFAULT_TOKEN_BUDGET as u32) as usize;
        let concise = is_concise(&params.format);
        // `by=symbols` swaps the file ranking out for a symbol-level view.
        // Any other value (including the default) keeps the historical
        // file-ranked output — that path is the baseline every existing
        // benchmark scenario expects, so changing it silently would skew
        // regression reports.
        let by_symbols = params
            .by
            .as_deref()
            .map(|s| s.eq_ignore_ascii_case("symbols"))
            .unwrap_or(false);
        if by_symbols {
            return self.build_symbol_overview(top_n, token_budget, concise);
        }
        self.build_overview(
            top_n,
            token_budget,
            params.boost_files.as_deref(),
            params.boost_terms.as_deref(),
            concise,
            all_files,
        )
    }
}
