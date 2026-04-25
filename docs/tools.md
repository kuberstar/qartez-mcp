# Tool reference

Qartez exposes 39 tools via MCP, organized into four tiers. By default all
tools are available. With `QARTEZ_PROGRESSIVE=1`, only the core tier and
`qartez_tools` are visible at startup; unlock others on demand.

## Tiers

| Tier | Tools | Purpose |
|------|-------|---------|
| **core** | 8 | Navigate, read, assess - the daily-driver set |
| **analysis** | 18 | Deep investigation, debugging, review, architecture |
| **refactor** | 7 | Codebase-wide rename, move, replace, insert, and safe-delete operations |
| **meta** | 5 | Build toolchain, documentation, workspace admin |
| **discovery** | 1 | `qartez_tools` - always visible, manages tier visibility |

---

## Core tools

### `qartez_map`

Project skeleton ranked by file importance (PageRank). Start here to
understand the codebase structure.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `top_n` | u32 | 20 | Number of files to show |
| `all_files` | bool | false | Show all files (ignores top_n) |
| `token_budget` | u32 | 4000 | Approximate output token limit |
| `boost_files` | string[] | — | Boost ranking for these file paths |
| `boost_terms` | string[] | — | Boost files containing these symbols |
| `format` | enum | detailed | `detailed` or `concise` |
| `by` | string | files | `files` (default) or `symbols` for symbol-level ranking |

### `qartez_find`

Jump to a symbol definition by name. Returns the file, line range, signature,
and kind.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | **required** | Symbol name to find |
| `kind` | string | — | Filter by kind (function, class, struct, etc.) |
| `format` | enum | detailed | `detailed` or `concise` |
| `regex` | bool | false | Treat name as a regex pattern |

### `qartez_grep`

Search indexed symbols by name, kind, or file path. Uses FTS5 for fast
full-text search. Can also search symbol bodies.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `query` | string | **required** | Search query |
| `limit` | u32 | 20 | Max results |
| `format` | enum | detailed | `detailed` or `concise` |
| `token_budget` | u32 | 4000 | Approximate output token limit |
| `regex` | bool | false | Use regex instead of FTS5 |
| `search_bodies` | bool | false | Search function bodies, not just names |

### `qartez_read`

Read symbol source code with line numbers. Faster than a raw file read —
jumps directly to the symbol without scanning.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `symbol_name` | string | — | Single symbol to read |
| `symbols` | string[] | — | Batch mode: multiple symbols in one call |
| `file_path` | string | — | Disambiguate or read raw file range |
| `max_bytes` | u32 | 25000 | Max response size |
| `context_lines` | u32 | 0 | Lines of context before the symbol |
| `start_line` | u32 | — | Read a raw line range (with file_path) |
| `end_line` | u32 | — | End of raw line range |
| `limit` | u32 | — | Line count (alternative to end_line) |

Pass `file_path` alone to read raw file content (imports, headers). Pass
`symbol_name` for targeted reads. Pass `symbols` for batch reads in one call.

### `qartez_outline`

File symbol table — a table of contents for a file, grouped by kind
(functions, classes, structs, etc.) with line numbers and signatures.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `file_path` | string | **required** | Relative file path |
| `format` | enum | detailed | `detailed` or `concise` |
| `token_budget` | u32 | 4000 | Approximate output token limit |
| `offset` | u32 | 0 | Skip first N symbols (pagination) |

### `qartez_impact`

Blast radius analysis. Shows direct importers, transitive dependents,
co-change partners, and test coverage. **Call this before modifying any
heavily-imported file.**

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `file_path` | string | **required** | File to analyze |
| `format` | enum | detailed | `detailed` or `concise` |
| `include_tests` | bool | false | Include test files in output |

A file is "load-bearing" when its PageRank is >= 0.05 or its transitive
blast radius is >= 10.

### `qartez_deps`

File dependency graph. Shows what a file imports and what imports it.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `file_path` | string | **required** | File to analyze |
| `format` | enum | detailed | `detailed` or `concise` |
| `token_budget` | u32 | 4000 | Approximate output token limit |

### `qartez_stats`

Codebase metrics at a glance: files, symbols, edges by language, most
connected files, and index coverage.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `file_path` | string | — | Per-file stats (LOC, symbol count, imports) |

---

## Analysis tools

### `qartez_refs`

All usages of a symbol across the codebase. Queries the `symbol_refs` table
for precise symbol-level references, not just file-level imports.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `symbol` | string | **required** | Symbol name |
| `transitive` | bool | false | Include transitive references |
| `format` | enum | detailed | `detailed` or `concise` |
| `token_budget` | u32 | 4000 | Approximate output token limit |

### `qartez_calls`

