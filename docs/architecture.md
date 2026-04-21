# Architecture

Qartez is a code-intelligence MCP server. It indexes a codebase into a SQLite
database, computes graph metrics over the result, and exposes 30 query and
refactoring tools via the Model Context Protocol. The design trades indexing
time (seconds) for query-time speed (milliseconds) and token efficiency.

## High-level data flow

```
source files
    │
    ▼
┌──────────┐     ┌────────────┐     ┌───────────┐
│  Walker   │────▶│  Indexer    │────▶│  SQLite   │
│ (ignore)  │     │ (tree-sit) │     │  index.db │
└──────────┘     └────────────┘     └─────┬─────┘
                                          │
                              ┌───────────┼───────────┐
                              ▼           ▼           ▼
                         PageRank    Co-change    Leiden
                         (files +   (git log)   clusters
                          symbols)
                              │           │           │
                              └─���─────────┼───────────┘
                                          ▼
                                   ┌────────────┐
                                   │ MCP Server │◀── JSON-RPC (stdio)
                                   │  35 tools  │
                                   └────────────┘
```

## Startup sequence

When an MCP client (Claude Code, Cursor, etc.) launches qartez, `main.rs`
runs this sequence:

1. **Config resolution** — detect project root(s) from cwd, expand workspace
   members (Cargo, npm, Go), determine DB path
2. **Schema creation** — open or create `.qartez/index.db`, run idempotent
   `CREATE TABLE IF NOT EXISTS` + `ALTER TABLE` migrations
3. **Background indexing** — spawn a blocking task that runs:
   - `full_index_multi` — walk files, parse with tree-sitter, insert symbols/edges/refs
   - `compute_pagerank` — file-level importance ranking
   - `compute_symbol_pagerank` — symbol-level importance ranking
   - `analyze_cochanges` — git log mining for co-change pairs
4. **MCP serve** — start listening on stdin/stdout immediately, even while
   indexing runs in the background. Tool calls before indexing completes see
   whatever the DB carried from a previous run.
5. **File watcher** — per-root `notify`-based watcher triggers incremental
   re-indexing on file changes

The CLI subcommand path (`qartez map`, `qartez grep`, etc.) skips step 4 and
runs indexing synchronously before dispatching the tool.

## Indexing pipeline

### Walker (`src/index/walker.rs`)

Uses the `ignore` crate's `WalkBuilder` to traverse the project root. This
automatically respects:

- `.gitignore` (local, global, and `.git/info/exclude`)
- `.qartezignore` — qartez-specific exclusion file, same glob syntax
- Hidden files (excluded by default)

Files are filtered to supported extensions (`.rs`, `.py`, `.ts`, `.go`, etc.),
known filenames (`Dockerfile`, `Makefile`), and known prefixes
(`Dockerfile.prod`). Files larger than 1 MB are skipped by default
(override with `QARTEZ_MAX_FILE_BYTES`).

### Language support (`src/index/languages/`)

Each supported language implements the `LanguageSupport` trait:

```rust
trait LanguageSupport {
    fn extensions(&self) -> &[&str];
    fn language_name(&self) -> &str;
    fn tree_sitter_language(&self, ext: &str) -> Language;
    fn extract(&self, source: &[u8], tree: &tree_sitter::Tree) -> ParseResult;
}
```

The `extract` method produces a `ParseResult` containing:

- **Symbols** — functions, methods, classes, structs, enums, constants, etc.
  with name, kind, line range, signature, export status, cyclomatic complexity,
  AST shape hash (for clone detection), and parent/owner relationships
- **Imports** — what this file depends on (resolved to file paths where possible)
- **References** — symbol-to-symbol call/use/type edges within the file
- **Type relations** — implements/extends relationships for type hierarchy

**37 languages supported:** Bash, C, C++, Caddyfile, C#, CSS, Dart,
Dockerfile, Elixir, Go, Haskell, HCL (Terraform), Helm, Java, Jenkinsfile,
Jsonnet, Kotlin, Lua, Makefile, Nginx, Nix, OCaml, PHP, Protobuf, Python, R,
Ruby, Rust, Scala, SQL, Starlark, Swift, systemd, TOML, TypeScript/JavaScript,
YAML, Zig.

### Tree-sitter parsing (`src/index/symbols.rs`, `src/index/parser.rs`)

All parsing is done via tree-sitter, a concrete syntax tree parser that works
on source bytes without requiring compilation or type-checking. This gives
qartez several properties:

- **Fast** — parses thousands of files per second
- **Language-agnostic** — same pipeline for all 37 languages
- **Partial-file resilient** — tree-sitter produces a tree even for files with
  syntax errors
- **No build system dependency** — no need for `cargo build`, `npm install`,
  or a working compiler

The `ParserPool` manages a pool of tree-sitter parsers to avoid
allocation overhead across files. Each language module provides tree-sitter
queries specific to its syntax.

### Four-pass indexing (`src/index/mod.rs`)

`full_index_root` runs four passes per root:

