# Changelog

## [0.8.4] - 2026-04-21

### Fixed

- **Windows CI tests for `find_server_binary` no longer race the build artifact** - `find_binary` now routes its "sibling of the setup binary" fallback through a new `setup_exe_dir()` helper that consults `QARTEZ_SETUP_EXE_DIR_OVERRIDE` before falling back to `current_exe().parent()`. The two regression tests added in v0.8.3 now set the override to an empty tmp dir, so the `qartez.exe` built into `target/release/deps/` during the same CI job no longer satisfies `find_binary("qartez")` while the test was preparing only `qartez-mcp.exe` under a tmp `HOME`. Production behavior is unchanged - without the override, the fallback still locates siblings of the running `qartez-setup.exe`, which is what the Windows installer relies on.
- **Env-var-sensitive tests are poisoning-safe** - switched six tests in `bin/setup.rs` (`find_server_binary_*`, `has_claude_vscode_extension_*`) from `ENV_LOCK.lock().unwrap()` to the existing `env_lock()` helper that recovers via `poisoned.into_inner()`. A single failing test no longer cascade-panics every subsequent test that serializes on `$HOME` / `$PATH`.

## [0.8.3] - 2026-04-21

### Fixed

- **Windows installation with canonical `qartez` binary name** - `qartez-setup` now locates the MCP server via a new `find_server_binary` helper that tries the canonical `qartez` name first and falls back to the legacy `qartez-mcp` symlink. Windows `install.ps1` only ships `qartez.exe` (hardlinks / symlinks need elevation), so the previous hardcoded `qartez-mcp` lookup blocked IDE configuration on Windows with "qartez-mcp binary not found". Fixes #24.
- **Multi-root embeddings resolve to the right root on file-name collisions** - `resolve_abs_path` now iterates `project_roots` directly instead of keying a `HashMap` by `file_name()`. Two roots sharing a final path component (e.g. `frontend/web` + `backend/web`) used to collapse into a single HashMap entry, so half of a multi-root semantic index resolved to the wrong filesystem path. The first root in order now wins deterministically, matching `helpers::resolve_prefixed_path`.
- **Incremental indexer skips out-of-root paths instead of writing garbage keys** - `delete_single_file` and `try_reingest_changed_file` used to fall through to the absolute path on `strip_prefix` failure, producing a `rel_path` like `workspace1//tmp/foo.rs` that could never be looked up again. Out-of-root paths (usually a symlink or mount-point escape out of the watched tree) are now surfaced as a `tracing::warn!` and skipped.
- **`qartez_rename_file` renames the file last** - the filesystem `rename` now happens after importer rewrites, matching the target-first discipline already used by `qartez_move`. A mid-way write failure used to strand importers pointing at a vanished source path; the source now stays in place on partial failure and the tool is idempotent on retry.
- **`find_parent_mod_file` rejects crate entry points** - renaming `src/lib.rs` or `src/main.rs` no longer scans for a parent `mod` declaration. `Cargo.toml` registers crate roots directly, so a sibling `src/mod.rs` or `src.rs` is never a parent module; without the early return, the caller could rewrite unrelated `mod lib;` / `mod main;` lines in those siblings.
- **`parse_semver` strips SemVer §10 build metadata** - versions like `v1.2.3+build1`, `v1.2.3+linux-gnu`, or `v1.2.3-rc1+build.42` now parse to `(1, 2, 3, ...)` by stripping the `+...` segment before the prerelease split. Without this, `parts[2].parse::<u32>()` would fail on `"3+build1"` and the auto-updater silently refused to roll forward onto any release whose tag carried build metadata.
- **Watcher recovers from a poisoned DB mutex** - `Watcher::reindex` now mirrors the `into_inner()` recovery already used for the ignore-cache lock, so a one-off panic during incremental indexing no longer kills the long-running watcher task for the rest of the session. SQLite rolls the open transaction back when the guard drops, so the `Connection` remains usable.
- **`safe_resolve` strips wrapper syntax from path arguments** - clients (or copied tool traces) that pass values like `` `src/main.rs` ``, `"src/main.rs"`, `'src/main.rs'`, or `[file_path=src/main.rs]` now resolve cleanly. A new `normalize_user_path_arg` shim strips matching quote / backtick pairs and the three recognised bracketed-assignment forms (`[file_path=...]`, `[path=...]`, `[file=...]`) iteratively, while preserving literal filenames like `[notes].md` or `file=notes.md`. Traversal rejection still runs after normalization, so `` `../secret.txt` `` remains blocked.

