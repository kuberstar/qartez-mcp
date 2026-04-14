# Qartez MCP Per-Tool Benchmark

- Generated at: `1776064403` (unix)
- Git SHA: `b5c4406`
- Tokenizer: `cl100k_base`
- Winner column uses a 5% tie margin on token savings.
- Latency is the mean of trimmed samples; expect noisy ratios near 1×.
- ✱ 1 scenario(s) marked with ✱ have an incomplete non-MCP sim — the non-MCP side cannot produce a comparable answer, so the token and Savings columns are shown as `—`. MCP is awarded the win on correctness; the Speedup column still reflects the real latency cost the non-MCP side paid for its partial output.

## Headline

**Aggregate token savings vs Glob+Grep+Read: +91.5%** (Σ MCP 8604 / Σ non-MCP 101740 tokens across 23 scenarios)

_Note: 1/23 scenario(s) have an incomplete non-MCP sim. Those rows still contribute their MCP tokens to both sums, so this headline is a conservative under-count of the real win._

**Avg LLM-judge quality (claude-opus-4-6): MCP 7.9/10 vs non-MCP 5.3/10** (n=23; 5 axes: correctness/completeness/usability/groundedness/conciseness)

## Session cost context

Typical Claude Code session base cost: ~20,000 tokens (system prompt + CLAUDE.md + tools).
MCP tool savings this run: 93136 tokens (4.7 empty sessions).

## Matrix

| Tool | MCP tok | non-MCP tok | Savings | MCP ms | non-MCP ms | Speedup | Precision | Recall | Eff. Savings | Quality (MCP / non-MCP) | Grounding (MCP / non-MCP) | Winner |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|:---:|:---:|---|
| `qartez_map` | 92 | 405 | +77.3% | 0.09 | 0.37 | 4.07× | 1.00 | 1.00 | +77.3% | 9.4 / 6.2 | 100% (5/5) / 100% (63/63) | **mcp** |
| `qartez_find` | 50 | 1648 | +97.0% | 0.01 | 1.85 | 200.80× | 1.00 | 0.00 | — | 8.0 / 4.0 | 100% (2/2) / 79% (15/19) | **mcp** |
| `qartez_read` | 55 | 445 | +87.6% | 0.07 | 1.93 | 26.10× | 1.00 | 0.00 | — | 4.6 / 3.2 | 100% (1/1) / 83% (5/6) | **mcp** |
| `qartez_grep` | 98 | 706 | +86.1% | 0.03 | 1.85 | 58.03× | 0.60 | 0.17 | +14.4% | 7.4 / 4.0 | 100% (8/8) / 94% (33/35) | **mcp** |
| `qartez_outline` | 3582 | 54414 | +93.4% | 0.18 | 0.53 | 2.88× | 0.00 | 1.00 | +93.4% | 10.0 / 5.2 | 98% (113/115) / 74% (108/145) | **mcp** |
| `qartez_deps` | 85 | 1255 | +93.2% | 0.03 | 3.81 | 119.79× | 1.00 | 1.00 | +93.2% | 8.4 / 6.2 | 100% (6/6) / 98% (64/65) | **mcp** |
| `qartez_refs` | 110 | 636 | +82.7% | 0.10 | 1.77 | 18.58× | 1.00 | 0.00 | — | 6.0 / 4.6 | 45% (5/11) / 97% (30/31) | **mcp** |
| `qartez_impact` | 308 | 5418 | +94.3% | 0.17 | 21.28 | 122.15× | 1.00 | 1.00 | +94.3% | 8.8 / 5.2 | 90% (19/21) / 87% (133/153) | **mcp** |
| `qartez_cochange` | 92 | 4361 | +97.9% | 6.50 | 18.62 | 2.87× | — | — | — | 8.4 / 5.0 | 67% (4/6) / 81% (86/106) | **mcp** |
| `qartez_unused` | 408 | 4621 | +91.2% | 0.10 | 1.96 | 19.91× | 1.00 | 0.00 | — | 6.8 / 3.2 | 100% (5/5) / 92% (392/426) | **mcp** |
| `qartez_stats` | 107 | 650 | +83.5% | 0.36 | 0.40 | 1.09× | — | — | — | 9.4 / 4.6 | 100% (5/5) / 89% (86/97) | **mcp** |
| `qartez_calls` | 516 | 2626 | +80.4% | 0.73 | 1.92 | 2.63× | — | — | — | 8.4 / 4.6 | 76% (25/33) / 80% (41/51) | **mcp** |
| `qartez_context` | 118 | 2848 | +95.9% | 0.07 | 21.80 | 315.09× | — | — | — | 7.8 / 4.6 | 67% (4/6) / 94% (119/127) | **mcp** |
| `qartez_rename` | 180 | 327 | +45.0% | 0.13 | 2.13 | 16.07× | — | — | — | 7.8 / 5.8 | 80% (4/5) / 95% (18/19) | **mcp** |
| `qartez_move` | 117 | 676 | +82.7% | 0.07 | 4.04 | 57.74× | — | — | — | 5.6 / 5.4 | 50% (1/2) / 83% (5/6) | **mcp** |
| `qartez_rename_file` | 22 | 168 | +86.9% | 0.01 | 1.92 | 184.19× | — | — | — | 7.8 / 5.6 | 67% (2/3) / 100% (8/8) | **mcp** |
| `qartez_project` | 38 | 916 | +95.9% | 0.00 | 0.01 | 12.80× | — | — | — | 8.8 / 6.0 | — / 100% (3/3) | **mcp** |
| `qartez_find` | 13 | 48 | +72.9% | 0.00 | 1.90 | 9506.00× | 1.00 | 1.00 | +72.9% | 8.8 / 6.6 | — / 100% (2/2) | **mcp** |
| `qartez_grep` | 10 | 0 | +0.0% | 1.15 | 1.89 | 1.65× | 0.00 | 1.00 | +0.0% | 8.8 / 7.8 | — / — | **tie** |
| `qartez_read` | 964 | 916 | -5.2% | 0.01 | 0.01 | 1.18× | 1.00 | 1.00 | -5.2% | 7.6 / 9.4 | 75% (3/4) / 100% (3/3) | **non_mcp** |
| `qartez_outline` | 1581 | 14020 | +88.7% | 0.06 | 0.14 | 2.35× | 0.00 | 1.00 | +88.7% | 8.8 / 5.2 | 100% (54/54) / 77% (67/87) | **mcp** |
| `qartez_unused` ✱ | 58 | — | — | 0.04 | 1.91 | 46.79× | 1.00 | 0.00 | — | 8.0 / 3.2 | 100% (1/1) / 92% (392/426) | **mcp** |
| `qartez_impact` | 0 | 15 | +100.0% | 0.00 | 0.97 | 483.50× | 1.00 | 1.00 | +100.0% | 7.4 / 6.6 | — / 100% (2/2) | **non_mcp** |
## Quality (LLM judge + programmatic)