1. **Parse + symbol insert** — for each walked file: parse with tree-sitter,
   extract symbols/imports/references, insert symbols into DB (returns row IDs
   for pass 4), insert type hierarchy relations
2. **Stale cleanup** — delete DB rows for files that no longer exist on disk
3. **Import resolution** — resolve import specifiers (e.g., `from ./utils import foo`)
   to target file IDs, write file-level `edges` rows
4. **Reference resolution** — translate parse-local symbol indices into DB
   symbol IDs, write `symbol_refs` rows (call/use/type edges)

### Multi-root / workspace support

Qartez detects and expands workspaces automatically:

- **Cargo** — parses `Cargo.toml` `[workspace] members`, expands globs
- **npm/yarn/pnpm** — parses `package.json` `"workspaces"` (array or object form)
- **Go** — parses `go.work` `use` directives
- **Meta-directory** — if cwd has no project markers but child directories do,
  treats each child as a separate root

In multi-root mode, DB paths are prefixed with the root name
(`repo-a/src/main.rs`) to prevent collisions. Import resolution works across
roots via a shared `known_paths` set.

The DB is placed in `.qartez/index.db` inside the project root for single-root
projects, or in cwd for multi-root.

## Storage layer (`src/storage/`)

### Schema

All data lives in a single SQLite database. Core tables:

| Table | Purpose |
|-------|---------|
| `files` | One row per indexed file: path, mtime, size, language, line count, PageRank, change count |
| `symbols` | Every extracted symbol: name, kind, line range, signature, export status, shape hash, parent, PageRank, complexity, owner type |
| `edges` | File-level import relationships: from_file → to_file with kind and specifier |
| `symbol_refs` | Symbol-level references: from_symbol → to_symbol with kind (call/use/type) |
| `co_changes` | Git-mined co-change counts: file_a ↔ file_b with frequency |
| `symbols_fts` | FTS5 virtual table over symbol name + kind + file path |
| `symbols_body_fts` | FTS5 virtual table over symbol source bodies |
| `unused_exports` | Materialized view: exported symbols with no importers |
| `file_clusters` | Leiden community detection results |
| `type_hierarchy` | Implements/extends relationships between types |
| `meta` | Key-value metadata (schema version, index timestamp) |
| `symbol_embeddings` | Dense vectors for semantic search (feature-gated) |

### Migrations

Schema migrations use `ALTER TABLE ADD COLUMN` wrapped in a
try-and-ignore-duplicate helper. This makes migrations idempotent — running
on an already-migrated DB is a no-op. New tables use `CREATE TABLE IF NOT
EXISTS`. The approach avoids a version counter and the need for rollback logic.

### Incremental re-indexing

When the file watcher detects changes, `incremental_index` processes only
changed and deleted files. For changed files, `clear_file_content` removes
all symbols, edges, FTS entries, and type hierarchy rows for that file ID
(preserving incoming edges from other files), then re-indexes the file.
After the incremental pass, PageRank and symbol PageRank are recomputed
with warm-start (converges in 1-3 iterations vs 15-20 from cold).

## Graph algorithms (`src/graph/`)

### PageRank (`src/graph/pagerank.rs`)

Computed at two levels:

- **File-level** — over the `edges` table (import graph). Determines which
  files are most central to the codebase. Used by `qartez_map` to rank the
  project skeleton, by `qartez_impact` for blast-radius scoring, and by
  `qartez_hotspots` as the coupling dimension.
- **Symbol-level** — over the `symbol_refs` table (call/use/type graph).
  Determines which functions/types are most referenced. Used by `qartez_map
  by=symbols` and by the outline tool to order symbols within a file.

Implementation: standard power-iteration with damping factor 0.85, leak
redistribution for dangling nodes (no outgoing edges), epsilon-based early
termination. Warm-start from stored ranks means incremental updates converge
in 1-3 iterations.

### Leiden clustering (`src/graph/leiden.rs`)

Community detection over the file import graph. Groups files into modules
based on edge density, producing clusters used by:

- `qartez_wiki` — auto-generated architecture documentation organized by cluster
- `qartez_boundaries` — architecture rule checking against cluster assignments

Configurable via `resolution` (higher = more smaller clusters) and
`min_cluster_size` (clusters below threshold fold into a `misc` bucket).

### Co-change analysis (`src/git/cochange.rs`)

Walks git history (up to `git_depth` commits, default 300) and records which
files change together. Commits with fewer than `min_files` (2) or more than
`max_files` (20) are excluded to filter out trivial and bulk-change commits.
Results stored as symmetric pairs in `co_changes` table.

Also populates `files.change_count` — the number of commits touching each
file within the analysis window. This feeds the churn dimension of hotspot
analysis.

## Caching (`src/server/cache.rs`)

The server maintains an LRU-style parse cache (`ParseCache`) to avoid
re-reading and re-parsing files across tool calls within a session:

- **Source text** — raw file content, `Arc<String>` shared across callers
- **Tree-sitter tree** — parsed AST, avoids re-parsing for outline/calls/refs
- **Call names** — extracted `(function_name, line_number)` pairs for call hierarchy
- **Identifier map** — grouped identifier occurrences for reference resolution