### Contributors

- **Zir** ([@Zireael](https://github.com/Zireael)) - added the `safe_resolve` compatibility shim that normalizes wrapper syntax around path arguments (quotes, backticks, bracketed assignments), with regression tests covering acceptance, traversal rejection, and literal-filename preservation (#18).

## [0.8.2] - 2026-04-20

### Fixed

- **`qartez_move` preserves intentional blank-line separators** - the post-extraction blank-line collapse now folds a single adjacent pair at the extraction seam only, instead of globally flattening every triple-newline gap in the file. Multi-blank separators between unrelated symbol groups survive the move. Regression test in `tests/tools.rs` covers a `fn a` / `const X` / `fn b` layout with two-blank separators.
- **`qartez_move` writes target before source** - the target file now lands on disk first, so a mid-operation write failure (disk full, read-only filesystem, permission denied) leaves the source intact and the caller can retry. The previous source-first order truncated the source before the target was safely written, converting a transient write error into silent data loss.
- **`qartez_rename_file` rewrites `use crate::<old>` importers for root-level Rust files** - renaming `src/foo.rs` → `src/baz.rs` now also emits a `crate::foo` → `crate::baz` rewrite pair so every `use crate::foo::...;` importer is updated. Before, only the internal `src::foo` stem was emitted (a form that never appears in real Rust code), so crate-relative importers were silently left dangling. The `crate::` prefix keeps the match unambiguous even when the divergent suffix is a bare single segment, so unrelated local identifiers sharing the stem are unaffected.
- **`sanitize_fts_query` quotes misplaced `*` wildcards** - FTS5 only accepts `*` as a trailing prefix marker on an otherwise alphanumeric token (`foo*`). Leading, embedded, or standalone `*` used to pass through verbatim, surfacing as an FTS parse failure at query time. Such inputs now take the quoted-phrase path and become a literal match. Legitimate `foo*` prefix queries are unchanged.
- **File watcher `RenameMode::Both` requires exactly two paths** - `notify` emits `[from, to]` for a real rename, so anything with a different shape is backend noise and now falls through to the existence-check branch instead of being treated as a rename. The old `len >= 2` guard misread 3-plus-entry events as "one source, many destinations", producing spurious index updates.
- **`qartez_cochange` counts deletions and renames** - the per-commit delta walker now collects both the pre- and post-commit paths from each diff delta, so commits that delete or rename a file still contribute signal. The previous path-only-from-`new_file` branch silently dropped these cases, under-counting real co-change for files that were moved or removed.

## [0.8.1] - 2026-04-20

### Fixed

- **Workspace-alias path resolution across indexed-file tools** - `qartez_read`, `qartez_move`, `qartez_rename_file`, and the parse cache (`cached_source`, `file_mtime_ns`) now resolve file paths through `safe_resolve` instead of `project_root.join`. Any DB path prefixed with a workspace alias (e.g. `WS/src/widget.rs`) previously resolved against the primary root and returned "No such file or directory". The `qartez_rename` error messages in the two `cached_source` fallbacks now also surface the `safe_resolve`-derived path so failures point at the actual location.
- **Watcher and walker coverage of dotted config** - `.github/`, `.gitlab-ci.yml`, `.claude/`, and similar dotted config files are now indexed. Switched the walker off `.hidden(true)` and filter out `.git/` and `.qartez/` via `.filter_entry` instead. The watcher mirrors the three-tier match so `Makefile`, `Dockerfile`, and friends trigger a reindex, hot-reloads `.qartezignore` on mtime change, and translates rename events into a remove+create pair.
- **Incremental index preserves cross-file refs** - snapshot and restore cross-file `symbol_refs` so they are not cascaded out when a changed file's old symbols get wiped. Added the missing `/` in the multi-root incremental path-prefix join so the path key matches the full-index formatter.
- **Rename tools stop over-replacing stems and cover all importers** - `qartez_rename_file` and `qartez_move` no longer over-replace bare file stems and now rewrite `use crate::foo::bar;` imports by pairing the full and divergent-suffix stems. `qartez_move` includes every edge-graph importer (glob / parent-module imports were silently skipped) and preserves the trailing newline.
- **`.qartez/workspace.toml` bulk-purge escapes LIKE metacharacters** - `%`, `_`, and `\` in a workspace alias are now escaped before the `LIKE` and an `ESCAPE '\'` clause is added. Prevents an alias like `my_ws` from also purging `myXws/` and friends.
- **Multi-root dedup robust to non-existent paths** - `canonicalize().unwrap_or` silent fallback replaced with a `normalize_for_dedup` helper that falls back to `std::path::absolute` and collapses `.`/`..` components, so two logically-equal roots share a dedup key even when neither exists on disk.
- **`opencode-plugin.ts` shell-injection hardening** - swapped `execSync` for `execFileSync` when calling `sqlite3`. Removes shell interpretation of the db path and of the caller-controlled file path, closing a `$(whoami)`-style injection surface.
- **`release.sh` stops embedding `GITHUB_TOKEN` in the clone URL** - uses a `GIT_ASKPASS` helper so the token is not persisted in `.git/config`, process args, or git stderr.
- **`graph/security` scan resolves aliased roots** - reads aliased roots via `root_aliases` so multi-root workspaces with overrides are actually scanned.
- **ONNX embedding error instead of silent zero-length vector** - `embeddings.rs` now returns an error on an empty output shape rather than silently producing zero-length embeddings that broke downstream similarity. Extracted `hidden_dim_from_shape` into a pure helper with tests for empty shape, zero last axis, negative last axis, and valid shapes.
- **`git/cochange` skips merge commits** - matches `git log --no-merges`; previously the merge-commit diff vs `parent(0)` overcounted branch-merge changes.
- **`benchmark` cache invalidates on SHA pin upgrade** - invalidates when the caller pins a SHA and the stored report has no SHA (older schema); previously the cache was silently reused.
- **`guard.relativize_file_path` handles missing-leaf paths** - canonicalizes the parent for files that do not exist yet so macOS `/tmp` vs `/private/tmp` no longer breaks the prefix check.
- **`has_inline_rust_tests` rejects macro false positives** - requires the `::test]` match to be inside a `#[...::test]` attribute so `vec![mod::test]` and similar stop being flagged as having inline tests.

### Contributors

- **Rudolf Troger** ([@DolphRoger](https://github.com/DolphRoger)) - fixed workspace-alias path resolution across `qartez_read`, `qartez_move`, `qartez_rename_file`, and the parse cache, including a regression test that reads a symbol from a workspace-aliased file end-to-end (#23).

## [0.8.0] - 2026-04-19

### Added

- **`qartez_workspace` tool and `.qartez/workspace.toml`** - declarative multi-root workspace config plus a runtime MCP tool and `qartez workspace` CLI subcommand to add or remove project domains without a server restart. Adding a domain re-runs indexing, PageRank, symbol PageRank, and co-change analysis for the new root; removing a domain bulk-purges its files and symbols from the index. Aliases are validated against SQL LIKE metacharacters (`%`, `_`) so bulk-purge stays scoped to the intended prefix. Tool count grows from 30 to 31; `qartez_workspace` lives in the Meta tier because it mutates on-disk config and the index.
- **SLSA Build L3 provenance on release archives** - every `tar.xz`, `zip`, and `SHA256SUMS` attached to a GitHub Release is now signed with `actions/attest-build-provenance`. Verify with `gh attestation verify <file> -R kuberstar/qartez-mcp`.
- **`cargo-deny` workflow** - advisories, bans, licenses, and sources run on PRs touching `Cargo.{toml,lock}` or `deny.toml`, on pushes to main, and weekly. `deny.toml` pins the v2 schema and ignores `RUSTSEC-2024-0436` (transitive `paste` via `tokenizers`) pending upstream replacement.
- **`harden-runner` audit mode on every Linux job** - `step-security/harden-runner` prepended across all workflows to feed egress telemetry into Scorecard without blocking any network traffic.
- **`cargo doc` lint on CI** - `cargo doc --locked --no-deps --all-features` with `RUSTDOCFLAGS=-D warnings` catches broken intra-doc links on every push.

### Changed

- **Setup pre-release comparison** - `parse_semver` now tracks a stability flag so `is_newer_version` treats a stable release as newer than the matching pre-release. A user on `1.2.3-beta` now sees `1.2.3` as the upgrade target instead of staying pinned forever.
- **`release.sh` / `prerelease.sh` portability** - replaced macOS-only `sed -i ''` with `perl -pi -e` so both scripts run unchanged on Linux CI. Co-author filter uses `grep -iwvE` for word-boundary matches.
- **Legacy `qartez-guard.sh` removed** - the shell hook was replaced by the `qartez-guard` binary in v0.7.0; `qartez-setup` now removes the stale shell script on install.

### Fixed

- **Multi-root file watcher orphaned prefixed rows** - the incremental watcher wrote rows without the per-root path prefix that `full_index_multi` uses. In multi-root mode the first save left the original prefixed row unreferenced. `delete_single_file`, `try_reingest_changed_file`, and the new `incremental_index_with_prefix` now thread the prefix through; `main.rs` derives it via `root_prefix()` when more than one root is configured.
- **`qartez_rename` non-AST preview produced incomplete replacements** - the word-boundary branch pushed one row per hit, each with a `new_line` that replaced only that single site. Two or more occurrences on the same line now produce a single row with all occurrences replaced.
- **`git_sha` used the current working directory** - it now runs from the project root so the reported SHA matches the indexed repo instead of whatever directory the server was spawned in.
- **`renderRadar` and `Dogfood` panicked on empty inputs** - both now guard zero-item cases before rendering.
- **Poisoned-lock panic in `build_overview`** - replaced `.read().unwrap()` with an explicit error path so overview generation surfaces a readable lock error instead of aborting the server.

### Contributors

- **Rudolf Troger** ([@DolphRoger](https://github.com/DolphRoger)) - designed and implemented `workspace.toml` support and the `qartez_workspace` MCP tool / CLI subcommand, including alias validation, domain-scoped bulk purge, and the round-trip integration test (#19).

## [0.7.3] - 2026-04-18

### Added

- **Linux musl prebuilt binaries** - `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` archives are back in the release matrix, so Alpine and other musl-based distros install in under 10 seconds instead of falling through to the cargo build path.

### Changed

- **`git2` compiled without default features** - only local repository operations are used (Repository, BlameOptions, RevWalk, Signature), so the `ssh` and `https` transport features are now disabled. This removes `libssh2-sys` and `openssl-sys` from the dependency tree and unblocks musl cross-compilation on GitHub Actions runners that do not ship OpenSSL.

### Fixed

- **Node.js 20 deprecation warning in release workflow** - upgraded `softprops/action-gh-release` from v2.6.2 to v3.0.0, which runs on the Node 24 Actions runtime. GitHub is forcing Node 24 as the default on 2026-06-02.

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