| Tool | Correctness | Usability | Completeness† | Groundedness† | Conciseness† | Avg |
|---|:---:|:---:|:---:|:---:|:---:|:---:|
| `qartez_map` | 10/3 | 10/3 | 10/10 | 10/10 | 7/5 | 9.4/6.2 |
| `qartez_find` | 10/7 | 10/5 | 0/0 | 10/5 | 10/3 | 8.0/4.0 |
| `qartez_read` | 0/3 | 3/3 | 0/0 | 10/7 | 10/3 | 4.6/3.2 |
| `qartez_grep` | 7/7 | 10/3 | 0/0 | 10/7 | 10/3 | 7.4/4.0 |
| `qartez_outline` | 10/5 | 10/3 | 10/10 | 10/5 | 10/3 | 10.0/5.2 |
| `qartez_deps` | 5/5 | 7/3 | 10/10 | 10/10 | 10/3 | 8.4/6.2 |
| `qartez_refs` | 7/7 | 10/3 | 0/0 | 3/10 | 10/3 | 6.0/4.6 |
| `qartez_impact` | 7/3 | 10/3 | 10/10 | 7/7 | 10/3 | 8.8/5.2 |
| `qartez_cochange` | 10/5 | 10/3 | 7/7 | 5/7 | 10/3 | 8.4/5.0 |
| `qartez_unused` | 7/3 | 7/3 | 0/0 | 10/7 | 10/3 | 6.8/3.2 |
| `qartez_stats` | 10/3 | 10/3 | 7/7 | 10/7 | 10/3 | 9.4/4.6 |
| `qartez_calls` | 10/3 | 10/3 | 7/7 | 5/7 | 10/3 | 8.4/4.6 |
| `qartez_context` | 7/3 | 10/3 | 7/7 | 5/7 | 10/3 | 7.8/4.6 |
| `qartez_rename` | 10/5 | 10/5 | 7/7 | 7/7 | 5/5 | 7.8/5.8 |
| `qartez_move` | 3/5 | 3/5 | 7/7 | 5/7 | 10/3 | 5.6/5.4 |
| `qartez_rename_file` | 7/5 | 10/3 | 7/7 | 5/10 | 10/3 | 7.8/5.6 |
| `qartez_project` | 10/5 | 10/5 | 7/7 | 7/10 | 10/3 | 8.8/6.0 |
| `qartez_find` | 10/3 | 10/5 | 10/10 | 7/10 | 7/5 | 8.8/6.6 |
| `qartez_grep` | 10/10 | 10/5 | 10/10 | 7/7 | 7/7 | 8.8/7.8 |
| `qartez_read` | 10/10 | 10/10 | 10/10 | 5/10 | 3/7 | 7.6/9.4 |
| `qartez_outline` | 7/5 | 7/3 | 10/10 | 10/5 | 10/3 | 8.8/5.2 |
| `qartez_unused` | 10/3 | 10/3 | 0/0 | 10/7 | 10/3 | 8.0/3.2 |
| `qartez_impact` | 5/5 | 5/5 | 10/10 | 7/10 | 10/3 | 7.4/6.6 |

† Programmatic (not LLM-scored). Correctness and Usability are LLM-scored via single batch call.
Format: MCP/non-MCP.

Judge token budget: ~37,000 tokens (1 batch call, 23 scenarios).


## Per-tool detail

### `qartez_map` — qartez_map_top5_concise

Rank the top 5 files by PageRank (concise format).

**MCP side**

- Args: `{"format":"concise","top_n":5}`
- Response: 251 bytes → 92 tokens (naive 62)
- Latency: mean 0.092 ms, p50 0.092 ms, p95 0.093 ms, σ 0.001 ms (n=5)

**Non-MCP side**

- Steps:
  - `Glob **/*.rs`
- Response: 1475 bytes → 405 tokens (naive 368)
- Latency: mean 0.374 ms, p50 0.373 ms, p95 0.385 ms, σ 0.009 ms (n=5)

**Savings:** +77.3% tokens, +83.0% bytes, 4.07× speedup

**LLM-judge (claude-opus-4-6):** MCP 9.4/10 (correctness 10, completeness 10, usability 10, groundedness 10, conciseness 7) vs non-MCP 6.2/10 (correctness 3, completeness 10, usability 3, groundedness 10, conciseness 5) — _MCP ranks 5 files by PageRank with scores+blast; non-MCP is a flat unranked file list with no PR data_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 5/5 verified (1.000); 5 files, 0 lines, 0 symbols; unverified: []
- non-MCP: 63/63 verified (1.000); 63 files, 0 lines, 0 symbols; unverified: []

**Pros (MCP-only)**

- PageRank-based importance ranking
- Blast radius column in one call
- Elided source previews of exports
- Token-budgeted output
- `all_files: true` (or `top_n: 0`) returns every file PageRank-sorted

**Cons (what MCP loses vs Grep/Read)**

- Cannot return raw file contents
- Requires .qartez/ to be built

**Verdict:** MCP wins overwhelmingly: non-MCP path produces only a flat file list and still requires reading every file to approximate ranking.

---

### `qartez_find` — qartez_find_struct_qartezserver

Locate the QartezServer struct definition.

**MCP side**

- Args: `{"name":"QartezServer"}`
- Response: 166 bytes → 50 tokens (naive 41)
- Latency: mean 0.009 ms, p50 0.009 ms, p95 0.010 ms, σ 0.000 ms (n=5)

**Non-MCP side**

- Steps:
  - `Grep /struct\s+QartezServer/ (*.rs)`
  - `Read src/server/mod.rs lines 1-120`
- Response: 5630 bytes → 1648 tokens (naive 1407)
- Latency: mean 1.847 ms, p50 1.823 ms, p95 1.905 ms, σ 0.040 ms (n=5)

**Savings:** +97.0% tokens, +97.1% bytes, 200.80× speedup

**LLM-judge (claude-opus-4-6):** MCP 8.0/10 (correctness 10, completeness 0, usability 10, groundedness 10, conciseness 10) vs non-MCP 4.0/10 (correctness 7, completeness 0, usability 5, groundedness 5, conciseness 3) — _MCP pinpoints struct at L117 with signature+export status; non-MCP finds it but adds file dump and test-string noise_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 2/2 verified (1.000); 0 files, 1 lines, 1 symbols; unverified: []
- non-MCP: 15/19 verified (0.789); 1 files, 4 lines, 14 symbols; unverified: [u32, Arc, bool, schemars]

