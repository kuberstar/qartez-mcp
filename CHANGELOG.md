# Changelog

## [0.7.2] - 2026-04-17

### Added

- **Pre-built release binaries** - tagged `v*` pushes now trigger a GitHub Actions matrix that builds and attaches archives for macOS (arm64, x86_64), Linux (x86_64/aarch64, gnu and musl), and Windows (x86_64) plus a single `SHA256SUMS` manifest, so the installer no longer has to compile from source on every machine.

### Changed

- **`install.sh` / `install.ps1` bootstrap pre-built binaries first** - the installer detects the host target, downloads the matching archive from the latest GitHub Release, verifies the SHA-256 checksum, and installs atomically. First-run time drops from the old 2-5 minute cargo build to under 10 seconds on supported platforms. `--from-source` / `-FromSource` forces the previous cargo path, and any unsupported target or download failure falls through to cargo build automatically. A SHA-256 mismatch is treated as a hard failure (never falls through to source) so tampered or corrupted downloads cannot silently be masked.
- **`install.ps1` binary list corrected** - the Windows installer now references the `qartez`, `qartez-guard`, `qartez-setup` binaries declared in `Cargo.toml` instead of a non-existent `qartez-mcp.exe`.

## [0.7.1] - 2026-04-17

### Added

- **CodeQL SAST workflow** - weekly CodeQL analysis for Rust and GitHub Actions with `security-and-quality` query pack; SHA-pinned actions, read-only default permissions.
- **cargo-fuzz harness** - `qartez-mcp-fuzz` crate with two fuzz targets (`parse_boundary_config`, `parse_security_config`) plus a weekly `fuzz.yml` workflow that uploads crash artifacts on failure.
- **`osv-scanner.toml` allowlist** - documents the transitive `paste` RUSTSEC-2024-0436 "unmaintained" notice as non-exploitable (compile-time-only proc-macro via optional `tokenizers`).

### Changed

- **notify 7 -> 8, tokenizers 0.21 -> 0.22** - routine dependency upgrades; full test suite passes unchanged.

### Fixed

- **`install.sh` SC2015 anti-pattern** - replaced `cd ... && pwd || true` with an explicit `|| SCRIPT_DIR=""` fallback so a failing `pwd` can no longer mask a failing `cd`.
- **Windows one-liner installer URL** - switched PowerShell docs/comments from `https://qartez.dev/install.ps1` to the working `https://raw.githubusercontent.com/kuberstar/qartez-mcp/main/install.ps1` endpoint.

## [0.7.0] - 2026-04-17

### Added

- **Native Windows support** - PowerShell installer (`install.ps1`), automated installer test (`test-install.ps1`), and end-to-end build/install flow that does not require WSL or Git Bash. Quickstart documents the `iwr | iex` one-liner.
- **Binary-invoked hooks** - `qartez-guard` handles Glob/Grep denials directly and `qartez-setup --session-start` replaces the shell session-start wrapper, eliminating bash as a runtime dependency on Windows.
- **Cross-platform home detection** - `HOME` / `USERPROFILE` / `HOMEDRIVE`+`HOMEPATH` fallbacks land in `qartez-mcp`, `qartez-setup`, and the config loader so user-scoped paths resolve everywhere.
- **Qartez skill expansion** - 9 new reference docs shipped with the Claude skill (`runtime-contract`, `subagent-contract`, `host-matrix`, `confidence-model`, and 5 doctrine guides: explore, debug, review, refactor, premerge) giving the skill full host parity and guard contract coverage.
- **Documentation suite** - new `docs/architecture.md`, `docs/tools.md`, `docs/configuration.md`, and `docs/agent-guide.md` covering project layout, every tool, configuration, and agent integration.

### Changed

- **`binary_available` rewritten in pure Rust** - replaced the `which` shell-out with `PATH` splitting plus Windows `.exe`/`.cmd`/`.bat`/`.com` extension probing, removing a Unix-only dependency.
- **Auto-update is Unix-only** - on Windows the updater prints a manual download link instead of running the `curl | sh` installer path.

