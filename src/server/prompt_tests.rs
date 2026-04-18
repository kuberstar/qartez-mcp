// Rust guideline compliant 2026-04-12

//! Unit tests for the canned MCP workflow prompts.
//!
//! Each test calls the sync prompt method directly on a `QartezServer` and
//! asserts the returned `GetPromptResult` contains:
//!   * exactly one user-role message (prompts are single-turn recipes)
//!   * the caller-supplied argument interpolated back into the recipe
//!   * references to every Qartez tool the recipe promises to invoke
//!
//! The `prompt_router()` assertion at the bottom verifies that all six
//! prompts are actually registered so a missing `#[prompt(name = ...)]`
//! attribute cannot silently drop a slash command.

use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{PromptMessageContent, PromptMessageRole};
use rusqlite::Connection;
use tempfile::TempDir;

use super::QartezServer;
use super::prompts::{
    SoulArchReviewArgs, SoulArchitectureArgs, SoulDebugArgs, SoulOnboardArgs, SoulPreMergeArgs,
    SoulReviewArgs,
};
use crate::storage::schema;

fn make_server() -> (QartezServer, TempDir) {
    let dir = TempDir::new().unwrap();
    let conn = Connection::open_in_memory().unwrap();
    schema::create_schema(&conn).unwrap();
    (QartezServer::new(conn, dir.path().to_path_buf(), 300), dir)
}

/// Assert the single user-role text payload of a `GetPromptResult`.
fn user_text(result: &rmcp::model::GetPromptResult) -> &str {
    assert_eq!(
        result.messages.len(),
        1,
        "prompts are single-turn recipes with exactly one user message"
    );
    let message = &result.messages[0];
    assert_eq!(message.role, PromptMessageRole::User);
    match &message.content {
        PromptMessageContent::Text { text } => text.as_str(),
        other => panic!("expected text content, got {other:?}"),
    }
}

#[test]
fn qartez_review_prompt_cites_core_tools_and_target() {
    let (server, _dir) = make_server();
    let result = server.qartez_review_prompt(Parameters(SoulReviewArgs {
        target: "src/server/mod.rs".into(),
    }));

    let text = user_text(&result);
    assert!(text.contains("src/server/mod.rs"));
    assert!(text.contains("qartez_impact"));
    assert!(text.contains("qartez_outline"));
    assert!(text.contains("qartez_refs"));
    assert!(text.contains("qartez_cochange"));
    assert!(
        result
            .description
            .as_deref()
            .is_some_and(|d| d.contains("src/server/mod.rs"))
    );
}

#[test]
fn qartez_architecture_prompt_defaults_top_n_and_cites_core_tools() {
    let (server, _dir) = make_server();

    let default_result =
        server.qartez_architecture_prompt(Parameters(SoulArchitectureArgs { top_n: None }));
    let default_text = user_text(&default_result);
    assert!(default_text.contains("qartez_map"));
    assert!(default_text.contains("top_n=15"));
    assert!(default_text.contains("qartez_stats"));
    assert!(default_text.contains("qartez_project"));

    let custom_result = server.qartez_architecture_prompt(Parameters(SoulArchitectureArgs {
        top_n: Some("30".into()),
    }));
    assert!(user_text(&custom_result).contains("top_n=30"));
}

#[test]
fn qartez_debug_prompt_injects_file_hint_when_supplied() {
    let (server, _dir) = make_server();

    let without_hint = server.qartez_debug_prompt(Parameters(SoulDebugArgs {
        target: "qartez_impact".into(),
        file_path: None,
    }));
    let text_without = user_text(&without_hint);
    assert!(text_without.contains("qartez_find"));
    assert!(text_without.contains("qartez_read"));
    assert!(text_without.contains("qartez_calls"));
    assert!(text_without.contains("qartez_refs"));
    assert!(text_without.contains("qartez_impact"));
    assert!(!text_without.contains("file_path=\""));

    let with_hint = server.qartez_debug_prompt(Parameters(SoulDebugArgs {
        target: "build_overview".into(),
        file_path: Some("src/server/mod.rs".into()),
    }));
    let text_with = user_text(&with_hint);
    assert!(text_with.contains("file_path=\"src/server/mod.rs\""));
    assert!(text_with.contains("build_overview"));
}

#[test]
fn qartez_onboard_prompt_biases_map_on_area_keyword() {
    let (server, _dir) = make_server();

    let untargeted = server.qartez_onboard_prompt(Parameters(SoulOnboardArgs { area: None }));
    let text_untargeted = user_text(&untargeted);
    assert!(text_untargeted.contains("qartez_map"));
    assert!(text_untargeted.contains("qartez_context"));
    assert!(text_untargeted.contains("qartez_outline"));
    assert!(!text_untargeted.contains("boost_terms"));

    let targeted = server.qartez_onboard_prompt(Parameters(SoulOnboardArgs {
        area: Some("benchmark".into()),
    }));
    let text_targeted = user_text(&targeted);
    assert!(text_targeted.contains("boost_terms=[\"benchmark\"]"));
}

#[test]
fn qartez_pre_merge_prompt_recommends_diff_impact() {
    let (server, _dir) = make_server();
    let result = server.qartez_pre_merge_prompt(Parameters(SoulPreMergeArgs {
        files: "src/server/mod.rs, src/server/prompts.rs\nsrc/lib.rs".into(),
    }));

    let text = user_text(&result);
    assert!(text.contains("src/server/mod.rs"));
    assert!(text.contains("src/server/prompts.rs"));
    assert!(text.contains("src/lib.rs"));
    assert!(
        text.contains("qartez_diff_impact"),
        "should recommend qartez_diff_impact for batch analysis"
    );
    assert!(text.contains("qartez_unused"));
}

#[test]
fn qartez_pre_merge_prompt_handles_empty_file_list() {
    let (server, _dir) = make_server();
    let result = server.qartez_pre_merge_prompt(Parameters(SoulPreMergeArgs {
        files: "   ".into(),
    }));
    let text = user_text(&result);
    assert!(
        text.contains("qartez_diff_impact"),
        "empty file list should recommend qartez_diff_impact"
    );
    assert!(text.contains("qartez_unused"));
}

#[test]
fn qartez_arch_review_prompt_biases_map_on_focus_keyword() {
    let (server, _dir) = make_server();

    let untargeted =
        server.qartez_arch_review_prompt(Parameters(SoulArchReviewArgs { focus: None }));
    let text_untargeted = user_text(&untargeted);
    assert!(text_untargeted.contains("qartez_map"));
    assert!(text_untargeted.contains("qartez_hotspots"));
    assert!(text_untargeted.contains("qartez_boundaries"));
    assert!(text_untargeted.contains("qartez_security"));
    assert!(!text_untargeted.contains("boost_terms"));

    let targeted = server.qartez_arch_review_prompt(Parameters(SoulArchReviewArgs {
        focus: Some("auth".into()),
    }));
    let text_targeted = user_text(&targeted);
    assert!(text_targeted.contains("boost_terms=[\"auth\"]"));
    assert!(text_targeted.contains("focused on `auth`"));
}

#[test]
fn prompt_router_registers_all_six_canned_prompts() {
    let router = QartezServer::prompt_router();
    let names: Vec<String> = router.list_all().into_iter().map(|p| p.name).collect();

    for expected in [
        "qartez_review",
        "qartez_architecture",
        "qartez_debug",
        "qartez_onboard",
        "qartez_pre_merge",
        "qartez_arch_review",
    ] {
        assert!(
            names.iter().any(|n| n == expected),
            "prompt `{expected}` is not registered (found: {names:?})"
        );
    }
}