**Pros (MCP-only)**

- Pre-indexed exact line range (no brace counting)
- Signature pre-extracted
- Kind filter, export-status flag
- `regex: true` walks the indexed symbol table for pattern matches

**Cons (what MCP loses vs Grep/Read)**

- Only the definition site, not usages
- Misses macro-synthesized symbols — tree-sitter opaque-tokens the macro body, so `lazy_static! { pub static ref FOO }` doesn't surface `FOO` in the index

**Verdict:** MCP wins on precision and compactness, but the non-MCP path is viable for unique symbol names.

---

### `qartez_read` — qartez_read_truncate_path

Read the body of the `truncate_path` helper.

**MCP side**

- Args: `{"symbol_name":"truncate_path"}`
- Response: 189 bytes → 55 tokens (naive 47)
- Latency: mean 0.074 ms, p50 0.075 ms, p95 0.079 ms, σ 0.004 ms (n=5)

**Non-MCP side**

- Steps:
  - `Grep /fn truncate_path/ (*.rs)`
  - `Read src/server/mod.rs lines 260-290`
- Response: 1656 bytes → 445 tokens (naive 414)
- Latency: mean 1.931 ms, p50 1.908 ms, p95 2.051 ms, σ 0.083 ms (n=5)

**Savings:** +87.6% tokens, +88.6% bytes, 26.10× speedup

**LLM-judge (claude-opus-4-6):** MCP 4.6/10 (correctness 0, completeness 0, usability 3, groundedness 10, conciseness 10) vs non-MCP 3.2/10 (correctness 3, completeness 0, usability 3, groundedness 7, conciseness 3) — _MCP returns wrong code (stale index, L575 no longer truncate_path); non-MCP greps correct L659 but reads wrong offset_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 1/1 verified (1.000); 0 files, 1 lines, 0 symbols; unverified: []
- non-MCP: 5/6 verified (0.833); 0 files, 1 lines, 5 symbols; unverified: [None]

**Pros (MCP-only)**

- Jumps directly to the symbol by indexed line range
- No brace counting or over-reading
- Numbered output matches Read semantics
- `start_line` / `end_line` support raw line-range reads for non-symbol code (imports, file headers)
- `context_lines` opt-in surrounding lines (default 0)

**Cons (what MCP loses vs Grep/Read)**

- Line-range mode requires knowing the target file up-front

**Verdict:** MCP wins decisively — a non-MCP agent must over-read to guarantee body coverage.

---

### `qartez_grep` — qartez_grep_find_symbol_prefix

FTS5 prefix search for symbols starting with `find_symbol`.

**MCP side**

- Args: `{"query":"find_symbol*"}`
- Response: 372 bytes → 98 tokens (naive 93)
- Latency: mean 0.032 ms, p50 0.032 ms, p95 0.035 ms, σ 0.003 ms (n=5)

**Non-MCP side**

- Steps:
  - `Grep /find_symbol/ (*.rs)`
- Response: 2704 bytes → 706 tokens (naive 676)
- Latency: mean 1.845 ms, p50 1.844 ms, p95 1.867 ms, σ 0.018 ms (n=5)

**Savings:** +86.1% tokens, +86.2% bytes, 58.03× speedup

**LLM-judge (claude-opus-4-6):** MCP 7.4/10 (correctness 7, completeness 0, usability 10, groundedness 10, conciseness 10) vs non-MCP 4.0/10 (correctness 7, completeness 0, usability 3, groundedness 7, conciseness 3) — _MCP gives 4 clean semantic symbol matches; non-MCP returns 25+ text hits including comments and string literals_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 8/8 verified (1.000); 0 files, 4 lines, 4 symbols; unverified: []
- non-MCP: 33/35 verified (0.943); 1 files, 28 lines, 6 symbols; unverified: [function, find_symbol]

**Pros (MCP-only)**

- Searches indexed symbols only — no comment/string noise
- Prefix matching via FTS5
- Returns kind + line range per hit
- `regex: true` falls back to in-memory regex over indexed symbol names
- `search_bodies: true` hits a pre-indexed FTS5 body table for text inside function bodies

**Cons (what MCP loses vs Grep/Read)**

- Only indexed languages
- Body FTS storage grows ~1-2× the codebase when `search_bodies` is used

**Verdict:** MCP wins on signal-to-noise; grep still wins when you need to find text inside bodies.

---

### `qartez_outline` — qartez_outline_server_mod

Outline all symbols in src/server/mod.rs grouped by kind.

**MCP side**

- Args: `{"file_path":"src/server/mod.rs"}`
- Response: 12679 bytes → 3582 tokens (naive 3169)
- Latency: mean 0.182 ms, p50 0.171 ms, p95 0.214 ms, σ 0.021 ms (n=5)

**Non-MCP side**

- Steps:
  - `Read src/server/mod.rs`
- Response: 202070 bytes → 54414 tokens (naive 50517)
- Latency: mean 0.526 ms, p50 0.531 ms, p95 0.541 ms, σ 0.015 ms (n=5)

**Savings:** +93.4% tokens, +93.7% bytes, 2.88× speedup

**LLM-judge (claude-opus-4-6):** MCP 10.0/10 (correctness 10, completeness 10, usability 10, groundedness 10, conciseness 10) vs non-MCP 5.2/10 (correctness 5, completeness 10, usability 3, groundedness 5, conciseness 3) — _MCP outlines 175 symbols grouped by kind with signatures; non-MCP dumps 200K raw source with no symbol extraction_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 113/115 verified (0.983); 1 files, 0 lines, 114 symbols; unverified: [std, all_fi]
- non-MCP: 108/145 verified (0.745); 12 files, 0 lines, 133 symbols; unverified: [so, u32, Arc, get, bar]

**Pros (MCP-only)**

- Symbols pre-grouped by kind
- Signatures pre-parsed
- Token-budgeted — no full-file read
- Struct fields are emitted as child rows nested under their parent

**Cons (what MCP loses vs Grep/Read)**

- Token budget truncates very large files
- Tuple-struct members are skipped — nothing meaningful to name

**Verdict:** MCP wins by ~20x on a 2300-line file; non-MCP path reads the entire file.

---

### `qartez_deps` — qartez_deps_server_mod

Show incoming/outgoing file-level dependencies for src/server/mod.rs.

**MCP side**

- Args: `{"file_path":"src/server/mod.rs"}`
- Response: 304 bytes → 85 tokens (naive 76)
- Latency: mean 0.032 ms, p50 0.032 ms, p95 0.037 ms, σ 0.004 ms (n=5)

**Non-MCP side**

