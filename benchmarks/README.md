# Qartez MCP benchmark fixtures

This directory owns the recipe for the multi-language benchmark corpus. It
holds one file, `fixtures.toml`, plus this README. The fixture code itself
is **not** checked in — see [Why we don't commit the code](#why-we-dont-commit-the-code).

The actual clones land in `target/benchmark-fixtures/<lang>/` after you run
`scripts/setup-benchmark-fixtures.sh`, and `target/` is gitignored.

## What the fixtures are for

`src/bin/benchmark.rs` is a comparative benchmark that measures MCP tool
output against the equivalent `Glob + Grep + Read + git log` workflow for
a set of scenarios. Until now it ran only against the `qartez-mcp` repo
itself, so every measurement was Rust-flavored.

Phase 2 extends the harness with per-language scenario profiles
(TypeScript / Python / Go / Java). To get meaningful, reproducible numbers
for each language we need an equally reproducible slab of real-world code
in each language. That's what the fixtures are.

Each fixture is:

* a well-known OSS library with a stable project layout
* pinned to a specific commit (see `fixtures.toml`)
* cloned shallow/blobless so the working tree is cheap
* indexed with `qartez-mcp` so its `.qartez/index.db` is ready to query

## Quick start

```bash
# Build the indexer once:
cargo build --release --bin qartez-mcp

# Clone + index every language:
./scripts/setup-benchmark-fixtures.sh

# Or only one:
./scripts/setup-benchmark-fixtures.sh typescript

# Or two:
./scripts/setup-benchmark-fixtures.sh go java
```

Once the script finishes, `target/benchmark-fixtures/<lang>/` is ready for
the benchmark harness:

```bash
cargo run --release --bin benchmark -- \
    --project-root target/benchmark-fixtures/typescript \
    --filter qartez_find
```

(Phase 2 will teach the harness a `--lang typescript` flag that implicitly
points `--project-root` at the right fixture.)

## The pinned commit strategy

Benchmark deltas are only interesting if the code under test is the same
from run to run. Touching `fixtures.toml`'s `commit` field therefore **is**
a baseline-bumping event — the numbers for that language will move, and
you should call it out in the commit message and refresh any stored
baseline JSON that the regression checker compares against.

Rules of thumb:

* Pick commits from roughly mid-2025 so the code under test reflects
  modern idioms of each language.
* Prefer a merge commit or release tag over a random WIP commit — a
  stable SHA is less likely to be garbage-collected.
* Don't chase the tip of `main` — that defeats the point of pinning.
* If a pinned commit disappears (force push, rebase), treat it as an
  unplanned bump: pick a nearby sibling commit from the same period and
  record the reason in the commit that updates `fixtures.toml`.

## Current fixtures

| lang        | repo                                  | purpose                    |
|-------------|---------------------------------------|----------------------------|
| typescript  | `colinhacks/zod`                      | schema validation, pnpm    |
| python      | `encode/httpx`                        | async HTTP client          |
| go          | `spf13/cobra`                         | CLI framework              |
| java        | `FasterXML/jackson-core`              | JSON parser, Maven layout  |

The exact commit SHAs live in `fixtures.toml`.

## Per-language notes for scenario authors

These are the things a Phase 2 scenario author should know before writing
ext-filters or excludes for the language profile. **The clone itself is
kept verbatim** — any filtering happens in the scenario layer, not at
clone time.

### TypeScript (zod)

* Layout is a pnpm workspace. Source lives under
  `packages/zod/src/**`, with tests under `packages/zod/src/**/*.test.ts`.
* There's a top-level `play.ts` and a few one-off driver scripts under
  `scripts/` — fine to leave in the index, they're small.
* There is **no** `node_modules/` in the clone (we never install), so
  you don't need to exclude it at index time. If Phase 2 starts running
  `pnpm install` in the fixture, add `node_modules/` to the profile's
  exclude list at that point.
* `.test.ts` doubles the symbol count. If your scenario is about
  library surface area, exclude `**/*.test.ts` in the profile.

### Python (httpx)

* Flat layout: `httpx/` is the package, `tests/` is tests, `scripts/`
  is tooling. Each is indexed.
* `docs/` is Markdown + a few Python snippets for the site. Low signal
  for symbol benchmarks — consider excluding in scenarios that care
  about import fan-out.
* No `__pycache__` or `.pyc` in the clone (git-ignored upstream), so
  no exclusion needed.

### Go (cobra)

* Flat layout — every `.go` file at the repo root is package `cobra`.
  `cobra.go`, `command.go`, `args.go`, etc.
* There is a `site/` directory of docs/examples — exclude it from the
  scenario if you want code-only numbers.
* No `vendor/` in the clone. If Phase 2 ever runs `go mod vendor` in
  the fixture, exclude `vendor/` in the profile from that day forward.

### Java (jackson-core)

* Standard Maven layout: `src/main/java/` (library) and `src/test/java/`
  (tests). **Test code roughly doubles the file count** — scenarios
  that measure library structure should filter on `src/main/java/**`.
* Packages live under `tools.jackson.core.*` in the 3.x line pinned here.
  2.x uses `com.fasterxml.jackson.*` — if you bump to a 2.x commit, the
  package prefix changes and scenario queries must change with it.
* No `target/` or `.class` files in the clone.

## Adding a new fixture

1. Pick a repo (stable layout, pinned commit, mid-2025-ish).
2. Add a new `[lang]` section to `fixtures.toml` with `repo`, `commit`,
   `committed_at`, and `description`.
3. Re-run `./scripts/setup-benchmark-fixtures.sh <lang>`.
4. Run the benchmark harness with `--project-root target/benchmark-fixtures/<lang>`
   to smoke-test the new fixture before committing.

The setup script treats every top-level `[section]` in `fixtures.toml` as
a language name — there is no hard-coded list.

## Why we don't commit the code

Two reasons:

1. **Size.** Four fixtures at ~25 MB each is ~100 MB of source code that
   has nothing to do with qartez-mcp. Adding that to git history
   permanently inflates `git clone qartez-mcp` for every user.
2. **License propagation.** Each fixture repo has its own license
   (MIT / Apache-2.0 for the ones we picked, but still). Vendoring
   someone else's code into ours means either inheriting their license
   boundary or relying on "test data" exemptions that are not worth
   arguing about in a PR review. Cloning at `setup` time sidesteps both.

The cost is that CI (and first-time contributors) need network access the
first time they run the benchmark. The setup script is idempotent, so the
cost is paid exactly once per clone.