Call hierarchy — who calls a function and what it calls. Uses tree-sitter AST
analysis to distinguish actual calls from type annotations.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | **required** | Function/method name |
| `direction` | enum | both | `callers`, `callees`, or `both` |
| `depth` | u32 | 1 | Max traversal depth (2 = transitive) |
| `format` | enum | detailed | `detailed` or `concise` |

### `qartez_cochange`

Files that historically change together in git. Useful for finding hidden
dependencies and understanding which files need coordinated changes.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `file_path` | string | **required** | File to analyze |
| `limit` | u32 | 10 | Max co-change partners |
| `format` | enum | detailed | `detailed` or `concise` |
| `max_commit_size` | u32 | 20 | Ignore commits touching more files |

### `qartez_context`

Related files for a task. Surfaces files you might miss by combining import
edges, co-change history, and transitive dependencies. Pass a task description
for text-boosted relevance.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `files` | string[] | **required** | Seed files you're working on |
| `task` | string | — | Natural-language task description for boosting |
| `limit` | u32 | 10 | Max related files |
| `format` | enum | detailed | `detailed` or `concise` |
| `token_budget` | u32 | 4000 | Approximate output token limit |
| `explain` | bool | false | Show scoring breakdown per file |

### `qartez_unused`

Dead exports and unreferenced symbols. Checked against both file-level edges
and symbol-level refs. Trait implementations and macro-generated symbols are
excluded from false positives.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `limit` | u32 | 30 | Max results |
| `offset` | u32 | 0 | Pagination offset |

### `qartez_diff_impact`

Blast radius of a git diff. Shows which files are affected by changes in a
commit range, with per-file risk scoring.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `base` | string | **required** | Git range (e.g., `main`, `HEAD~3`) |
| `format` | enum | detailed | `detailed` or `concise` |
| `include_tests` | bool | false | Include test files |

### `qartez_hotspots`

Ranks files or symbols by a composite score: complexity x coupling x churn.
High-scoring files are the ones most likely to cause problems — they're
complex, highly coupled, and frequently changed.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `limit` | u32 | 20 | Max results |
| `level` | enum | file | `file` or `symbol` |
| `format` | enum | detailed | `detailed` or `concise` |

### `qartez_clones`

Duplicate code detection via AST shape hashing. Groups symbols with identical
AST structure (ignoring identifier names) so you can find copy-paste code.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `limit` | u32 | 10 | Max clone groups |
| `offset` | u32 | 0 | Pagination offset |
| `min_lines` | u32 | 5 | Minimum lines for a clone |
| `format` | enum | detailed | `detailed` or `concise` |

### `qartez_smells`

Code smell detection: god functions (high complexity), long parameter lists,
and feature envy (functions that reference more external symbols than internal).

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `file_path` | string | — | Scope to a single file |
| `limit` | u32 | 20 | Max results |
| `format` | enum | detailed | `detailed` or `concise` |

### `qartez_health`

Prioritized fix list that cross-references `qartez_hotspots` with
`qartez_smells`. Files that score badly in both signals are bucketed as
**Critical**; hotspot-only as **High**; smell-only as **Medium**. Every
surfaced file carries a concrete suggested refactor technique so the agent can
move from "here is a bad file" to "here is what to do about it" in one call.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `limit` | u32 | 15 | Max files to surface across all buckets |
| `max_health` | f64 | 5.0 | Only show files with health at or below this value (0-10) |
| `min_complexity` | u32 | 15 | God-function CC threshold |
| `min_lines` | u32 | 50 | God-function body-lines threshold |
| `min_params` | u32 | 5 | Long-params threshold (self/&self excluded) |
| `format` | enum | detailed | `detailed` or `concise` |

### `qartez_refactor_plan`

Ordered, safety-annotated refactor plan for a single file. Each step names a
concrete technique (Extract Method for god functions, Introduce Parameter
Object for long param lists), categorizes the expected CC impact (High /
Medium / Low) with a **range**, and folds in safety signals derived from
existing tools: caller count, `is_exported`, and whether tests cover the file.

CC impact is emitted as a conservative range, not a single number. Re-index
after each step to see the real delta.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `file_path` | string | **required** | Target file |
| `limit` | u32 | 8 | Max steps to surface |
| `min_complexity` | u32 | 15 | God-function CC threshold |
| `min_lines` | u32 | 50 | God-function body-lines threshold |
| `min_params` | u32 | 5 | Long-params threshold (self/&self excluded) |
| `format` | enum | detailed | `detailed` or `concise` |

### `qartez_boundaries`

Architecture boundary rule checking. Validates import rules between
architectural layers defined in `.qartez/boundaries.toml`.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `config_path` | string | `.qartez/boundaries.toml` | Path to boundary config |
| `suggest` | bool | false | Generate starter config from Leiden clusters |
| `write_to` | string | — | Write suggested config to file |
| `format` | enum | detailed | `detailed` or `concise` |

