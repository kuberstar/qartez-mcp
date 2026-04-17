<p align="center">
  <img src="logo.png" alt="Qartez" width="128" height="128">
  <h1 align="center">Qartez MCP</h1>
  <p align="center">
    <strong>X-ray vision for your codebase - built for AI agents, not humans.</strong>
  </p>
  <p align="center">
    The first code-intelligence server designed from day one to be<br>
    <em>consumed by language models</em>, not read by people. Cuts AI token usage by ~94%.
  </p>
  <p align="center">
    <a href="#quickstart">Quickstart</a> ·
    <a href="#the-30-tools">30 Tools</a> ·
    <a href="#modification-guard">Guard</a> ·
    <a href="#benchmarks">Benchmarks</a> ·
    <a href="#comparison-with-alternatives">Comparison</a> ·
    <a href="#supported-languages">37 Languages</a> ·
    <a href="#command-line-options">CLI</a> ·
    <a href="#contributing">Contributing</a> ·
    <a href="#security">Security</a> ·
    <a href="CHANGELOG.md">Changelog</a>
  </p>
  <p align="center">
    <a href="LICENSE"><img alt="License" src="https://img.shields.io/badge/license-dual-blue.svg"></a>
    <img alt="MSRV 1.88" src="https://img.shields.io/badge/MSRV-1.88-orange.svg">
    <img alt="37 languages" src="https://img.shields.io/badge/languages-37-green.svg">
    <img alt="30 MCP tools" src="https://img.shields.io/badge/MCP_tools-30-purple.svg">
  </p>
</p>

---

## Why this exists

`grep`, `find`, `cat`, and `ls` were invented in the **1970s** for humans reading one file at a time in a terminal. Half a century later, your AI assistant is still using them - scanning files byte by byte, re-reading the same directories on every question, guessing at what matters, and burning your tokens on work the tools were never designed to do.