- Steps:
  - `Grep /^use crate::/ (*.rs)`
  - `Grep /use crate::server/ (*.rs)`
- Response: 4474 bytes → 1255 tokens (naive 1118)
- Latency: mean 3.809 ms, p50 3.773 ms, p95 3.897 ms, σ 0.057 ms (n=5)

**Savings:** +93.2% tokens, +93.2% bytes, 119.79× speedup

**LLM-judge (claude-opus-4-6):** MCP 8.4/10 (correctness 5, completeness 10, usability 7, groundedness 10, conciseness 10) vs non-MCP 6.2/10 (correctness 5, completeness 10, usability 3, groundedness 10, conciseness 3) — _MCP shows structured deps but may miss importers (benchmark/mod.rs); non-MCP is raw grep of all use statements_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 6/6 verified (1.000); 6 files, 0 lines, 0 symbols; unverified: []
- non-MCP: 64/65 verified (0.985); 1 files, 64 lines, 0 symbols; unverified: [src/index/languages/rust_lang.rs:840]

**Pros (MCP-only)**

- Edges pre-resolved at index time
- Bidirectional (imports + importers) in one call

**Cons (what MCP loses vs Grep/Read)**

- Flattens to file-level — loses per-symbol specifier
- Doesn't show which items are imported

**Verdict:** MCP wins on accuracy (resolved paths) and compactness.

---

### `qartez_refs` — qartez_refs_find_symbol_by_name

List references to find_symbol_by_name.

**MCP side**

- Args: `{"symbol":"find_symbol_by_name"}`
- Response: 372 bytes → 110 tokens (naive 93)
- Latency: mean 0.095 ms, p50 0.096 ms, p95 0.101 ms, σ 0.005 ms (n=5)

**Non-MCP side**

- Steps:
  - `Grep /find_symbol_by_name/ (*.rs)`
- Response: 2432 bytes → 636 tokens (naive 608)
- Latency: mean 1.769 ms, p50 1.777 ms, p95 1.822 ms, σ 0.048 ms (n=5)

**Savings:** +82.7% tokens, +84.7% bytes, 18.58× speedup

**LLM-judge (claude-opus-4-6):** MCP 6.0/10 (correctness 7, completeness 0, usability 10, groundedness 3, conciseness 10) vs non-MCP 4.6/10 (correctness 7, completeness 0, usability 3, groundedness 10, conciseness 3) — _MCP gives 10 AST-resolved call sites grouped by file; non-MCP returns 25+ text mentions including docs and strings_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 5/11 verified (0.455); 4 files, 1 lines, 6 symbols; unverified: [L973, L1654, L2654, L3429, L3559]
- non-MCP: 30/31 verified (0.968); 1 files, 25 lines, 5 symbols; unverified: [function]

**Pros (MCP-only)**

- Specifier-aware filtering (vs raw text)
- Optional transitive BFS
- Def + uses in one response
- AST-resolved call sites now listed alongside file-edge importers

**Cons (what MCP loses vs Grep/Read)**

- Misses dynamic dispatch: trait-object calls resolve at runtime and leave no static edge or call-site name the tree-sitter walker can anchor to
- Transitive BFS can balloon on hub symbols — a symbol whose file is imported by 50 crates yields 50 * avg-fanout rows

**Verdict:** MCP now surfaces both file-level edges and AST call sites, closing the gap with grep while keeping the tree-sitter precision that skips strings and comments.

---

### `qartez_impact` — qartez_impact_storage_read

Blast radius and co-change partners for src/storage/read.rs.

**MCP side**

- Args: `{"file_path":"src/storage/read.rs","include_tests":false}`
- Response: 1150 bytes → 308 tokens (naive 287)
- Latency: mean 0.174 ms, p50 0.166 ms, p95 0.204 ms, σ 0.019 ms (n=5)

**Non-MCP side**

- Steps:
  - `BFS grep from 'storage::read' (depth 2)`
  - `git log -n100 --name-only + pair counts for src/storage/read.rs (top 10)`
- Response: 16673 bytes → 5418 tokens (naive 4168)
- Latency: mean 21.279 ms, p50 21.096 ms, p95 21.750 ms, σ 0.357 ms (n=5)

**Savings:** +94.3% tokens, +93.1% bytes, 122.15× speedup

**LLM-judge (claude-opus-4-6):** MCP 8.8/10 (correctness 7, completeness 10, usability 10, groundedness 7, conciseness 10) vs non-MCP 5.2/10 (correctness 3, completeness 10, usability 3, groundedness 7, conciseness 3) — _MCP computes blast radius (14 files) + co-change (10 partners); non-MCP is raw grep+git with no analysis_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 19/21 verified (0.905); 21 files, 0 lines, 0 symbols; unverified: [reports/baseline.js, reports/benchmark.js]
- non-MCP: 133/153 verified (0.869); 106 files, 47 lines, 0 symbols; unverified: [scripts/CLAUDE.md, reports/baseline.js, reports/benchmark.js, reports/baseline-go.js, reports/benchmark-v2.js]

**Pros (MCP-only)**

- Combines static blast radius + git co-change in one call
- Transitive BFS pre-computed
- `include_tests: false` by default excludes test modules from the blast radius

**Cons (what MCP loses vs Grep/Read)**

- Co-change is statistical, not causal

**Verdict:** MCP wins on correctness and output size: the non-MCP equivalent is a 2-level import BFS plus a full git-log mine with in-process pair counting — reproducible now that the sim matches what a real agent would actually have to do.

---

### `qartez_cochange` — qartez_cochange_server_mod

Top 5 co-change partners for src/server/mod.rs.

**MCP side**

- Args: `{"file_path":"src/server/mod.rs","limit":5,"max_commit_size":30}`
- Response: 385 bytes → 92 tokens (naive 96)
- Latency: mean 6.500 ms, p50 6.496 ms, p95 6.570 ms, σ 0.064 ms (n=5)

**Non-MCP side**

- Steps:
  - `git log -n200 --name-only + pair counts for src/server/mod.rs (top 5)`
- Response: 12898 bytes → 4361 tokens (naive 3224)
- Latency: mean 18.624 ms, p50 18.537 ms, p95 19.152 ms, σ 0.441 ms (n=5)

**Savings:** +97.9% tokens, +97.0% bytes, 2.87× speedup

**LLM-judge (claude-opus-4-6):** MCP 8.4/10 (correctness 10, completeness 7, usability 10, groundedness 5, conciseness 10) vs non-MCP 5.0/10 (correctness 5, completeness 7, usability 3, groundedness 7, conciseness 3) — _MCP returns counted ranked table of 5 partners; non-MCP dumps raw git log requiring manual co-occurrence counting_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 4/6 verified (0.667); 6 files, 0 lines, 0 symbols; unverified: [reports/baseline.js, reports/benchmark.js]
- non-MCP: 86/106 verified (0.811); 106 files, 0 lines, 0 symbols; unverified: [scripts/CLAUDE.md, reports/baseline.js, reports/benchmark.js, reports/baseline-go.js, reports/benchmark-v2.js]

