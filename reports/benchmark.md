# Qartez MCP Per-Tool Benchmark

- Generated at: `1776352461` (unix)
- Git SHA: `05e5509`
- Tokenizer: `cl100k_base`
- Winner column uses a 5% tie margin on token savings.
- Latency is the mean of trimmed samples; expect noisy ratios near 1×.
- ✱ 10 scenario(s) marked with ✱ have an incomplete non-MCP sim - the non-MCP side cannot produce a comparable answer, so the token and Savings columns are shown as `-`. MCP is awarded the win on correctness; the Speedup column still reflects the real latency cost the non-MCP side paid for its partial output.

## Headline

**Aggregate token savings vs Glob+Grep+Read: +91.8%** (Σ MCP 38789 / Σ non-MCP 472109 tokens across 28 scenarios)

_Note: 10/28 scenario(s) have an incomplete non-MCP sim. Those rows still contribute their MCP tokens to both sums, so this headline is a conservative under-count of the real win._

**Avg LLM-judge quality (claude-opus-4-6): MCP 8.3/10 vs non-MCP 4.3/10** (n=28; 5 axes: correctness/completeness/usability/groundedness/conciseness)

## Session cost context

Typical Claude Code session base cost: ~20,000 tokens (system prompt + CLAUDE.md + tools).
MCP tool savings this run: 433320 tokens (21.7 empty sessions).

## Matrix

| Tool | MCP tok | non-MCP tok | Savings | MCP ms | non-MCP ms | Speedup | Precision | Recall | Eff. Savings | Quality (MCP / non-MCP) | Grounding (MCP / non-MCP) | Winner |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|:---:|:---:|---|
| `qartez_map` | 87 | 674 | +87.1% | 0.40 | 0.50 | 1.25× | 1.00 | 1.00 | +87.1% | 10.0 / 4.6 | 100% (5/5) / 100% (101/101) | **mcp** |
| `qartez_find` | 52 | 1490 | +96.5% | 0.02 | 3.63 | 209.50× | 1.00 | 0.00 | - | 8.0 / 4.0 | 100% (2/2) / 71% (10/14) | **mcp** |
| `qartez_read` | 150 | 495 | +69.7% | 0.03 | 3.04 | 100.36× | 1.00 | 0.00 | - | 6.2 / 3.2 | 100% (2/2) / 67% (4/6) | **mcp** |
| `qartez_grep` | 127 | 763 | +83.4% | 0.04 | 3.05 | 71.94× | 0.67 | 0.21 | +17.5% | 7.4 / 4.0 | 100% (10/10) / 95% (37/39) | **mcp** |
| `qartez_outline` | 3009 | 77843 | +96.1% | 0.13 | 0.70 | 5.45× | 0.00 | 1.00 | +96.1% | 10.0 / 4.2 | 99% (85/86) / 73% (90/123) | **mcp** |
| `qartez_deps` | 166 | 2286 | +92.7% | 0.05 | 5.90 | 118.03× | 1.00 | 1.00 | +92.7% | 9.4 / 6.2 | 100% (11/11) / 100% (117/117) | **mcp** |
| `qartez_refs` | 201 | 692 | +71.0% | 0.11 | 2.92 | 25.63× | 1.00 | 0.00 | - | 5.8 / 5.0 | 50% (6/12) / 97% (34/35) | **mcp** |
| `qartez_impact` | 352 | 9243 | +96.2% | 0.19 | 27.21 | 140.02× | 1.00 | 1.00 | +96.2% | 10.0 / 4.4 | 100% (26/26) / 36% (81/226) | **mcp** |
| `qartez_cochange` | 92 | 14622 | +99.4% | 25.98 | 46.75 | 1.80× | - | - | - | 8.8 / 3.2 | 83% (5/6) / 25% (54/217) | **mcp** |
| `qartez_unused` | 468 | 6750 | +93.1% | 0.14 | 3.01 | 21.53× | 1.00 | 0.00 | - | 6.8 / 2.6 | 100% (12/12) / 97% (621/638) | **mcp** |
| `qartez_stats` | 155 | 848 | +81.7% | 0.62 | 0.40 | 0.65× | - | - | - | 9.4 / 4.0 | 100% (5/5) / 95% (114/120) | **mcp** |
| `qartez_calls` | 564 | 2409 | +76.6% | 0.96 | 3.10 | 3.21× | - | - | - | 7.8 / 4.6 | 78% (32/41) / 78% (35/45) | **mcp** |
| `qartez_context` | 107 | 4489 | +97.6% | 0.07 | 39.47 | 533.39× | - | - | - | 8.8 / 4.6 | 100% (6/6) / 97% (191/196) | **mcp** |
| `qartez_rename` | 439 | 648 | +32.3% | 0.30 | 3.28 | 10.87× | - | - | - | 7.8 / 6.4 | 86% (6/7) / 100% (36/36) | **mcp** |
| `qartez_move` | 161 | 701 | +77.0% | 0.04 | 6.40 | 158.72× | - | - | - | 7.6 / 6.0 | 100% (2/2) / 100% (7/7) | **mcp** |
| `qartez_rename_file` | 27 | 185 | +85.4% | 0.01 | 2.96 | 211.48× | - | - | - | 7.8 / 5.6 | 75% (3/4) / 100% (9/9) | **mcp** |
| `qartez_project` | 68 | 1394 | +95.1% | 2.48 | 0.02 | 0.01× | - | - | - | 8.8 / 5.2 | - / 100% (4/4) | **mcp** |
| `qartez_hotspots` ✱ | 523 | - | - | 3.87 | 45.76 | 11.81× | 1.00 | 0.00 | - | 8.0 / 2.0 | 100% (10/10) / 93% (5572/5995) | **mcp** |
| `qartez_clones` ✱ | 4833 | - | - | 1.45 | 1.20 | 0.83× | 1.00 | 1.00 | +94.9% | 10.0 / 4.0 | 100% (37/37) / 83% (263/315) | **mcp** |
| `qartez_smells` ✱ | 939 | - | - | 9.37 | 3.65 | 0.39× | 1.00 | 0.00 | - | 8.0 / 2.0 | 100% (26/26) / 95% (5473/5764) | **mcp** |
| `qartez_test_gaps` ✱ | 291 | - | - | 4.46 | 6.88 | 1.54× | 1.00 | 0.00 | - | 8.0 / 3.2 | 100% (10/10) / 97% (1847/1910) | **mcp** |
| `qartez_wiki` ✱ | 3091 | - | - | 0.46 | 3.41 | 7.37× | 1.00 | 1.00 | +23.9% | 9.0 / 5.0 | 99% (148/149) / 98% (302/308) | **mcp** |
| `qartez_boundaries` ✱ | 138 | - | - | 0.07 | 3.37 | 48.56× | 1.00 | 1.00 | +95.6% | 8.8 / 4.6 | 100% (3/3) / 100% (230/230) | **mcp** |
| `qartez_hierarchy` | 735 | 2056 | +64.3% | 0.05 | 6.25 | 127.48× | 1.00 | 1.00 | +64.3% | 9.4 / 6.8 | 100% (72/72) / 91% (43/47) | **mcp** |
| `qartez_trend` ✱ | 17796 | - | - | 120.21 | 12.06 | 0.10× | 1.00 | 1.00 | -11924.3% | 8.0 / 4.0 | 100% (1/1) / 0% (0/1) | **mcp** |
| `qartez_knowledge` ✱ | 82 | - | - | 11.78 | 43.43 | 3.69× | 1.00 | 1.00 | +98.8% | 8.2 / 3.2 | - / 0% (0/132) | **mcp** |
| `qartez_diff_impact` ✱ | 464 | - | - | 1.02 | 4.00 | 3.92× | 1.00 | 1.00 | +81.3% | 7.4 / 4.6 | 0% (0/18) / 100% (129/129) | **mcp** |
| `qartez_security` ✱ | 3672 | - | - | 15.77 | 18.10 | 1.15× | 0.00 | 0.00 | - | 8.0 / 3.2 | 100% (62/62) / 94% (2070/2204) | **mcp** |
## Quality (LLM judge + programmatic)