### `qartez_hierarchy`

Type/trait inheritance hierarchy. Shows subtypes, supertypes, or both for a
given type name. Optionally transitive.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `symbol` | string | **required** | Type/trait name |
| `direction` | string | both | `sub`, `super`, or `both` |
| `transitive` | bool | false | Walk the full hierarchy |
| `format` | enum | detailed | `detailed` or `concise` |

### `qartez_trend`

Symbol complexity trend over git history. Shows how a function's cyclomatic
complexity changed across commits — useful for spotting creeping complexity.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `file_path` | string | **required** | File to analyze |
| `symbol_name` | string | — | Filter to one symbol (exact match) |
| `limit` | u32 | 10 | Max commits to analyze (capped at 50) |
| `format` | enum | detailed | `detailed` or `concise` |

Requires `git_depth > 0`.

### `qartez_security`

Static vulnerability scanner. 13 built-in rules covering OWASP categories.
Findings scored by `severity * PageRank * (1 + is_exported)` so high-impact
code surfaces first. Customizable via `.qartez/security.toml`.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `file_path` | string | — | Scope to a file or directory |
| `category` | string | — | Filter by category |
| `min_severity` | string | — | Minimum severity (low/medium/high/critical) |
| `include_tests` | bool | false | Include test files |
| `format` | enum | detailed | `detailed` or `concise` |

### `qartez_semantic`

Semantic code search via embedding similarity. Accepts natural-language
queries and finds semantically similar symbols. Results fused with FTS5
lexical search via Reciprocal Rank Fusion.

Requires the `semantic` Cargo feature and a downloaded model
(`qartez-setup --download-model`).

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `query` | string | **required** | Natural-language search query |
| `limit` | u32 | 10 | Max results |
| `format` | enum | detailed | `detailed` or `concise` |

### `qartez_test_gaps`

Test-to-source mapping and coverage gap analysis. Three modes:

- **`map`** — which test files cover which source files (via import edges)
- **`gaps`** — untested source files ranked by risk (PageRank x health x blast)
- **`suggest`** — given a git diff range, which test files to run

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `mode` | string | gaps | `map`, `gaps`, or `suggest` |
| `file_path` | string | — | Scope to one file (map mode) |
| `base` | string | — | Git range for suggest mode |
| `limit` | u32 | 30 | Max results |
| `format` | enum | detailed | `detailed` or `concise` |
| `min_pagerank` | f64 | 0.0 | Filter threshold for gaps mode |
| `include_symbols` | bool | false | Show exported symbols in map mode |

### `qartez_knowledge`

Git authorship analysis and bus factor per file or module. Uses `git blame`
to attribute lines to authors and compute knowledge concentration.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `file_path` | string | — | Scope to file or directory prefix |
| `level` | enum | file | `file` or `module` (per-directory rollup) |
| `author` | string | — | Filter by author (case-insensitive substring) |
| `limit` | u32 | 20 | Max results |
| `format` | enum | detailed | `detailed` or `concise` |

Bus factor = minimum number of authors whose combined line count exceeds 50%
of total. Bus factor 1 means a single person owns most of the code.

Requires `git_depth > 0`.

---

## Refactor tools

### `qartez_rename`

Rename a symbol across all files. Operates in preview mode by default —
shows what would change without writing. Pass `apply=true` to execute.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `old_name` | string | **required** | Current symbol name |
| `new_name` | string | **required** | New symbol name |
| `apply` | bool | false | Actually write changes |

### `qartez_move`

Move a symbol from one file to another. Updates all import references.
Preview mode by default.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `symbol` | string | **required** | Symbol to move |
| `to_file` | string | **required** | Destination file path |
| `apply` | bool | false | Actually write changes |
| `kind` | string | — | Disambiguate by symbol kind |

### `qartez_rename_file`

Rename a file and update all import references across the codebase.
Preview mode by default.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `from` | string | **required** | Current file path |
| `to` | string | **required** | New file path |
| `apply` | bool | false | Actually write changes |

### `qartez_replace_symbol`

Replace a symbol's whole line range (`line_start..line_end`) with a new
definition. Caller supplies the full replacement including the signature;
the tool performs an atomic line-range rewrite. Preview mode by default.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `symbol` | string | **required** | Symbol to replace. Aliases: `name`, `symbol_name`. |
| `new_code` | string | **required** | Full replacement source (must include the signature). |
| `kind` | string | — | Disambiguate by symbol kind |
| `file_path` | string | — | Disambiguate by file when the name exists in multiple files |
| `apply` | bool | false | Actually write changes |

### `qartez_insert_before_symbol`