**Pros (MCP-only)**

- Pre-computed pair counts
- Instant response
- `max_commit_size` arg skips huge refactor commits (default 30) when recomputing from git

**Cons (what MCP loses vs Grep/Read)**

- Depends on git-history granularity

**Verdict:** MCP wins on tokens and latency: the non-MCP sim now faithfully reproduces the full git log mine + in-process pair counting that an agent would have to run by hand.

---

### `qartez_unused` — qartez_unused_whole_repo

Find all unused exported symbols across the repo.

**MCP side**

- Args: `{}`
- Response: 1193 bytes → 408 tokens (naive 298)
- Latency: mean 0.099 ms, p50 0.096 ms, p95 0.109 ms, σ 0.007 ms (n=5)

**Non-MCP side**

- Steps:
  - `Grep /^pub (fn|struct|enum|trait|const)/ (*.rs)`
- Response: 16513 bytes → 4621 tokens (naive 4128)
- Latency: mean 1.963 ms, p50 1.948 ms, p95 2.024 ms, σ 0.050 ms (n=5)

**Savings:** +91.2% tokens, +92.8% bytes, 19.91× speedup

**LLM-judge (claude-opus-4-6):** MCP 6.8/10 (correctness 7, completeness 0, usability 7, groundedness 10, conciseness 10) vs non-MCP 3.2/10 (correctness 3, completeness 0, usability 3, groundedness 7, conciseness 3) — _MCP identifies 158 unused exports with pagination; non-MCP just lists all pub declarations with no usage analysis_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 5/5 verified (1.000); 5 files, 0 lines, 0 symbols; unverified: []
- non-MCP: 392/426 verified (0.920); 0 files, 223 lines, 203 symbols; unverified: [Foo, compare, Wrapper, AppConfig, create_app]

**Pros (MCP-only)**

- Pre-materialized at index time — query is a single indexed SELECT
- `limit` / `offset` pagination (default 50) keeps default output small
- Trait-impl methods are excluded at parse time via the `unused_excluded` flag

**Cons (what MCP loses vs Grep/Read)**

- Requires human filtering before action — dynamic dispatch callers don't register as static importers
- Pre-materialization is invalidated wholesale on re-index; a stale index may miss recently-added imports

**Verdict:** MCP wins massively: the non-MCP step captured here is only the candidate list — a real agent would need hundreds more greps.

---

### `qartez_stats` — qartez_stats_basic

Overall codebase statistics and language breakdown.

**MCP side**

- Args: `{}`
- Response: 273 bytes → 107 tokens (naive 68)
- Latency: mean 0.363 ms, p50 0.362 ms, p95 0.374 ms, σ 0.009 ms (n=5)

**Non-MCP side**

- Steps:
  - `Glob **/*`
- Response: 2461 bytes → 650 tokens (naive 615)
- Latency: mean 0.397 ms, p50 0.405 ms, p95 0.426 ms, σ 0.023 ms (n=5)

**Savings:** +83.5% tokens, +88.9% bytes, 1.09× speedup

**LLM-judge (claude-opus-4-6):** MCP 9.4/10 (correctness 10, completeness 7, usability 10, groundedness 10, conciseness 10) vs non-MCP 4.6/10 (correctness 3, completeness 7, usability 3, groundedness 7, conciseness 3) — _MCP gives computed stats (101 files, 2119 syms, lang breakdown); non-MCP is just a file listing with no metrics_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 5/5 verified (1.000); 5 files, 0 lines, 0 symbols; unverified: []
- non-MCP: 86/97 verified (0.887); 97 files, 0 lines, 0 symbols; unverified: [scripts/CLAUDE.md, reports/baseline.js, reports/benchmark.js, reports/baseline-go.js, reports/benchmark-go.js]

**Pros (MCP-only)**

- Single-call summary
- Most-connected-files list included
- Optional `file_path` arg drills into per-file LOC / symbol / importer counts
- Aggregate output splits src and test files/LOC

**Cons (what MCP loses vs Grep/Read)**

- Test/src split is filename-based (`tests/`, `_test.rs`, `benches/`) — not build-graph-aware, so integration tests wired via Cargo `test` target live in `src/` look like production code
- Language buckets count files-only; weighted metrics (bytes, symbols per language) aren't broken out

**Verdict:** MCP wins — non-MCP glob dump requires external aggregation just to count files.

---

### `qartez_calls` — qartez_calls_build_overview

Call hierarchy for build_overview (default depth=1).

**MCP side**

- Args: `{"name":"build_overview"}`
- Response: 1881 bytes → 516 tokens (naive 470)
- Latency: mean 0.730 ms, p50 0.741 ms, p95 0.763 ms, σ 0.033 ms (n=5)

**Non-MCP side**

- Steps:
  - `Grep /build_overview/ (*.rs)`
  - `Read src/server/mod.rs lines 46-180`
- Response: 9292 bytes → 2626 tokens (naive 2323)
- Latency: mean 1.923 ms, p50 1.901 ms, p95 2.011 ms, σ 0.068 ms (n=5)

**Savings:** +80.4% tokens, +79.8% bytes, 2.63× speedup

**LLM-judge (claude-opus-4-6):** MCP 8.4/10 (correctness 10, completeness 7, usability 10, groundedness 5, conciseness 10) vs non-MCP 4.6/10 (correctness 3, completeness 7, usability 3, groundedness 7, conciseness 3) — _MCP shows 17 callers + 28 callees via AST; non-MCP is grep of text mentions + unrelated source dump_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 25/33 verified (0.758); 6 files, 1 lines, 26 symbols; unverified: [is_some, sort_by, push_str, is_empty, unwrap_or]
- non-MCP: 41/51 verified (0.804); 4 files, 26 lines, 21 symbols; unverified: [Arc, None, bodies, mod.rs, Response]

**Pros (MCP-only)**

- Tree-sitter AST distinguishes calls from references
- Callees resolved to definition file:line
- Transitive depth available (default 1, opt-in depth=2)
- Per-session parse cache — repeat invocations are in-memory

**Cons (what MCP loses vs Grep/Read)**

- Misses dynamic dispatch — trait-object `Box<dyn Foo>` calls leave no static call-site name
- Depth=2 output can still balloon on hub functions; the grouping elision helps but the graph is inherently O(N^depth)

