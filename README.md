<p align="center">
  <h1 align="center">Qartez MCP</h1>
  <p align="center">
    <strong>X-ray vision for your codebase - built for AI agents, not humans.</strong>
  </p>
  <p align="center">
    The first code-intelligence server designed from day one to be<br>
    <em>consumed by language models</em>, not read by people. Cuts AI token usage by ~91%.
  </p>
  <p align="center">
    <a href="#why-this-exists">Why</a> ·
    <a href="#quickstart">Quickstart</a> ·
    <a href="#the-21-tools">21 Tools</a> ·
    <a href="#benchmarks">Benchmarks</a> ·
    <a href="#comparison-with-alternatives">Comparison</a> ·
    <a href="#star-history">Star</a>
  </p>
  <p align="center">
    <img alt="License" src="https://img.shields.io/badge/license-dual-blue.svg">
    <img alt="Rust" src="https://img.shields.io/badge/rust-2024-orange.svg">
    <img alt="34 languages" src="https://img.shields.io/badge/languages-34-green.svg">
    <img alt="21 MCP tools" src="https://img.shields.io/badge/MCP_tools-21-purple.svg">
    <img alt="Agent-native" src="https://img.shields.io/badge/agent--native-yes-ff69b4.svg">
  </p>
</p>

---

## Why this exists

`grep`, `find`, `cat`, and `ls` were invented in the **1970s** for humans reading one file at a time in a terminal. Half a century later, your AI assistant is still using them - scanning files byte by byte, re-reading the same directories on every question, guessing at what matters, and burning your tokens on work the tools were never designed to do.

