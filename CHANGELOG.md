# Changelog

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