**Verdict:** MCP wins on correctness (tree-sitter distinguishes call sites from type-position references) and tokens. A per-invocation parse cache and a textual pre-filter keep the AST walk from re-visiting files that cannot possibly contain the callee, so the cold-parse cost stays in the low milliseconds.

---

### `qartez_context` — qartez_context_server_mod

Top 5 related files for src/server/mod.rs.

**MCP side**

- Args: `{"files":["src/server/mod.rs"],"limit":5}`
- Response: 350 bytes → 118 tokens (naive 87)
- Latency: mean 0.069 ms, p50 0.069 ms, p95 0.073 ms, σ 0.003 ms (n=5)

**Non-MCP side**

- Steps:
  - `Grep /use crate::/ (*.rs)`
  - `Grep /use crate::server/ (*.rs)`
  - `git log -n50 -- src/server/mod.rs`
- Response: 10221 bytes → 2848 tokens (naive 2555)
- Latency: mean 21.804 ms, p50 21.763 ms, p95 21.994 ms, σ 0.139 ms (n=5)

**Savings:** +95.9% tokens, +96.6% bytes, 315.09× speedup

**LLM-judge (claude-opus-4-6):** MCP 7.8/10 (correctness 7, completeness 7, usability 10, groundedness 5, conciseness 10) vs non-MCP 4.6/10 (correctness 3, completeness 7, usability 3, groundedness 7, conciseness 3) — _MCP ranks 5 related files with multi-signal scores; non-MCP greps import patterns with no relationship scoring_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 4/6 verified (0.667); 6 files, 0 lines, 0 symbols; unverified: [reports/baseline.js, reports/benchmark.js]
- non-MCP: 119/127 verified (0.937); 5 files, 115 lines, 7 symbols; unverified: [func_, a/mod.rs, src/index/mod.rs:1187, src/benchmark/scenarios.rs:853, src/index/languages/rust_lang.rs:790]

**Pros (MCP-only)**

- Multi-signal scoring: deps + cochange + PageRank + task FTS
- Reason tags explain every row

**Cons (what MCP loses vs Grep/Read)**

- Opaque composite score
- Cannot answer 'why was X excluded'

**Verdict:** MCP wins — no single Grep/Read chain can approximate the composite ranking.

---

### `qartez_rename` — qartez_rename_truncate_path_preview

Preview rename of truncate_path → trunc_path (no apply).

**MCP side**

- Args: `{"apply":false,"new_name":"trunc_path","old_name":"truncate_path"}`
- Response: 538 bytes → 180 tokens (naive 134)
- Latency: mean 0.132 ms, p50 0.131 ms, p95 0.142 ms, σ 0.007 ms (n=5)

**Non-MCP side**

- Steps:
  - `Grep /\btruncate_path\b/ (*.rs)`
- Response: 1270 bytes → 327 tokens (naive 317)
- Latency: mean 2.128 ms, p50 2.132 ms, p95 2.160 ms, σ 0.026 ms (n=5)

**Savings:** +45.0% tokens, +57.6% bytes, 16.07× speedup

**LLM-judge (claude-opus-4-6):** MCP 7.8/10 (correctness 10, completeness 7, usability 10, groundedness 7, conciseness 5) vs non-MCP 5.8/10 (correctness 5, completeness 7, usability 5, groundedness 7, conciseness 5) — _MCP previews 10 occurrences across 2 files with new names; non-MCP just greps text including benchmark refs_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 4/5 verified (0.800); 3 files, 0 lines, 2 symbols; unverified: [trunc_path]
- non-MCP: 18/19 verified (0.947); 2 files, 15 lines, 2 symbols; unverified: [src/benchmark/scenarios.rs:805]

**Pros (MCP-only)**

- Tree-sitter identifier matching skips strings/comments
- Atomic apply with word-boundary fallback
- Preview + apply in one API
- Correctly handles aliased imports (`use foo::bar as baz`) — enshrined by a unit test

**Cons (what MCP loses vs Grep/Read)**

- Only indexed languages

**Verdict:** MCP wins on tokens and safety. The AST-based identifier match on a 2300-line file runs in the low single-digit milliseconds — slower than a raw grep but the cost buys correct skipping of strings, comments, and same-spelled but unrelated identifiers.

---

### `qartez_move` — qartez_move_capitalize_kind_preview

Preview moving the `capitalize_kind` helper to a new file.

**MCP side**

- Args: `{"apply":false,"symbol":"capitalize_kind","to_file":"src/server/helpers.rs"}`
- Response: 583 bytes → 117 tokens (naive 145)
- Latency: mean 0.070 ms, p50 0.067 ms, p95 0.078 ms, σ 0.006 ms (n=5)

**Non-MCP side**

- Steps:
  - `Grep /fn capitalize_kind/ (*.rs)`
  - `Read src/server/mod.rs lines 2180-2225`
  - `Grep /\bcapitalize_kind\b/ (*.rs)`
- Response: 2784 bytes → 676 tokens (naive 696)
- Latency: mean 4.042 ms, p50 4.066 ms, p95 4.122 ms, σ 0.075 ms (n=5)

**Savings:** +82.7% tokens, +79.1% bytes, 57.74× speedup

**LLM-judge (claude-opus-4-6):** MCP 5.6/10 (correctness 3, completeness 7, usability 3, groundedness 5, conciseness 10) vs non-MCP 5.4/10 (correctness 5, completeness 7, usability 5, groundedness 7, conciseness 3) — _MCP extracts wrong code block despite correct line range; non-MCP locates definition and callers via grep_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 1/2 verified (0.500); 2 files, 0 lines, 0 symbols; unverified: [src/server/helpers.rs]
- non-MCP: 5/6 verified (0.833); 1 files, 4 lines, 1 symbols; unverified: [src/benchmark/scenarios.rs:823]

**Pros (MCP-only)**

- Atomic extraction + insertion + importer rewriting
- Refuses on ambiguous symbols
- Refuses when destination already defines a same-kind same-name symbol
- Importer rewriting uses regex word boundaries (no substring over-match)

**Cons (what MCP loses vs Grep/Read)**

- Does not rewrite doc-comment references like `[`foo::bar`]` — the rewriter targets `use` paths and qualified call sites only
- Ambiguity check is by symbol name, not by fully-qualified path — a free function `foo` and a method `foo` on a struct both count as ambiguous even when only one matches the move target kind

**Verdict:** MCP wins — non-MCP path is a 3-step sequence with several edit-time pitfalls.

---

### `qartez_rename_file` — qartez_rename_file_server_mod_preview

Preview renaming src/server/mod.rs → src/server/server.rs.

**MCP side**