| Tool | Correctness | Usability | Completeness† | Groundedness† | Conciseness† | Avg |
|---|:---:|:---:|:---:|:---:|:---:|:---:|
| `qartez_map` | 10/0 | 10/0 | 10/10 | 10/10 | 10/3 | 10.0/4.6 |
| `qartez_find` | 10/7 | 10/5 | 0/0 | 10/5 | 10/3 | 8.0/4.0 |
| `qartez_read` | 7/3 | 7/3 | 0/0 | 10/5 | 7/5 | 6.2/3.2 |
| `qartez_grep` | 7/7 | 10/3 | 0/0 | 10/7 | 10/3 | 7.4/4.0 |
| `qartez_outline` | 10/3 | 10/0 | 10/10 | 10/5 | 10/3 | 10.0/4.2 |
| `qartez_deps` | 7/5 | 10/3 | 10/10 | 10/10 | 10/3 | 9.4/6.2 |
| `qartez_refs` | 7/7 | 10/3 | 0/0 | 5/10 | 7/5 | 5.8/5.0 |
| `qartez_impact` | 10/3 | 10/3 | 10/10 | 10/3 | 10/3 | 10.0/4.4 |
| `qartez_cochange` | 10/3 | 10/0 | 7/7 | 7/3 | 10/3 | 8.8/3.2 |
| `qartez_unused` | 7/0 | 7/0 | 0/0 | 10/10 | 10/3 | 6.8/2.6 |
| `qartez_stats` | 10/0 | 10/0 | 7/7 | 10/10 | 10/3 | 9.4/4.0 |
| `qartez_calls` | 10/3 | 10/3 | 7/7 | 5/5 | 7/5 | 7.8/4.6 |
| `qartez_context` | 7/3 | 10/0 | 7/7 | 10/10 | 10/3 | 8.8/4.6 |
| `qartez_rename` | 10/5 | 10/5 | 7/7 | 7/10 | 5/5 | 7.8/6.4 |
| `qartez_move` | 7/5 | 7/3 | 7/7 | 10/10 | 7/5 | 7.6/6.0 |
| `qartez_rename_file` | 7/5 | 10/3 | 7/7 | 5/10 | 10/3 | 7.8/5.6 |
| `qartez_project` | 10/3 | 10/3 | 7/7 | 7/10 | 10/3 | 8.8/5.2 |
| `qartez_hotspots` | 10/0 | 10/0 | 0/0 | 10/7 | 10/3 | 8.0/2.0 |
| `qartez_clones` | 10/0 | 10/0 | 10/10 | 10/7 | 10/3 | 10.0/4.0 |
| `qartez_smells` | 10/0 | 10/0 | 0/0 | 10/7 | 10/3 | 8.0/2.0 |
| `qartez_test_gaps` | 10/3 | 10/0 | 0/0 | 10/10 | 10/3 | 8.0/3.2 |
| `qartez_wiki` | 10/0 | 10/0 | 10/10 | 10/10 | 5/5 | 9.0/5.0 |
| `qartez_boundaries` | 7/0 | 7/0 | 10/10 | 10/10 | 10/3 | 8.8/4.6 |
| `qartez_hierarchy` | 10/7 | 10/5 | 10/10 | 10/7 | 7/5 | 9.4/6.8 |
| `qartez_trend` | 10/0 | 7/0 | 10/10 | 10/3 | 3/7 | 8.0/4.0 |
| `qartez_knowledge` | 7/0 | 7/0 | 10/10 | 7/3 | 10/3 | 8.2/3.2 |
| `qartez_diff_impact` | 7/0 | 7/0 | 10/10 | 3/10 | 10/3 | 7.4/4.6 |
| `qartez_security` | 10/3 | 10/3 | 0/0 | 10/7 | 10/3 | 8.0/3.2 |

† Programmatic (not LLM-scored). Correctness and Usability are LLM-scored via single batch call.
Format: MCP/non-MCP.

Judge token budget: ~37,000 tokens (1 batch call, 28 scenarios).


## Per-tool detail

### `qartez_map` - qartez_map_top5_concise

Rank the top 5 files by PageRank (concise format).

**MCP side**

- Args: `{"format":"concise","top_n":5}`
- Response: 230 bytes → 87 tokens (naive 57)
- Latency: mean 0.401 ms, p50 0.401 ms, p95 0.402 ms, σ 0.001 ms (n=3)

**Non-MCP side**

- Steps:
  - `Glob **/*.rs`
- Response: 2455 bytes → 674 tokens (naive 613)
- Latency: mean 0.501 ms, p50 0.497 ms, p95 0.508 ms, σ 0.006 ms (n=3)

**Savings:** +87.1% tokens, +90.6% bytes, 1.25× speedup

**LLM-judge (claude-opus-4-6):** MCP 10.0/10 (correctness 10, completeness 10, usability 10, groundedness 10, conciseness 10) vs non-MCP 4.6/10 (correctness 0, completeness 10, usability 0, groundedness 10, conciseness 3) - _MCP gives ranked PageRank top-5 with scores/blast; non-MCP dumps all 100+ files unranked with no PR data_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 5/5 verified (1.000); 5 files, 0 lines, 0 symbols; unverified: []
- non-MCP: 101/101 verified (1.000); 101 files, 0 lines, 0 symbols; unverified: []

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

### `qartez_find` - qartez_find_struct_qartezserver

Locate the QartezServer struct definition.

**MCP side**

- Args: `{"name":"QartezServer"}`
- Response: 161 bytes → 52 tokens (naive 40)
- Latency: mean 0.017 ms, p50 0.017 ms, p95 0.018 ms, σ 0.000 ms (n=3)

**Non-MCP side**

- Steps:
  - `Grep /struct\s+QartezServer/ (*.rs)`
  - `Read src/server/mod.rs lines 1-120`
- Response: 5494 bytes → 1490 tokens (naive 1373)
- Latency: mean 3.631 ms, p50 3.651 ms, p95 3.660 ms, σ 0.035 ms (n=3)

**Savings:** +96.5% tokens, +97.1% bytes, 209.50× speedup

**LLM-judge (claude-opus-4-6):** MCP 8.0/10 (correctness 10, completeness 0, usability 10, groundedness 10, conciseness 10) vs non-MCP 4.0/10 (correctness 7, completeness 0, usability 5, groundedness 5, conciseness 3) - _Both locate the struct; MCP is precise (1 match, signature, exported); non-MCP buries it in grep noise + file dump_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 2/2 verified (1.000); 0 files, 1 lines, 1 symbols; unverified: []
- non-MCP: 10/14 verified (0.714); 3 files, 4 lines, 7 symbols; unverified: [mod.rs, path_prefix, symbols_body_fts, repo-a/src/main.rs]

**Pros (MCP-only)**

- Pre-indexed exact line range (no brace counting)
- Signature pre-extracted
- Kind filter, export-status flag
- `regex: true` walks the indexed symbol table for pattern matches

**Cons (what MCP loses vs Grep/Read)**

- Only the definition site, not usages
- Misses macro-synthesized symbols - tree-sitter opaque-tokens the macro body, so `lazy_static! { pub static ref FOO }` doesn't surface `FOO` in the index

**Verdict:** MCP wins on precision and compactness, but the non-MCP path is viable for unique symbol names.

---

### `qartez_read` - qartez_read_truncate_path

Read the body of the `truncate_path` helper.

**MCP side**

- Args: `{"symbol_name":"truncate_path"}`
- Response: 483 bytes → 150 tokens (naive 120)
- Latency: mean 0.030 ms, p50 0.030 ms, p95 0.031 ms, σ 0.000 ms (n=3)

**Non-MCP side**

- Steps:
  - `Grep /fn truncate_path/ (*.rs)`
  - `Read src/server/mod.rs lines 260-290`
- Response: 1922 bytes → 495 tokens (naive 480)
- Latency: mean 3.044 ms, p50 3.011 ms, p95 3.110 ms, σ 0.054 ms (n=3)

**Savings:** +69.7% tokens, +74.9% bytes, 100.36× speedup

**LLM-judge (claude-opus-4-6):** MCP 6.2/10 (correctness 7, completeness 0, usability 7, groundedness 10, conciseness 7) vs non-MCP 3.2/10 (correctness 3, completeness 0, usability 3, groundedness 5, conciseness 5) - _MCP shows function body at L85-92 (slightly truncated); non-MCP grep finds location but reads unrelated code_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 2/2 verified (1.000); 0 files, 1 lines, 1 symbols; unverified: []
- non-MCP: 4/6 verified (0.667); 0 files, 3 lines, 3 symbols; unverified: [truncate_path_short_no_change, truncate_path_with_unicode_dirs]

**Pros (MCP-only)**