**Qartez is a different species of tooling.** It is not a wrapper around grep. It is a pre-computed knowledge graph of your repository - symbols, imports, call edges, blast radii, PageRank, git co-change, cyclomatic complexity - served to any LLM through the [Model Context Protocol](https://modelcontextprotocol.io/). The agent stops *reading* your codebase and starts *querying* it.

> Think of it as the first purpose-built **sensory organ for coding agents**. Grep sees one line at a time. Qartez sees the entire shape of the codebase in one glance.

This is a new interface layer - the same way LSP was a new layer between editors and compilers. If you build AI tooling, you care about this. If you pay an AI bill, you *really* care about this.

---

## The problem

Every time your AI assistant touches code, three expensive things happen:

**1. It reads the same files over and over.** No memory of the repo. Every question starts from scratch. You pay for every token - again and again.

**2. It can't see what will break.** Your assistant edits `utils.ts` without knowing 14 other files import it. You find out in CI. Or in production.

**3. It wastes tokens finding things.** "Where is `handleRequest` defined?" turns into Grep across 200 files, Read on 5 candidates, and 1,600 tokens burned before it finds the answer. Qartez answers that in 50 tokens.

The fix isn't a smarter model. It's a smarter index.

---

## What Qartez does

Qartez builds a **knowledge graph** of your codebase - once - and serves it to any AI assistant through MCP. Instead of scanning files from scratch on every question, your assistant queries a pre-computed index that knows:

- Which files matter most (PageRank on the import graph)
- What breaks if you change a file (blast radius analysis)
- Which files always change together (git co-change mining)
- Which functions are the most dangerous to touch (cyclomatic complexity × coupling × churn)
- Where every symbol is defined, who uses it, and who calls it
- Which blocks of code are duplicated (structural AST shape hashing)
- Which architecture boundaries the imports are violating

The result: your AI works faster, uses fewer tokens, refactors safely, and stops making blind changes to load-bearing files.

### Before and after

| Task | Without Qartez | With Qartez |
|---|---|---|
| "Where is `QartezServer` defined?" | Grep 200 files, Read candidates. **1,648 tokens.** | `qartez_find`. **50 tokens.** |
| "What breaks if I change `storage/read.rs`?" | Can't know. Hope for the best. | `qartez_impact`: direct + transitive importers + co-change. **308 tokens.** |
| "Outline `src/server/mod.rs` (175 symbols)" | Read full 200KB file. **54,414 tokens.** | `qartez_outline` with signatures. **3,582 tokens.** |
| "Find all dead exports" | Impossible without tooling. | `qartez_unused`: pre-materialized, instant. **408 tokens.** |
| "Which functions are the riskiest to refactor?" | Nothing to query. | `qartez_hotspots`: complexity × PageRank × churn. |

---

## Quickstart

Three commands. Under two minutes.

```bash
git clone https://github.com/kuberstar/qartez-mcp.git
cd qartez-mcp
make deploy
```

`make deploy` builds the release binaries, installs them to `~/.local/bin/`, runs the test suite, and launches the `qartez-setup` wizard in non-interactive mode - it auto-detects every MCP-capable IDE on your machine and configures them all in one pass, including the modification-guard hooks for Claude Code.

**Then, in any project you want to index:**

```bash
cd /path/to/your/project
qartez-mcp --reindex
```

Done. Your AI assistant now has structural understanding of the entire codebase, and the file watcher keeps the index fresh as you edit.

### Works with 7 editors and agents

A single Rust binary (`qartez-setup`) detects and configures every supported editor. No per-editor shell scripts, no copy-paste JSON.

```bash
make deploy                          # Configure every detected IDE (non-interactive)
make setup                           # Same, but interactive checkbox prompt
qartez-setup --ide cursor,zed       # Configure specific IDEs only
make uninstall                       # Remove qartez from every IDE and delete binaries
```

Supported out of the box: **Claude Code**, **Cursor**, **Windsurf**, **Zed**, **Continue.dev**, **OpenCode**, **Codex CLI**.

---

## Benchmarks

Not claims. Measured. Reproducible. Run `make bench` and verify yourself.

### Headline

**Aggregate token savings vs `Glob + Grep + Read + git log`: +91.5%**
(Σ MCP 8,604 / Σ non-MCP 101,740 tokens across 23 scenarios on the Qartez self-bench.)

**LLM-judge quality (claude-opus-4-6):** **MCP 7.9 / 10** vs non-MCP **5.3 / 10** across five axes (correctness, completeness, usability, groundedness, conciseness), n=23.

**Session cost context.** A typical Claude Code session starts at ~20,000 tokens of prompt overhead. A single `make bench` run saves ~93,000 tokens - **4.7 empty sessions** worth of budget bought back, just from routing questions through the right tool.

### Per-tool breakdown (Rust self-bench)

| Tool | MCP tokens | Without MCP | Savings | Speedup |
|---|---:|---:|---:|---:|
| `qartez_find` | 50 | 1,648 | **+97.0%** | **200×** |
| `qartez_cochange` | 92 | 4,361 | **+97.9%** | 3× |
| `qartez_context` | 118 | 2,848 | **+95.9%** | **315×** |
| `qartez_project` | 38 | 916 | **+95.9%** | 13× |
| `qartez_impact` | 308 | 5,418 | **+94.3%** | **122×** |
| `qartez_outline` | 3,582 | 54,414 | **+93.4%** | 3× |
| `qartez_deps` | 85 | 1,255 | **+93.2%** | **120×** |
| `qartez_unused` | 408 | 4,621 | **+91.2%** | 20× |
| `qartez_read` | 55 | 445 | **+87.6%** | 26× |
| `qartez_rename_file` | 22 | 168 | **+86.9%** | 184× |
| `qartez_grep` | 98 | 706 | **+86.1%** | 58× |
| `qartez_stats` | 107 | 650 | **+83.5%** | 1× |
| `qartez_move` | 117 | 676 | **+82.7%** | 58× |
| `qartez_refs` | 110 | 636 | **+82.7%** | 19× |
| `qartez_calls` | 516 | 2,626 | **+80.4%** | 3× |
| `qartez_map` | 92 | 405 | **+77.3%** | 4× |
| `qartez_rename` | 180 | 327 | **+45.0%** | 16× |

`qartez_hotspots`, `qartez_clones`, `qartez_boundaries`, and `qartez_wiki` are analytical tools with no meaningful grep/read equivalent - they solve problems the non-MCP stack cannot solve at all.

### Multi-language bench

`make bench-all` runs the same 23-scenario harness against five pinned OSS fixtures - **`colinhacks/zod`** (TypeScript), **`spf13/cobra`** (Go), **`encode/httpx`** (Python), **`FasterXML/jackson-core`** (Java), plus the Qartez self-bench (Rust) - then emits a cross-language summary to `reports/benchmark-<lang>.md` plus a combined matrix. Every tool, every language, every scenario - measured with the `cl100k_base` tokenizer against a faithful `Glob + Grep + Read + git log` simulation.

---

## The 21 tools

Think of these as the **standard library for AI code understanding**. Each one replaces a multi-step human workflow with a single, token-efficient call the agent can reason about.

### Navigate and understand

| Tool | What it does |
|---|---|
| `qartez_map` | **Start here.** Project skeleton ranked by importance - PageRank, exports, blast radii. Boost by files or terms to focus on what you're working on. |
| `qartez_find` | Jump to a symbol definition by exact name. File, line range, signature, visibility - no scanning. |
| `qartez_grep` | FTS5 search across indexed symbols. Prefix matching, regex fallback, optional body search. |
| `qartez_read` | Read one or more symbols' source code with line numbers. No file scanning - jumps directly to the symbol. |
| `qartez_outline` | Table of contents for any file: every symbol grouped by kind, with signatures. |
| `qartez_stats` | Codebase dashboard: files, symbols, edges by language, most-connected files. |

### Analyze dependencies and risk

| Tool | What it does |
|---|---|
| `qartez_impact` | **Call before editing any important file.** Shows direct importers, transitive dependents, and co-change partners - everything that could break. |
| `qartez_deps` | Dependency graph for a file: what it imports, what imports it. |
| `qartez_refs` | Trace every usage of a symbol across the codebase, with optional transitive chains. |
| `qartez_calls` | Call hierarchy: who calls this function, and what does it call. |
| `qartez_cochange` | Files that historically change together in git - logical coupling invisible to the import graph. |
| `qartez_context` | Smart context builder: given files you plan to modify, returns the optimal set of related files to read first. |
| `qartez_unused` | Dead-code finder: exported symbols with zero importers, pre-materialized at index time. |

### Find risk and duplication

| Tool | What it does |
|---|---|
| `qartez_hotspots` | **The refactor radar.** Ranks files and functions by hotspot score = **cyclomatic complexity × PageRank × (1 + churn)**. Points straight at the highest-risk code in the repo. |
| `qartez_clones` | Structural code-clone detection via AST shape hashing (normalized past identifiers, literals, and comments). Finds duplicate logic the human reviewer would never spot. |
| `qartez_boundaries` | Architecture-boundary enforcement. Declare "these modules may not import those" in `.qartez/boundaries.toml` and get every violating edge back. `suggest=true` seeds a starter config from the Leiden clustering. |

### Refactor safely

| Tool | What it does |
|---|---|
| `qartez_rename` | Rename a symbol across the entire codebase - definition, imports, all usages. Preview by default, `apply=true` to execute. |
| `qartez_move` | Move a symbol to another file and rewrite all import paths. One MCP call. |
| `qartez_rename_file` | Rename a file and update every import pointing to it. |

### Build, test, document

| Tool | What it does |
|---|---|
| `qartez_project` | Auto-detects your toolchain (Cargo, npm/bun/yarn, Go, Python, Make, Gradle) and runs test/build/lint/typecheck through a single tool. |
| `qartez_wiki` | Generates a markdown architecture wiki using Leiden community detection on the import graph. Partitions files into clusters, names each one, and emits `ARCHITECTURE.md` with inter-cluster edges. |

---

## Workflow prompts

Five ready-to-use recipes that chain the tools above in the right order. Invoke them as slash commands in Claude Code or any MCP client that supports prompts.

| Prompt | What it does |
|---|---|
| `/qartez_review <file>` | Code review: blast radius, outline, references, co-change - then a focused checklist. |
| `/qartez_architecture [top_n]` | One-minute architecture overview grounded in PageRank data. |
| `/qartez_debug <symbol>` | Definition + callers + callees + references in one shot. |
| `/qartez_onboard [area]` | Five-file reading list for new contributors, ranked by importance. |
| `/qartez_pre_merge <files>` | Pre-merge safety check with a ship/hold recommendation. |

---

## Modification guard

Qartez ships a **safety net** that prevents your AI from blindly editing load-bearing files.

The `qartez-guard` binary hooks into Claude Code's `PreToolUse` system and blocks `Edit`/`Write`/`MultiEdit` on any file that exceeds a PageRank or blast-radius threshold - until the AI calls `qartez_impact` first to acknowledge the risk.

**How it works:**

1. AI tries to edit `src/server/mod.rs`
2. Guard checks: PageRank 0.23 (> 0.05 threshold), blast radius 10 (≥ 10 threshold)
3. Edit is **blocked** with an explanation listing which thresholds fired
4. AI calls `qartez_impact file_path=src/server/mod.rs` - reviews the blast radius
5. Guard grants a 10-minute edit window for that file
6. AI retries the edit - **allowed**

Zero configuration. Tuneable via `QARTEZ_GUARD_PAGERANK_MIN`, `QARTEZ_GUARD_BLAST_MIN`, `QARTEZ_GUARD_ACK_TTL_SECS`, or disabled with `QARTEZ_GUARD_DISABLE=1`.

---

## How it works under the hood

Four layers, computed once, queried from SQLite on every tool call.

### 1. Tree-sitter parsing

Every source file is parsed by a language-specific tree-sitter grammar. No LSP server, no per-language SDK installs, no cold-start penalty. The parser extracts symbols (functions, methods, types, constants), their signatures, line ranges, export visibility, import relationships, and - for 16 imperative languages - cyclomatic complexity per function.

### 2. Structural shape hashing

Function bodies are canonicalized into an AST skeleton (identifiers, literals, and comments normalized away) and hashed. Two symbols with the same hash are structural clones. That's what `qartez_clones` queries.

### 3. Graph analysis

Import edges form a directed graph. Three algorithms run on top:

- **PageRank** - the same random-walk algorithm Google used for web pages. Applied to your import graph, it surfaces the files that form the architectural backbone of your project.
- **Blast radius** - reverse BFS that counts how many files are transitively affected by a change. `qartez_impact` uses this to warn before edits.
- **Leiden clustering** - community detection that partitions your codebase into logical modules for the auto-generated architecture wiki and the `qartez_boundaries` starter config.

### 4. Git history mining

Walks the last N commits (default 300) and counts file pairs that appear in the same commit. This reveals *logical coupling* that the import graph can't see - files that aren't linked by imports but are always edited together.

`qartez_impact`, `qartez_context`, and `qartez_hotspots` fuse these signals - PageRank + blast + co-change + complexity - into one ranked answer. No other MCP server combines all four.

### Storage

Everything lives in `.qartez/index.db` - a single SQLite file with FTS5 full-text indices. On startup, Qartez re-parses only files whose modification time changed. The file watcher is enabled automatically while the server is running - edits and new files are re-indexed in the background with zero downtime. Pass `--no-watch` to disable it.

---

## 34 supported languages & formats

One binary. No per-language setup. All parsed by tree-sitter (with regex fallbacks for formats lacking a compatible grammar). 16 imperative languages also get cyclomatic complexity per function, powering `qartez_hotspots`.

| Language | Extensions / Filenames |
|---|---|
| TypeScript / JavaScript | `.ts` `.tsx` `.js` `.jsx` `.mts` `.cts` `.mjs` `.cjs` |
| Rust | `.rs` |
| Go | `.go` |
| Python | `.py` `.pyi` |
| Java | `.java` |
| Kotlin | `.kt` `.kts` |
| Swift | `.swift` |
| C# | `.cs` |
| C | `.c` `.h` |
| C++ | `.cpp` `.cc` `.cxx` `.hpp` `.hh` `.hxx` |
| Ruby | `.rb` |
| PHP | `.php` |
| Bash | `.sh` `.bash` |
| CSS | `.css` `.scss` |
| Scala | `.scala` `.sc` - classes, traits, objects, case classes |
| Dart | `.dart` - classes, mixins, enums, underscore-based privacy |
| Lua | `.lua` - functions, methods (`M.f`/`M:f`), `require` imports |
| Elixir | `.ex` `.exs` - `defmodule`, `def`/`defp`, `defstruct`, `alias`/`use`/`import` |
| Zig | `.zig` - `pub fn`, structs, enums, unions, `@import` |
| Nix | `.nix` - attribute bindings, functions, `import` paths |
| Protobuf | `.proto` - `message`, `service`, `rpc`, `enum`, `import` |
| SQL | `.sql` - `CREATE TABLE`/`VIEW`/`FUNCTION`/`PROCEDURE`, `ALTER`, `BEGIN...END` blocks |
| HCL / Terraform | `.tf` - cross-file `var`/`local`/`module`/`data`/`resource` references |
| YAML | `.yaml` `.yml` - K8s, GitHub Actions, GitLab CI, docker-compose, Ansible |
| Dockerfile | `Dockerfile`, `Dockerfile.*`, `.dockerfile` - multi-stage `COPY --from` refs |
| Makefile | `Makefile`, `GNUmakefile`, `.mk` - targets, variables, `include` imports |
| TOML | `.toml` - tables, keys, arrays of tables |
| Nginx | `.conf`, `.nginx` - `server`, `location`, `upstream` blocks |
| Helm / Go templates | `.tpl` - `define`/`include`/`template` blocks |
| Jenkinsfile / Groovy | `Jenkinsfile`, `.groovy` - `pipeline`, `stage`, `node`, `def` |
| Starlark / Bazel | `BUILD`, `WORKSPACE`, `.bzl` - `load`, rules with `name=`, `def` |
| Jsonnet | `.jsonnet` `.libsonnet` - `local` functions/vars, fields, `import`/`importstr` |
| Caddyfile | `Caddyfile`, `.caddyfile` - site blocks, `handle`, `reverse_proxy`, snippets |
| Systemd units | `.service` `.timer` `.socket` `.mount` `.target` - sections, `ExecStart`, directives |

---

## Comparison with alternatives

The MCP codebase-intelligence space is crowded in 2026. This section covers direct OSS competitors, enterprise platforms, and adjacent ecosystems. All star counts were cross-checked against each project's GitHub repository in April 2026.

### Direct OSS MCP competitors

Nine projects share the "MCP server for codebase intelligence" niche, sorted by GitHub stars.

| Project | Stars | Impl. | Indexing approach | Languages | MCP tools |
|---|---:|---|---|---:|---:|
| **Qartez** (this repo) | new | **Rust** | tree-sitter + SQLite + PageRank + blast radius + co-change + complexity + clones + boundaries | **34** | **21** |
| [Serena](https://github.com/oraios/serena) | 22.8k | Python | LSP (per-language language servers) | **46+** | ~35 |
| [code-review-graph](https://github.com/tirth8205/code-review-graph) | 9.2k | Python | tree-sitter + SQLite + Leiden clustering | 22+ | 22 |
| [Claude-Context](https://github.com/zilliztech/claude-context) | 5.9k | TypeScript | Embeddings + Milvus/Zilliz vector DB | 14 | 4 |
| [CodeGraphContext](https://github.com/CodeGraphContext/CodeGraphContext) | 2.9k | Python | tree-sitter + KuzuDB / FalkorDB / Neo4j | 14 | 21 |
| [Codebase-Memory MCP](https://github.com/DeusData/codebase-memory-mcp) | 1.5k | C | tree-sitter + SQLite + hybrid type resolution | **66** | 14 |
| [Repowise](https://github.com/repowise-dev/repowise) | 1.1k | Python | Dependency graph + git history + LLM-generated docs | - | 7 |
| [Code Index MCP](https://github.com/johnhuang316/code-index-mcp) | 903 | Python | tree-sitter (10 langs) + ripgrep fallback for 50+ | 10 + 50 | 11 |
| [Codanna](https://github.com/bartolli/codanna) | 651 | **Rust** | tree-sitter + tantivy FTS + fastembed | 15 | ~9 |

### Feature-by-feature comparison

| Capability | Qartez | Serena | code-review-graph | Claude-Context | CodeGraphContext | Codebase-Memory | Repowise | Code Index MCP | Codanna |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| Tree-sitter parsing | **Yes** | No (LSP) | Yes | Chunking only | Yes | Yes | No | Yes (10 langs) | Yes |
| **PageRank importance ranking** | **Yes** | No | No | No | No | No | No | No | No |
| **Blast radius (transitive dependents)** | **Yes** | No | Yes | No | No | Yes | No | No | Yes |
| **Git co-change mining** | **Yes** | No | No | No | No | Yes | Yes | No | No |
| **Cyclomatic complexity per function** | **Yes (16 langs)** | No | No | No | No | No | No | No | No |
| **Hotspot scoring (complexity × PR × churn)** | **Yes** | No | No | No | No | No | No | No | No |
| **Structural code-clone detection** | **Yes** | No | No | No | No | No | No | No | No |
| **Architecture-boundary enforcement** | **Yes** | No | No | No | No | No | No | No | No |
| **Quad-signal impact (PR + blast + co-change + complexity)** | **Yes** | No | No | No | No | No | No | No | No |
| Call graph (caller / callee) | Yes | Partial | Yes | No | Yes | Yes | No | No | Yes |
| **Refactoring (rename / move / rename-file)** | **Yes (preview + apply)** | Rename only (LSP); move via JetBrains plugin (paid) | Rename preview only | No | No | No | No | No | No |
| **Toolchain command runner (test / build / lint)** | **Yes** | Shell only | No | No | No | No | No | No | No |
| **Smart multi-signal context builder** | **Yes** | No | Partial | No | No | No | No | No | No |
| **MCP prompt templates** | **Yes (5)** | No | Yes (5) | No | No | No | No | No | No |
| **One-command multi-IDE install** | **Yes (7 IDEs, Rust wizard)** | No (manual) | Yes (9 IDEs) | No (manual) | Yes (10 IDEs) | Yes (10 agents) | No | No | No |
| Semantic / vector search | FTS5 only | No | Optional (FTS5 hybrid) | **Yes (Milvus)** | No | No | No | No | **Yes (fastembed)** |
| Community detection + auto-wiki | **Yes (Leiden + wiki)** | No | Yes (Leiden + wiki) | No | No | Partial (Louvain, no wiki) | No | No | No |
| Graph visualization | No | No | Yes (D3.js) | No | Yes (Neo4j + HTML) | Yes (3D interactive) | No | No | No |
| Watch mode (incremental re-index) | **Yes (auto-on)** | Partial | Yes | Partial | Yes | Yes | No | Yes | Yes |
| **Published per-tool benchmarks with LLM judge** | **Yes (23 scenarios, 7.9/10 vs 5.3/10)** | Third-party only | Yes (6 repos, 8.2× avg) | Limited (~40% claim) | No | Yes (arXiv paper, 10× tokens) | No | No | Partial (criterion) |
| **Modification guard (blocks risky edits)** | **Yes** | No | No | No | No | No | No | No | No |
| Embedding model / vector DB required | No | No | Optional | **Yes** | No | No | No | No | **Yes** |
| Cloud dependency | No | No | No | Yes (default) | No | No | No | No | Optional |

### Enterprise and IDE-native alternatives

Commercial platforms solving the same problem for users willing to trade local-first and open-source for polish or cross-repo scale:

- **[Sourcegraph Cody / Amp](https://sourcegraph.com/)** - compiler-grade SCIP indexers, official MCP server since 2026. Cloud-first, enterprise pricing.
- **[Augment Code](https://www.augmentcode.com/)** - $227M Series B. Real-time semantic index + code-relationship graph across 400k+ files, official MCP server since Oct 2025. Cloud dependency.
- **[Deep Graph MCP (CodeGPT)](https://github.com/JudiniLabs/mcp-code-graph)** - 392 stars. Cloud-hosted knowledge graph backend; swap `github.com` to `deepgraph.co` in any repo URL for a pre-built code graph. No local indexing needed.
- **JetBrains AI Assistant (IntelliJ 2025.2+)** - embedded MCP server exposing IDE-grade symbols and diagnostics. JetBrains-only.
- **[Cursor](https://cursor.sh/)** - custom embedding model, team-shared index in Turbopuffer. Closed IDE, no MCP exposure.
- **[Windsurf Cascade](https://codeium.com/windsurf)** - RAG-based M-Query retrieval. Closed IDE, no MCP server.

Qartez gives you the same structural intelligence these platforms sell - running entirely on your laptop, for free.

### Also notable

Smaller projects in the same space, sorted by stars:

| Project | Stars | Impl. | Niche |
|---|---:|---|---|
| [Drift](https://github.com/dadbodgeoff/drift) | 772 | TS / Rust | Learns codebase patterns and conventions, teaches them to AI across sessions |
| [Octocode](https://github.com/Muvon/octocode) | 319 | Rust | GraphRAG knowledge graph + hybrid semantic search (4 MCP tools) |
| [mcp-server-tree-sitter](https://github.com/wrale/mcp-server-tree-sitter) | 287 | Python | Raw tree-sitter query exposure for agents to compose their own analyses (~20 tools) |
| [CodeGraph](https://github.com/Jakedismo/codegraph-rust) | 179 | Rust | SurrealDB + LSP + ReAct / LATS agentic architecture, partial blast radius |
| [RepoMapper](https://github.com/pdavis68/RepoMapper) | 150 | Python | Aider's PageRank-on-tree-sitter as a single MCP tool |
| [Narsil-MCP](https://github.com/postrv/narsil-mcp) | 134 | Rust | 90 MCP tools, 32 languages, call graphs + taint analysis + SBOM security scanning |
| [Code Pathfinder](https://github.com/shivasurya/code-pathfinder) | 118 | Go | Security-focused SAST with cross-file taint/dataflow analysis via MCP |
| [Code Graph RAG MCP](https://github.com/er77/code-graph-rag-mcp) | 102 | TypeScript | Graph + RAG hybrid, 26 MCP methods, clone detection |
| [Tree-sitter Analyzer](https://github.com/aimasteracc/tree-sitter-analyzer) | 28 | Python | PageRank + `modification_guard` that blocks unsafe edits (17 languages) |
| [AiDex](https://github.com/CSCSoftware/AiDex) | 25 | TypeScript | 30 MCP tools, task management, screenshot capture, Log Hub (11 languages) |

### Adjacent ecosystems (different category, same problem)

- **[Aider repo-map](https://github.com/Aider-AI/aider)** - Paul Gauthier's CLI pioneered tree-sitter + PageRank in October 2023. Lives inside the aider CLI, not as an MCP server. RepoMapper wraps the single `repo_map` output as MCP.
- **[Continue.dev](https://continue.dev/)** - MCP *client*, not server. Its documentation explicitly recommends pairing Continue with a dedicated code-graph MCP server - the role Qartez fills.
- **[Context7](https://context7.com/)**, **Mem0**, **Pieces LTM** - memory and documentation tools, not codebase indexers. Complementary, not competing.
- **Block Goose**, **Cline**, **Codebuff** - coding agent clients that consume MCP servers. They are the *users* of tools like Qartez.

---

## What makes Qartez different

**1. Quad-signal impact analysis.** `qartez_impact`, `qartez_context`, and `qartez_hotspots` fuse PageRank importance, static blast radius, git co-change, and cyclomatic complexity into one ranked answer. Codebase-Memory MCP ships blast radius and co-change separately but no PageRank and no fusion. code-review-graph ships blast radius alone. No other project combines all four.

**2. Hotspots, clones, and boundaries - all in one server.** `qartez_hotspots` ranks the most dangerous functions in the repo by complexity × coupling × churn. `qartez_clones` finds duplicated logic via AST shape hashing. `qartez_boundaries` enforces architecture rules declared in `.qartez/boundaries.toml`. These are three separate commercial products elsewhere - one MCP call each here.

**3. Refactoring through MCP with preview and apply.** `qartez_rename`, `qartez_move`, and `qartez_rename_file` give the assistant atomic, reviewable refactors in a single MCP call. Serena offers rename via LSP (requires per-language server install); move and rename-file need its paid JetBrains plugin. code-review-graph has rename preview only. The remaining six OSS servers ship no refactoring tools at all.

**4. Built-in safety net.** The modification guard blocks your AI from editing high-impact files without reviewing the blast radius first. Tree-sitter Analyzer (28 stars) has a similar concept; no one else in the main competitor table ships this.

**5. Measured, not claimed.** 23 scenarios, 7.9/10 vs 5.3/10 LLM-judge quality, per-tool token counts and latency - all reproducible with `make bench` (single-language) or `make bench-all` (5 languages with cross-language summary). code-review-graph publishes aggregate benchmarks (8.2×). Codebase-Memory MCP has an arXiv paper (10× tokens). No other OSS MCP publishes per-tool cross-language numbers with an LLM-judge quality axis.

**6. Rust-native, local-first, embedding-free.** Three binaries (`qartez-mcp`, `qartez-guard`, `qartez-setup`). No Python runtime, no embedding model, no vector database, no cloud account. Everything runs on your machine. No code leaves the box. Codanna is also Rust but requires a 150 MB embedding model. Codebase-Memory MCP is C and embedding-free, but has no PageRank, no refactoring, no hotspot scoring, no clone detection, no boundary enforcement, and no LLM-judge benchmarks.

---

## Installation

### Prerequisites

- [Rust toolchain](https://rustup.rs/) (stable, edition 2024)
- [jq](https://jqlang.github.io/jq/) - used by the Makefile for version discovery

### One-shot deploy

```bash
git clone https://github.com/kuberstar/qartez-mcp.git
cd qartez-mcp
make deploy
```

This runs tests, builds the three release binaries (`qartez-mcp`, `qartez-guard`, `qartez-setup`), installs them to `~/.local/bin/`, and configures every detected IDE non-interactively via `qartez-setup --yes` - including hooks, MCP server registration, and the `CLAUDE.md` snippet for Claude Code.

Restart your IDEs after install.

### Interactive install

```bash
make setup
```

Launches `qartez-setup` in interactive mode - it detects your installed IDEs and presents a checkbox list so you can pick which ones to configure.

### Targeted install

```bash
qartez-setup --ide cursor,zed,claude
```

Configure a specific subset of IDEs only. Detected paths:

| IDE | Config path |
|---|---|
| Claude Code | `~/.claude/settings.json` |
| Cursor | `~/.cursor/mcp.json` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` |
| Zed | `~/.config/zed/settings.json` |
| Continue.dev | `~/.continue/config.yaml` |
| OpenCode | `~/.config/opencode/opencode.json` |
| Codex CLI | `~/.codex/config.toml` |

Every install path is idempotent and backs up the existing config.

### Enable Qartez in a project

```bash
cd /path/to/your/project
qartez-mcp --reindex
```

### Claude Desktop (manual)

```json
{
  "mcpServers": {
    "qartez": {
      "command": "/absolute/path/to/qartez-mcp",
      "args": []
    }
  }
}
```

### Uninstall

```bash
make uninstall
```

Removes Qartez from every configured IDE and deletes the binaries.

---

## Command-line options

| Option | Description | Default |
|---|---|---|
| `--root <path>` | Project root to index (repeatable for monorepos) | Auto-detected |
| `--reindex` | Force full re-index | Off |
| `--git-depth <n>` | Commits to analyze for co-change | `300` |
| `--db-path <path>` | Override index location | `.qartez/index.db` |
| `--no-watch` | Disable the automatic file watcher (on by default) | Watcher on |
| `--wiki <path>` | Generate architecture wiki after indexing | Off |
| `--leiden-resolution <f>` | Cluster granularity (larger = more clusters) | `1.0` |
| `--log-level <level>` | `error`, `warn`, `info`, `debug` | `info` |

---

## Project layout

```
src/
  main.rs                  Entry point: index, compute, start server
  cli.rs                   CLI argument parsing
  server/
    mod.rs                 MCP server - all 21 tool handlers
    prompts.rs             5 workflow prompt templates
  index/
    walker.rs              File discovery (respects .gitignore)
    parser.rs              Tree-sitter parser pool
    symbols.rs             Symbols / imports / references + AST shape hashing
    languages/             34 language adapters (16 with cyclomatic complexity)
  graph/
    pagerank.rs            PageRank on import graph
    blast.rs               Blast radius BFS
    leiden.rs              Community detection (Leiden clustering)
    boundaries.rs          Architecture-boundary rules engine
    wiki.rs                Architecture wiki renderer
  git/
    cochange.rs            Co-change pair mining
  storage/
    schema.rs              SQLite + FTS5 schema
    read.rs / write.rs     Query and mutation helpers
    models.rs              Row structs
  bin/
    setup.rs               Interactive IDE setup wizard
    guard.rs               PreToolUse modification guard
    benchmark.rs           Benchmark harness entry point
  benchmark/               Benchmark internals (cargo feature)
scripts/                   Hook + snippet assets embedded by qartez-setup
benchmarks/fixtures.toml   Pinned OSS repos for multi-language benchmarks
```

---

## Running benchmarks

```bash
make bench          # Rust self-bench only - fresh measurements
make bench-all      # All 5 languages (Rust, TypeScript, Python, Go, Java) + cross-language summary
make bench-fixtures # Clone and index the pinned fixture repos
```

Reports land in `reports/benchmark.md` / `reports/benchmark.json` for the single-language run, or `reports/benchmark-<lang>.md` plus a combined cross-language summary for `bench-all`.

---

## License

Free for individuals, commercial license for businesses - see [`LICENSE`](LICENSE).

---

## Star history

If Qartez saves you even 10% of your monthly AI bill, **star the repo** - it's the only thing that tells other builders this approach is worth trying.

If you're working on AI agent infrastructure, a coding assistant, or your own MCP server: **fork it, break it, and open an issue with what you broke**. This is an open specification for what agent-native code tooling should look like, and every real-world bug report moves the standard forward.

<p align="center">
  <strong>Grep was for humans. Qartez is for agents.</strong><br>
  <code>make deploy</code> - and give your assistant the senses it was missing.
</p>