- Args: `{"apply":false,"from":"src/server/mod.rs","to":"src/server/server.rs"}`
- Response: 87 bytes → 22 tokens (naive 21)
- Latency: mean 0.010 ms, p50 0.011 ms, p95 0.011 ms, σ 0.001 ms (n=5)

**Non-MCP side**

- Steps:
  - `Grep /crate::server/ (*.rs)`
- Response: 622 bytes → 168 tokens (naive 155)
- Latency: mean 1.916 ms, p50 1.970 ms, p95 1.980 ms, σ 0.072 ms (n=5)

**Savings:** +86.9% tokens, +86.0% bytes, 184.19× speedup

**LLM-judge (claude-opus-4-6):** MCP 7.8/10 (correctness 7, completeness 7, usability 10, groundedness 5, conciseness 10) vs non-MCP 5.6/10 (correctness 5, completeness 7, usability 3, groundedness 10, conciseness 3) — _MCP gives concise rename preview with 1 importer; non-MCP greps crate::server mentions including comments_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 2/3 verified (0.667); 3 files, 0 lines, 0 symbols; unverified: [src/server/server.rs]
- non-MCP: 8/8 verified (1.000); 0 files, 7 lines, 1 symbols; unverified: []

**Pros (MCP-only)**

- Atomic mv + import path rewriting
- Handles mod.rs → named-module transform
- Regex word-boundary matching prevents over-match on import stems

**Cons (what MCP loses vs Grep/Read)**

- `mod.rs → named.rs` edge case: the parent module's `mod foo;` declaration is *not* rewritten because the rename tool only touches files that import via `use crate::foo::…`, not files that declare the module
- Doc links (`[`crate::foo`]` in `///` comments) aren't rewritten — the rewriter is limited to `use`-path tokens

**Verdict:** MCP wins for single-shot refactors; non-MCP path produces correct results only with careful scoping.

---

### `qartez_project` — qartez_project_info

Detect the toolchain and report its commands.

**MCP side**

- Args: `{"action":"info"}`
- Response: 141 bytes → 38 tokens (naive 35)
- Latency: mean 0.001 ms, p50 0.001 ms, p95 0.001 ms, σ 0.000 ms (n=5)

**Non-MCP side**

- Steps:
  - `Read Cargo.toml`
- Response: 2315 bytes → 916 tokens (naive 578)
- Latency: mean 0.013 ms, p50 0.012 ms, p95 0.016 ms, σ 0.002 ms (n=5)

**Savings:** +95.9% tokens, +93.9% bytes, 12.80× speedup

**LLM-judge (claude-opus-4-6):** MCP 8.8/10 (correctness 10, completeness 7, usability 10, groundedness 7, conciseness 10) vs non-MCP 6.0/10 (correctness 5, completeness 7, usability 5, groundedness 10, conciseness 3) — _MCP auto-detects Rust toolchain with build/test/lint commands; non-MCP shows raw Cargo.toml needing interpretation_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: —
- non-MCP: 3/3 verified (1.000); 3 files, 0 lines, 0 symbols; unverified: []

**Pros (MCP-only)**

- Auto-detection across 5+ ecosystems (Cargo, npm/bun, Go, Python, Make)
- Consistent output format
- `run` action resolves a subcommand to its shell form without executing

**Cons (what MCP loses vs Grep/Read)**

- Detects toolchain by file presence only — `Cargo.toml` / `package.json` / `go.mod` existing is enough, the tool never runs a probe command to confirm the toolchain actually works
- No polyglot support — a Rust + Node monorepo resolves to the first detected toolchain (Cargo wins, Node is silent), so callers need to scope to a sub-directory to see the other

**Verdict:** Near-tie: reading Cargo.toml is already cheap; MCP wins only on portability across ecosystems.

---

### `qartez_find` — qartez_find_nonexistent (tier 2)

Search for a symbol that does not exist. Validates empty-result handling.

**MCP side**

- Args: `{"name":"ThisSymbolDoesNotExist__xyz"}`
- Response: 55 bytes → 13 tokens (naive 13)
- Latency: mean 0.000 ms, p50 0.000 ms, p95 0.001 ms, σ 0.000 ms (n=5)

**Non-MCP side**

- Steps:
  - `Grep /ThisSymbolDoesNotExist__xyz/ (*.rs)`
- Response: 173 bytes → 48 tokens (naive 43)
- Latency: mean 1.901 ms, p50 1.890 ms, p95 1.966 ms, σ 0.058 ms (n=5)

**Savings:** +72.9% tokens, +68.2% bytes, 9506.00× speedup

**LLM-judge (claude-opus-4-6):** MCP 8.8/10 (correctness 10, completeness 10, usability 10, groundedness 7, conciseness 7) vs non-MCP 6.6/10 (correctness 3, completeness 10, usability 5, groundedness 10, conciseness 5) — _MCP correctly reports no symbol found; non-MCP false-positives on benchmark scenario defs containing the string_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: —
- non-MCP: 2/2 verified (1.000); 0 files, 2 lines, 0 symbols; unverified: []

**Pros (MCP-only)**

- Graceful empty result

**Cons (what MCP loses vs Grep/Read)**

- No faster than grep for zero matches

**Verdict:** Validates error/empty path — both sides should return empty or a clear 'not found' message.

---

### `qartez_grep` — qartez_grep_regex (tier 2)

Regex search for `^handle_.*request$`. Tests regex mode correctness.

**MCP side**

- Args: `{"query":"^handle_.*request$","regex":true}`
- Response: 40 bytes → 10 tokens (naive 10)
- Latency: mean 1.148 ms, p50 1.159 ms, p95 1.198 ms, σ 0.046 ms (n=5)

**Non-MCP side**

- Steps:
  - `Grep /^handle_.*request$/ (*.rs)`
- Response: 0 bytes → 0 tokens (naive 0)
- Latency: mean 1.889 ms, p50 1.879 ms, p95 1.935 ms, σ 0.031 ms (n=5)

**Savings:** +0.0% tokens, +0.0% bytes, 1.65× speedup

**LLM-judge (claude-opus-4-6):** MCP 8.8/10 (correctness 10, completeness 10, usability 10, groundedness 7, conciseness 7) vs non-MCP 7.8/10 (correctness 10, completeness 10, usability 5, groundedness 7, conciseness 7) — _Both correctly find no matches; MCP states it explicitly, non-MCP returns silent empty output_
- Self-consistency runs: 0; flags: batch

**Pros (MCP-only)**

- Regex support against indexed symbol names

**Cons (what MCP loses vs Grep/Read)**

- Regex anchors match symbol names, not full lines

**Verdict:** Tests regex handling — MCP searches indexed symbols while non-MCP greps raw lines.