- Jumps directly to the symbol by indexed line range
- No brace counting or over-reading
- Numbered output matches Read semantics
- `start_line` / `end_line` support raw line-range reads for non-symbol code (imports, file headers)
- `context_lines` opt-in surrounding lines (default 0)

**Cons (what MCP loses vs Grep/Read)**

- Line-range mode requires knowing the target file up-front

**Verdict:** MCP wins decisively - a non-MCP agent must over-read to guarantee body coverage.

---

### `qartez_grep` - qartez_grep_find_symbol_prefix

FTS5 prefix search for symbols starting with `find_symbol`.

**MCP side**

- Args: `{"query":"find_symbol*"}`
- Response: 479 bytes → 127 tokens (naive 119)
- Latency: mean 0.042 ms, p50 0.042 ms, p95 0.044 ms, σ 0.001 ms (n=3)

**Non-MCP side**

- Steps:
  - `Grep /find_symbol/ (*.rs)`
- Response: 2955 bytes → 763 tokens (naive 738)
- Latency: mean 3.045 ms, p50 3.023 ms, p95 3.094 ms, σ 0.040 ms (n=3)

**Savings:** +83.4% tokens, +83.8% bytes, 71.94× speedup

**LLM-judge (claude-opus-4-6):** MCP 7.4/10 (correctness 7, completeness 0, usability 10, groundedness 10, conciseness 10) vs non-MCP 4.0/10 (correctness 7, completeness 0, usability 3, groundedness 7, conciseness 3) - _Both find matching symbols; MCP gives clean 5-row table with kinds/files; non-MCP dumps 30+ raw grep lines with noise_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 10/10 verified (1.000); 0 files, 5 lines, 5 symbols; unverified: []
- non-MCP: 37/39 verified (0.949); 1 files, 31 lines, 7 symbols; unverified: [function, find_symbol]

**Pros (MCP-only)**

- Searches indexed symbols only - no comment/string noise
- Prefix matching via FTS5
- Returns kind + line range per hit
- `regex: true` falls back to in-memory regex over indexed symbol names
- `search_bodies: true` hits a pre-indexed FTS5 body table for text inside function bodies

**Cons (what MCP loses vs Grep/Read)**

- Only indexed languages
- Body FTS storage grows ~1-2× the codebase when `search_bodies` is used

**Verdict:** MCP wins on signal-to-noise; grep still wins when you need to find text inside bodies.

---

### `qartez_outline` - qartez_outline_server_mod

Outline all symbols in src/server/mod.rs grouped by kind.

**MCP side**

- Args: `{"file_path":"src/server/mod.rs"}`
- Response: 10275 bytes → 3009 tokens (naive 2568)
- Latency: mean 0.129 ms, p50 0.128 ms, p95 0.131 ms, σ 0.001 ms (n=3)

**Non-MCP side**

- Steps:
  - `Read src/server/mod.rs`
- Response: 299877 bytes → 77843 tokens (naive 74969)
- Latency: mean 0.704 ms, p50 0.700 ms, p95 0.717 ms, σ 0.011 ms (n=3)

**Savings:** +96.1% tokens, +96.6% bytes, 5.45× speedup

**LLM-judge (claude-opus-4-6):** MCP 10.0/10 (correctness 10, completeness 10, usability 10, groundedness 10, conciseness 10) vs non-MCP 4.2/10 (correctness 3, completeness 10, usability 0, groundedness 5, conciseness 3) - _MCP gives 96-symbol outline grouped by kind with CC/visibility; non-MCP dumps raw source file (295K truncated)_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 85/86 verified (0.988); 1 files, 0 lines, 85 symbols; unverified: [std]
- non-MCP: 90/123 verified (0.732); 11 files, 0 lines, 112 symbols; unverified: [in, Arc, bar, foo, Foo]

**Pros (MCP-only)**

- Symbols pre-grouped by kind
- Signatures pre-parsed
- Token-budgeted - no full-file read
- Struct fields are emitted as child rows nested under their parent

**Cons (what MCP loses vs Grep/Read)**

- Token budget truncates very large files
- Tuple-struct members are skipped - nothing meaningful to name

**Verdict:** MCP wins by ~20x on a 2300-line file; non-MCP path reads the entire file.

---

### `qartez_deps` - qartez_deps_server_mod

Show incoming/outgoing file-level dependencies for src/server/mod.rs.

**MCP side**

- Args: `{"file_path":"src/server/mod.rs"}`
- Response: 594 bytes → 166 tokens (naive 148)
- Latency: mean 0.050 ms, p50 0.050 ms, p95 0.051 ms, σ 0.001 ms (n=3)

**Non-MCP side**

- Steps:
  - `Grep /^use crate::/ (*.rs)`
  - `Grep /use crate::server/ (*.rs)`
- Response: 8197 bytes → 2286 tokens (naive 2049)
- Latency: mean 5.901 ms, p50 5.881 ms, p95 5.957 ms, σ 0.047 ms (n=3)

**Savings:** +92.7% tokens, +92.8% bytes, 118.03× speedup

**LLM-judge (claude-opus-4-6):** MCP 9.4/10 (correctness 7, completeness 10, usability 10, groundedness 10, conciseness 10) vs non-MCP 6.2/10 (correctness 5, completeness 10, usability 3, groundedness 10, conciseness 3) - _MCP shows clear 8 imports-from + 2 imported-by; non-MCP greps all project imports needing manual filtering_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 11/11 verified (1.000); 11 files, 0 lines, 0 symbols; unverified: []
- non-MCP: 117/117 verified (1.000); 2 files, 115 lines, 0 symbols; unverified: []

**Pros (MCP-only)**

- Edges pre-resolved at index time
- Bidirectional (imports + importers) in one call

**Cons (what MCP loses vs Grep/Read)**

- Flattens to file-level - loses per-symbol specifier
- Doesn't show which items are imported

**Verdict:** MCP wins on accuracy (resolved paths) and compactness.

---

### `qartez_refs` - qartez_refs_find_symbol_by_name

List references to find_symbol_by_name.

**MCP side**

- Args: `{"symbol":"find_symbol_by_name"}`
- Response: 751 bytes → 201 tokens (naive 187)
- Latency: mean 0.114 ms, p50 0.114 ms, p95 0.117 ms, σ 0.002 ms (n=3)

**Non-MCP side**

- Steps:
  - `Grep /find_symbol_by_name/ (*.rs)`
- Response: 2681 bytes → 692 tokens (naive 670)
- Latency: mean 2.922 ms, p50 2.890 ms, p95 2.983 ms, σ 0.050 ms (n=3)

**Savings:** +71.0% tokens, +72.0% bytes, 25.63× speedup

**LLM-judge (claude-opus-4-6):** MCP 5.8/10 (correctness 7, completeness 0, usability 10, groundedness 5, conciseness 7) vs non-MCP 5.0/10 (correctness 7, completeness 0, usability 3, groundedness 10, conciseness 5) - _Both find references; MCP separates definition/refs/11 call sites; non-MCP mixes code refs with string literals_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 6/12 verified (0.500); 5 files, 1 lines, 6 symbols; unverified: [L483, L1934, L2792, L2946, L2998]
- non-MCP: 34/35 verified (0.971); 1 files, 28 lines, 6 symbols; unverified: [function]

**Pros (MCP-only)**

- Specifier-aware filtering (vs raw text)
- Optional transitive BFS
- Def + uses in one response
- AST-resolved call sites now listed alongside file-edge importers

**Cons (what MCP loses vs Grep/Read)**

- Misses dynamic dispatch: trait-object calls resolve at runtime and leave no static edge or call-site name the tree-sitter walker can anchor to
- Transitive BFS can balloon on hub symbols - a symbol whose file is imported by 50 crates yields 50 * avg-fanout rows

**Verdict:** MCP now surfaces both file-level edges and AST call sites, closing the gap with grep while keeping the tree-sitter precision that skips strings and comments.

---

### `qartez_impact` - qartez_impact_storage_read

Blast radius and co-change partners for src/storage/read.rs.

**MCP side**

- Args: `{"file_path":"src/storage/read.rs","include_tests":false}`
- Response: 1123 bytes → 352 tokens (naive 280)
- Latency: mean 0.194 ms, p50 0.195 ms, p95 0.197 ms, σ 0.002 ms (n=3)

**Non-MCP side**

- Steps:
  - `BFS grep from 'storage::read' (depth 2)`
  - `git log -n100 --name-only + pair counts for src/storage/read.rs (top 10)`
