# Changelog

## [0.3.0] — 2026-04-15

### Added

- **Dart/Flutter language support** — full resolver with barrel export resolution, receiver-type heuristics, and reference tracking
- **Gemini CLI support** — automated setup with hooks for `gemini` alongside Claude Code
- **`qartez_hierarchy` tool** — query type relationships (subtypes, supertypes) with transitive traversal
- **`qartez_diff_impact` tool** — batch pre-merge blast radius analysis across multiple changed files
- **`.qartezignore` support** — exclude paths from indexing beyond `.gitignore` rules
- **OpenCode plugin** — edit guard and MCP instructions for OpenCode IDE
- **MCP static resources** — `qartez://hotspots` and `qartez://stats` for precomputed data access
- **IDE rules** — MCP instructions for Cursor, Codex, and OpenCode alongside Claude Code
- **Background indexing on startup** — MCP tools load immediately while indexing runs in a background thread

### Changed

- **Server modularized** — split monolithic `mod.rs` into cohesive submodules for maintainability
- **Storage layer deduplicated** — unified `SymbolRow`/`FileRow` deserialization in JOINed queries
- **PageRank warm-start** — incremental re-index reuses prior iteration values for faster convergence
- **DB mutex released earlier** — dropped before FS reads and tree-sitter parsing to reduce lock contention
- **Resolver upgraded** — kind-filter and receiver-type heuristics for more accurate symbol resolution
- **Type-aware resolution** — symbol lookup now considers type context for disambiguation
- **README restructured** — corrected tool count (23), language count (34), and updated navigation

### Fixed

- **Path traversal protection** — `safe_resolve` rejects `../` escape attempts in user-supplied file paths
- **Comma-separated `Vec<String>` params** — MCP tools now correctly parse `"a,b,c"` as a list
- **SQL column aliases** — corrected hierarchy query column names and added integration tests
- **CLAUDE.md snippet location** — writes only to `~/.claude`, skips variant directories
- **Cochange phantom files** — filtered out files no longer in the repo from co-change results
- **Cargo metadata** — removed redundant `license` field, kept `license-file` only

### Contributors

- **josh** ([@josh](https://github.com/josh)) — Dart/Flutter support, resolver improvements, background indexing
- **Rudolf Troger** ([@DolphRoger](https://github.com/DolphRoger)) — Gemini CLI support

## [0.2.0] — 2026-04-15

### Added

- **Background auto-update** — `qartez-mcp` checks GitHub for newer releases on startup (24h TTL, cross-process flock) and rebuilds from source via `install.sh` when a new version is available. Opt out with `QARTEZ_NO_AUTO_UPDATE=1`
- **One-liner install** — `curl -sSfL https://qartez.dev/install | sh` downloads and builds from source without cloning the repo
- **Runtime state mirroring** — setup wizard now writes MCP config into Claude Code's `.claude.json` state file so accounts with existing state pick up qartez immediately

### Changed

- **License upgraded to Small Team tier** — free for up to 3 users and <$1M annual revenue (was: individuals only). Added patent grant, explicit eligibility examples, and 30-day grace period
- **Atomic binary install** — `install.sh` uses copy-to-`.new`-then-`mv` to avoid ETXTBSY and corruption during in-place upgrades

### Fixed

- Setup wizard now cleans up `.claude.json` state file on uninstall (previously only cleaned `settings.json`)
- Update cache file only touched on successful GitHub API check, preventing stale cache from masking transient failures

## [0.1.1] — 2026-04-14

### Added

- **Zero-dependency installer** (`install.sh`) — single script that auto-installs Rust, builds, tests, and configures IDEs. Works on macOS and Linux without jq or bash
- **Install portability test suite** — 50 checks covering POSIX compliance, error paths, download safety, and Docker integration

### Changed

- `make deploy` now delegates to `install.sh` instead of inline Makefile logic

### Fixed

- Ad-hoc codesign binaries on macOS to prevent Gatekeeper SIGKILL (exit 137)
- 7 portability bugs in install flow (platform detection, shell compatibility)
- All platforms handled in dependency auto-install (apt, dnf, pacman, apk)
- Clippy warning in Kotlin complexity counter

## [0.1.0] — 2026-04-14

Initial public release.

### Features

- **34 language parsers** via tree-sitter — Rust, Go, Python, TypeScript, JavaScript, Java, C, C++, C#, Ruby, Kotlin, Swift, PHP, Bash, CSS, Lua, Scala, Dart, Elixir, Zig, Nix, SQL, Protobuf, and more
- **DevOps format support** — Dockerfile, Helm charts, HCL/Terraform, YAML (Kubernetes, GitLab CI, GitHub Actions), Makefile, Nginx, TOML, Caddyfile, Systemd, Jenkinsfile/Groovy, Starlark, Jsonnet
- **21 MCP tools:**
  - `qartez_map` — codebase skeleton ranked by PageRank
  - `qartez_find` — symbol lookup by name
  - `qartez_grep` — full-text search across the codebase
  - `qartez_read` — read symbol source with semantic context
  - `qartez_outline` — file structure with all symbols and their signatures
  - `qartez_refs` — find all references to a symbol
  - `qartez_calls` — call hierarchy (callers and callees)
  - `qartez_deps` — dependency graph between files or modules
  - `qartez_stats` — per-file and per-symbol metrics
  - `qartez_impact` — blast radius and transitive dependents before editing
  - `qartez_cochange` — files that historically change together
  - `qartez_unused` — detect dead exports and unreferenced symbols
  - `qartez_hotspots` — cyclomatic complexity hotspots
  - `qartez_clones` — structural code clone detection via shape hashing
  - `qartez_boundaries` — architecture boundary enforcement
  - `qartez_context` — scope-aware context elision with configurable `token_budget`
  - `qartez_wiki` — auto-generated project documentation
  - `qartez_rename` — AST-aware symbol rename across the codebase
  - `qartez_move` — move a symbol between files with import updates
  - `qartez_rename_file` — rename a file and update all imports
  - `qartez_project` — run project commands (test, build, lint) with auto-detected toolchain
- **5 MCP prompt templates** — `/qartez_review`, `/qartez_architecture`, `/qartez_debug`, `/qartez_onboard`, `/qartez_pre_merge`
- **PageRank-based importance ranking** for files and symbols
- **Blast radius estimation** — transitive impact analysis before modifying code
- **Cyclomatic complexity analysis** — per-function scoring and hotspot detection
- **Monorepo / multi-root support**
- **Automatic file watching** with incremental re-indexing
- **Interactive IDE setup wizard** — auto-detects Claude Code, Cursor, Windsurf, and other MCP-compatible editors
- **Modification guard** — PreToolUse hook that warns before editing high-impact files (PageRank + blast radius thresholds)
- **Per-tool benchmark harness** — measures MCP vs Glob/Grep/Read token and latency savings across multiple languages