### Fixed

- **Windows index keys always forward-slash** - normalized every boundary that writes or resolves index paths (ingest/reingest/delete, TS/JS/Rust/Python/Go/Dart resolvers, walker), resolving 26 Windows-only test failures observed in v0.6.0 CI.
- **MCP tool path input accepts either separator on Windows** - `get_file_by_path` and the file-path filters in `qartez_knowledge`, `qartez_read`, `qartez_security`, `qartez_smells`, and `qartez_test_gaps` now normalize user input so `src\foo.rs` matches the forward-slash DB key without a confusing "File not found".
- **Guard tolerates Windows canonicalization variants** - the hot-file guard now probes `/`, `\`, and `./` prefix variants before fail-open, so the Edit/Write deny decision matches the indexed path on Windows.

### Contributors

- **Zir** ([@Zireael](https://github.com/Zireael)) - native Windows installer, hook portability, CI validation, and qartez-skill guard contract / host parity improvements (#10, #11)
- **josh** ([@ninthhousestudios](https://github.com/ninthhousestudios)) - architecture, tools, configuration, and agent-guide documentation suite (#12)

## [0.6.0] - 2026-04-17

### Changed

- **MSRV raised to 1.88** (from 1.85) - transitive deps (`time-core 0.1.8` via `rusqlite 0.39`, `darling 0.23` via `rmcp-macros`) now require rustc 1.88. The installer auto-updates rustup-managed toolchains; users on older pinned toolchains will see a clear upgrade message.

### Added

- **CI supply-chain hardening** - `cargo-audit` on every dep change, `cargo-deny` license/source allowlist (`deny.toml`), OpenSSF Scorecard workflow, and Dependabot coverage for Cargo + GitHub Actions. All workflows SHA-pinned.

### Fixed

- **Security scanner cfg(test) filter** - switched from line-based brace counting to tree-sitter AST detection, correctly excluding symbols inside inline `#[cfg(test)] mod tests { ... }` blocks even when strings, lifetimes, or raw strings contain braces.
- **SEC001 shell-variable false positives** - hardcoded-secret detection no longer flags `"$VAR"` expansions.
- **SEC007 allowlist** - insecure-URL scanner now exempts `xmlns:` namespace declarations and single-label/internal hostnames (Docker, K8s configs) to cut noise on legitimate LAN/cluster references.
- **`is_test_path` substring matching** - path is normalized before matching so platform path separators do not cause misclassification.
- **`qartez_test_gaps` inline `#[cfg(test)]` coverage** - modules defined inline are now counted as test coverage in both `report` and `suggest` modes; `suggest` previously missed them.
- **`qartez_find` kind filter + regex** - in `regex` mode, `kind` filter is now applied before the result `limit`, so requesting e.g. `kind="struct"` with `limit=100` no longer returns zero matches when the first 100 regex hits are other kinds. User regexes are also capped at a 1 MiB compiled-program size (matching the security-scanner regex cap) to guard against pathological patterns.
- **Setup IDE detection** - `qartez-setup` now finds IDE CLIs installed outside `PATH` (including VS Code extension installs) instead of skipping them silently.
- **Helper correctness** - guarded `replace_whole_word` boundary checks, fixed Rust self-import handling at crate root, tightened retry symmetry in batch judge, deduplicated `git_sha` lookups, and removed unreachable `unwrap`s in whole-word replace.

## [0.5.1] - 2026-04-16

### Fixed

- **Installer auto-upgrades old Rust toolchains** - `install.sh` now reads `rust-version = "1.85"` from `Cargo.toml`, compares against `rustc --version`, and runs `rustup update stable` when the local toolchain is too old. Users without rustup receive a clear upgrade message instead of cryptic feature-gate errors.
- **Setup builds on stable Rust** - replaced the nightly-only `File::try_lock` with the `fs4` crate in `qartez-setup`, so the update-check lock works on stable toolchains.