- Response: 27707 bytes → 9243 tokens (naive 6926)
- Latency: mean 27.211 ms, p50 27.084 ms, p95 27.455 ms, σ 0.202 ms (n=3)

**Savings:** +96.2% tokens, +95.9% bytes, 140.02× speedup

**LLM-judge (claude-opus-4-6):** MCP 10.0/10 (correctness 10, completeness 10, usability 10, groundedness 10, conciseness 10) vs non-MCP 4.4/10 (correctness 3, completeness 10, usability 3, groundedness 3, conciseness 3) - _MCP gives 8 direct + 20 transitive blast radius + hot symbols; non-MCP greps imports with no blast analysis_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 26/26 verified (1.000); 21 files, 0 lines, 5 symbols; unverified: []
- non-MCP: 81/226 verified (0.358); 146 files, 80 lines, 0 symbols; unverified: [scripts/release.sh, github-pr/SKILL.md, TEMPLATE/config.yml, qartez-public/CLA.md, scripts/import-pr.sh]

**Pros (MCP-only)**

- Combines static blast radius + git co-change in one call
- Transitive BFS pre-computed
- `include_tests: false` by default excludes test modules from the blast radius

**Cons (what MCP loses vs Grep/Read)**

- Co-change is statistical, not causal

**Verdict:** MCP wins on correctness and output size: the non-MCP equivalent is a 2-level import BFS plus a full git-log mine with in-process pair counting - reproducible now that the sim matches what a real agent would actually have to do.

---

### `qartez_cochange` - qartez_cochange_server_mod

Top 5 co-change partners for src/server/mod.rs.

**MCP side**

- Args: `{"file_path":"src/server/mod.rs","limit":5,"max_commit_size":30}`
- Response: 385 bytes → 92 tokens (naive 96)
- Latency: mean 25.976 ms, p50 25.869 ms, p95 26.307 ms, σ 0.277 ms (n=3)

**Non-MCP side**

- Steps:
  - `git log -n200 --name-only + pair counts for src/server/mod.rs (top 5)`
- Response: 42133 bytes → 14622 tokens (naive 10533)
- Latency: mean 46.749 ms, p50 46.401 ms, p95 47.424 ms, σ 0.559 ms (n=3)

**Savings:** +99.4% tokens, +99.1% bytes, 1.80× speedup

**LLM-judge (claude-opus-4-6):** MCP 8.8/10 (correctness 10, completeness 7, usability 10, groundedness 7, conciseness 10) vs non-MCP 3.2/10 (correctness 3, completeness 7, usability 0, groundedness 3, conciseness 3) - _MCP gives ranked top-5 co-change table with counts; non-MCP dumps raw commit file lists (38K+ truncated)_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 5/6 verified (0.833); 6 files, 0 lines, 0 symbols; unverified: [reports/benchmark.js]
- non-MCP: 54/217 verified (0.249); 217 files, 0 lines, 0 symbols; unverified: [scripts/CLAUDE.md, scripts/release.sh, github-pr/SKILL.md, TEMPLATE/config.yml, presentation/app.js]

**Pros (MCP-only)**

- Pre-computed pair counts
- Instant response
- `max_commit_size` arg skips huge refactor commits (default 30) when recomputing from git

**Cons (what MCP loses vs Grep/Read)**

- Depends on git-history granularity

**Verdict:** MCP wins on tokens and latency: the non-MCP sim now faithfully reproduces the full git log mine + in-process pair counting that an agent would have to run by hand.

---

### `qartez_unused` - qartez_unused_whole_repo

Find all unused exported symbols across the repo.

**MCP side**

- Args: `{}`
- Response: 1335 bytes → 468 tokens (naive 333)
- Latency: mean 0.140 ms, p50 0.139 ms, p95 0.142 ms, σ 0.002 ms (n=3)

**Non-MCP side**

- Steps:
  - `Grep /^pub (fn|struct|enum|trait|const)/ (*.rs)`
- Response: 24020 bytes → 6750 tokens (naive 6005)
- Latency: mean 3.007 ms, p50 2.962 ms, p95 3.126 ms, σ 0.098 ms (n=3)

**Savings:** +93.1% tokens, +94.4% bytes, 21.53× speedup

**LLM-judge (claude-opus-4-6):** MCP 6.8/10 (correctness 7, completeness 0, usability 7, groundedness 10, conciseness 10) vs non-MCP 2.6/10 (correctness 0, completeness 0, usability 0, groundedness 10, conciseness 3) - _MCP identifies 108 unused exports by file; non-MCP lists all pub symbols without any usage analysis_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 12/12 verified (1.000); 12 files, 0 lines, 0 symbols; unverified: []
- non-MCP: 621/638 verified (0.973); 0 files, 334 lines, 304 symbols; unverified: [Bob, Foo, add, Read, read]

**Pros (MCP-only)**

- Pre-materialized at index time - query is a single indexed SELECT
- `limit` / `offset` pagination (default 50) keeps default output small
- Trait-impl methods are excluded at parse time via the `unused_excluded` flag

**Cons (what MCP loses vs Grep/Read)**

- Requires human filtering before action - dynamic dispatch callers don't register as static importers
- Pre-materialization is invalidated wholesale on re-index; a stale index may miss recently-added imports

**Verdict:** MCP wins massively: the non-MCP step captured here is only the candidate list - a real agent would need hundreds more greps.

---

### `qartez_stats` - qartez_stats_basic

Overall codebase statistics and language breakdown.

**MCP side**

- Args: `{}`
- Response: 346 bytes → 155 tokens (naive 86)
- Latency: mean 0.618 ms, p50 0.597 ms, p95 0.654 ms, σ 0.030 ms (n=3)

**Non-MCP side**

- Steps:
  - `Glob **/*`
- Response: 3084 bytes → 848 tokens (naive 771)
- Latency: mean 0.399 ms, p50 0.404 ms, p95 0.405 ms, σ 0.007 ms (n=3)

**Savings:** +81.7% tokens, +88.8% bytes, 0.65× speedup

**LLM-judge (claude-opus-4-6):** MCP 9.4/10 (correctness 10, completeness 7, usability 10, groundedness 10, conciseness 10) vs non-MCP 4.0/10 (correctness 0, completeness 7, usability 0, groundedness 10, conciseness 3) - _MCP gives files/LOC/symbols/edges/language breakdown; non-MCP just lists filenames with no statistics_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 5/5 verified (1.000); 5 files, 0 lines, 0 symbols; unverified: []
- non-MCP: 114/120 verified (0.950); 119 files, 0 lines, 1 symbols; unverified: [LICENSE, scripts/AGENTS.md, scripts/GEMINI.md, scripts/CLAUDE.md, reports/benchmark.js]

**Pros (MCP-only)**

- Single-call summary
- Most-connected-files list included
- Optional `file_path` arg drills into per-file LOC / symbol / importer counts
- Aggregate output splits src and test files/LOC

**Cons (what MCP loses vs Grep/Read)**

- Test/src split is filename-based (`tests/`, `_test.rs`, `benches/`) - not build-graph-aware, so integration tests wired via Cargo `test` target live in `src/` look like production code
- Language buckets count files-only; weighted metrics (bytes, symbols per language) aren't broken out

**Verdict:** MCP wins - non-MCP glob dump requires external aggregation just to count files.

---

### `qartez_calls` - qartez_calls_build_overview

Call hierarchy for build_overview (default depth=1).

**MCP side**

- Args: `{"name":"build_overview"}`
- Response: 1986 bytes → 564 tokens (naive 496)
- Latency: mean 0.965 ms, p50 0.958 ms, p95 0.984 ms, σ 0.016 ms (n=3)

**Non-MCP side**

- Steps:
  - `Grep /build_overview/ (*.rs)`
  - `Read src/server/mod.rs lines 46-180`
- Response: 9254 bytes → 2409 tokens (naive 2313)
- Latency: mean 3.102 ms, p50 3.127 ms, p95 3.204 ms, σ 0.101 ms (n=3)

**Savings:** +76.6% tokens, +78.5% bytes, 3.21× speedup

**LLM-judge (claude-opus-4-6):** MCP 7.8/10 (correctness 10, completeness 7, usability 10, groundedness 5, conciseness 7) vs non-MCP 4.6/10 (correctness 3, completeness 7, usability 3, groundedness 5, conciseness 5) - _MCP shows 17 callers + 35 callees with line numbers; non-MCP greps text mentions with no call hierarchy_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 32/41 verified (0.780); 11 files, 1 lines, 29 symbols; unverified: [is_some, is_empty, push_str, as_deref, unwrap_or]
- non-MCP: 35/45 verified (0.778); 4 files, 26 lines, 15 symbols; unverified: [bodies, mod.rs, Response, path_prefix, httpx/_models.py]