---

### `qartez_read` — qartez_read_whole_file (tier 2)

Read a small file via file_path only (no symbol). Tests file-level read mode.

**MCP side**

- Args: `{"file_path":"Cargo.toml"}`
- Response: 2332 bytes → 964 tokens (naive 583)
- Latency: mean 0.012 ms, p50 0.012 ms, p95 0.013 ms, σ 0.000 ms (n=5)

**Non-MCP side**

- Steps:
  - `Read Cargo.toml`
- Response: 2315 bytes → 916 tokens (naive 578)
- Latency: mean 0.015 ms, p50 0.014 ms, p95 0.016 ms, σ 0.001 ms (n=5)

**Savings:** -5.2% tokens, -0.7% bytes, 1.18× speedup

**LLM-judge (claude-opus-4-6):** MCP 7.6/10 (correctness 10, completeness 10, usability 10, groundedness 5, conciseness 3) vs non-MCP 9.4/10 (correctness 10, completeness 10, usability 10, groundedness 10, conciseness 7) — _Identical output — both return complete Cargo.toml with line numbers, no difference_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 3/4 verified (0.750); 3 files, 0 lines, 1 symbols; unverified: [serde_json]
- non-MCP: 3/3 verified (1.000); 3 files, 0 lines, 0 symbols; unverified: []

**Pros (MCP-only)**

- Direct file read with no symbol resolution overhead

**Cons (what MCP loses vs Grep/Read)**

- No advantage over plain Read for whole-file reads

**Verdict:** Near-tie: file-level reads are equivalent; MCP adds no semantic value.

---

### `qartez_outline` — qartez_outline_small_file (tier 2)

Outline a small file (~50 lines) instead of a 2300-line module. Tests proportional output.

**MCP side**

- Args: `{"file_path":"src/storage/read.rs"}`
- Response: 5362 bytes → 1581 tokens (naive 1340)
- Latency: mean 0.059 ms, p50 0.058 ms, p95 0.063 ms, σ 0.003 ms (n=5)

**Non-MCP side**

- Steps:
  - `Read src/storage/read.rs`
- Response: 48541 bytes → 14020 tokens (naive 12135)
- Latency: mean 0.138 ms, p50 0.136 ms, p95 0.148 ms, σ 0.006 ms (n=5)

**Savings:** +88.7% tokens, +89.0% bytes, 2.35× speedup

**LLM-judge (claude-opus-4-6):** MCP 8.8/10 (correctness 7, completeness 10, usability 7, groundedness 10, conciseness 10) vs non-MCP 5.2/10 (correctness 5, completeness 10, usability 3, groundedness 5, conciseness 3) — _MCP outlines 54 symbols grouped by kind (truncated); non-MCP dumps 44K raw source with no symbol extraction_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 54/54 verified (1.000); 1 files, 0 lines, 53 symbols; unverified: []
- non-MCP: 67/87 verified (0.770); 10 files, 0 lines, 77 symbols; unverified: [is, term, edges, tests, mod.rs]

**Pros (MCP-only)**

- Still structured even for small files

**Cons (what MCP loses vs Grep/Read)**

- Marginal benefit when the whole file fits in context

**Verdict:** MCP advantage shrinks for small files — the outline is nearly as large as reading the file.

---

### `qartez_unused` — qartez_unused_with_limit (tier 2)

List unused symbols with limit=5. Tests pagination.

**MCP side**

- Args: `{"limit":5}`
- Response: 172 bytes → 58 tokens (naive 43)
- Latency: mean 0.041 ms, p50 0.039 ms, p95 0.046 ms, σ 0.004 ms (n=5)

**Non-MCP side** ✱ **incomplete** — the step sequence below does not produce a comparable answer; byte/token counts are noise, not a measure of efficiency

- Steps:
  - `Grep /^pub (fn|struct|enum|trait|const)/ (*.rs)`
- Response: 16513 bytes → 4621 tokens (naive 4128)
- Latency: mean 1.909 ms, p50 1.893 ms, p95 1.957 ms, σ 0.031 ms (n=5)

**Savings:** — tokens, — bytes, 46.79× speedup (token comparison skipped: non-MCP sim is incomplete)

**LLM-judge (claude-opus-4-6):** MCP 8.0/10 (correctness 10, completeness 0, usability 10, groundedness 10, conciseness 10) vs non-MCP 3.2/10 (correctness 3, completeness 0, usability 3, groundedness 7, conciseness 3) — _MCP returns exactly 5 unused exports with pagination metadata; non-MCP dumps all pub declarations unfiltered_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 1/1 verified (1.000); 1 files, 0 lines, 0 symbols; unverified: []
- non-MCP: 392/426 verified (0.920); 0 files, 223 lines, 203 symbols; unverified: [Foo, compare, Wrapper, AppConfig, create_app]

**Pros (MCP-only)**

- Pagination limits output size

**Cons (what MCP loses vs Grep/Read)**

- Non-MCP has no equivalent pagination

**Verdict:** MCP wins — limit parameter keeps output bounded while non-MCP emits all matches.

---

### `qartez_impact` — qartez_impact_nonexistent (tier 2)

Impact analysis on a file that does not exist. Tests error handling.

**MCP side**

- Args: `{"file_path":"src/this_file_does_not_exist.rs"}`
- Response: 0 bytes → 0 tokens (naive 0)
- Latency: mean 0.002 ms, p50 0.002 ms, p95 0.002 ms, σ 0.000 ms (n=5)
- **ERROR:** `File 'src/this_file_does_not_exist.rs' not found in index`

**Non-MCP side**

- Steps:
  - `Grep --files-with-matches /this_file_does_not_exist/ (*.rs)`
- Response: 54 bytes → 15 tokens (naive 13)
- Latency: mean 0.967 ms, p50 0.979 ms, p95 1.001 ms, σ 0.033 ms (n=5)

**Savings:** +100.0% tokens, +100.0% bytes, 483.50× speedup

**LLM-judge (claude-opus-4-6):** MCP 7.4/10 (correctness 5, completeness 10, usability 5, groundedness 7, conciseness 10) vs non-MCP 6.6/10 (correctness 5, completeness 10, usability 5, groundedness 10, conciseness 3) — _skipped (error or empty)_
- Self-consistency runs: 0; flags: batch-skipped

**Grounding (claim-level fact check):**
- MCP: —
- non-MCP: 2/2 verified (1.000); 2 files, 0 lines, 0 symbols; unverified: []

**Pros (MCP-only)**

- Clear error message for missing files

**Cons (what MCP loses vs Grep/Read)**

- Error path, not a real use case

**Verdict:** Validates error handling — both sides should report 'file not found' gracefully.

---

