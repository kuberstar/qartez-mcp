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

#[tool_router(router = qartez_tools_router, vis = "pub(super)")]
impl QartezServer {
    #[tool(
        name = "qartez_tools",
        description = "Discover and enable additional Qartez tools. Call with no arguments to see all available tiers and tools. Use enable/disable to dynamically add or remove tool tiers or individual tools. Tier names: 'core' (always on), 'analysis', 'refactor', 'meta'. Pass 'all' to enable everything.",
        annotations(
            title = "Tool Discovery",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    pub(in crate::server) async fn qartez_tools(
        &self,
        context: RequestContext<RoleServer>,
        Parameters(params): Parameters<ToolsParams>,
    ) -> Result<CallToolResult, ErrorData> {
        let is_listing = params.enable.is_none() && params.disable.is_none();

        if is_listing {
            let enabled = self
                .enabled_tools
                .read()
                .expect("enabled_tools lock poisoned");
            let progressive = tiers::is_progressive_mode();
            let mut out = String::from("# Qartez Tool Tiers\n\n");
            let mode_label = if progressive {
                "progressive mode - only `core` is always on; opt into the rest via `enable: [...]`."
            } else {
                "non-progressive mode - every tool is enabled at startup; set `QARTEZ_PROGRESSIVE=1` to require opt-in for non-core tiers."
            };
            out.push_str(&format!("Mode: {mode_label}\n\n"));
            for &tier_name in tiers::ALL_TIER_NAMES {
                let tools = tiers::tier_tools(tier_name).unwrap_or_default();
                let all_enabled = tools.iter().all(|t| enabled.contains(*t));
                // `core` is always on by construction; every other tier
                // is opt-in under progressive mode but enabled at
                // startup under the legacy default. Label the two
                // distinct states explicitly so callers know whether a
                // tier is structurally protected or simply toggled on.
                let status = if tier_name == "core" {
                    "always on"
                } else if all_enabled {
                    "enabled"
                } else {
                    "disabled"
                };
                // Summarise the tier with a short `tools: a, b, c`
                // line before the per-tool table so callers scanning
                // the listing can see at a glance which names live
                // under each tier. Without this, the no-arg response
                // only exposed tier names and forced a second round
                // trip to `enable: [...]` to discover the tool set.
                let tool_names: Vec<&str> = tools.to_vec();
                out.push_str(&format!("## {tier_name} ({status})\n"));
                if !tool_names.is_empty() {
                    out.push_str(&format!("tools: {}\n", tool_names.join(", ")));
                }
                for &tool_name in tools {
                    let mark = if enabled.contains(tool_name) {
                        "x"
                    } else {
                        " "
                    };
                    let desc = self
                        .tool_router
                        .get(tool_name)
                        .map(|t| t.description.as_deref().unwrap_or(""))
                        .unwrap_or("");
                    let short = desc.split('.').next().unwrap_or(desc);
                    out.push_str(&format!("- [{mark}] `{tool_name}` -- {short}\n"));
                }
                out.push('\n');
            }
            out.push_str("Use `enable: [\"analysis\"]` or `enable: [\"all\"]` to unlock tiers.\n");
            out.push_str("Use `disable: [\"refactor\"]` to hide tiers.\n");
            return Ok(CallToolResult::success(vec![Content::text(out)]));
        }

        // Reject overlap between the two lists up front. Previously a
        // payload of `{enable: ["analysis"], disable: ["analysis"]}`
        // applied enable first and disable-wins-second, stranding the
        // caller in a no-op state that looked successful. Returning a
        // hard error makes the conflict visible to both humans and
        // scripts.
        if let (Some(enable_targets), Some(disable_targets)) = (&params.enable, &params.disable) {
            let disable_set: HashSet<&str> = disable_targets.iter().map(String::as_str).collect();
            if let Some(conflict) = enable_targets
                .iter()
                .find(|t| disable_set.contains(t.as_str()))
            {
                return Err(ErrorData::invalid_params(
                    format!(
                        "Conflict: '{conflict}' is in both enable and disable lists. Pass it in only one."
                    ),
                    None,
                ));
            }
        }

        // Split-validation: classify every target as known or unknown
        // so the write path can partial-apply the valid names while
        // surfacing the bogus ones as warnings. Previously the tool
        // all-or-nothing-rejected, which meant a single typo
        // ("analysiz") alongside valid tier names ("refactor") wiped
        // the whole call out. Scripts that accumulate tier names over
        // multiple sessions benefit most from the partial-apply
        // behaviour; a purely bogus list still returns an error so
        // typos-only calls remain visible.
        let mut unknown_enable: Vec<String> = Vec::new();
        let mut unknown_disable: Vec<String> = Vec::new();
        let enable_total = params.enable.as_ref().map(Vec::len).unwrap_or(0);
        let disable_total = params.disable.as_ref().map(Vec::len).unwrap_or(0);
        if let Some(ref targets) = params.enable {
            for target in targets {
                if target == "all" {
                    continue;
                }
                if tiers::tier_tools(target).is_some() {
                    continue;
                }
                if self.tool_router.get(target).is_some() {
                    continue;
                }
                unknown_enable.push(target.clone());
            }
        }
        if let Some(ref targets) = params.disable {
            for target in targets {
                if target == "core" || target == tiers::META_TOOL_NAME {
                    continue;
                }
                if tiers::tier_tools(target).is_some() {
                    continue;
                }
                if self.tool_router.get(target).is_some() {
                    continue;
                }
                unknown_disable.push(target.clone());
            }
        }
        let enable_all_bogus = enable_total > 0 && unknown_enable.len() == enable_total;
        let disable_all_bogus = disable_total > 0 && unknown_disable.len() == disable_total;
        if (enable_all_bogus || enable_total == 0)
            && (disable_all_bogus || disable_total == 0)
            && (!unknown_enable.is_empty() || !unknown_disable.is_empty())
        {
            let mut all_unknown: Vec<String> = unknown_enable.clone();
            all_unknown.extend(unknown_disable.iter().cloned());
            return Err(ErrorData::invalid_params(
                format!(
                    "Unknown tier/tool name(s): {}. Valid tiers: {}. Run `qartez_tools` with no arguments for the full list.",
                    all_unknown.join(", "),
                    tiers::ALL_TIER_NAMES.join(", "),
                ),
                None,
            ));
        }

        let mut changed = false;
        let mut rejected_disable: Vec<String> = Vec::new();
        // Count how many disable targets resolved to the ignore-only
        // set so all-ignored requests surface as an error instead of
        // `Ok("No changes")` with a warning. Scripts can now rely on a
        // non-zero exit code when every target is rejected.
        let mut disable_requested: usize = 0;
        {
            let mut enabled = self
                .enabled_tools
                .write()
                .expect("enabled_tools lock poisoned");

            if let Some(ref targets) = params.enable {
                for target in targets {
                    if target == "all" {
                        let all_tools = self.tool_router.list_all();
                        for tool in &all_tools {
                            if enabled.insert(tool.name.to_string()) {
                                changed = true;
                            }
                        }
                    } else if let Some(tools) = tiers::tier_tools(target) {
                        for &name in tools {
                            if enabled.insert(name.to_owned()) {
                                changed = true;
                            }
                        }
                    } else if self.tool_router.get(target).is_some()
                        && enabled.insert(target.clone())
                    {
                        changed = true;
                    }
                }
            }

            if let Some(ref targets) = params.disable {
                disable_requested = targets.len();
                for target in targets {
                    // `core` and the meta tool are structurally
                    // protected: disabling them would strand the client
                    // without the commands to re-enable anything. Flag
                    // the reject so the caller sees "ignored" instead
                    // of a silent "No changes".
                    if target == "core" || target == tiers::META_TOOL_NAME {
                        rejected_disable.push(target.clone());
                        continue;
                    }
                    if let Some(tools) = tiers::tier_tools(target) {
                        for &name in tools {
                            if enabled.remove(name) {
                                changed = true;
                            }
                        }
                    } else if self.tool_router.get(target).is_some()
                        && enabled.remove(target.as_str())
                    {
                        changed = true;
                    }
                }
            }
        }

        if !rejected_disable.is_empty()
            && disable_requested == rejected_disable.len()
            && params.enable.as_ref().is_none_or(Vec::is_empty)
        {
            return Err(ErrorData::invalid_params(
                format!(
                    "All disable target(s) were rejected: {} (core tier and qartez_tools cannot be disabled).",
                    rejected_disable.join(", ")
                ),
                None,
            ));
        }

        if changed {
            let _ = context.peer.notify_tool_list_changed().await;
        }

        let enabled = self
            .enabled_tools
            .read()
            .expect("enabled_tools lock poisoned");
        let count = enabled.len();
        let mut msg = if changed {
            format!("Tool list updated. {count} tools now enabled.")
        } else {
            format!("No changes. {count} tools currently enabled.")
        };
        if !rejected_disable.is_empty() {
            msg.push_str(&format!(
                "\nIgnored disable target(s): {} (core tier and qartez_tools cannot be disabled).",
                rejected_disable.join(", "),
            ));
        }
        if !unknown_enable.is_empty() {
            msg.push_str(&format!(
                "\n// warning: unknown enable target(s) ignored: {}. Valid tiers: {}.",
                unknown_enable.join(", "),
                tiers::ALL_TIER_NAMES.join(", "),
            ));
        }
        if !unknown_disable.is_empty() {
            msg.push_str(&format!(
                "\n// warning: unknown disable target(s) ignored: {}. Valid tiers: {}.",
                unknown_disable.join(", "),
                tiers::ALL_TIER_NAMES.join(", "),
            ));
        }
        Ok(CallToolResult::success(vec![Content::text(msg)]))
    }
}