## [0.5.0] - 2026-04-16

### Added

- **`qartez_smells` tool** - detect code smells (long functions, long parameter lists, feature envy) with configurable thresholds
- **`qartez_test_gaps` tool** - identify public symbols missing test coverage by cross-referencing source and test files
- **`qartez_knowledge` tool** - git-blame-based bus factor and authorship analysis per file and module
- **Standalone CLI** - `qartez` binary now works as a direct CLI tool (map, outline, grep, find, read, deps, stats, impact) in addition to MCP server mode
- **Mermaid diagram output** - `qartez_deps`, `qartez_calls`, and `qartez_hierarchy` support `format: "mermaid"` for visual dependency graphs
- **Risk scoring in `qartez_diff_impact`** - `risk: true` parameter adds weighted risk scores to blast radius results
- **Benchmark suite** - 28 LLM-judge scenarios covering all tools across Rust, TypeScript, Python, Go, and Java fixtures

### Changed

- **Binary renamed** - primary binary is now `qartez` (backward-compat symlink `qartez-mcp` preserved)
- **Monorepo support** - git tools use `Repository::discover` for subdirectory and monorepo compatibility

### Fixed

- **Panic on multi-byte signatures** - `qartez_smells` long-param table no longer slices on byte boundaries; uses `floor_char_boundary` for safe truncation
- **Em dashes removed** - replaced all em dashes and double hyphens with single hyphens across codebase and shell scripts
- **Nightly-only `floor_char_boundary`** - replaced with stable alternative for Rust stable compatibility
- **Mermaid output wrapping** - removed spurious markdown fences from mermaid diagram tool output
- **Interactive help** - `qartez` now shows help when run interactively without a subcommand
- **Duplicate binary target** - removed redundant binary definition in Cargo.toml

## [0.4.0] - 2026-04-16

### Added

- **`qartez_security` tool** - OWASP-style vulnerability scanning with 14 built-in rules (hardcoded secrets, SQL injection, path traversal, insecure HTTP, etc.) and user-extensible patterns via `.qartez/security.toml`
- **`qartez_semantic` tool** - hybrid FTS + vector search for semantic code queries (requires `semantic` feature flag)
- **`qartez_tools` meta-tool** - progressive tool discovery with tier-based enable/disable (core, analysis, refactor)
- **`qartez_trend` tool** - symbol complexity trend over git history with configurable commit depth
- **Haskell, OCaml, R language parsers** - language count increased from 34 to 37
- **Hotspot health score** - normalized 0-10 score combining complexity, coupling, and churn with half-life decay; threshold filtering and sort_by parameter
- **19 IDE/CLI integrations** - added Kiro, Claude Desktop, Copilot CLI, Amazon Q, Amp, Goose, Cline, Roo Code, Warp, Augment, and Google Antigravity
- **Qartez skill for Claude Code** - replaces CLAUDE.md instructions with a reusable skill containing tool reference and workflows
- **CLA with GitHub Action** - contributor license agreement with automated checking
- **Unified setup instructions** - IDE rules files generated for all supported editors

### Changed

- **Token estimator** - `estimate_tokens` now uses char count / 3 (was byte length / 4), producing ~33% higher estimates for ASCII. Tools may truncate output earlier as a result.
- **MCP instructions updated** - tool count corrected from 25 to 27, added `qartez_security` and `qartez_semantic` to analysis tier
- **README corrected** - tool count, IDE count, complexity count, and comparison table updated to match reality

### Fixed