**Pros (MCP-only)**

- Tree-sitter AST distinguishes calls from references
- Callees resolved to definition file:line
- Transitive depth available (default 1, opt-in depth=2)
- Per-session parse cache - repeat invocations are in-memory

**Cons (what MCP loses vs Grep/Read)**

- Misses dynamic dispatch - trait-object `Box<dyn Foo>` calls leave no static call-site name
- Depth=2 output can still balloon on hub functions; the grouping elision helps but the graph is inherently O(N^depth)

**Verdict:** MCP wins on correctness (tree-sitter distinguishes call sites from type-position references) and tokens. A per-invocation parse cache and a textual pre-filter keep the AST walk from re-visiting files that cannot possibly contain the callee, so the cold-parse cost stays in the low milliseconds.

---

### `qartez_context` - qartez_context_server_mod

Top 5 related files for src/server/mod.rs.

**MCP side**

- Args: `{"files":["src/server/mod.rs"],"limit":5}`
- Response: 291 bytes → 107 tokens (naive 72)
- Latency: mean 0.074 ms, p50 0.074 ms, p95 0.075 ms, σ 0.001 ms (n=3)

**Non-MCP side**

- Steps:
  - `Grep /use crate::/ (*.rs)`
  - `Grep /use crate::server/ (*.rs)`
  - `git log -n50 -- src/server/mod.rs`
- Response: 16098 bytes → 4489 tokens (naive 4024)
- Latency: mean 39.471 ms, p50 39.787 ms, p95 39.838 ms, σ 0.488 ms (n=3)

**Savings:** +97.6% tokens, +98.2% bytes, 533.39× speedup

**LLM-judge (claude-opus-4-6):** MCP 8.8/10 (correctness 7, completeness 7, usability 10, groundedness 10, conciseness 10) vs non-MCP 4.6/10 (correctness 3, completeness 7, usability 0, groundedness 10, conciseness 3) - _MCP gives 5 scored related files; non-MCP dumps all project imports (12K+ truncated) with no relevance ranking_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 6/6 verified (1.000); 6 files, 0 lines, 0 symbols; unverified: []
- non-MCP: 191/196 verified (0.974); 6 files, 181 lines, 9 symbols; unverified: [func_, caller, a/mod.rs, test_helper, qartez-public/src/server/mod.rs]

**Pros (MCP-only)**

- Multi-signal scoring: deps + cochange + PageRank + task FTS
- Reason tags explain every row

**Cons (what MCP loses vs Grep/Read)**

- Opaque composite score
- Cannot answer 'why was X excluded'

**Verdict:** MCP wins - no single Grep/Read chain can approximate the composite ranking.

---

### `qartez_rename` - qartez_rename_truncate_path_preview

Preview rename of truncate_path → trunc_path (no apply).

**MCP side**

- Args: `{"apply":false,"new_name":"trunc_path","old_name":"truncate_path"}`
- Response: 1256 bytes → 439 tokens (naive 314)
- Latency: mean 0.302 ms, p50 0.302 ms, p95 0.319 ms, σ 0.016 ms (n=3)

**Non-MCP side**

- Steps:
  - `Grep /\btruncate_path\b/ (*.rs)`
- Response: 2629 bytes → 648 tokens (naive 657)
- Latency: mean 3.282 ms, p50 3.284 ms, p95 3.306 ms, σ 0.022 ms (n=3)

**Savings:** +32.3% tokens, +52.2% bytes, 10.87× speedup

**LLM-judge (claude-opus-4-6):** MCP 7.8/10 (correctness 10, completeness 7, usability 10, groundedness 7, conciseness 5) vs non-MCP 6.4/10 (correctness 5, completeness 7, usability 5, groundedness 10, conciseness 5) - _MCP shows 27 renamed occurrences with new code; non-MCP greps current name without rename preview_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 6/7 verified (0.857); 5 files, 0 lines, 2 symbols; unverified: [trunc_path]
- non-MCP: 36/36 verified (1.000); 2 files, 32 lines, 2 symbols; unverified: []

**Pros (MCP-only)**

- Tree-sitter identifier matching skips strings/comments
- Atomic apply with word-boundary fallback
- Preview + apply in one API
- Correctly handles aliased imports (`use foo::bar as baz`) - enshrined by a unit test

**Cons (what MCP loses vs Grep/Read)**

- Only indexed languages

**Verdict:** MCP wins on tokens and safety. The AST-based identifier match on a 2300-line file runs in the low single-digit milliseconds - slower than a raw grep but the cost buys correct skipping of strings, comments, and same-spelled but unrelated identifiers.

---

### `qartez_move` - qartez_move_capitalize_kind_preview

Preview moving the `capitalize_kind` helper to a new file.

**MCP side**

- Args: `{"apply":false,"symbol":"capitalize_kind","to_file":"src/server/helpers.rs"}`
- Response: 692 bytes → 161 tokens (naive 173)
- Latency: mean 0.040 ms, p50 0.040 ms, p95 0.041 ms, σ 0.000 ms (n=3)

**Non-MCP side**

- Steps:
  - `Grep /fn capitalize_kind/ (*.rs)`
  - `Read src/server/mod.rs lines 2180-2225`
  - `Grep /\bcapitalize_kind\b/ (*.rs)`
- Response: 2465 bytes → 701 tokens (naive 616)
- Latency: mean 6.402 ms, p50 6.390 ms, p95 6.454 ms, σ 0.044 ms (n=3)

**Savings:** +77.0% tokens, +71.9% bytes, 158.72× speedup

**LLM-judge (claude-opus-4-6):** MCP 7.6/10 (correctness 7, completeness 7, usability 7, groundedness 10, conciseness 7) vs non-MCP 6.0/10 (correctness 5, completeness 7, usability 3, groundedness 10, conciseness 5) - _MCP previews extraction with code + import analysis; non-MCP finds location but dumps unrelated code around it_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 2/2 verified (1.000); 2 files, 0 lines, 0 symbols; unverified: []
- non-MCP: 7/7 verified (1.000); 1 files, 4 lines, 2 symbols; unverified: []

**Pros (MCP-only)**

- Atomic extraction + insertion + importer rewriting
- Refuses on ambiguous symbols
- Refuses when destination already defines a same-kind same-name symbol
- Importer rewriting uses regex word boundaries (no substring over-match)

**Cons (what MCP loses vs Grep/Read)**

- Does not rewrite doc-comment references like `[`foo::bar`]` - the rewriter targets `use` paths and qualified call sites only
- Ambiguity check is by symbol name, not by fully-qualified path - a free function `foo` and a method `foo` on a struct both count as ambiguous even when only one matches the move target kind

**Verdict:** MCP wins - non-MCP path is a 3-step sequence with several edit-time pitfalls.

---

### `qartez_rename_file` - qartez_rename_file_server_mod_preview

Preview renaming src/server/mod.rs → src/server/server.rs.

**MCP side**

- Args: `{"apply":false,"from":"src/server/mod.rs","to":"src/server/server.rs"}`
- Response: 106 bytes → 27 tokens (naive 26)
- Latency: mean 0.014 ms, p50 0.014 ms, p95 0.014 ms, σ 0.000 ms (n=3)

**Non-MCP side**

- Steps:
  - `Grep /crate::server/ (*.rs)`
- Response: 675 bytes → 185 tokens (naive 168)
- Latency: mean 2.961 ms, p50 2.924 ms, p95 3.074 ms, σ 0.095 ms (n=3)

**Savings:** +85.4% tokens, +84.3% bytes, 211.48× speedup

**LLM-judge (claude-opus-4-6):** MCP 7.8/10 (correctness 7, completeness 7, usability 10, groundedness 5, conciseness 10) vs non-MCP 5.6/10 (correctness 5, completeness 7, usability 3, groundedness 10, conciseness 3) - _MCP identifies 2 importers in one line; non-MCP greps crate::server mixing real imports with string literals_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 3/4 verified (0.750); 4 files, 0 lines, 0 symbols; unverified: [src/server/server.rs]
- non-MCP: 9/9 verified (1.000); 0 files, 8 lines, 1 symbols; unverified: []

**Pros (MCP-only)**

- Atomic mv + import path rewriting
- Handles mod.rs → named-module transform
- Regex word-boundary matching prevents over-match on import stems

