//! Tokenization for benchmark outputs.
//!
//! Uses `cl100k_base` (GPT-4's tokenizer) as a proxy for Claude's proprietary
//! BPE. Absolute counts will differ from Claude's real tokenizer, but the
//! ratio between MCP and non-MCP outputs is stable because both sides use
//! the same estimator. The naive `bytes/4` count is also captured so readers
//! can cross-check that ratios hold across estimation methods.

use std::sync::OnceLock;

use tiktoken_rs::{CoreBPE, cl100k_base};

static TOKENIZER: OnceLock<CoreBPE> = OnceLock::new();

fn tokenizer() -> &'static CoreBPE {
    TOKENIZER.get_or_init(|| cl100k_base().expect("cl100k_base is bundled with tiktoken-rs"))
}

/// Count tokens using `cl100k_base`. Reference for claude-proxy benchmarks.
pub fn count_tokens(text: &str) -> usize {
    if text.is_empty() {
        return 0;
    }
    tokenizer().encode_with_special_tokens(text).len()
}

/// Naive `bytes/4` estimator for benchmark comparison.
///
/// Not suitable as a primary metric but reported alongside the BPE count for
/// cross-verification of the savings ratio. Note: `server::estimate_tokens`
/// uses `chars/3` for tighter accuracy; this keeps the legacy formula for
/// benchmark stability.
pub fn naive_count(text: &str) -> usize {
    text.len() / 4
}