**Qartez is a different species of tooling.** It is not a wrapper around grep. It is a pre-computed knowledge graph of your repository - symbols, imports, call edges, blast radii, PageRank, git co-change, cyclomatic complexity - served to any LLM through the [Model Context Protocol](https://modelcontextprotocol.io/). The agent stops *reading* your codebase and starts *querying* it.

> Think of it as the first purpose-built **sensory organ for coding agents**. Grep sees one line at a time. Qartez sees the entire shape of the codebase in one glance.

Every time your AI assistant touches code, three expensive things happen:

**1. It reads the same files over and over.** No memory of the repo. Every question starts from scratch. You pay for every token - again and again.

**2. It can't see what will break.** Your assistant edits `utils.ts` without knowing 14 other files import it. You find out in CI. Or in production.

**3. It wastes tokens finding things.** "Where is `handleRequest` defined?" turns into Grep across 200 files, Read on 5 candidates, and 1,600 tokens burned before it finds the answer. Qartez answers that in 50 tokens.

The fix isn't a smarter model. It's a smarter index.

---

## Quickstart

**Platform support:** macOS 13+, Ubuntu 22.04+ (and other modern Linux), Windows (native PowerShell 5.1+/7+) and WSL 2. Architectures: x86_64 and arm64. Rust MSRV is **1.88** - the installer fetches it via [rustup](https://rustup.rs/) if missing.

### Install (recommended)

```bash
curl -sSfL https://qartez.dev/install | sh
```

The installer checks for Rust, builds the three release binaries (`qartez`, `qartez-guard`, `qartez-setup`), installs them to `~/.local/bin/`, and launches `qartez-setup` in non-interactive mode. The setup wizard auto-detects every MCP-capable IDE on your machine and configures them all in one pass, including the modification-guard hooks for Claude Code.

Windows (native PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "iwr https://raw.githubusercontent.com/kuberstar/qartez-mcp/main/install.ps1 -useb | iex"
```

Open any project in your IDE - Qartez indexes it automatically on session start. No manual step needed. The file watcher keeps the index fresh as you edit.

### Install via Cargo

```bash
cargo install qartez-mcp
qartez-setup        # then run the IDE wizard manually
```

<details>
<summary>Alternative: install from source</summary>

```bash
git clone https://github.com/kuberstar/qartez-mcp.git
cd qartez-mcp
make deploy
```

Want to inspect the install script before piping it into `sh`? Read it on GitHub: [`install.sh`](install.sh).
</details>

<details>
<summary>Interactive install, targeted install, and other options</summary>

### Works with 19 editors and agents

A single Rust binary (`qartez-setup`) detects and configures every supported editor. No per-editor shell scripts, no copy-paste JSON.

```bash
make deploy                          # Configure every detected IDE (non-interactive)
make setup                           # Same, but interactive checkbox prompt
qartez-setup --ide cursor,zed       # Configure specific IDEs only
make uninstall                       # Remove qartez from every IDE and delete binaries
```

Supported out of the box: **Claude Code**, **Claude Desktop**, **Gemini**, **Cursor**, **Windsurf**, **Kiro**, **Zed**, **Continue.dev**, **Copilot CLI**, **Amazon Q**, **Amp**, **Cline**, **Roo Code**, **Goose**, **Warp**, **Augment**, **OpenCode**, **Codex CLI**, **Antigravity**.

### Targeted install

```bash
qartez-setup --ide cursor,zed,claude
```

Configure a specific subset of IDEs only. Detected paths:

| IDE | Config path |
|---|---|
| Claude Code | `~/.claude/settings.json` |
| Claude Desktop | `~/Library/Application Support/Claude/claude_desktop_config.json` |
| Gemini | `~/.gemini/settings.json` |
| Cursor | `~/.cursor/mcp.json` |
| Windsurf | `~/.codeium/windsurf/mcp_config.json` |
| Kiro | `~/.kiro/settings/mcp.json` |
| Zed | `~/.config/zed/settings.json` |
| Continue.dev | `~/.continue/config.yaml` |
| Copilot CLI | `~/.copilot/mcp-config.json` |
| Amazon Q | `~/.aws/amazonq/mcp.json` |
| Amp | `~/.config/amp/settings.json` |
| Cline | VS Code global storage `saoudrizwan.claude-dev/settings/cline_mcp_settings.json` |
| Roo Code | VS Code global storage `rooveterinaryinc.roo-cline/settings/cline_mcp_settings.json` |
| Goose | `~/.config/goose/config.yaml` |
| Warp | `~/.warp/mcp_settings.json` |
| Augment | `~/.augment/settings.json` |
| OpenCode | `~/.config/opencode/opencode.json` (or `opencode.jsonc`) |
| Codex CLI | `~/.codex/config.toml` |
| Antigravity | `~/.gemini/antigravity/mcp_config.json` |

Every install path is idempotent and backs up the existing config.

### Enable Qartez in a project

Qartez indexes automatically on session start. For manual re-indexing:

```bash
qartez --root /path/to/your/project --reindex
```

### Claude Desktop (manual)

```json
{
  "mcpServers": {
    "qartez": {
      "command": "/absolute/path/to/qartez",
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

</details>

---

## What Qartez does

Qartez builds a **knowledge graph** of your codebase - once - and serves it to any AI assistant through MCP. Instead of scanning files from scratch on every question, your assistant queries a pre-computed index that knows:

- Which files matter most (PageRank on the import graph)
- What breaks if you change a file (blast radius analysis)
- Which files always change together (git co-change mining)
- Which functions are the most dangerous to touch (cyclomatic complexity x coupling x churn)
- Where every symbol is defined, who uses it, and who calls it
- Which blocks of code are duplicated (structural AST shape hashing)
- Which architecture boundaries the imports are violating
- What types implement a trait/interface, and vice versa

The result: your AI works faster, uses fewer tokens, refactors safely, and stops making blind changes to load-bearing files.

### Before and after

| Task | Without Qartez | With Qartez |
|---|---|---|
| "Where is `QartezServer` defined?" | Grep 200 files, Read candidates. **1,490 tokens.** | `qartez_find`. **52 tokens.** |
| "What breaks if I change `storage/read.rs`?" | BFS grep from imports, depth 2. **9,243 tokens.** | `qartez_impact`: direct + transitive importers + co-change. **352 tokens.** |
| "Outline `src/server/mod.rs` (96 symbols)" | Read full 300KB file. **77,843 tokens.** | `qartez_outline` with signatures. **3,009 tokens.** |
| "Find all dead exports" | Impossible without tooling. | `qartez_unused`: pre-materialized, instant. **468 tokens.** |
| "Which functions are the riskiest to refactor?" | Nothing to query. | `qartez_hotspots`: complexity x PageRank x churn. |

---

## The 30 tools

Think of these as the **standard library for AI code understanding**. Each one replaces a multi-step human workflow with a single, token-efficient call the agent can reason about.

Tools are organized into **tiers** with progressive disclosure. Core tools are always available. Additional tiers can be unlocked on demand via `qartez_tools enable: ["analysis"]` (or `"all"`).

### Core (always available)

| Tool | What it does |
|---|---|
| `qartez_map` | **Start here.** Project skeleton ranked by importance. PageRank, exports, blast radii. Boost by files or terms to focus on what you're working on. |
| `qartez_find` | Jump to a symbol definition by exact name. File, line range, signature, visibility. No scanning. |
| `qartez_grep` | FTS5 search across indexed symbols. Prefix matching, regex fallback, optional body search. |
| `qartez_read` | Read one or more symbols' source code with line numbers. No file scanning. Jumps directly to the symbol. |
| `qartez_outline` | Table of contents for any file: every symbol grouped by kind, with signatures. |
| `qartez_impact` | **Call before editing any important file.** Shows direct importers, transitive dependents, and co-change partners. Everything that could break. |
| `qartez_deps` | Dependency graph for a file: what it imports, what imports it. |
| `qartez_stats` | Codebase dashboard: files, symbols, edges by language, most-connected files. |

### Analysis (unlock via `qartez_tools`)

| Tool | What it does |
|---|---|
| `qartez_refs` | Trace every usage of a symbol across the codebase, with optional transitive chains. |
| `qartez_calls` | Call hierarchy: who calls this function, and what does it call. |
| `qartez_cochange` | Files that historically change together in git. Logical coupling invisible to the import graph. |
| `qartez_context` | Smart context builder: given files you plan to modify, returns the optimal set of related files to read first. |
| `qartez_unused` | Dead-code finder: exported symbols with zero importers, pre-materialized at index time. |
| `qartez_diff_impact` | **Batch impact for a git diff range.** Pass a revspec like `main..HEAD` to get changed files with PageRank, union blast radius, convergence points, and co-change omissions. One call replaces N calls to `qartez_impact` + `qartez_cochange`. |
| `qartez_hotspots` | **The refactor radar.** Ranks files and functions by hotspot score = **cyclomatic complexity x PageRank x (1 + churn)**. Points straight at the highest-risk code in the repo. |
| `qartez_clones` | Structural code-clone detection via AST shape hashing (identifiers, literals, and comments are normalized away). Finds duplicate logic the human reviewer would never spot. |
| `qartez_boundaries` | Architecture-boundary enforcement. Declare "these modules may not import those" in `.qartez/boundaries.toml` and get every violating edge back. `suggest=true` seeds a starter config from the Leiden clustering. |
| `qartez_hierarchy` | Type hierarchy queries: find all types implementing a trait/interface, or all traits/interfaces a type implements. Works across Rust, TypeScript, Java, Python, and Go. |
| `qartez_trend` | Complexity trend over git history: tracks how a function's cyclomatic complexity evolved commit by commit. Flags functions that are GROWING, STABLE, or SHRINKING. |
| `qartez_security` | Security scanner with 13 built-in rules. Regex-based pattern matching scored by PageRank to prioritize high-impact files. Custom rules via `.qartez/security.toml`. Filters by severity (low/medium/high/critical) and category. |
| `qartez_smells` | Code smell detector: finds **god functions** (high complexity + long body), **long parameter lists**, and **feature envy** (methods that use another type more than their own). Tuneable thresholds. |
| `qartez_test_gaps` | Test coverage gap analysis via the import graph. Three modes: `gaps` ranks untested source files by risk, `map` shows test-to-source mappings, `suggest` recommends tests to run for a git diff range. |
| `qartez_knowledge` | **Bus-factor analysis.** Git-blame-based authorship at file and module level. Surfaces single-author files and modules where knowledge is concentrated in one contributor. |
| `qartez_semantic` | Semantic search using a local embedding model. Natural-language queries ranked by hybrid FTS5 + vector similarity (RRF). Requires the `semantic` cargo feature and a one-time model download (~270 MB). |

### Refactor (unlock via `qartez_tools`)

| Tool | What it does |
|---|---|
| `qartez_rename` | Rename a symbol across the entire codebase. Definition, imports, all usages. Preview by default, `apply=true` to execute. |
| `qartez_move` | Move a symbol to another file and rewrite all import paths. One MCP call. |
| `qartez_rename_file` | Rename a file and update every import pointing to it. |

### Meta (unlock via `qartez_tools`)

| Tool | What it does |
|---|---|
| `qartez_project` | Auto-detects your toolchain (Cargo, npm/bun/yarn, Go, Python, Make, Gradle) and runs test/build/lint/typecheck through a single tool. |
| `qartez_wiki` | Generates a markdown architecture wiki using Leiden community detection on the import graph. Partitions files into clusters, names each one, and emits `ARCHITECTURE.md` with inter-cluster edges. |

### Tier management

| Tool | What it does |
|---|---|
| `qartez_tools` | **Always visible.** Lists all tiers and their tools. Use `enable: ["analysis"]`, `enable: ["all"]`, or `disable: ["refactor"]` to control which tools are exposed to the agent. Core tools cannot be disabled. |

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
2. Guard checks: PageRank 0.23 (> 0.05 threshold), blast radius 10 (>= 10 threshold)
3. Edit is **blocked** with an explanation listing which thresholds fired
4. AI calls `qartez_impact file_path=src/server/mod.rs` - reviews the blast radius
5. Guard grants a 10-minute edit window for that file
6. AI retries the edit - **allowed**

Zero configuration. Tuneable via `QARTEZ_GUARD_PAGERANK_MIN`, `QARTEZ_GUARD_BLAST_MIN`, `QARTEZ_GUARD_ACK_TTL_SECS`, or disabled with `QARTEZ_GUARD_DISABLE=1`.

---

## Benchmarks

Not claims. Measured. Reproducible. Run `make bench` and verify yourself.

### Headline

**Aggregate token savings vs `Glob + Grep + Read + git log`: +91.8%**
(sum of MCP 38,789 / sum of non-MCP 472,109 tokens across all **28 scenarios** on the Qartez self-bench. Conservative under-count: 10 of 28 scenarios have an incomplete non-MCP sim - those rows still contribute their MCP tokens to both sums. On the 18 scenarios with a fair token-to-token comparison the saving rises to **+94.5%**.)

**LLM-judge quality (claude-opus-4-6):** **MCP 8.3 / 10** vs non-MCP **4.3 / 10** across five axes (correctness, completeness, usability, groundedness, conciseness), n=28.

**Session cost context.** A typical Claude Code session starts at ~20,000 tokens of prompt overhead. A single `make bench` run saves ~433,000 tokens - **~21 empty sessions** worth of budget bought back, just from routing questions through the right tool.

### Per-tool breakdown (Rust self-bench)

18 tools with complete non-MCP simulations (fair token-to-token comparison):

| Tool | MCP tokens | Without MCP | Savings | Speedup |
|---|---:|---:|---:|---:|
| `qartez_cochange` | 92 | 14,622 | **+99.4%** | 2x |
| `qartez_context` | 107 | 4,489 | **+97.6%** | **533x** |
| `qartez_find` | 52 | 1,490 | **+96.5%** | **210x** |
| `qartez_impact` | 352 | 9,243 | **+96.2%** | **140x** |
| `qartez_outline` | 3,009 | 77,843 | **+96.1%** | 5x |
| `qartez_project` | 68 | 1,394 | **+95.1%** | 0x |
| `qartez_unused` | 468 | 6,750 | **+93.1%** | 22x |
| `qartez_deps` | 166 | 2,286 | **+92.7%** | **118x** |
| `qartez_map` | 87 | 674 | **+87.1%** | 1x |
| `qartez_rename_file` | 27 | 185 | **+85.4%** | **211x** |
| `qartez_grep` | 127 | 763 | **+83.4%** | 72x |
| `qartez_stats` | 155 | 848 | **+81.7%** | 1x |
| `qartez_move` | 161 | 701 | **+77.0%** | **159x** |
| `qartez_calls` | 564 | 2,409 | **+76.6%** | 3x |
| `qartez_refs` | 201 | 692 | **+71.0%** | 26x |
| `qartez_read` | 150 | 495 | **+69.7%** | **100x** |
| `qartez_hierarchy` | 735 | 2,056 | **+64.3%** | **127x** |
| `qartez_rename` | 439 | 648 | **+32.3%** | 11x |

10 additional analytical tools have no meaningful grep/read equivalent - they solve problems the non-MCP stack cannot solve at all:

`qartez_hotspots`, `qartez_clones`, `qartez_smells`, `qartez_test_gaps`, `qartez_wiki`, `qartez_boundaries`, `qartez_trend`, `qartez_knowledge`, `qartez_diff_impact`, `qartez_security`.

<details>
<summary>Multi-language bench</summary>

`make bench-all` runs the same 28-scenario harness against five pinned OSS fixtures - **`colinhacks/zod`** (TypeScript), **`spf13/cobra`** (Go), **`encode/httpx`** (Python), **`FasterXML/jackson-core`** (Java), plus the Qartez self-bench (Rust) - then emits a cross-language summary to `reports/benchmark-<lang>.md` plus a combined matrix. Every tool, every language, every scenario - measured with the `cl100k_base` tokenizer against a faithful `Glob + Grep + Read + git log` simulation.

```bash
make bench          # Rust self-bench only - fresh measurements
make bench-all      # All 5 languages (Rust, TypeScript, Python, Go, Java) + cross-language summary
make bench-fixtures # Clone and index the pinned fixture repos
```

Reports land in `reports/benchmark.md` / `reports/benchmark.json` for the single-language run, or `reports/benchmark-<lang>.md` plus a combined cross-language summary for `bench-all`.

</details>

---

## How it works under the hood

Four layers, computed once, queried from SQLite on every tool call.

### 1. Tree-sitter parsing

Every source file is parsed by a language-specific tree-sitter grammar. No LSP server, no per-language SDK installs, no cold-start penalty. The parser extracts symbols (functions, methods, types, constants), their signatures, line ranges, export visibility, import relationships, and - for 21 imperative languages - cyclomatic complexity per function.

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

### Transport

Qartez communicates over **stdio** (stdin/stdout JSON-RPC), the standard MCP transport. No HTTP server, no port allocation, no network exposure. The IDE launches the `qartez` binary as a child process and exchanges messages over pipes.

---

## Supported languages

One binary. No per-language setup. All 37 languages parsed by tree-sitter (with regex fallbacks for formats lacking a compatible grammar). 21 imperative languages also get cyclomatic complexity per function, powering `qartez_hotspots`.

<details>
<summary>Full language table (37 languages)</summary>

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
| Haskell | `.hs` `.lhs` - top-level functions, `data`, `newtype`, `type`, typeclasses, `import` |
| OCaml | `.ml` `.mli` - `let` bindings, `type`, `module`, `class`, `exception`, `open`/`include` |
| R | `.r` `.R` - function/variable assignments, S4/R6 classes, `library`/`require`/`source` |
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
| Starlark / Bazel | `BUILD`, `BUILD.bazel`, `WORKSPACE`, `WORKSPACE.bazel`, `.bzl`, `.star`, `.bazel` - `load`, rules with `name=`, `def` |
| Jsonnet | `.jsonnet` `.libsonnet` - `local` functions/vars, fields, `import`/`importstr` |
| Caddyfile | `Caddyfile`, `.caddyfile` - site blocks, `handle`, `reverse_proxy`, snippets |
| Systemd units | `.service` `.timer` `.socket` `.mount` `.target` `.path` `.slice` `.scope` - sections, `ExecStart`, directives |

</details>

**Highlights:** TypeScript, Rust, Go, Python, Java, Kotlin, Swift, C#, C/C++, Ruby, PHP, Dart, Scala, Elixir, Zig, Lua, Haskell, OCaml, R, and 17 more. All 21 imperative languages include cyclomatic complexity scoring.

---

## Comparison with alternatives

The MCP codebase-intelligence space is crowded in 2026. This section covers direct OSS competitors, enterprise platforms, and adjacent ecosystems. All star counts were cross-checked against each project's GitHub repository in April 2026.

### Direct OSS MCP competitors

Nine projects share the "MCP server for codebase intelligence" niche, sorted by GitHub stars.

| Project | Stars | Impl. | Indexing approach | Languages | MCP tools |
|---|---:|---|---|---:|---:|
| **Qartez** (this repo) | new | **Rust** | tree-sitter + SQLite + PageRank + blast radius + co-change + complexity + clones + boundaries | **37** | **30** |
| [Serena](https://github.com/oraios/serena) | 23k | Python | LSP (per-language language servers) | **46+** | ~35 |
| [code-review-graph](https://github.com/tirth8205/code-review-graph) | 10.4k | Python | tree-sitter + SQLite + Leiden clustering | 23+ | 28 |
| [Claude-Context](https://github.com/zilliztech/claude-context) | 5.9k | TypeScript | Embeddings + Milvus/Zilliz vector DB | 14 | 4 |
| [CodeGraphContext](https://github.com/CodeGraphContext/CodeGraphContext) | 3k | Python | tree-sitter + KuzuDB / FalkorDB / Neo4j | 14 | 21 |
| [Codebase-Memory MCP](https://github.com/DeusData/codebase-memory-mcp) | 1.6k | C | tree-sitter + SQLite + hybrid type resolution | **66** | 14 |
| [Repowise](https://github.com/repowise-dev/repowise) | 1.2k | Python | Dependency graph + git history + LLM-generated docs | 14 | 7 |
| [Code Index MCP](https://github.com/johnhuang316/code-index-mcp) | 903 | Python | tree-sitter (10 langs) + ripgrep fallback for 50+ | 10 + 50 | 11 |
| [Codanna](https://github.com/bartolli/codanna) | 651 | **Rust** | tree-sitter + tantivy FTS + fastembed | 15 | ~9 |

<details>
<summary>Feature-by-feature comparison</summary>

| Capability | Qartez | Serena | code-review-graph | Claude-Context | CodeGraphContext | Codebase-Memory | Repowise | Code Index MCP | Codanna |
|---|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
| Tree-sitter parsing | **Yes** | No (LSP) | Yes | Chunking only | Yes | Yes | No | Yes (10 langs) | Yes |
| **PageRank importance ranking** | **Yes** | No | No | No | No | No | No | No | No |
| **Blast radius (transitive dependents)** | **Yes** | No | Yes | No | No | Yes | No | No | Yes |
| **Git co-change mining** | **Yes** | No | No | No | No | Yes | Yes | No | No |
| **Cyclomatic complexity per function** | **Yes (21 langs)** | No | No | No | No | No | No | No | No |
| **Hotspot scoring (complexity x PR x churn)** | **Yes** | No | No | No | No | No | No | No | No |
| **Structural code-clone detection** | **Yes** | No | No | No | No | No | No | No | No |
| **Architecture-boundary enforcement** | **Yes** | No | No | No | No | No | No | No | No |
| **Quad-signal impact (PR + blast + co-change + complexity)** | **Yes** | No | No | No | No | No | No | No | No |
| **Code smell detection (god functions, feature envy)** | **Yes** | No | No | No | No | No | No | No | No |
| **Test coverage gap analysis** | **Yes** | No | No | No | No | No | No | No | No |
| **Bus-factor / knowledge analysis** | **Yes** | No | No | No | No | No | No | No | No |
| **Type hierarchy queries** | **Yes** | Via LSP | No | No | No | No | No | No | No |
| Call graph (caller / callee) | Yes | Partial | Yes | No | Yes | Yes | No | No | Yes |
| **Refactoring (rename / move / rename-file)** | **Yes (preview + apply)** | Rename only (LSP); move via JetBrains plugin (paid) | Rename preview only | No | No | No | No | No | No |
| **Toolchain command runner (test / build / lint)** | **Yes** | Shell only | No | No | No | No | No | No | No |
| **Smart multi-signal context builder** | **Yes** | No | Partial | No | No | No | No | No | No |
| **Batch diff impact analysis** | **Yes** | No | No | No | No | No | No | No | No |
| **MCP prompt templates** | **Yes (5)** | No | Yes (5) | No | No | No | No | No | No |
| **One-command multi-IDE install** | **Yes (19 IDEs, Rust wizard)** | No (manual) | Yes (9 IDEs) | No (manual) | Yes (10 IDEs) | Yes (10 agents) | No | No | No |
| **Security scanning (regex + PageRank scoring)** | **Yes** | No | No | No | No | No | No | No | No |
| **Complexity trend over git history** | **Yes** | No | No | No | No | No | No | No | No |
| **Progressive tool disclosure (tiers)** | **Yes (4 tiers)** | No | No | No | No | No | No | No | No |
| Semantic / vector search | **Yes (opt-in, local embedding)** | No | Optional (FTS5 hybrid) | **Yes (Milvus)** | No | No | No | No | **Yes (fastembed)** |
| Community detection + auto-wiki | **Yes (Leiden + wiki)** | No | Yes (Leiden + wiki) | No | No | Partial (Louvain, no wiki) | No | No | No |
| Graph visualization | No | No | Yes (D3.js) | No | Yes (Neo4j + HTML) | Yes (3D interactive) | No | No | No |
| Watch mode (incremental re-index) | **Yes (auto-on)** | Partial | Yes | Partial | Yes | Yes | No | Yes | Yes |
| **Published per-tool benchmarks with LLM judge** | **Yes (28 scenarios, 8.3/10 vs 4.3/10)** | Third-party only | Yes (6 repos, 8.2x avg) | Limited (~40% claim) | No | Yes (arXiv paper, 10x tokens) | No | No | Partial (criterion) |
| **Modification guard (blocks risky edits)** | **Yes** | No | No | No | No | No | No | No | No |
| Embedding model / vector DB required | No | No | Optional | **Yes** | No | No | No | No | **Yes** |
| Cloud dependency | No | No | No | Yes (default) | No | No | No | No | Optional |

</details>

### Enterprise and IDE-native alternatives

Commercial platforms solving the same problem for users willing to trade local-first and open-source for polish or cross-repo scale:

- **[Sourcegraph Cody / Amp](https://sourcegraph.com/)** - compiler-grade SCIP indexers, official MCP server since 2026. Cloud-first, enterprise pricing.
- **[Augment Code](https://www.augmentcode.com/)** - $227M Series B. Real-time semantic index + code-relationship graph across 400k+ files, official MCP server since Oct 2025. Cloud dependency.
- **[Deep Graph MCP (CodeGPT)](https://github.com/JudiniLabs/mcp-code-graph)** - 392 stars. Cloud-hosted knowledge graph backend; swap `github.com` to `deepgraph.co` in any repo URL for a pre-built code graph. No local indexing needed.
- **JetBrains AI Assistant (IntelliJ 2025.2+)** - embedded MCP server exposing IDE-grade symbols and diagnostics. JetBrains-only.
- **[Cursor](https://cursor.sh/)** - custom embedding model, team-shared index in Turbopuffer. Closed IDE, no MCP exposure.
- **[Windsurf Cascade](https://codeium.com/windsurf)** - RAG-based M-Query retrieval. Closed IDE, no MCP server.

Qartez gives you the same structural intelligence these platforms sell - running entirely on your laptop, for free.

<details>
<summary>Also notable (smaller projects)</summary>

| Project | Stars | Impl. | Niche |
|---|---:|---|---|
| [Drift](https://github.com/dadbodgeoff/drift) | 772 | TS / Rust | Learns codebase patterns and conventions, teaches them to AI across sessions |
| [Octocode](https://github.com/Muvon/octocode) | 319 | Rust | GraphRAG knowledge graph + hybrid semantic search (4 MCP tools) |
| [mcp-server-tree-sitter](https://github.com/wrale/mcp-server-tree-sitter) | 287 | Python | Raw tree-sitter query exposure for agents to compose their own analyses (~20 tools) |
| [CodeGraph](https://github.com/Jakedismo/codegraph-rust) | 179 | Rust | SurrealDB + LSP + ReAct / LATS agentic architecture, partial blast radius |
| [RepoMapper](https://github.com/pdavis68/RepoMapper) | 150 | Python | Aider's PageRank-on-tree-sitter as a single MCP tool |
| [Narsil-MCP](https://github.com/postrv/narsil-mcp) | 134 | Rust | 90 MCP tools, 32 languages, call graphs + taint analysis + SBOM security scanning |
| [Code Pathfinder](https://github.com/shivasurya/code-pathfinder) | 118 | Go | Security-focused SAST with cross-file taint/dataflow analysis via MCP |
| [Code Graph RAG MCP](https://github.com/er77/code-graph-rag-mcp) | 86 | TypeScript | Graph + RAG hybrid, 26 MCP methods, clone detection |
| [Tree-sitter Analyzer](https://github.com/aimasteracc/tree-sitter-analyzer) | 20 | Python | PageRank + `modification_guard` that blocks unsafe edits (17 languages) |
| [AiDex](https://github.com/CSCSoftware/AiDex) | 25 | TypeScript | 30 MCP tools, task management, screenshot capture, Log Hub (11 languages) |

</details>

### Adjacent ecosystems (different category, same problem)

- **[Aider repo-map](https://github.com/Aider-AI/aider)** - Paul Gauthier's CLI pioneered tree-sitter + PageRank in October 2023. Lives inside the aider CLI, not as an MCP server. RepoMapper wraps the single `repo_map` output as MCP.
- **[Continue.dev](https://continue.dev/)** - MCP *client*, not server. Its documentation explicitly recommends pairing Continue with a dedicated code-graph MCP server - the role Qartez fills.
- **[Context7](https://context7.com/)**, **Mem0**, **Pieces LTM** - memory and documentation tools, not codebase indexers. Complementary, not competing.
- **Block Goose**, **Cline**, **Codebuff** - coding agent clients that consume MCP servers. They are the *users* of tools like Qartez.

---

## What makes Qartez different

**1. Quad-signal impact analysis.** `qartez_impact`, `qartez_diff_impact`, `qartez_context`, and `qartez_hotspots` fuse PageRank importance, static blast radius, git co-change, and cyclomatic complexity into one ranked answer. No other project combines all four.

**2. Hotspots, clones, boundaries, security, smells, test gaps, knowledge, and trends in one server.** `qartez_hotspots` ranks the most dangerous functions in the repo by complexity x coupling x churn. `qartez_clones` finds duplicated logic via AST shape hashing. `qartez_boundaries` enforces architecture rules declared in `.qartez/boundaries.toml`. `qartez_security` scans for vulnerability patterns scored by PageRank. `qartez_smells` detects god functions, long parameter lists, and feature envy. `qartez_test_gaps` finds untested source files ranked by risk. `qartez_knowledge` surfaces bus-factor risks from git blame. `qartez_trend` tracks how a function's complexity evolved commit by commit. These are eight separate commercial products elsewhere, one MCP call each here.

**3. Refactoring through MCP with preview and apply.** `qartez_rename`, `qartez_move`, and `qartez_rename_file` give the assistant atomic, reviewable refactors in a single MCP call. Serena offers rename via LSP (requires per-language server install); the remaining servers ship no refactoring tools at all.

**4. Built-in safety net.** The modification guard blocks your AI from editing high-impact files without reviewing the blast radius first. No other server in the main competitor table ships this.

**5. Measured, not claimed.** 28 scenarios, 8.3/10 vs 4.3/10 LLM-judge quality, per-tool token counts and latency. All reproducible with `make bench` (single-language) or `make bench-all` (5 languages with cross-language summary).

**6. Rust-native, local-first, zero cloud dependency.** Three binaries (`qartez`, `qartez-guard`, `qartez-setup`). No Python runtime, no vector database, no cloud account. Everything runs on your machine. No code leaves the box. An optional `semantic` cargo feature adds local embedding search, but the default build needs no model download.

---

## Command-line options

Qartez also works as a standalone CLI. Run `qartez <tool_name>` (e.g., `qartez map`, `qartez find Config`, `qartez impact src/server/mod.rs`) to use any core or analysis tool directly from the terminal without an MCP client.

| Option | Description | Default |
|---|---|---|
| `--root <path>` | Project root to index (repeatable for monorepos) | Auto-detected |
| `--reindex` | Force full re-index | Off |
| `--git-depth <n>` | Commits to analyze for co-change | `300` |
| `--db-path <path>` | Override index location | `.qartez/index.db` |
| `--no-watch` | Disable the automatic file watcher (on by default) | Watcher on |
| `--wiki <path>` | Generate architecture wiki after indexing | Off |
| `--leiden-resolution <f>` | Cluster granularity (larger = more clusters) | `1.0` |
| `--format <format>` | Output format for CLI subcommands: `human`, `json`, `compact` | `human` |
| `--log-level <level>` | `error`, `warn`, `info`, `debug`, `trace` (any `tracing` directive accepted) | `info` |

---

<details>
<summary>Project layout</summary>

```
src/
  main.rs                  Entry point: index, compute, start server
  lib.rs                   Library root (re-exports)
  cli.rs                   CLI argument parsing (19 subcommands)
  cli_runner.rs            CLI subcommand dispatcher
  config.rs                Project configuration and root detection
  error.rs                 Error types
  str_utils.rs             String utilities (stable floor_char_boundary polyfill)
  toolchain.rs             Toolchain detection (Cargo, npm, Go, etc.)
  watch.rs                 File watcher for incremental re-indexing
  guard.rs                 Modification guard evaluation engine
  embeddings.rs            Local embedding model for qartez_semantic (opt-in)
  server/
    mod.rs                 MCP server entrypoint - dispatches to per-tool handlers
    tools/                 30 per-tool handler modules (one file per MCP tool)
    prompts.rs             5 workflow prompt templates
    tiers.rs               Progressive tool disclosure (core/analysis/refactor/meta)
    cache.rs               Tree-sitter parse cache
    helpers.rs             Shared handler utilities
    overview.rs            Overview/map generation
    params.rs              Tool parameter structs
    treesitter.rs          Tree-sitter integration helpers
    mcp_instructions.md    Embedded MCP server instructions
  index/
    mod.rs                 Core indexing engine (full + incremental, import resolution)
    walker.rs              File discovery (respects .gitignore + .qartezignore)
    parser.rs              Tree-sitter parser pool
    symbols.rs             Symbols / imports / references + AST shape hashing
    languages/             37 language adapters (21 with cyclomatic complexity)
  graph/
    mod.rs                 Graph module root
    pagerank.rs            PageRank on import graph
    blast.rs               Blast radius BFS
    leiden.rs              Community detection (Leiden clustering)
    boundaries.rs          Architecture-boundary rules engine
    security.rs            Security rule engine (powers qartez_security)
    wiki.rs                Architecture wiki renderer
  git/
    mod.rs                 Git module root
    cochange.rs            Co-change pair mining
    diff.rs                Diff range analysis (for qartez_diff_impact)
    trend.rs               Complexity trend over git history
    knowledge.rs           Code authorship and bus-factor analysis
  storage/
    mod.rs                 Storage module root
    schema.rs              SQLite + FTS5 schema
    read.rs / write.rs     Query and mutation helpers
    models.rs              Row structs
  bin/
    setup.rs               Interactive IDE setup wizard (19 IDEs)
    guard.rs               PreToolUse modification guard
    benchmark.rs           Benchmark harness entry point
  benchmark/               Benchmark internals (cargo feature)
    profiles/              Per-language benchmark profiles (Rust, TS, Python, Go, Java)
    scenarios.rs           28 benchmark scenarios
    judge.rs               LLM-judge harness
    report.rs              Markdown / JSON report writers
    tokenize.rs            cl100k_base token accounting
scripts/                   Hook + snippet assets embedded by qartez-setup
benchmarks/fixtures.toml   Pinned OSS repos for multi-language benchmarks
reports/                   Generated benchmark.md / benchmark.json artifacts
```

</details>

---

## Contributing

Found a bug? Open an [issue](https://github.com/kuberstar/qartez-mcp/issues). Want to add a language, fix a parser, or improve a tool? Pull requests are welcome - read [`CONTRIBUTING.md`](CONTRIBUTING.md) and [`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md) first. Non-trivial PRs require signing the [`CLA.md`](CLA.md).

```bash
git clone https://github.com/kuberstar/qartez-mcp.git
cd qartez-mcp
cargo build
cargo test
```

Release notes for every version live in [`CHANGELOG.md`](CHANGELOG.md).

---

## Security

Found a vulnerability? **Do not open a public issue.** Follow the disclosure policy in [`SECURITY.md`](SECURITY.md).

---

## License

Dual-licensed under the **Qartez Small Team License** (free for individuals and small teams) and the **Qartez Commercial License** (for everyone else). Read the full text in [`LICENSE`](LICENSE), and see [`COMMERCIAL.md`](COMMERCIAL.md) for the commercial terms summary. SPDX identifier: `LicenseRef-Qartez-Dual`.

---

<p align="center">
  <strong>Grep was for humans. Qartez is for agents.</strong><br><br>
  If Qartez saves you even 10% of your monthly AI bill, <a href="https://github.com/kuberstar/qartez-mcp">star the repo</a> - it's the only signal that tells other builders this approach is worth trying.
</p>
