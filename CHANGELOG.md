# Changelog

## [0.9.9] - 2026-04-30

### Fixed

- **`cargo-deny` red on `release: v0.9.8`** - the v0.9.8 push to `main` red-flagged the `cargo-deny` workflow with two errors that nobody saw on the release branch because `deny.yml` only triggered on push to `main`, not on `release/*`. (1) `error[unlicensed]: qartez-dashboard = 0.1.0 is unlicensed` - the `qartez-dashboard` workspace member declared `license-file = "../LICENSE"` for the proprietary `LicenseRef-Qartez-Small-Team`, but `deny.toml` only had a `[[licenses.clarify]]` for `qartez-mcp`; cargo-deny fell back to a low-confidence text match (score=0.35 < threshold 0.93) and rejected the crate. A matching `[[licenses.clarify]]` + `[[licenses.exceptions]]` for `qartez-dashboard` (with `path = "../LICENSE"`) is now in place. (2) `error[wildcard]: found 1 wildcard dependency for crate 'qartez-mcp'` - the workspace dep `qartez-dashboard = { path = "qartez-dashboard" }` had no `version = ` field, which cargo-deny treats as a wildcard, and `[bans] wildcards = "deny"` rejects. The dep now pins `version = "0.1.0"` alongside the path.

### Changed