**Cons (what MCP loses vs Grep/Read)**

- `mod.rs → named.rs` edge case: the parent module's `mod foo;` declaration is *not* rewritten because the rename tool only touches files that import via `use crate::foo::…`, not files that declare the module
- Doc links (`[`crate::foo`]` in `///` comments) aren't rewritten - the rewriter is limited to `use`-path tokens

**Verdict:** MCP wins for single-shot refactors; non-MCP path produces correct results only with careful scoping.

---

### `qartez_project` - qartez_project_info

Detect the toolchain and report its commands.

**MCP side**

- Args: `{"action":"info"}`
- Response: 253 bytes → 68 tokens (naive 63)
- Latency: mean 2.476 ms, p50 2.551 ms, p95 2.555 ms, σ 0.110 ms (n=3)

**Non-MCP side**

- Steps:
  - `Read Cargo.toml`
- Response: 3508 bytes → 1394 tokens (naive 877)
- Latency: mean 0.021 ms, p50 0.020 ms, p95 0.025 ms, σ 0.003 ms (n=3)

**Savings:** +95.1% tokens, +92.8% bytes, 0.01× speedup

**LLM-judge (claude-opus-4-6):** MCP 8.8/10 (correctness 10, completeness 7, usability 10, groundedness 7, conciseness 10) vs non-MCP 5.2/10 (correctness 3, completeness 7, usability 3, groundedness 10, conciseness 3) - _MCP detects rust+make toolchains with all commands; non-MCP shows raw Cargo.toml requiring manual derivation_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: -
- non-MCP: 4/4 verified (1.000); 4 files, 0 lines, 0 symbols; unverified: []

**Pros (MCP-only)**

- Auto-detection across 5+ ecosystems (Cargo, npm/bun, Go, Python, Make)
- Consistent output format
- `run` action resolves a subcommand to its shell form without executing

**Cons (what MCP loses vs Grep/Read)**

- Detects toolchain by file presence only - `Cargo.toml` / `package.json` / `go.mod` existing is enough, the tool never runs a probe command to confirm the toolchain actually works
- No polyglot support - a Rust + Node monorepo resolves to the first detected toolchain (Cargo wins, Node is silent), so callers need to scope to a sub-directory to see the other

**Verdict:** Near-tie: reading Cargo.toml is already cheap; MCP wins only on portability across ecosystems.

---

### `qartez_hotspots` - qartez_hotspots_top10

Top 10 hotspots by composite score (complexity x coupling x churn).

**MCP side**

- Args: `{"limit":10}`
- Response: 1326 bytes → 523 tokens (naive 331)
- Latency: mean 3.874 ms, p50 3.843 ms, p95 3.941 ms, σ 0.055 ms (n=3)

**Non-MCP side** ✱ **incomplete** - the step sequence below does not produce a comparable answer; byte/token counts are noise, not a measure of efficiency

- Steps:
  - `Glob **/*.rs`
  - `Grep /fn |class |def |func / (*.rs)`
  - `git log -n200 -- .`
- Response: 292602 bytes → 82822 tokens (naive 73150)
- Latency: mean 45.764 ms, p50 45.747 ms, p95 45.892 ms, σ 0.111 ms (n=3)

**Savings:** - tokens, - bytes, 11.81× speedup (token comparison skipped: non-MCP sim is incomplete)

**LLM-judge (claude-opus-4-6):** MCP 8.0/10 (correctness 10, completeness 0, usability 10, groundedness 10, conciseness 10) vs non-MCP 2.0/10 (correctness 0, completeness 0, usability 0, groundedness 7, conciseness 3) - _MCP ranks 10 hotspots with composite score/CC/churn/PageRank; non-MCP just lists files with no metrics_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 10/10 verified (1.000); 10 files, 0 lines, 0 symbols; unverified: []
- non-MCP: 5572/5995 verified (0.929); 238 files, 3372 lines, 2385 symbols; unverified: [hi, gr, as, it, App]

**Pros (MCP-only)**

- Composite score from three independent signals in one call
- Health score (0-10) for quick triage
- File-level and symbol-level granularity
- Sortable by individual factor (complexity, coupling, churn)

**Cons (what MCP loses vs Grep/Read)**

- Churn depends on git history depth at index time
- Composite weights are fixed, not tunable

**Verdict:** MCP wins overwhelmingly: non-MCP path requires three separate data-gathering passes (complexity parsing, import counting, git log mining) plus manual correlation.

---

### `qartez_clones` - qartez_clones_top10

Top 10 duplicate code groups by AST hash similarity.

**MCP side**

- Args: `{"limit":10}`
- Response: 16351 bytes → 4833 tokens (naive 4087)
- Latency: mean 1.454 ms, p50 1.455 ms, p95 1.456 ms, σ 0.003 ms (n=3)

**Non-MCP side** ✱ **incomplete** - the step sequence below does not produce a comparable answer; byte/token counts are noise, not a measure of efficiency

- Steps:
  - `Glob **/*.rs`
  - `Read src/server/mod.rs`
  - `Read src/storage/read.rs`
  - `≈12000B representative padding`
- Response: 370541 bytes → 95154 tokens (naive 92635)
- Latency: mean 1.201 ms, p50 1.202 ms, p95 1.204 ms, σ 0.003 ms (n=3)

**Savings:** - tokens, - bytes, 0.83× speedup (token comparison skipped: non-MCP sim is incomplete)

**LLM-judge (claude-opus-4-6):** MCP 10.0/10 (correctness 10, completeness 10, usability 10, groundedness 10, conciseness 10) vs non-MCP 4.0/10 (correctness 0, completeness 10, usability 0, groundedness 7, conciseness 3) - _MCP finds 176 clone groups with AST hashing, lists duplicates; non-MCP dumps file list + source (366K truncated)_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 37/37 verified (1.000); 37 files, 0 lines, 0 symbols; unverified: []
- non-MCP: 263/315 verified (0.835); 116 files, 0 lines, 199 symbols; unverified: [in, OR, f_, is, Arc]

**Pros (MCP-only)**

- AST-hash-based detection skips whitespace and comment differences
- Groups clones by structural similarity, not textual
- Pagination support (limit/offset)
- min_lines filter excludes trivial getters

**Cons (what MCP loses vs Grep/Read)**

- Only detects Type-2 clones (structurally identical after normalization)
- Cross-file detection requires reading the full index

**Verdict:** MCP wins: no non-MCP toolchain can do AST-level clone detection in a single call; text-based diff is the only alternative and misses renamed-variable clones.

---

### `qartez_smells` - qartez_smells_all_kinds

Detect all code smell kinds (god functions, long params, feature envy).

**MCP side**

- Args: `{}`
- Response: 2870 bytes → 939 tokens (naive 717)
- Latency: mean 9.368 ms, p50 9.272 ms, p95 9.565 ms, σ 0.163 ms (n=3)

**Non-MCP side** ✱ **incomplete** - the step sequence below does not produce a comparable answer; byte/token counts are noise, not a measure of efficiency

- Steps:
  - `Grep /fn |class |def |func / (*.rs)`
  - `≈4000B representative padding`
- Response: 269785 bytes → 75370 tokens (naive 67446)
- Latency: mean 3.652 ms, p50 3.648 ms, p95 3.671 ms, σ 0.016 ms (n=3)

**Savings:** - tokens, - bytes, 0.39× speedup (token comparison skipped: non-MCP sim is incomplete)

**LLM-judge (claude-opus-4-6):** MCP 8.0/10 (correctness 10, completeness 0, usability 10, groundedness 10, conciseness 10) vs non-MCP 2.0/10 (correctness 0, completeness 0, usability 0, groundedness 7, conciseness 3) - _MCP detects 142 smells (god funcs, long params) with CC/lines; non-MCP dumps test signatures (265K truncated)_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 26/26 verified (1.000); 17 files, 0 lines, 9 symbols; unverified: []
- non-MCP: 5473/5764 verified (0.950); 7 files, 3372 lines, 2385 symbols; unverified: [hi, gr, as, it, App]

**Pros (MCP-only)**

- Three smell detectors in one call: god functions, long parameter lists, feature envy
- Configurable thresholds per smell kind
- File-scoped mode available
- Cyclomatic complexity pre-computed at index time

**Cons (what MCP loses vs Grep/Read)**

- Feature envy only reliable for Rust and Java where owner_type is populated
- Thresholds are heuristic, not project-calibrated

**Verdict:** MCP wins: a non-MCP agent needs to read every function body and manually count branches, parameters, and cross-type calls.