- **Unbounded database growth** - prevented excessive DB size on large codebases (#9)
- **UTF-8 panic in security scanner** - snippet truncation no longer slices on byte boundaries; uses char-safe truncation instead
- **Silenced embedding deletion errors** - `delete_file_data` and `clear_file_content` now propagate `symbol_embeddings` DELETE errors instead of discarding them
- **Regex DoS via security.toml** - user-supplied regex patterns are now compiled with a 1 MiB size limit to prevent pathological backtracking
- **install_goose panics on malformed YAML** - replaced `expect()` with error-returning `.ok_or_else()` in Goose/Continue YAML handling
- **SEC007 false positives** - `http://localhost`, `http://127.*`, and other loopback URLs are now excluded from insecure-HTTP findings
- **Dead code in trend.rs** - removed unused language detection that could reject files `parse_file` handles fine
- **Hierarchy max_depth** - added depth limit to transitive traversal; BFS now exits early when depth exceeded
- **Multi-root path collision** - fixed cross-root imports and workspace detection
- **Semantic tool restored** - fixed parallel merge conflict that broke `qartez_semantic`
- **MCP client compatibility** - `limit` param uses `flexible::u32_opt` for broader client support
- **Setup skip uninstalled IDEs** - no longer errors on IDEs that are not present

### Contributors

- **Matt** ([@corbym](https://github.com/corbym)) - fix for unbounded database growth on large codebases (#9)

## [0.3.0] - 2026-04-15

### Added

- **Dart/Flutter language support** - full resolver with barrel export resolution, receiver-type heuristics, and reference tracking
- **Gemini CLI support** - automated setup with hooks for `gemini` alongside Claude Code
- **`qartez_hierarchy` tool** - query type relationships (subtypes, supertypes) with transitive traversal
- **`qartez_diff_impact` tool** - batch pre-merge blast radius analysis across multiple changed files
- **`.qartezignore` support** - exclude paths from indexing beyond `.gitignore` rules
- **OpenCode plugin** - edit guard and MCP instructions for OpenCode IDE
- **MCP static resources** - `qartez://hotspots` and `qartez://stats` for precomputed data access
- **IDE rules** - MCP instructions for Cursor, Codex, and OpenCode alongside Claude Code
- **Background indexing on startup** - MCP tools load immediately while indexing runs in a background thread

### Changed

- **Server modularized** - split monolithic `mod.rs` into cohesive submodules for maintainability
- **Storage layer deduplicated** - unified `SymbolRow`/`FileRow` deserialization in JOINed queries
- **PageRank warm-start** - incremental re-index reuses prior iteration values for faster convergence
- **DB mutex released earlier** - dropped before FS reads and tree-sitter parsing to reduce lock contention
- **Resolver upgraded** - kind-filter and receiver-type heuristics for more accurate symbol resolution
- **Type-aware resolution** - symbol lookup now considers type context for disambiguation
- **README restructured** - corrected tool count (23), language count (34), and updated navigation

### Fixed

- **Path traversal protection** - `safe_resolve` rejects `../` escape attempts in user-supplied file paths
- **Comma-separated `Vec<String>` params** - MCP tools now correctly parse `"a,b,c"` as a list
- **SQL column aliases** - corrected hierarchy query column names and added integration tests
- **CLAUDE.md snippet location** - writes only to `~/.claude`, skips variant directories
- **Cochange phantom files** - filtered out files no longer in the repo from co-change results
- **Cargo metadata** - removed redundant `license` field, kept `license-file` only

### Contributors

- **josh** ([@ninthhousestudios](https://github.com/ninthhousestudios)) - Dart/Flutter support, resolver improvements, background indexing (#5, #6, #7)
- **Rudolf Troger** ([@DolphRoger](https://github.com/DolphRoger)) - Gemini CLI support with automated setup and hooks (#1)

## [0.2.0] - 2026-04-15

### Added

- **Background auto-update** - `qartez-mcp` checks GitHub for newer releases on startup (24h TTL, cross-process flock) and rebuilds from source via `install.sh` when a new version is available. Opt out with `QARTEZ_NO_AUTO_UPDATE=1`
- **One-liner install** - `curl -sSfL https://qartez.dev/install | sh` downloads and builds from source without cloning the repo
- **Runtime state mirroring** - setup wizard now writes MCP config into Claude Code's `.claude.json` state file so accounts with existing state pick up qartez immediately

### Changed

- **License upgraded to Small Team tier** - free for up to 3 users and <$1M annual revenue (was: individuals only). Added patent grant, explicit eligibility examples, and 30-day grace period
- **Atomic binary install** - `install.sh` uses copy-to-`.new`-then-`mv` to avoid ETXTBSY and corruption during in-place upgrades

### Fixed

- Setup wizard now cleans up `.claude.json` state file on uninstall (previously only cleaned `settings.json`)
- Update cache file only touched on successful GitHub API check, preventing stale cache from masking transient failures

## [0.1.1] - 2026-04-14

### Added

- **Zero-dependency installer** (`install.sh`) - single script that auto-installs Rust, builds, tests, and configures IDEs. Works on macOS and Linux without jq or bash
- **Install portability test suite** - 50 checks covering POSIX compliance, error paths, download safety, and Docker integration

### Changed

- `make deploy` now delegates to `install.sh` instead of inline Makefile logic

### Fixed

- Ad-hoc codesign binaries on macOS to prevent Gatekeeper SIGKILL (exit 137)
- 7 portability bugs in install flow (platform detection, shell compatibility)
- All platforms handled in dependency auto-install (apt, dnf, pacman, apk)
- Clippy warning in Kotlin complexity counter

## [0.1.0] - 2026-04-14

Initial public release.

### Features

- **34 language parsers** via tree-sitter - Rust, Go, Python, TypeScript, JavaScript, Java, C, C++, C#, Ruby, Kotlin, Swift, PHP, Bash, CSS, Lua, Scala, Dart, Elixir, Zig, Nix, SQL, Protobuf, and more
- **DevOps format support** - Dockerfile, Helm charts, HCL/Terraform, YAML (Kubernetes, GitLab CI, GitHub Actions), Makefile, Nginx, TOML, Caddyfile, Systemd, Jenkinsfile/Groovy, Starlark, Jsonnet
- **21 MCP tools:**
 - `qartez_map` - codebase skeleton ranked by PageRank
 - `qartez_find` - symbol lookup by name
 - `qartez_grep` - full-text search across the codebase
 - `qartez_read` - read symbol source with semantic context
 - `qartez_outline` - file structure with all symbols and their signatures
 - `qartez_refs` - find all references to a symbol
 - `qartez_calls` - call hierarchy (callers and callees)
 - `qartez_deps` - dependency graph between files or modules
 - `qartez_stats` - per-file and per-symbol metrics
 - `qartez_impact` - blast radius and transitive dependents before editing
 - `qartez_cochange` - files that historically change together
 - `qartez_unused` - detect dead exports and unreferenced symbols
 - `qartez_hotspots` - cyclomatic complexity hotspots
 - `qartez_clones` - structural code clone detection via shape hashing
 - `qartez_boundaries` - architecture boundary enforcement
 - `qartez_context` - scope-aware context elision with configurable `token_budget`
 - `qartez_wiki` - auto-generated project documentation
 - `qartez_rename` - AST-aware symbol rename across the codebase
 - `qartez_move` - move a symbol between files with import updates
 - `qartez_rename_file` - rename a file and update all imports
 - `qartez_project` - run project commands (test, build, lint) with auto-detected toolchain
- **5 MCP prompt templates** - `/qartez_review`, `/qartez_architecture`, `/qartez_debug`, `/qartez_onboard`, `/qartez_pre_merge`
- **PageRank-based importance ranking** for files and symbols
- **Blast radius estimation** - transitive impact analysis before modifying code
- **Cyclomatic complexity analysis** - per-function scoring and hotspot detection
- **Monorepo / multi-root support**
- **Automatic file watching** with incremental re-indexing
- **Interactive IDE setup wizard** - auto-detects Claude Code, Cursor, Windsurf, and other MCP-compatible editors
- **Modification guard** - PreToolUse hook that warns before editing high-impact files (PageRank + blast radius thresholds)
- **Per-tool benchmark harness** - measures MCP vs Glob/Grep/Read token and latency savings across multiple languages