Splice new code immediately before an anchor symbol's first line. Lets the
caller add helpers, tests, or related items next to existing code without
needing the exact surrounding context. Preview mode by default.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `symbol` | string | **required** | Anchor symbol. Aliases: `name`, `symbol_name`. |
| `new_code` | string | **required** | Source text to insert |
| `kind` | string | — | Disambiguate by symbol kind |
| `file_path` | string | — | Disambiguate by file |
| `apply` | bool | false | Actually write changes |

### `qartez_insert_after_symbol`

Splice new code immediately after an anchor symbol's last line. Same
anchor-based addressing as `qartez_insert_before_symbol`. Preview mode
by default.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `symbol` | string | **required** | Anchor symbol. Aliases: `name`, `symbol_name`. |
| `new_code` | string | **required** | Source text to insert |
| `kind` | string | — | Disambiguate by symbol kind |
| `file_path` | string | — | Disambiguate by file |
| `apply` | bool | false | Actually write changes |

### `qartez_safe_delete`

Delete a symbol after reporting every file that still imports its defining
file. Refuses to apply when importers exist unless `force=true`; the caller
is then responsible for fixing the dangling uses. Preview always lists the
importers.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `symbol` | string | **required** | Symbol to delete. Aliases: `name`, `symbol_name`. |
| `kind` | string | — | Disambiguate by symbol kind |
| `file_path` | string | — | Disambiguate by file |
| `force` | bool | false | Delete even when importers exist (leaves dangling imports) |
| `apply` | bool | false | Actually write changes |

---

## Meta tools

### `qartez_project`

Detects the project's build toolchain and optionally runs commands. Supports
Cargo, npm/bun/yarn/pnpm, Go, Python, Dart/Flutter, Maven, Gradle, sbt,
Ruby, Make.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `action` | enum | info | `info`, `run`, `test`, `build`, `lint`, `typecheck` |
| `filter` | string | — | Filter (e.g., test name pattern) |
| `timeout` | u32 | 60 | Timeout in seconds |

### `qartez_wiki`

Auto-generate an architecture document using Leiden community detection.
Groups files into modules by import density, writes a markdown document
with module descriptions and file listings.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `write_to` | string | - | Write to file instead of returning inline |
| `resolution` | f64 | 1.0 | Leiden resolution (higher = more clusters) |
| `min_cluster_size` | u32 | 3 | Minimum files per cluster |
| `max_files_per_section` | u32 | 20 | Max files listed per cluster |
| `recompute` | bool | false | Force cluster recomputation |

### `qartez_workspace`

Add or remove workspace domains at runtime without restarting. Registers
an external directory under a custom alias, indexes it, and wires it into
the server's in-memory state; the mapping is persisted in
`.qartez/workspace.toml`. `remove` purges all associated files and symbols
from the index in one bulk pass.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `action` | enum | **required** | `add` or `remove` |
| `alias` | string | **required** | Domain prefix (ASCII letters, digits, `-`, `_`, `.`) |
| `path` | string | - | Directory to register (required for `add`; `~/` and relative paths allowed) |

### `qartez_add_root`

Register an additional project root at runtime. Indexes the directory,
refreshes pagerank/co-change, and hot-attaches a file watcher so saves
under the new root reindex live. Distinct from `qartez_workspace add`
in that the alias is optional (derived from the path basename and
disambiguated with a numeric suffix on collision) and persistence to
`.qartez/workspace.toml` can be toggled off for ephemeral roots.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `path` | string | **required** | Directory to register (`~/` and relative paths allowed) |
| `alias` | string | derived | Alias for the new root; defaults to a sanitized form of the basename |
| `persist` | bool | `true` | Persist into `.qartez/workspace.toml` so the root is reattached on next start |
| `watch` | bool | server default | Attach a `notify` watcher; respects the server's `--no-watch` flag when omitted |

### `qartez_list_roots`

List every project root currently tracked by the server. Each entry
shows the canonical path, alias, source (`cli` for CLI args, `config`
for `.qartez/workspace.toml` reattachments, `runtime` for additions
made via `qartez_add_root` or `qartez_workspace`), watcher attachment
state, the indexed file count under that root, and the last index
timestamp.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `format` | enum | `detailed` | `concise` returns a bullet list; `detailed` renders the full markdown table |

---

## Discovery tool

### `qartez_tools`

List available tiers and tools. Enable or disable tiers or individual tools
at runtime. Always visible regardless of progressive mode.

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `enable` | string[] | — | Tiers or tool names to enable (`"all"` for everything) |
| `disable` | string[] | — | Tiers or tool names to disable |

Call with no arguments to see what's available and what's currently enabled.

```
qartez_tools enable=["analysis"]        # unlock all analysis tools
qartez_tools enable=["all"]             # unlock everything
qartez_tools enable=["qartez_refs"]     # unlock a single tool
qartez_tools disable=["refactor"]       # hide refactor tier
```

Core tools and `qartez_tools` itself cannot be disabled.