Cache entries are keyed by file path and validated by mtime — if the file
changed since caching, the entry is evicted and re-read from disk. The entire
cache is wrapped in `Arc<Mutex<ParseCache>>` on the server.

## MCP server (`src/server/mod.rs`)

### Tool dispatch

Each tool is implemented as a method on `QartezServer`:

```rust
fn qartez_find(&self, Parameters(params): Parameters<SoulFindParams>) -> Result<String, String>
```

All tool methods follow the same pattern:
1. Lock the DB mutex (short-lived, released between phases)
2. Query the index
3. For tools that need source code, read from disk via the parse cache (no DB lock)
4. Format output as text (concise or detailed based on `format` param)

The `call_tool_by_name` method dispatches by string name, enabling both MCP
JSON-RPC and CLI subcommand paths to share the same tool implementations.

### Progressive disclosure (`src/server/tiers.rs`)

When `QARTEZ_PROGRESSIVE=1` is set, the server starts with only 8 core tools
(plus `qartez_tools`)
visible. Additional tiers are unlocked on demand via `qartez_tools`, which
sends a `notifications/tools/list_changed` MCP notification to the client.
Without the env var, all tools are visible from the start.

### Prompts

Five workflow prompts orchestrate multiple tools in sequence:

- `/qartez_review <file>` — code review with blast radius and co-change
- `/qartez_architecture [top_n]` — architecture overview via PageRank
- `/qartez_debug <symbol>` — definition + body + call hierarchy + references
- `/qartez_onboard [area]` — five-file reading list for new contributors
- `/qartez_pre_merge <files>` — pre-merge safety check

### Resources

Three MCP resources provide read-only snapshots:

- `qartez://overview` — ranked codebase overview
- `qartez://hotspots` — top files by hotspot score
- `qartez://stats` — language, LOC, symbol counts

## Git integration (`src/git/`)

Beyond co-change analysis, qartez uses git for:

- **Complexity trend** (`git/trend.rs`) — walks git history for a file, parses
  each revision with tree-sitter, extracts per-symbol cyclomatic complexity.
  Shows whether functions are getting more or less complex over time.
- **Knowledge/bus factor** (`git/knowledge.rs`) — runs `git blame` per file,
  aggregates author line counts, computes bus factor (minimum authors owning
  >50% of lines). Available per-file or rolled up per-directory.
- **Diff analysis** (`git/diff.rs`) — extracts changed files from a git range
  for `qartez_diff_impact` and `qartez_test_gaps suggest`.

All git features are gated behind `git_depth > 0` and use `git2` (libgit2
bindings), not shell-out to the git CLI.

## Optional features

### Semantic search (`src/embeddings.rs`)

Gated behind the `semantic` Cargo feature. Uses the Jina Code v2 embedding
model (768-dim, 8192-token context) via ONNX Runtime for local inference.

Embeddings are computed per-symbol during indexing and stored in
`symbol_embeddings` as raw f32 BLOBs. At query time, `qartez_semantic`
encodes the natural-language query, computes cosine similarity against all
stored embeddings, and fuses the result with FTS5 lexical search via
Reciprocal Rank Fusion (RRF).

Model files are downloaded separately via `qartez-setup` and stored in
`~/.qartez/models/`.

### Security scanning (`src/graph/security.rs`)

Static vulnerability scanner with 13 built-in regex rules covering OWASP
categories: hardcoded secrets, SQL injection, command injection, weak crypto,
path traversal, unsafe blocks, eval, innerHTML, and more.

Findings are scored by `severity_weight * file_pagerank * (1 + is_exported)`
so vulnerabilities in central, public-facing code surface first.

Custom rules and rule disabling via `.qartez/security.toml`.

## File watcher (`src/watch.rs`)

Uses the `notify` crate to watch project roots for file changes. On change:

1. Debounce (waits for burst to settle)
2. Classify changed paths as changed or deleted
3. Run `incremental_index` on a separate DB connection
4. Recompute PageRank (warm-start)
5. Rebuild FTS and body indexes for affected files

This keeps the index fresh during development without requiring manual re-indexing.

## Environment variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `QARTEZ_PROGRESSIVE` | unset | Set to `1` to start with only core tools visible |
| `QARTEZ_MAX_FILE_BYTES` | 1000000 | Skip files larger than this during indexing |
| `QARTEZ_NO_AUTO_UPDATE` | unset | Disable background update checks |

## Key dependencies

| Crate | Role |
|-------|------|
| `tree-sitter` + language grammars | Parsing source code into concrete syntax trees |
| `rusqlite` | SQLite database access |
| `git2` | Git repository access (blame, log, diff) |
| `rmcp` | Model Context Protocol server implementation |
| `ignore` | Gitignore-aware file walking |
| `notify` | Filesystem change notifications |
| `ort` (semantic feature) | ONNX Runtime for embedding inference |
| `tokenizers` (semantic feature) | HuggingFace tokenizer for embedding input |