---

### `qartez_test_gaps` - qartez_test_gaps_top10

Top 10 untested source files ranked by risk (PageRank).

**MCP side**

- Args: `{"limit":10,"mode":"gaps"}`
- Response: 686 bytes → 291 tokens (naive 171)
- Latency: mean 4.455 ms, p50 4.459 ms, p95 4.502 ms, σ 0.044 ms (n=3)

**Non-MCP side** ✱ **incomplete** - the step sequence below does not produce a comparable answer; byte/token counts are noise, not a measure of efficiency

- Steps:
  - `Glob **/*.rs`
  - `Grep /#\[cfg\(test\)\]|#\[test\]|@Test|describe\(|it\(|def test_/ (*.rs)`
  - `Grep /^use crate::|^import |^from .* import/ (*.rs)`
- Response: 85853 bytes → 26289 tokens (naive 21463)
- Latency: mean 6.882 ms, p50 6.835 ms, p95 6.992 ms, σ 0.092 ms (n=3)

**Savings:** - tokens, - bytes, 1.54× speedup (token comparison skipped: non-MCP sim is incomplete)

**LLM-judge (claude-opus-4-6):** MCP 8.0/10 (correctness 10, completeness 0, usability 10, groundedness 10, conciseness 10) vs non-MCP 3.2/10 (correctness 3, completeness 0, usability 0, groundedness 10, conciseness 3) - _MCP ranks 10 untested files by risk (PR x blast x churn); non-MCP lists files + test annotations unconnected_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 10/10 verified (1.000); 10 files, 0 lines, 0 symbols; unverified: []
- non-MCP: 1847/1910 verified (0.967); 106 files, 1788 lines, 16 symbols; unverified: [Foo, field, Module, src/a.rs, src/b.rs]

**Pros (MCP-only)**

- Pre-computed test-to-source mapping via import graph
- Risk-ranked by PageRank so high-impact gaps surface first
- 'suggest' mode recommends tests to run for a git diff range
- 'map' mode shows bidirectional test-source relationships

**Cons (what MCP loses vs Grep/Read)**

- Mapping relies on import edges; integration tests with no direct imports are missed
- Test detection is filename/pattern-based, not build-graph-aware

**Verdict:** MCP wins: non-MCP path requires globbing test files, grepping their imports, diffing against source files, and manually ranking by importance.

---

### `qartez_wiki` - qartez_wiki_auto_cluster

Auto-generate architecture wiki from Leiden clustering.

**MCP side**

- Args: `{}`
- Response: 9220 bytes → 3091 tokens (naive 2305)
- Latency: mean 0.462 ms, p50 0.458 ms, p95 0.505 ms, σ 0.037 ms (n=3)

**Non-MCP side** ✱ **incomplete** - the step sequence below does not produce a comparable answer; byte/token counts are noise, not a measure of efficiency

- Steps:
  - `Glob **/*.rs`
  - `Grep /^use crate::|^import |^from .* import|^mod / (*.rs)`
- Response: 14587 bytes → 4063 tokens (naive 3646)
- Latency: mean 3.409 ms, p50 3.432 ms, p95 3.477 ms, σ 0.070 ms (n=3)

**Savings:** - tokens, - bytes, 7.37× speedup (token comparison skipped: non-MCP sim is incomplete)

**LLM-judge (claude-opus-4-6):** MCP 9.0/10 (correctness 10, completeness 10, usability 10, groundedness 10, conciseness 5) vs non-MCP 5.0/10 (correctness 0, completeness 10, usability 0, groundedness 10, conciseness 5) - _MCP generates full architecture wiki with 10 clusters/edges/symbols; non-MCP dumps file list + grep output_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 148/149 verified (0.993); 98 files, 0 lines, 51 symbols; unverified: [Makefile]
- non-MCP: 302/308 verified (0.981); 101 files, 207 lines, 0 symbols; unverified: [src/server/mod.rs:5972, src/server/mod.rs:6076, src/server/mod.rs:6138, src/server/mod.rs:6203, src/server/mod.rs:6206]

**Pros (MCP-only)**

- Leiden community detection groups files by structural affinity
- Configurable resolution and min cluster size
- Inline or write-to-file output
- Recompute option for stale clusters

**Cons (what MCP loses vs Grep/Read)**

- Clustering quality depends on import-graph density
- No semantic naming of clusters (uses file-path heuristics)

**Verdict:** MCP wins: no non-MCP toolchain can perform graph-based community detection; manual module grouping requires reading every import.

---

### `qartez_boundaries` - qartez_boundaries_suggest

Suggest architecture boundary rules from current clustering.

**MCP side**

- Args: `{"suggest":true}`
- Response: 535 bytes → 138 tokens (naive 133)
- Latency: mean 0.069 ms, p50 0.069 ms, p95 0.070 ms, σ 0.000 ms (n=3)

**Non-MCP side** ✱ **incomplete** - the step sequence below does not produce a comparable answer; byte/token counts are noise, not a measure of efficiency

- Steps:
  - `Glob **/*.rs`
  - `Grep /^use crate::|^import |^from .* import/ (*.rs)`
- Response: 11360 bytes → 3125 tokens (naive 2840)
- Latency: mean 3.367 ms, p50 3.316 ms, p95 3.507 ms, σ 0.117 ms (n=3)

**Savings:** - tokens, - bytes, 48.56× speedup (token comparison skipped: non-MCP sim is incomplete)

**LLM-judge (claude-opus-4-6):** MCP 8.8/10 (correctness 7, completeness 10, usability 7, groundedness 10, conciseness 10) vs non-MCP 4.6/10 (correctness 0, completeness 10, usability 0, groundedness 10, conciseness 3) - _MCP generates TOML boundary rules from clustering (some duplication); non-MCP dumps file list + imports_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 3/3 verified (1.000); 0 files, 0 lines, 3 symbols; unverified: []
- non-MCP: 230/230 verified (1.000); 101 files, 129 lines, 0 symbols; unverified: []

**Pros (MCP-only)**

- Auto-generates boundary TOML from Leiden clusters
- Checker mode validates rules against live import graph
- Write-to-file for CI integration

**Cons (what MCP loses vs Grep/Read)**

- Suggested rules may need manual curation
- Only checks file-level imports, not symbol-level

**Verdict:** MCP wins: non-MCP agent must manually trace every cross-module import to identify boundary violations.

---

### `qartez_hierarchy` - qartez_hierarchy_implementors

List all implementors of a trait/interface.

**MCP side**

- Args: `{"symbol":"LanguageSupport"}`
- Response: 3071 bytes → 735 tokens (naive 767)
- Latency: mean 0.049 ms, p50 0.048 ms, p95 0.053 ms, σ 0.004 ms (n=3)

**Non-MCP side**

- Steps:
  - `Grep /impl LanguageSupport/ (*.rs)`
  - `Grep /(extends|implements)\s+LanguageSupport/ (*.rs)`
  - `Read src/server/mod.rs lines 1-120`
- Response: 7886 bytes → 2056 tokens (naive 1971)
- Latency: mean 6.247 ms, p50 6.318 ms, p95 6.340 ms, σ 0.119 ms (n=3)

**Savings:** +64.3% tokens, +61.1% bytes, 127.48× speedup

**LLM-judge (claude-opus-4-6):** MCP 9.4/10 (correctness 10, completeness 10, usability 10, groundedness 10, conciseness 7) vs non-MCP 6.8/10 (correctness 7, completeness 10, usability 5, groundedness 7, conciseness 5) - _Both find 37 implementors; MCP sorted with struct names/locations; non-MCP grep correct but mixed with code dump_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 72/72 verified (1.000); 37 files, 0 lines, 35 symbols; unverified: []
- non-MCP: 43/47 verified (0.915); 2 files, 37 lines, 8 symbols; unverified: [mod.rs, path_prefix, symbols_body_fts, repo-a/src/main.rs]

**Pros (MCP-only)**

- Tree-sitter-based impl/extends resolution
- Transitive hierarchy traversal available
- Mermaid output for visualization
- Bidirectional: sub (implementors) and super (parents)

**Cons (what MCP loses vs Grep/Read)**

- Misses blanket impls and macro-generated impls
- Transitive depth can balloon on widely-implemented traits

**Verdict:** MCP wins on precision: grepping `impl Trait` catches string matches and misses `implements` variants across languages.

---

### `qartez_trend` - qartez_trend_complexity_over_time