- **`deny.yml` now gates `release/*` and tag pushes, not just `main`** - the workflow's `on.push.branches` is widened from `[main]` to `[main, 'release/*']` and `tags: ['v*']` is added. Combined with the script changes below, this means a license / wildcard / advisory regression caught by `cargo-deny` now blocks the release at branch-push time, before the tag is cut and before any artefact is built.
- **`scripts/release.sh` and `scripts/release-gate.sh` poll `deny.yml` alongside `ci.yml`** - both scripts previously waited only for the `CI` workflow run on the release / gate branch. The release branch v0.9.8 was tagged because `cargo-deny` was running in parallel and the script never noticed it was red. Both scripts are refactored to a `wait_for_workflow` / `gate_workflow` helper that polls each required workflow in turn; a red `deny.yml` now aborts the release the same way a red `ci.yml` would (release branch is deleted, tag is never created).
- **`make ci` parity target** - new top-level Make target that runs `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo-deny check advisories bans licenses sources`, `cargo build --locked --release`, `cargo test --locked --release --no-fail-fast`, and `cargo doc --locked --no-deps -D warnings` in sequence. Mirrors `.github/workflows/{ci,deny}.yml` so a release prep run can be reproduced locally with one command. Also exposes `make ci-fmt` / `ci-clippy` / `ci-deny` / `ci-build` / `ci-test` / `ci-doc` for selective re-runs.

## [0.9.8] - 2026-04-29

### Added

- **`qartez dashboard` subcommand - free local-only web dashboard** - new `qartez-dashboard` workspace crate ships as part of the qartez binary and is launched via `qartez dashboard {start,stop,status,open}`. The daemon binds an `axum` 0.8 HTTP+WebSocket server to `127.0.0.1:0`, persists the chosen ephemeral port to `~/.qartez/dashboard.port` (atomic temp+rename), and enforces a three-layer CSRF defense: loopback bind + `Origin` allow-list middleware (HTTP and WS upgrade) + 32-byte hex auth token written to `~/.qartez/auth.token` at mode `0600`, exchanged for an HttpOnly + Secure + SameSite=Strict cookie at `/auth?token=...`. Refuses to start as root, writes a PID file at `~/.qartez/dashboard.pid`, and wires `tokio_util::CancellationToken` through `ctrl_c` + `SIGTERM` (Unix) into `axum::serve.with_graceful_shutdown`. Reads `<root>/.qartez/qartez.db` directly via `rusqlite`, so the dashboard works without a running MCP server. Live updates flow over a `tokio::sync::broadcast` channel from a `notify-debouncer-full` watcher (native FS events; `target/`, `node_modules/`, `.git`, `dist`, `build` skipped post-event). The bundled SvelteKit frontend covers the architecture map (M4), map filters and symbol focus (M5), hotspots clusters and subgraph (M6), symbol-level intelligence (M7), symbol cochanges + snapshot diff + canvas perf (M8), polish + settings + reindex + pivot graph (M9), and the health / hotspots / smells / clones / dead-code views (M10). Default port is `7427` with `--port` override and ephemeral fallback when the port is taken.

### Fixed

- **`#31` quick install no longer builds the user's adjacent Rust project** - `install.sh` invoked through `curl ... | sh` reads `$0` as the interpreter name (`sh`), so `dirname` resolved to `.` and `SCRIPT_DIR` silently anchored to the user's CWD. If the user happened to be inside any Rust project, the `LOCAL_REPO` check then mistook that project for the qartez source tree and ran `cargo build` on it. The script now only honours `$0` when it points at a readable script file, and additionally verifies the detected `Cargo.toml`'s `package.name` is `qartez-mcp` before treating `SCRIPT_DIR` as the local source tree. `install.ps1` had the same root cause: `$PSCommandPath` is `$null` under `iwr ... | iex`, and the existing fallback to `(Get-Location).Path` anchored both `Resolve-SourceDirectory` and the top-level `$LocalRepo` check to the user's CWD. Drops the CWD fallback on the PowerShell side and adds the same `qartez-mcp` package-name verification.
- **`#32` `qartez-setup` model.onnx 404s and silent setup-success after download failure** - Jina restructured the upstream Hugging Face repo and moved `model.onnx` under `onnx/`; the hardcoded download URL is updated. Two follow-on issues surfaced once the URL was fixed: `tokenizer.json` downloads were never reached because the loop bailed on the model.onnx 404 first (now reached automatically), and the new ONNX export drops the optional `token_type_ids` input - `EmbeddingModel::load` inspects `session.inputs()` at load time and only feeds `token_type_ids` when the graph actually declares it. Separately, `download_semantic_model` errors were logged but never propagated into the `any_error` flag that drives the final "Setup complete!" line, so users saw a green summary even when assets were missing; the error is now wired in. Verified end-to-end against the real upstream export using a path-dep test crate: detection of the missing input is correct, `encode_one` returns a correctly L2-normalized 768-dim vector, and repeated calls are deterministic.
- **Watcher/dashboard parity around gitignore + `core.excludesfile`** - the MCP file watcher (`watch.rs`) and the dashboard watcher previously diverged from each other and from the indexer's walker on which paths to skip during incremental indexing. Both now combine `.gitignore`, `.git/info/exclude`, `.qartezignore`, and the resolved global ignore file (`core.excludesfile` first, then XDG fallback) into a project-rooted `GitignoreBuilder`, hot-reload on any source mtime change, and hard-skip `.git` / `.qartez`. Without the explicit `core.excludesfile` lookup, `ignore::Gitignore::global()` only saw the XDG file, so patterns like `.megaclaude/` configured via `core.excludesfile` leaked into incremental indexing even though the initial walker scan filtered them out.
- **Atomic writes for `qartez_rename_file` importer + `mod.rs` rewrites** - importer rewrites and parent-`mod.rs` rewrites in `rename_file.rs` previously used `std::fs::write` directly, so a `kill -9` mid-refactor left affected files truncated or empty. Both now route through `refactor_common::write_atomic` (temp + rename), matching the durability discipline `qartez_move`, `qartez_replace_symbol`, and `qartez_insert_*` already use. `write_atomic` itself was hardened: it reads the original file's permissions and reapplies them to the temp file before rename, so `chmod 600` / `0755` / sticky bits survive a refactor write.
- **Dead-code follow-through and SvelteKit convention filter** - 5 verified-dead public exports surfaced by `qartez_unused` removed: `storage::read::clones_get_all_ordered_groups` (superseded by `get_clone_groups`), `storage::read::hierarchy_direct_subtypes` and `hierarchy_direct_supertypes` (callers use `get_subtypes` / `get_supertypes`), `benchmark::sim_runner::run` (callers thread `Options` via `run_with`), and `dashboard::paths::logs_dir` + `LOGS_DIR` (no rotating-log consumers). The `qartez_unused` framework-convention filter was extended to skip SvelteKit route entry-points (`+page.*`, `+page.server.*`, `+layout.*`, `+layout.server.*`, `+server.*`, `+error.*`, `hooks.server.*`, `hooks.client.*`) plus build configs (`svelte.config.*`, `vite.config.*`, `playwright.config.*`); these are loaded by the SvelteKit runtime via filename, so the static reference graph cannot observe the caller. The dashboard `/api/dead-code` endpoint mirrors the same filter via the shared `is_framework_runtime_entry_path` predicate so its output matches `qartez_unused`. Helper renamed from `is_plugin_manifest_*` to a name that describes the broader scope.
- **Dashboard `reindex` no longer drops 19 languages** - the `qartez-dashboard` reindex API previously gated paths through an 18-extension hardcoded filter (`.rs`, `.ts`, `.tsx`, `.js`, `.jsx`, `.py`, `.go`, `.java`, `.cpp`, `.c`, `.h`, `.hpp`, `.swift`, `.kt`, `.rb`, `.php`, `.scala`, `.cs`) and silently dropped `.dart`, `.lua`, `.zig`, `.nix`, `.ex`, `.hs`, `.ml`, `.r`, `.toml`, `.yaml`, `.css`, `.sql`, `.proto`, `.star`, etc. The filter is removed; the downstream `IncrementalIndexer` already applies the 37-language registry.
- **Dashboard `project_health` summary now describes the full project** - critical / medium / low / `file_count` / `avg_health` were computed *after* `files.truncate(cap)`, so the summary numbers described only the displayed page. The aggregate is now computed before truncation, while the displayed list is still capped.

### Changed

- **README quickstart split per OS, drop unpublished `cargo install qartez-mcp` path** - `cargo install qartez-mcp` does not work because the crate is not on crates.io. The misleading section is removed from `README.md`, and the `qartez_semantic` tool's rebuild instructions are rewritten to use `git clone` + `cargo install --path .`. The README quickstart now labels each one-liner with its target OS (macOS / Linux / WSL 2 vs. native Windows PowerShell) so users do not have to guess which command applies.

### Notes

- The new dashboard ships as a separate workspace crate (`qartez-dashboard`) at version `0.1.0`. It is wired into the `qartez` CLI binary via the `qartez dashboard` subcommand and inherits the qartez-mcp release cadence; no separate install step is required.

## [0.9.7] - 2026-04-25

### Added

- **Runtime root management tools** - `qartez_add_root` registers an additional project root at runtime (indexes the directory, refreshes pagerank/co-change, and hot-attaches a file watcher) without requiring a server restart, while `qartez_list_roots` reports every tracked root with its origin (cli / config / runtime), watcher attachment state, file count, and last index timestamp. Watcher attachment was moved off `main.rs` onto a `QartezServer::attach_watcher` method so startup roots and runtime adds share the same code path; the join handles are tracked on the server so a future remove can abort them. Closes #29.
- **Cross-process file lock around write-heavy index phases** - `RepoLock` (in `src/lock.rs`) wraps an OS-level advisory lock at `.qartez/index.lock` so that two qartez processes touching the same index database can no longer race into `SQLITE_BUSY`. Held around `full_index_multi`, PageRank, co-change, and the WAL checkpoint; the watcher uses a 2 s try-acquire and skips with a log when another process is indexing. MCP read-only serving is unaffected because the lock is only on the writer paths. Holders write their PID into the lock file so the timeout error reports `held by PID N`. Closes #28.
- **Workspace fingerprint** - the MCP-server background indexer now stores a hex digest of the workspace inputs (sorted canonical roots + each `.qartezignore` content + `QARTEZ_MAX_FILE_BYTES` + crate version) under `meta.workspace_fingerprint`. On the next start the indexer compares the stored value against a freshly computed one; when they match and the caller did not pass `--reindex`, the indexer skips `full_index_multi`, `compute_pagerank`, `compute_symbol_pagerank`, and `analyze_cochanges`, so `initialize` and `tools/list` return immediately even on multi-GiB on-disk indexes. The watcher still incrementally re-indexes any file that changed since the last run, so freshness is preserved. A binary upgrade always invalidates the fingerprint via `CARGO_PKG_VERSION`. Closes #30.
- **`qartez_maintenance` MCP tool** - operator-driven `.qartez/index.db` upkeep with seven actions: `stats` (default, read-only) reports DB / WAL / SHM sizes, top tables by row count, `auto_vacuum` mode, current fingerprint, and last full-reindex timestamp; `checkpoint` runs `wal_checkpoint(TRUNCATE)`; `optimize_fts` triggers the FTS5 `INSERT INTO <fts>(<fts>) VALUES('optimize')` segment-merge on `symbols_body_fts` and `symbols_fts` (the primary cause of multi-GiB index bloat reported in #30); `vacuum_incremental` runs `PRAGMA incremental_vacuum`; `vacuum` runs a full `VACUUM`; `convert_incremental` runs `PRAGMA auto_vacuum=INCREMENTAL; VACUUM;` once to convert a legacy bloated DB; `purge_stale` drops file rows whose root prefix is no longer in the live workspace; `purge_orphaned` drops rows whose absolute path no longer exists on disk. Lives in the `meta` tier.
- **Startup telemetry** - after `open_db`, qartez logs DB and WAL sizes at INFO and escalates to WARN with a `qartez_maintenance` hint when the DB exceeds 1 GiB or the WAL exceeds 500 MiB. Catches the issue scenario (#30) where users discovered an 8.8 GiB DB only after their MCP client timed out.
- **Compound prepare-change surfaces** - `qartez_context` gained `include_impact` and `include_test_gaps` flags that append per-input blast-radius and test-coverage summaries so a single call covers the prepare-change checklist. New `qartez_understand` compound tool resolves one symbol (with `kind` / `file_path` disambiguation) and bundles definition + calls + refs + co-change in one round-trip; `sections=[...]` opts out of expensive sections and the `token_budget` is split equally across active sections. `qartez_map` gained `with_health=true` which annotates each top-ranked file row with `CC=N` plus a smell tag (`god_function` / `long_params`) using `qartez_health` thresholds; default `false` preserves the existing table shape. Total tool count now 41.

### Fixed

- **114-item audit batch (April 2026)** - comprehensive safety, validation, and UX sweep across the MCP server tools, the storage layer, the guard binary, and 7 new regression test suites (44 files, ~3500 net insertions). Highlights: `qartez_workspace` add now refuses paths inside, equal to, or containing the primary root (mirrors the existing remove side); `purge_stale_roots` was rewritten so an unprefixed primary root's rows are explicitly preserved; `purge_orphaned` action drops rows whose absolute path no longer exists on disk; `convert_incremental` is idempotent (no-op when `auto_vacuum` already INCREMENTAL); `IndexStats` reports per-table derived gaps so callers see when pagerank/body_fts are stale after add/remove. `qartez_clones` rejects `format=mermaid` like the rest of the family; `qartez_project` rejects `timeout=0` on every action; `qartez_hierarchy` validates non-empty `symbol` and clamps `max_depth` at 50; `qartez_smells` rejects `envy_ratio<=0` and non-finite values; `limit=0` is unified on no-cap across `unused`, `hotspots`, `health`, `cochange`, and `context` (`clones` / `trend` keep the explicit reject). `qartez_diff_impact` and `qartez_test_gaps` now share `friendly_git_error` to wrap libgit2 leakage on bogus revspecs. `qartez_find` recommends `regex=true` for FTS5-special punctuation instead of a self-defeating prefix-search hint. `qartez_test_gaps` distinguishes "all covered" from "all filtered out". `qartez_context` filters task FTS hits through `is_testable_source_path` and adds a same-language guard so a Rust seed does not surface JS plugins. `qartez_grep` detects the `NOT` reserved token alongside `AND` / `OR` / `NEAR`; body-FTS zero-hit cross-checks the name index and points at alias-prefixed-path skew. `qartez_unused` page cursor advances per row (kept or filtered) so the next-offset hint and page boundaries stop overlapping. `qartez_cochange` documents `max_commit_size` as a live-walk-only filter and warns when fallback is used. `qartez_calls` caps ambiguous-callee enumeration at 20 with overflow count; depth-clamp warning lives in the response footer. `qartez_hotspots` accepts `limit=0`; `qartez_health` classifies dispatcher functions as `flat_dispatcher` with per-variant recommendations instead of generic Extract Method. `qartez_rename_file` refuses build manifests (`Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`, etc.), rejects missing parent directories, and validates `..` traversal on both `to` and `from`. `qartez_rename` caps disambiguator-miss listings at 20 entries with an overflow count. `qartez_replace_symbol` emits structural-change warnings in preview output. `qartez_refactor_plan` floors god-step rationale at CC=5 so trivial CC<5 entries no longer trip the past-review-budget wording. `qartez_tools` listing footer branches on progressive vs non-progressive mode so `enable[]` / `disable[]` hints are accurate. `qartez_boundaries` writes a commented placeholder on 0-rule suggest. `qartez_wiki` clamps `token_budget` to 1024 with a footer note and warns on zero edges before rendering the misc bucket. `qartez_semantic` validates query before checking feature flag. `qartez_project` marks closest-to-root toolchain as primary on info and pre-checks test filter substring against indexed function symbols to avoid spawning a full cargo build when the filter has no hits. `qartez_diff_impact` co-change source labels promote to parent/basename on basename collisions (`server/mod.rs` / `index/mod.rs` no longer collapse), and the ACK footer differentiates per-file ACKs from the diff-markers/ idempotency marker. `guard.rs` `touch_ack` writes the rel_path on a second line of the ACK file as an audit manifest.
- **Eight audit findings - safety, validation, body FTS** - `qartez_replace_symbol` now rejects comment-only / all-prelude `new_code` that would silently erase a symbol body (the downstream introducer / identifier checks short-circuited to `None` when `first_real_introducer_line` found no real definition, letting `// just a comment` slip past every safety net). `index/full_index_root` no longer calls `rebuild_symbol_bodies` (wholesale `DELETE FROM symbols_body_fts` then reinsert); on `qartez_workspace add` for a secondary root the function wiped primary-root bodies because the file resolution used `project_roots[0]`. The startup body-FTS self-heal trigger relaxed from `body_count == 0` to `body_count.saturating_mul(2) < symbol_count` so an orphaned-row remnant from a prior buggy rebuild no longer gates the heal off. `qartez_read` clamps `context_lines` to 50 with a warning note (9999 used to expand the window into the whole file), rejects `start_line` / `end_line` / `limit` when a symbol is requested (silently dropped before), and dedupes `symbols=[a,a,a]` while preserving first-seen order. `qartez_grep` rejects `regex=true && search_bodies=true` (regex was silently dropped) and only suggests `try regex=true` in the 0-row hint when regex was not already passed. `qartez_hotspots` rejects `threshold > 10` explicitly (the previous `.min(10)` clamp turned `threshold=100` into a no-op). `SoulFindParams` regex schemars description rewritten to drop the contradictory "anchored match semantics: `is_match`" line and document the `(?i)` case-insensitive default with the `(?-i)` opt-out.
- **Two audit risk-zones - lifetime gate bug + allowlist dedup** - `check_trailing_content` (in `replace.rs`) treated Rust lifetimes (`'a`, `'static`, `'_`) as opening char literals, consuming braces / semicolons up to the next `'` and silently passing inputs like `fn foo<'a>(x: &'a str) -> &'a str { x }\ntrailing();` through the trailing-content gate, letting `qartez_replace_symbol` corrupt files by pasting trailing items past the definition boundary. New `is_rust_lifetime_start` helper disambiguates `'a` (lifetime) from `'a'` (char literal) by peeking at byte i+2; a `language` parameter was threaded from the source file at the call site. Other languages keep the original char-literal logic so JS / Python single-quoted strings still pair. Separately, `security.rs::scan` extracted the duplicated SEC001/004/005/007/008 allowlist dispatch into a single `is_match_allowlisted` helper so a future SEC013 with an allowlist is a one-line change in one place; both blocks already had a comment warning about the drift hazard.
- **Storage edge case after runtime add** - after a runtime `qartez_add_root` on a previously single-root project the primary stays in `project_roots` without an alias entry while its on-disk rows still live at the empty prefix from the original indexing pass. `live_root_prefixes` now preserves the unprefixed bucket when a root has no alias, so `qartez_maintenance action=purge_stale` no longer classifies those rows as orphan and silently deletes every primary file living under any subdirectory.
- **Compound surfaces follow-up** - `qartez_context` `include_impact` / `include_test_gaps` were silently swallowed when the input file had no related context (early-return on empty ranked list fired before the new sections rendered); the early-return now only applies when both flags are off. `qartez_understand` concise output was inconsistent because the embedded `qartez_calls` / `qartez_refs` invocations did not receive the outer format selector; format is now forwarded. `include_test_gaps` uses a new shared `coverage_for_source` helper in `tools::test_gaps` so the answer matches what `qartez_test_gaps mode=gaps` would say.

### Changed

- **Compaction deferred off the MCP critical path** - the background indexer now sets `QARTEZ_DEFER_COMPACTION=1` so per-root `wal_checkpoint(TRUNCATE)` calls inside `full_index_root` and `incremental_index_with_prefix` skip; a single deferred checkpoint runs once at the end of the indexing burst. CLI and unit-test paths leave the variable unset and keep the original inline-checkpoint behaviour.
- **Stale-row purge on fingerprint mismatch** - the `purge_stale` action now uses the same prefix-derivation as `full_index_multi` so a user who edits `.qartez/workspace.toml` and removes a root can reclaim that root's index rows without `--reindex`. The existing `purge_orphan_prefixes` pass inside `full_index_multi` is unchanged.

### Notes

- Existing databases without `auto_vacuum=INCREMENTAL` are not auto-converted on startup; running `VACUUM` against an 8.8 GiB index would stall MCP startup for minutes. Users with bloated DBs should run `qartez_maintenance action=convert_incremental` once. The maintenance tool surfaces a hint to that effect in the `stats` output when it detects `auto_vacuum=NONE` on a >1 GiB DB.

## [0.9.6] - 2026-04-24

### Fixed

- **CI no longer red-tags a release when `cdn.pyke.io` is down** - the `semantic` feature pulls `ort-sys`, whose build script downloads prebuilt ONNX Runtime binaries from `cdn.pyke.io`. That CDN returned persistent HTTP 504 on the v0.9.4 and v0.9.5 ubuntu-stable jobs (retrying 3 times with exponential backoff still failed), red-tagging two consecutive releases even though the code under test was sound. The `cargo clippy` and `cargo doc` workflow steps are now split: a default-features run is the release gate (no external build-time downloads), and a `--features semantic,benchmark` run is `continue-on-error: true` so it still surfaces lints when the CDN is up but cannot block a release when the CDN is down. Deterministic lint regressions still fail the required step because they hit the default-features run first.

### Changed

- **`scripts/release.sh` now uses a branch-first CI gate** - prior releases tagged `main` on the public repo and pushed in one shot, so any CI red (transient flake, Windows-specific assertion, CDN outage) stamped a broken release that was already public. The rewrite pushes the release commit to a disposable `release/vX.Y.Z` branch first, polls the CI run until every job completes, and only proceeds to tag `main` + create the GitHub release + clean up the branch once the run reports `conclusion=success`. A red CI deletes the release branch and aborts the script without touching `main` or creating a tag, so there is no public artefact pointing at broken code. The prior direct-push behaviour is removed entirely.

## [0.9.5] - 2026-04-24

### Fixed

- **`qartez_rename_file` preview no longer leaks Windows backslashes in the parent-mod importer path** - on Windows the parent-mod lookup rendered `src/index\mod.rs` (forward slashes from the input combined with an OS-native backslash from `PathBuf::join`), which broke the `rename_file_preview_lists_parent_mod_as_importer` assertion and red-tagged the v0.9.4 release. The preview now normalises `\` to `/` so all callers receive the repo-canonical forward-slash form regardless of platform.
- **CI resilience against transient ONNX-runtime CDN failures** - `ort-sys` fetches prebuilt ONNX Runtime binaries from `cdn.pyke.io` during `--all-features` build scripts. The CDN returned HTTP 504 on the v0.9.4 ubuntu-stable job and red-tagged the release even though the code was sound. A new `scripts/ci-retry.sh` wrapper now retries the `cargo clippy` and `cargo doc` workflow steps up to 3 times with exponential backoff, but ONLY when stderr shows a transient-network signature (5xx HTTP status, DNS failure, connection reset, timeout). Deterministic failures (clippy lints, compile errors) still surface on the first attempt.
- **`scripts/prerelease.sh` gained a Windows path-separator leak lint** - any `to_string_lossy()` call in `src/server/` without a visible `'\\' → '/'` normalisation in the same file is now surfaced as a warning so the v0.9.4 class of bug is caught locally before tagging.

## [0.9.4] - 2026-04-24

### Fixed

- **P0 workspace data-loss guard** - `qartez_workspace` now rejects an `add` whose alias prefix-collides with a primary root subdirectory (the collision used to slip through and `remove` would later trigger `delete_files_by_prefix` on a matching path, wiping the entire index). The `remove` path also refuses primary-root aliases that leaked in from an older layout. Both sides share a unified error wording across add-miss, remove-miss, and add-dup.
- **Trait-impl awareness across `qartez_rename`, `qartez_refs`, `qartez_move`, and `qartez_safe_delete`** - a rename of a trait with N `impl Trait for X` blocks now rewrites every impl site, `qartez_refs` on a trait surfaces every implementor file (verified on `LanguageSupport`, all 37 impls), `qartez_move` includes every implementor in the importer rewrite set, and `qartez_safe_delete` enumerates implementors in the blast-radius preview. The shared path runs through `get_subtypes` + `type_hierarchy` so every refactor tool sees the same set.
- **`qartez_calls` owner_type scoping no longer over-counts hub functions** - caller-scan filtering by `owner_type` stops `QartezServer::new` from sweeping up every `HashMap::new`, `Vec::new`, and sibling-type `new` in the codebase; the over-count was roughly 6x (1387 vs. 223). Unqualified self/Self call sites now use the caller impl's owner_type to resolve, so `self.safe_resolve` no longer reports "ambiguous (7 candidates)". `direction=callees` drops non-code seed candidates (bash `main` from setup scripts) and a stdlib-stub denylist, surfaces a depth-clamp header warning instead of a trailing note, and adds a same-language guard on deep-callee resolution.
- **Mutation safety guards on `qartez_replace_symbol`** - refuses struct fields, enum variants, parameters, and locals (those are not standalone definitions and replacing their line range corrupts the parent container); rejects signature, visibility, and symbol-kind changes in `apply` mode via a tree-sitter-free signature-shape check (preview still warns); validates trailing content past the end-of-symbol (`struct Foo;\npub fn bar() {}` and similar), verifies `new_code` redefines the same identifier, strips leading BOM, and accepts stacked attribute + doc-comment preludes.
- **Mutation safety guards on `qartez_move`, `qartez_rename`, `qartez_rename_file`** - `qartez_move` refuses `to_file == from_file`, missing parent dirs, a builtin-method-name target (`new`/`default`/`clone`/`len`/...), intra-file self-references in apply mode, and destination same-name collisions across kinds; `qartez_rename` rejects the bare `_` placeholder, accepts `r#fn` raw identifiers, rejects empty `old_name`, distinguishes "symbol does not exist" from "filter excluded every candidate", caps preview output at 48 KB with a truncation footer, and explains why `file_path` does not override the builtin-method refusal; `qartez_rename_file` refuses crate-root targets (`lib.rs`, `main.rs`, `src/bin/*.rs`), `..` path components, trailing slashes, absolute paths, non-`.rs` destinations, `mod.rs` in either direction, `from == to`, and existing target files; it still auto-creates a missing parent directory on apply and reports parent `mod.rs` as an importer in preview.
- **`qartez_refs` importer dedup + trait implementations** - importer files were duplicated up to ~50x per hub symbol in detailed mode; the concise path now collapses to one row per file while keeping distinct call sites pinnable. `qartez_refs` on a trait surfaces all impl files via `type_hierarchy`, dedupes call sites by resolved `symbol_id` instead of name, and emits a global candidate-count footer.
- **Pagination + count accuracy** - `qartez_clones` no longer jumps `next_offset` past every filtered raw row (it tracks the raw offset of the last kept group so forward paging does not silently drop data), and the "no clones" message now names the `min_lines` filter when that is the cause. `qartez_unused` over-samples like clones so `showing=limit` holds even when plugin entries occupy the page, and reports `plugin_entries_skipped` in the footer rewritten as "K plugin-manifest entries hidden - they're intentional".
- **`qartez_safe_delete` safety guard uses symbol-level refs only** - the file-level `use`-edge signal was surfacing zero-caller helpers in `mod.rs` as "7 files reference this" because the signal was module-level, not symbol-level; the guard now consults `symbol_refs` exclusively. Preview splits the count into external importers and same-file references with per-line locations. The "call again with `apply=true`" hint is clearer when `force=true` is already set, and "this symbol" replaces "this file" for symbol deletion.
- **Schema-vs-runtime alignment exposes what the runtime actually accepts** - `range(min = 1)` added to `clones.min_lines`, `clones.limit`, `smells.*`, `health.*`, `refactor_plan.*`, `cochange.max_commit_size`, and `project.timeout` so the JSON Schema advertises the runtime contract; `extend("enum")` added to `security.severity`, `test_gaps.mode`, `map.by`, and `hierarchy.direction` so callers see valid values at tool-listing time instead of try-and-fail.
- **Unified validation across the tool surface** - `limit=0` now has a single canonical meaning per tool: `qartez_grep`, `qartez_refactor_plan`, and regex-mode `qartez_find` treat it as no-cap, while `qartez_unused`, `qartez_clones`, and `qartez_trend` reject it outright with a shared "limit must be > 0 (use a positive integer; there is no 'no-cap' mode)" message. Callers previously got three different behaviours across those tools; they now get a consistent response within each bucket and the schema documents the contract up front. `threshold=0` on `qartez_hotspots` is rejected as a hard error (the 0-10 health formula can never reach 0, so the old "no matches" response was misleading); `format=mermaid` is rejected on every tool except `qartez_deps`, `qartez_calls`, and `qartez_hierarchy`; `max_commit_size=0` on `qartez_cochange` is rejected explicitly; clamp notifications fire deterministically on `qartez_calls` depth, `qartez_trend` limit, and `qartez_refactor_plan` limit; `qartez_health` rejects `max_health` outside `[0.0, 10.0]` (including non-finite values) with a single canonical "must be in range 0..=10" message so caller typos like `999` instead of `9` surface at the call site instead of being silently clamped into the identity-9.9 branch.
- **Query-tool input hygiene** - `qartez_read` rejects `start_line=0` with a 1-based hint, floors `max_bytes` to 256, sanitizes `NotFound` OS errors so absolute paths no longer leak, rejects `symbol_name+symbols` combinations, validates `start<=end` before other shape checks, and caps implicit ambiguous reads at 4 files to match the refactor-tool disambiguation policy. `qartez_grep` trims the query, rejects `token_budget<256`, warns on FTS5 reserved tokens, backtick-wraps the query echo, notes body-FTS zero-result, anchors plain-name prefix to the name column, rejects bare `*` with a hint at `query=Config*` or `regex=true`, and wires the `kind` filter end-to-end. `qartez_find` trims the name, defaults to case-insensitive regex, wraps parse errors cleanly, treats `limit=0` as no-cap in regex mode, and surfaces a Levenshtein "did you mean" on exact-name miss. `qartez_map` hard-rejects unknown `by` axis values, hoists `boost_files` warnings as a markdown blockquote, embeds a concrete `token_budget` suggestion in the truncation hint, coerces `top_n=0` with a note, and clamps `token_budget` with a warning instead of rejecting outright.
- **`qartez_security` scope and regex tightening** - SEC001 skips env-variable indirections (`$VAR`, `${VAR}`, `process.env.X`, `os.environ['X']`); SEC004 skips `Command::new("LIT")` when the argument is a string literal with no `format!` / `String::from` / `to_string()` interpolation, and the 512-byte interpolation tail terminates at the first `;` so static `git rev-parse` / `claude -p` invocations no longer inherit a `.to_string()` from the following statement; SEC005 treats `include_str!` / `include_bytes!` / `concat!` as compile-time constants and treats `.starts_with("../")` / `.contains("../")` / `.ends_with("../")` as path-traversal detection rather than traversal itself; SEC008 ignores the word "unsafe" inside string literals and doc comments; unknown `category` values are rejected like unknown severity; the surface reports rule count and active filters on no-finding; case-insensitive severity matching. An explicit-missing `config_path` is a hard error instead of silently falling through to defaults.
- **`qartez_test_gaps` testable-source predicate** - CHANGELOG.md, Cargo.lock, README.md, install.ps1, SKILL.md, and other non-source artefacts are no longer flagged as missing tests; `.sh`, `.toml`, and `.md` files are dropped via a shared `is_testable_source_path`. Map-mode path resolution respects the caller's relative path instead of stripping the root prefix, `include_symbols` uses the intersection of file-defined and test-referenced symbols, and `mode=map` with no mapped tests emits an explicit "no effect" notice. FTS seed filtering also prevents `.yml` / `.toml` / `.md` from surfacing as seeds in `qartez_context`.
- **Deterministic errors, wrappings, and empty-result reasons** - `qartez_diff_impact` wraps raw libgit2 errors instead of leaking them, explains `base=HEAD` self-compares, rejects reversed ranges (`HEAD..HEAD~1`) with a forward-form hint instead of silently returning the symmetric delta, adds an opt-in `ack` marker (idempotent across same/different base revspecs), and a Risk/Health polarity legend in the table header; tests/ paths are excluded from the "needs tests" list. `qartez_cochange` splits "no data" into "no git history" vs. "has commits but no pairs"; `qartez_trend` distinguishes "file not found" / "too few commits" / "unmeasurable", and emits the clamp message on the empty-result path. `qartez_hotspots` surfaces a truthful `threshold=0` rejection instead of "Re-index with git history", and `qartez_outline` on `lib.rs` with only `mod`/`use` declares the real state instead of "may not be indexed yet" and falls back to a `## Modules (N)` header when indexed symbols are empty but `pub mod X;` declarations exist.
- **UX polish across the tool surface** - `qartez_tools` rejects enable/disable overlap, partial-applies on valid+unknown mixes, mode-banners core as always-on, and reports `disable(core) / disable(qartez_tools)` as ignored instead of silent "No changes". `qartez_workspace` rejects alias-reuse on different paths, canonicalises the path, defers TOML write until success, and is idempotent on same-alias-same-path. `qartez_boundaries` resolves relative `config_path` correctly, accepts absolute `write_to`, and errors on `auto_cluster=false+clusters` inconsistency. `qartez_project` defaults `action=run` to `build`, validates `filter` against shell metacharacters, and picks the first detected toolchain that actually defines the requested command. `qartez_refactor_plan` treats `limit=0` as no-cap distinct from the 50-item informational cap. `qartez_health` reorders filters so `max_health` applies before the severity-group limit. `qartez_knowledge` renders <1% for non-zero contributions, paginates the author-miss hint, and clarifies scanned vs. total in the header. `qartez_hierarchy` notes `max_depth=1` scope when transitive produced only direct impls, and distinguishes missing symbol from zero impls. `qartez_semantic` docstring and not-built error describe both prerequisites (feature flag AND model download). `qartez_smells` accepts known kinds with a warning on unknown ones instead of rejecting the whole call, emits zero-count category markers, harmonises trailing-comma and duplicate-kind validation, and lists only the categories the caller asked about in the header. `qartez_wiki` sandboxes absolute `write_to` to project root / `$HOME` / `TMPDIR` (including macOS `/var/folders`), rejects `resolution` outside `(0, 10]`, includes `min_cluster_size` in the cache key, floors `token_budget` at 1024, and injects modularity into the on-disk header. `qartez_insert_before_symbol` / `qartez_insert_after_symbol` reject empty and whitespace-only `new_code`, and emit a best-effort Rust-item-shape warning when the target is `.rs` and `new_code` lacks a known introducer. `qartez_refs` suppresses "imports via '(unspecified)'" noise lines. A unified "No symbol found with name 'X'" wording is shared across `qartez_calls`, `qartez_smells`, and `qartez_refactor_plan`.
- **Regression coverage** - 4 new `fp_regression_*` test files pin the observable contracts across refactor safety, query tools, analysis tools, and validation/UX (53 new edge-case tests on top of the fix pass). `fp_regression_audit_2026_04_24.rs` covers 12 critical contracts (`start_line=0` rejection, reversed diff range, structural-change refusal, builtin-name move guard, etc.). Full suite: ~1890 tests, zero failures; `cargo clippy --all-targets --all-features -D warnings` and `cargo fmt --check` clean.

## [0.9.3] - 2026-04-23

### Fixed

- **`selftest_wiki_write_to_absolute_missing_parent_is_rejected` is cross-platform** - the regression test baked in the Unix-style literal `/__nonexistent_parent_9823742/wiki.md`, but `Path::is_absolute()` on Windows requires a drive letter, so the path fell through to the relative branch of `resolve_write_target` and surfaced a different error. The Windows-only CI job for v0.9.2 went red while macOS and Linux stayed green. The test now builds the path from `TempDir::path()` (absolute on every platform) joined with a non-existent subdirectory, so the "parent missing" invariant holds without any platform-specific literal.

## [0.9.2] - 2026-04-23

### Fixed

- **Intra-file references now surface end-to-end across `qartez_refs`, `qartez_calls`, `qartez_safe_delete`, and `qartez_unused`** - three separate layers were hiding the intra-file refs that the 2026-04-23 extractor fix emitted: `storage/read.rs::get_symbol_references` returned only file-level importers without `from_symbol_id`, so same-file refs were indistinguishable from recursive self-refs downstream; `qartez_refs` blanket-filtered every same-file importer; `qartez_safe_delete` used the same path-based filter, so intra-file-used helpers still read "Safe to delete". `qartez_refs` now filters only true self-references (`from_symbol_id == sym.id`), so a `pub(super)` helper reached through a sibling in the same module surfaces in Direct references. `qartez_safe_delete` counts sibling-symbol refs as legitimate importers. `qartez_calls direction=callers` augments its AST `call_expression` scan with a `symbol_refs` reverse lookup so callback-style usages (`.map(helper)`) join syntactic calls, deduplicated by `(file, line)`. New `tests/fp_regression_2026_04_23_full.rs` exercises extractor → resolver → symbol_refs → tool handler in one flow.
- **`qartez_calls` stdlib-method fan-out no longer leaks across unrelated crates** - `direction=callees` now tags `field_expression` callees as `via_method_syntax` and `render_callee_row` drops cross-file unrelated candidates for those, so `.filter()` inside one crate no longer binds to a random free function named `filter` in an unrelated bin. The resolver single-candidate shortcut also rejects the lone candidate for method-syntax calls that lack a qualifier / receiver hint AND live in an unrelated file, closing the last hole behind the stdlib-method FP class.
- **`qartez_refs` deduplicates call sites by resolved symbol, not name** - overloaded names like `run`, `build`, `parse` used to surface 5-way fan-out because each unresolved candidate contributed a row. Dedup now keys on the resolved `symbol_id`.
- **Refactor disambiguation across `qartez_rename`, `qartez_move`, `qartez_rename_file`, `qartez_safe_delete`, `qartez_replace_symbol`, and the insert tools** - `qartez_rename` gained `kind`, `file_path`, and `allow_collision` parameters; it refuses on ambiguous defs, detects identity no-ops, surfaces collisions, and only applies the AST-keyed word-boundary fallback when `file_path` is pinned. `qartez_move` importer count unions `get_edges_to` with `get_symbol_references_filtered` so it matches `qartez_refs`. `qartez_rename_file` scans parent `mod.rs` / `lib.rs` for `mod <stem>;` declarations and rewrites them, and refuses the `mod.rs` basename in either direction. `qartez_safe_delete` blast radius now uses the full `get_symbol_references` path and preserves the DB-stored symbol kind. `qartez_replace_symbol` rejects body-only `new_code` via a signature-shape check. `qartez_insert_before_symbol` / `qartez_insert_after_symbol` previews surface `<newline-inserted>` markers plus byte-delta.
- **Stats, project, clones, and hotspots accuracy** - `qartez_stats::get_language_stats` SQL was multiplying LOC by symbol_count across a JOIN; fixed, and JavaScript is now classified correctly. `qartez_project` parses real Makefile targets and no longer advertises `make test / build / lint` when those targets are absent. `qartez_clones` loop-fetches oversampled rows so a small `limit` still returns production clones, and rejects `min_lines=0`. `qartez_hotspots` threshold comparison uses `<=` to match the documented contract.
- **Scope and side-effect fixes** - `qartez_test_gaps` `gaps` mode respects `file_path`, and `map` mode `include_symbols` uses the intersection of symbols defined in the file and those referenced by test files. `qartez_security` `cfg(test)` suppression is scoped to the block, not the whole file, so standalone `#[cfg(test)] fn` sections are treated like `#[cfg(test)] mod`. `qartez_trend` applies the `symbol_name` filter before the limit and enforces a `token_budget` cap. `qartez_diff_impact` adds an opt-in `ack` param (read-only by default) and returns deterministic cochange omissions.
- **Structure tools** - `qartez_boundaries` `auto_cluster=true` triggers on-demand clustering, `write_to` without `suggest` returns a clear error, absolute paths are supported, and `suggest` unions deny patterns per `from` while skipping self-targeting denies. `qartez_hierarchy` `max_depth=0` short-circuits. `qartez_wiki` force-recomputes the cache when `resolution` or `min_cluster_size` changes, and defaults `token_budget` to 8000.
- **Navigation and search** - `qartez_map` `top_n=0` or `all_files=true` now returns every file and honours `token_budget` with a truncation marker; `boost_files` is validated; `by=symbols` is deduplicated. `qartez_find` gained kind aliases (`fn` / `function` / `method`, `class` / `struct`, `trait` / `interface`) and rejects empty queries. `qartez_grep` `search_bodies` snippets show line-level matches and default `limit=200` with `token_budget` as the governor. `qartez_read` reports the actual shown range via `max_bytes`, warns on ambiguous symbols, errors on binary files, and gained a slice mode. `qartez_outline` is stable source-ordered with `offset=N` skipping exactly N non-field symbols. `qartez_impact` concise mode emits counts only and wires `include_tests` through the blast radius. `qartez_test_gaps` augments the import-map with a `call_tool_by_name("X")` literal scan.
- **Validation consistency** - `limit=0` now uniformly means "no cap" across `qartez_cochange`, `qartez_context`, `qartez_knowledge`, `qartez_health`, `qartez_smells`, `qartez_semantic`, and `qartez_unused`. Categorical params reject unknown values with a `valid: [...]` hint. `format=mermaid` is rejected with a clear error on the 16 tools that do not implement it; `qartez_deps`, `qartez_calls`, and `qartez_hierarchy` keep working.
- **Smells `feature_envy` no longer flags trait-dispatch fan-out** - `ParserPool::parse_file` calls three different trait methods (`tree_sitter_language`, `extract`, `language_name`) on `Box<dyn LanguageSupport>`, so each of the 37 `*Support` types used to surface with `ratio=3.0`. The analyzer now (a) aggregates every call that targets a trait-level method name per type and suppresses when trait calls cover the full `ext_count`, and (b) adds a caller-level `is_pure_trait_fanout` gate that drops an envy breakdown entirely when at least 5 distinct envied types each route 80%+ of their calls through trait methods. The analyzer also suppresses the service-handler pattern (`QartezServer` / `Controller` / `Handler` on DTO params).
- **Smells `god_function` dispatcher recognition** - `build_tool_call` (22 arms, CC=48) was mis-classified because CC exceeded `arms + CC_SLACK=5`. Real dispatchers often carry `if let Some(x) = ...` branching inside arms that inflates CC linearly. A second "dominant-arm" classification path now accepts `arms >= 12` AND `arms >= CC * 0.4`; flat-match dispatchers get a dedicated kind flag and a targeted recommendation instead of the generic "Extract Method". Deeply nested god-functions with a small outer match still fail both paths.
- **`qartez_unused` skips plugin / extension entry-point files** - `scripts/`, `plugins/`, `extensions/`, and `*-plugin.*` paths are now excluded, while real dead-export detection on the rest of the tree is preserved.
- **`qartez_clones` labels trait-boilerplate groups as default-method-impl candidates** - cross-impl identical bodies sharing the same method name are now recommended for promotion to a trait default method rather than generic deduplication.
- **`qartez_security` skips assert-defense patterns in tests** - `let r = target(payload); assert!(r.is_err())` blocks inside test bodies are no longer flagged when `include_tests=true`.
- **`qartez_test_gaps` filters to testable source languages** - `.sh`, `.toml`, `.md`, and generic scripts are dropped, and crate-rooted `use <crate>::<module>;` imports are resolved via FTS body lookup so integration tests that exercise `bin/` and `server/tools/` through `QartezServer::call_tool_by_name` are no longer reported as uncovered.
- **UX polish across tool output** - `qartez_boundaries` suggest unions deny patterns per `from` and skips self-targeting denies. `qartez_refs` suppresses "imports via '(unspecified)'" noise lines. `qartez_hotspots` uses a compact header when the result has 3 or fewer rows. `qartez_read` returns a single-symbol error without bracket noise. `qartez_safe_delete` says "this symbol" instead of "this file" for symbol deletion. `qartez_map top_n` documentation now includes the `0` / `all_files` hint. `qartez_wiki` footer surfaces an explicit `token_budget=` suggestion. `qartez_cochange` distinguishes missing-on-disk vs. not-indexed vs. no-shared-commits. `qartez_context` returns a validation error on unindexed paths instead of an isolated stub. `qartez_knowledge` returns the top-5 author roster on an unknown author. `qartez_workspace` returns "already registered" on duplicate add instead of silent no-op. `qartez_health` rejects negative `max_health` and clamps values above 10. A unified "File 'X' not found in index" error is shared across `qartez_stats`, `qartez_context`, `qartez_cochange`, `qartez_diff_impact`, and `qartez_outline`.

### Contributors

- **Dolph Prefect** ([@dolphprefect](https://github.com/dolphprefect)) - taught `qartez-setup` to clear existing Claude and Gemini hook entries before adding fresh ones, so re-running the installer no longer accumulates duplicate entries pointing at stale paths (#27).
- **Zir** ([@Zireael](https://github.com/Zireael)) - fixed the Windows `install.ps1` one-liner URL references in the README so the PowerShell install command actually resolves, and tidied the `qartez-mcp` → `qartez` branding across user-facing strings (#14).

## [0.9.1] - 2026-04-22

### Added

- **`qartez clones` and `qartez security` CLI subcommands gained filter flags** - both subcommands were previously bare (`qartez clones`, `qartez security`) with the JSON args built as `{}`, so CLI users could not reach the tool-level paging, severity, or category filters that the MCP schema already exposed. `qartez clones` now accepts `--min-lines`, `--limit`, `--offset`, `--include-tests`; `qartez security` accepts `--severity`, `--category`, `--file`, `--include-tests`, `--limit`, `--offset`, `--config-path`. Bare invocations still work - every flag is optional and omitted flags are left out of the JSON payload, so server-side defaults continue to apply.
- **`include_tests` parameter on `qartez_clones`** - parallel parser-fixture tests (21+ near-identical `test_module` / `test_simple_function` functions in `src/index/languages/*.rs`) are AST-shape-identical by design and were dominating the top clone groups without being refactorable. Test files and inline `#[cfg(test)] mod tests {}` blocks are now excluded by default; pass `include_tests=true` to restore the old behaviour. The `#[cfg(test)]` detector is shared with `qartez_security` via the new `graph::security::find_cfg_test_blocks` helper and memoised per file per call so parser cost stays linear in distinct files, not in clone-group members.
- **Shared `test_paths` module** - `is_test_path` now lives in `crate::test_paths` and is imported by both `graph/security.rs` and `server/tools/clones.rs` so the two analyzers agree on what counts as a test file. The previous `graph/security.rs`-local copy missed `quality_tests.rs` (no matching substring), so SEC005/SEC008 fired inside inline `#[cfg(test)] mod quality_tests` even though the containing file is test-only.

### Changed

- **Default `min_lines` for `qartez_clones` raised from 5 to 8** - short per-language parser-dispatch boilerplate (`fn parse_X(source) { parser.set_language(X); ... }`) cannot collapse into a single generic helper without a typeid map and was dominating the top groups on every run. The new default surfaces real refactor candidates instead of dispatch tables. Callers who still want the aggressive cutoff pass `min_lines=5` explicitly (the schema description documents this).
- **`compute_shape_hash` accepts a `kind` argument** - data declarations (`const`, `variable`, `local`, `data`, `resource`, `output`, `module`, `provider`, `service`, `network`, `volume`, `task`, `job`, `workflow`, `stage`, `target`) now keep their literal bodies in the hash via a new `normalize_source_preserve_literals` path, so `const CREATE_TABLE_A: &str = "CREATE TABLE a ..."` and `const CREATE_TABLE_B: &str = "CREATE TABLE b ..."` hash differently. Function/method normalization is unchanged. Before, every `const NAME: &str = "..."` collapsed to the same hash and showed up as a single massive clone group. Pre-1.0 signature change to a `pub` function that has no external callers.

### Fixed

- **`qartez_security` finding `line_start` and `snippet` pointed at different lines** - body-regex rules previously reported `line_start = sym.line_start` (the enclosing symbol's first line) while the `snippet` was pulled from the first matching line inside the body, so the table row and the code excerpt disagreed. The scanner now resolves one match position through the rule-specific allowlist (`SEC001` env-indirection, `SEC004` static-command, `SEC005`/`SEC007`/`SEC008` benign-context) and uses it for both `line_start`/`line_end` and `snippet`. Also fixes a second bug in the same loop: a function with an allowlisted `Command::new("git")` followed by a real `Command::new(user_input)` used to surface the allowlisted first line; the scanner now iterates `find_iter` and picks the first non-allowlisted match.
- **SEC004 interpolation tail no longer spans into the next statement** - the 512-byte lookahead after `Command::new(...).output()?;` used to scoop up unrelated `.to_string()` or `String::from_utf8_lossy(...)` calls on the NEXT line and flag static `git rev-parse` / `claude -p` invocations as command injection. The tail now terminates at the first `;` (512-byte safety cap retained). Also tightens the `subprocess` regex to `\bsubprocess[.(]` so Rust identifiers like `run_judge_subprocess` no longer self-match.
- **SEC005 no longer fires on compile-time embeds and path-shape checks** - `include_str!` / `include_bytes!` / `concat!` are compile-time constants, and `.starts_with("../")` / `.contains("../")` / `.ends_with("../")` detect path traversal rather than perform it. The new `is_sec005_benign` helper skips both. Eliminated 13 self-scan FPs.
- **SEC008 no longer fires on the word "unsafe" inside string literals** - the `\bunsafe\b` regex matched `.expect("FTS-unsafe query")` error messages and `description = "... 'unsafe' ..."` tool docs. The new `is_sec008_benign` helper tracks string-literal state per line and skips matches inside `"..."` or after `//`.
- **`qartez_refs` / `qartez_unused` no longer report zero users for language-support structs** - the Rust extractor now records value-position `scoped_identifier` nodes (e.g. `&bash::BashSupport` in the 37-way `ALL_LANGUAGES` dispatch table) as `ReferenceKind::Use` edges. `const_item` / `static_item` also set `new_enclosing` so initializer refs attribute to the const instead of being dropped. Before, every language-support struct appeared to have no users even though it was referenced by the main dispatch table.
- **Macro-router and format-body walkers no longer emit false-positive calls and uses** - inside `vec![...]`, `assert_eq!(...)`, `format!(...)`, and similar macros: bracket-token trees (`vec![a, b]`) are no longer treated as calls, macro argument identifiers (`assert_eq!(x, y)`) no longer surface as `Call`, and `Mod::build()` inside `format!` emits `Call(build)` only (the `TypeRef(Mod)` is suppressed in `CallsOnly` mode to avoid `{:?}` noise). Deep method chains (`a.b.c.d.e()`) emit `Call` only on the final segment. 17 new unit tests pin the expected behaviour so the walkers cannot regress silently.
- **Method-body refs were parsed from the signature only** - `qartez_refs` missed references inside method bodies whose enclosing node was a `method_item` because the parser walked the signature sub-node instead of the full method. The walker now processes the full method node, matching free-function behaviour. End-to-end regression tests added.
- **Smells `feature_envy` no longer miscounts associated-function calls** - `detect_feature_envy` previously treated every `from_symbol_id → to_symbol_id` ref as a method call for envy purposes, so `Step::new(...)` inside a method on `Task` scored as envy toward `Step` even though no instance of `Step` was used. The query now filters on `ref_kind = 'call'` and excludes associated-function calls via the target signature.

### Removed

- **Dead `embed_symbols_incremental` and `SymbolRefRow`** - pruned an unused 91-line incremental-embeddings helper in `storage/write.rs` (the watcher calls `rebuild_embeddings` instead) and an unused `SymbolRefRow` DTO in `storage/models.rs` surfaced by the harder `qartez_unused` run on this repo.

## [0.9.0] - 2026-04-21

### Added

- **`qartez_health`** - prioritized, actionable health report for the whole repo. Cross-references `qartez_hotspots` with `qartez_smells`: files that score badly in both surface as **Critical** with concrete refactor techniques (Extract Method, Introduce Parameter Object), hotspot-only as **High**, smell-only as **Medium**. Pure aggregator - reuses the same 0-10 health formula as `qartez_hotspots`. Added to `TIER_ANALYSIS`.
- **`qartez_refactor_plan`** - ordered, safety-annotated refactor plan for a single file. Each step names a concrete technique, categorizes the expected CC impact (High/Medium/Low) with a **range** (e.g. "-5 to -12 CC"), and folds in safety signals from existing tools: whether tests cover the file, caller count from `symbol_refs`, and `is_exported`. CC impact is conservative by design - ranges, not fake precision. Added to `TIER_ANALYSIS`.
- **`qartez_replace_symbol`** - replace a symbol's whole line range (`line_start..line_end`) with new source. Caller supplies the full replacement including the signature; the tool performs an atomic line-range rewrite via tmp-file + rename. Preview by default; `apply=true` executes. `kind` / `file_path` disambiguate when the name is shared. Added to `TIER_REFACTOR`.
- **`qartez_insert_before_symbol` / `qartez_insert_after_symbol`** - splice new code immediately before or after an anchor symbol. Avoids the "find the exact surrounding context" step that Edit requires; anchor lookup goes through the indexed symbol table. Preview by default; `apply=true` executes. Added to `TIER_REFACTOR`.
- **`qartez_safe_delete`** - delete a symbol after reporting every file that still imports it. Refuses to apply when importers exist unless `force=true`, so the caller sees the breakage before the file is modified. Preview always lists the importers. Added to `TIER_REFACTOR`.
- **Shared helper module `server/tools/refactor_common.rs`** - hosts `resolve_unique_symbol` (symbol + file + kind disambiguation shared with `qartez_move`), `validate_range` (stale-index detection), `write_atomic` (tmp-file + rename), and `join_lines_with_trailing` (POSIX trailing-newline preservation). Cuts duplication across the four new tools and `qartez_move`-style refactors.

### Fixed

- **`qartez_security` no longer fires on CSS and log strings** - SEC003 (SQL injection) now requires actual SQL syntax (`SELECT *|DISTINCT|TOP|<col>`, `INSERT INTO`, `UPDATE x SET`, `DELETE FROM`, `DROP TABLE|...`) inside `format!` literals, so patterns like `drop-shadow(...)`, `"Settings updated"` log lines, and `"selector:{key}={val}"` map keys stop matching. SEC001 (hardcoded secret) skips env-variable indirections (`$VAR`, `${VAR}`, `process.env.X`, `os.environ['X']`). SEC004 (command injection) skips `Command::new("LIT")` when the argument is a string literal and the builder chain has no `format!` / `String::from` / `to_string()` interpolation. The rule-definition file (`graph/security.rs`) is excluded from scanning so its own regex bodies no longer self-match. Findings on this repo drop 68 → 33 (51% noise reduction).
- **`qartez_unused` no longer fires on config-only languages** - `populate_unused_exports`, `count_unused_exports`, and `get_unused_exports_page` now filter out yaml, toml, hcl, json, ini, makefile, dockerfile, helm, css, nginx, caddyfile, bash, and systemd - languages without import semantics. Findings on this repo drop 947 → 82 (91% noise reduction). Regression test `test_populate_unused_exports_skips_config_languages`.
- **`write_atomic` tmp path is unique per call** - the tmp path used by the refactor tools (`qartez_replace_symbol`, `qartez_insert_before/after_symbol`, `qartez_safe_delete`) was deterministic (`file.qartez_edit_tmp`). Two concurrent tool calls on the same file both wrote to the same tmp, so the first `rename()` consumed the second call's bytes and the second `rename()` failed with ENOENT. The tmp path now carries a pid + thread-id + atomic-counter nonce so concurrent writes to different files never collide on the tmp name. Same-file concurrent writes still have last-writer-wins semantics (matches `qartez_move` / `qartez_rename`; MCP clients serialize tool calls anyway). Covered by a new concurrency regression test that spins 8 threads against `qartez_replace_symbol` on distinct files.
- **`qartez_replace_symbol` refuses empty `new_code`** - previously an empty string turned into "replace the symbol with one blank line" via the `"".split('\n') → [""]` quirk. Callers wanting to remove a symbol should use `qartez_safe_delete`.
- **`initialize` response tool count and refactor-tier listing were stale** - `mcp_instructions.md` (the text served in `initialize` responses) still claimed "27 tools" and listed only the three legacy refactor tools. Updated to reflect the current tool count and the full refactor tier.

### Changed

- **`qartez_read` and `qartez_move` god functions split into composable helpers** - `qartez_read` (233 lines, CC=40) is now split into `read_file_slice`, `read_symbol_batch`, `render_symbol_section`, and `parse_symbol_queries`; the largest remaining helper is CC=16. `qartez_move` (273 lines, CC=47) is now split into `validate_source`, `extract_lines`, `gather_importers`, `format_move_preview`, `write_atomic`, and `rewrite_importers`; the largest remaining helper is CC=19. Pure refactor, no behavior change.
- **`qartez_move` uses the unified `refactor_common::write_atomic`** - the per-tool atomic-write implementation is gone; both `qartez_move` and the new refactor tools now share one tmp-file + rename path. `is_test_path` in `helpers.rs` was also simplified (fewer allocations, same semantics).

## [0.8.5] - 2026-04-21

### Fixed

- **Git Bash on Windows runs the hook binary instead of collapsing the path** - Claude Code invokes hooks through `/usr/bin/bash` on Windows, which interprets backslashes as escape characters, so a raw `C:\Users\me\AppData\Local\Programs\qartez\bin\qartez-setup.exe` collapsed to `C:Usersme...qartez-setup.exe` before bash tried to spawn it ("command not found"). A new `format_hook_command_path` helper converts backslashes to forward slashes on Windows (Git Bash accepts these for `.exe` invocation) and double-quotes any path containing whitespace so usernames like `John Doe` survive word-splitting. Applied to `install_claude_one` and `install_gemini_one`; `mcpServers` commands are spawned directly and were never affected. Fixes #25.
- **SessionStart hook re-registration is idempotent** - `ensure_hook_entry_no_matcher` was still searching for the legacy substring `qartez-session-start` (which matched the old `bash ~/.claude/hooks/qartez-session-start.sh` form) instead of the current `qartez-setup --session-start` binary form. On every re-run of `qartez-setup` the refresh branch was skipped and a new SessionStart entry was appended, so v0.8.4 Windows users pulling the path fix above would otherwise have accumulated duplicate hooks - one with the broken backslash path, one with the fixed forward-slash path, and the broken one would still fire. Switched the search term to `qartez-setup`, which appears in every command string the binary-era installer has ever written, so the refresh path now rewrites the entry in place. Applied symmetrically in `install_claude_one` and `install_gemini_one`, covered by two new regression tests (`install_claude_one_is_idempotent_on_re_run` and `install_claude_one_refreshes_broken_v084_windows_paths`).

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