Complexity trend over last 10 commits for the top file.

**MCP side**

- Args: `{"file_path":"src/server/mod.rs","limit":10}`
- Response: 54728 bytes → 17796 tokens (naive 13682)
- Latency: mean 120.214 ms, p50 120.809 ms, p95 121.460 ms, σ 1.385 ms (n=3)

**Non-MCP side** ✱ **incomplete** - the step sequence below does not produce a comparable answer; byte/token counts are noise, not a measure of efficiency

- Steps:
  - `git log -n10 -- src/server/mod.rs`
  - `≈6000B representative padding`
- Response: 6381 bytes → 148 tokens (naive 1595)
- Latency: mean 12.060 ms, p50 12.084 ms, p95 12.190 ms, σ 0.126 ms (n=3)

**Savings:** - tokens, - bytes, 0.10× speedup (token comparison skipped: non-MCP sim is incomplete)

**LLM-judge (claude-opus-4-6):** MCP 8.0/10 (correctness 10, completeness 10, usability 7, groundedness 10, conciseness 3) vs non-MCP 4.0/10 (correctness 0, completeness 10, usability 0, groundedness 3, conciseness 7) - _MCP shows CC trend per symbol across 10 commits; non-MCP just repeats file path 10 times with no data_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 1/1 verified (1.000); 1 files, 0 lines, 0 symbols; unverified: []
- non-MCP: 0/1 verified (0.000); 1 files, 0 lines, 0 symbols; unverified: [qartez-public/src/server/mod.rs]

**Pros (MCP-only)**

- Per-symbol complexity tracked across git history
- Delta percentages show direction of change
- Limit parameter caps history depth

**Cons (what MCP loses vs Grep/Read)**

- Requires re-parsing each historic file version
- Only counts commits that changed the target file

**Verdict:** MCP wins: non-MCP path requires checking out each historic version, re-parsing, and manually computing deltas.

---

### `qartez_knowledge` - qartez_knowledge_bus_factor

Author knowledge distribution and bus factor analysis.

**MCP side**

- Args: `{"limit":10}`
- Response: 346 bytes → 82 tokens (naive 86)
- Latency: mean 11.780 ms, p50 11.654 ms, p95 12.073 ms, σ 0.243 ms (n=3)

**Non-MCP side** ✱ **incomplete** - the step sequence below does not produce a comparable answer; byte/token counts are noise, not a measure of efficiency

- Steps:
  - `git log -n500 -- .`
  - `≈2000B representative padding`
- Response: 26364 bytes → 6827 tokens (naive 6591)
- Latency: mean 43.431 ms, p50 43.481 ms, p95 43.508 ms, σ 0.093 ms (n=3)

**Savings:** - tokens, - bytes, 3.69× speedup (token comparison skipped: non-MCP sim is incomplete)

**LLM-judge (claude-opus-4-6):** MCP 8.2/10 (correctness 7, completeness 10, usability 7, groundedness 7, conciseness 10) vs non-MCP 3.2/10 (correctness 0, completeness 10, usability 0, groundedness 3, conciseness 3) - _MCP analyzes bus factor for 109 files showing single-author risk; non-MCP dumps raw commit file lists_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: -
- non-MCP: 0/132 verified (0.000); 132 files, 0 lines, 0 symbols; unverified: [TEMPLATE/config.yml, qartez-public/CLA.md, qartez-public/README.md, TEMPLATE/bug_report.yml, qartez-public/src/lib.rs]

**Pros (MCP-only)**

- Per-file author breakdown with ownership percentages
- Module-level bus factor summary mode
- Author filter for scoping to specific contributors

**Cons (what MCP loses vs Grep/Read)**

- Based on git blame line counts, not semantic ownership
- Does not account for code review or pair programming

**Verdict:** MCP wins on tokens and structure: non-MCP git-log mining produces raw commit data that requires in-process aggregation.

---

### `qartez_diff_impact` - qartez_diff_impact_head3

Blast radius of the last 3 commits (HEAD~3..HEAD).

**MCP side**

- Args: `{"base":"HEAD~3"}`
- Response: 1710 bytes → 464 tokens (naive 427)
- Latency: mean 1.023 ms, p50 1.020 ms, p95 1.042 ms, σ 0.017 ms (n=3)

**Non-MCP side** ✱ **incomplete** - the step sequence below does not produce a comparable answer; byte/token counts are noise, not a measure of efficiency

- Steps:
  - `Grep /^use crate::|^import |^from .* import/ (*.rs)`
  - `≈3000B representative padding`
- Response: 11906 bytes → 2476 tokens (naive 2976)
- Latency: mean 4.004 ms, p50 3.861 ms, p95 4.326 ms, σ 0.267 ms (n=3)

**Savings:** - tokens, - bytes, 3.92× speedup (token comparison skipped: non-MCP sim is incomplete)

**LLM-judge (claude-opus-4-6):** MCP 7.4/10 (correctness 7, completeness 10, usability 7, groundedness 3, conciseness 10) vs non-MCP 4.6/10 (correctness 0, completeness 10, usability 0, groundedness 10, conciseness 3) - _MCP lists 22 changed files with index status and 0 blast radius; non-MCP greps unrelated imports_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 0/18 verified (0.000); 18 files, 0 lines, 0 symbols; unverified: [qartez-website/app.js, qartez-website/nginx.c, qartez-website/index.html, qartez-website/lang/de.js, qartez-website/lang/en.js]
- non-MCP: 129/129 verified (1.000); 0 files, 129 lines, 0 symbols; unverified: []

**Pros (MCP-only)**

- Combines git diff with transitive import BFS in one call
- Per-file risk scoring with health, boundary, and test coverage
- Directly answers 'what could this change break'

**Cons (what MCP loses vs Grep/Read)**

- Risk scoring requires boundaries and test-gap data to be meaningful
- Large diffs produce proportionally large output

**Verdict:** MCP wins: non-MCP path is git diff + manual import chasing for every changed file, with no risk scoring.

---

### `qartez_security` - qartez_security_medium_severity

Scan for security vulnerabilities at medium+ severity. Consolidates 5+ grep passes into one scored report.

**MCP side**

- Args: `{"severity":"medium"}`
- Response: 13044 bytes → 3672 tokens (naive 3261)
- Latency: mean 15.774 ms, p50 15.857 ms, p95 15.859 ms, σ 0.119 ms (n=3)

**Non-MCP side** ✱ **incomplete** - the step sequence below does not produce a comparable answer; byte/token counts are noise, not a measure of efficiency

- Steps:
  - `Grep /(?i)(password|secret|api_key|token)\s*=/ (*.rs)`
  - `Grep /-----BEGIN .* PRIVATE KEY-----/ (*.rs)`
  - `Grep /(?i)(format!|\.format).*(?:SELECT|INSERT|DELETE|DROP)/ (*.rs)`
  - `Grep /\bunsafe\b/ (*.rs)`
  - `Grep /\.unwrap\(\)/ (*.rs)`
- Response: 169071 bytes → 48247 tokens (naive 42267)
- Latency: mean 18.096 ms, p50 18.078 ms, p95 18.242 ms, σ 0.127 ms (n=3)

**Savings:** - tokens, - bytes, 1.15× speedup (token comparison skipped: non-MCP sim is incomplete)

**LLM-judge (claude-opus-4-6):** MCP 8.0/10 (correctness 10, completeness 0, usability 10, groundedness 10, conciseness 10) vs non-MCP 3.2/10 (correctness 3, completeness 0, usability 3, groundedness 7, conciseness 3) - _MCP gives 51 scored findings by severity x PageRank; non-MCP greps security patterns unsorted and unscored_
- Self-consistency runs: 0; flags: batch

**Grounding (claim-level fact check):**
- MCP: 62/62 verified (1.000); 15 files, 45 lines, 2 symbols; unverified: []
- non-MCP: 2070/2204 verified (0.939); 41 files, 2130 lines, 33 symbols; unverified: [hi, foo, bar, dep, Api]

**Pros (MCP-only)**

- 13 built-in rules in a single call vs 5+ separate greps
- Risk scoring by PageRank prioritizes high-impact files
- Custom rules via .qartez/security.toml
- Category and severity filters reduce noise

**Cons (what MCP loses vs Grep/Read)**

- Regex patterns may produce false positives
- Cannot detect logic-level vulnerabilities (auth bypass, IDOR)

**Verdict:** MCP wins on consolidation and prioritization: non-MCP path requires one grep per pattern with no scoring or cross-referencing.

---

