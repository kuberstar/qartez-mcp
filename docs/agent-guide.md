# Agent guide

How an AI coding agent should use qartez effectively. This guide is written
for LLMs operating as coding assistants (Claude Code, Cursor, Windsurf, etc.)
but the patterns apply to any MCP client.

## Core principle: index first, read second

Qartez pre-computes a ranked, cross-referenced index of the entire codebase.
Every tool call queries this index in milliseconds and returns
token-efficient output. This is fundamentally different from raw file tools
(Glob, Grep, Read) which scan on every call and return unranked, unstructured
text.

**Use qartez tools instead of built-in file tools for all code exploration.**

| Instead of | Use | When |
|------------|-----|------|
| Glob / find | `qartez_map` | Understanding project structure |
| Grep / rg | `qartez_grep` | Searching for symbols, types, functions |
| Grep / rg | `qartez_find` | Looking up a specific symbol definition |
| Read / cat | `qartez_read` | Reading symbol source code |

Use built-in file tools only for non-code content: config files, prose,
binary files, or patterns qartez doesn't index.

## The navigate-assess-edit workflow

Every code modification should follow this sequence:

### 1. Orient: understand the codebase

```
qartez_map                              # project skeleton, top files by importance
qartez_map by=symbols                   # top symbols by importance
qartez_stats                            # metrics overview
```

### 2. Locate: find what you need

```
qartez_find name=process_request        # jump to a symbol
qartez_grep query=authentication        # search by keyword
qartez_grep query=auth search_bodies=true  # search inside function bodies
```

### 3. Read: understand the code

```
qartez_read symbol_name=process_request           # read one symbol
qartez_read symbols=["parse_input", "validate"]   # batch read multiple
qartez_outline file_path=src/server/mod.rs         # file table of contents
```

### 4. Assess: check blast radius before editing

```
qartez_impact file_path=src/storage/read.rs   # who depends on this file?
qartez_context files=["src/auth.rs"] task="add OAuth support"  # related files
```

**This step is mandatory for load-bearing files** (PageRank >= 0.05 or blast
radius >= 10). Skipping it risks breaking downstream consumers you didn't
know about.

### 5. Edit: make changes with confidence

Now you know what depends on your target and which files might need
coordinated changes. Make your edits.

### 6. Verify: check the impact of your changes

```
qartez_diff_impact base=main            # what did your changes affect?
qartez_test_gaps mode=suggest base=main # which tests should you run?
```

## Unlocking tools

By default, all 39 tools are available. With progressive disclosure
(`QARTEZ_PROGRESSIVE=1`), start with core tools and unlock more as needed:

```
qartez_tools                            # see what's available
qartez_tools enable=["analysis"]        # unlock analysis tier
qartez_tools enable=["all"]             # unlock everything
```

**When to unlock which tier:**

| You need to... | Unlock |
|----------------|--------|
| Trace who calls a function | analysis (`qartez_calls`) |
| Find all usages of a symbol | analysis (`qartez_refs`) |
| Check for dead code | analysis (`qartez_unused`) |
| Review architecture health | analysis (`qartez_hotspots`, `qartez_boundaries`) |
| Get a prioritized fix list for the repo | analysis (`qartez_health`) |
| Plan a refactor on a specific file | analysis (`qartez_refactor_plan`) |
| Find duplicate code | analysis (`qartez_clones`) |
| Rename a symbol across the codebase | refactor (`qartez_rename`) |
| Move a function to another file | refactor (`qartez_move`) |
| Run tests or build the project | meta (`qartez_project`) |
| Generate architecture docs | meta (`qartez_wiki`) |

## Common workflows

### Code review

```
qartez_impact file_path=<changed_file>
qartez_diff_impact base=main
qartez_test_gaps mode=suggest base=main
qartez_smells file_path=<changed_file>
qartez_security file_path=<changed_file>
```

### Debugging

```
qartez_find name=<broken_function>
qartez_read symbol_name=<broken_function>
qartez_calls name=<broken_function>     # who calls it? what does it call?
qartez_refs symbol=<broken_function>    # all usages across codebase
```

### Onboarding to a new codebase

```
qartez_map top_n=10                     # most important files
qartez_stats                            # size and language breakdown
qartez_wiki                             # auto-generated architecture doc
qartez_hotspots limit=5                 # where the complexity lives
qartez_knowledge level=module           # who owns what
```

### Refactoring

```
qartez_health                           # whole-repo prioritized fix list
qartez_refactor_plan file_path=<target> # ordered, safety-annotated step-by-step plan
qartez_impact file_path=<target>        # assess blast radius first
qartez_refs symbol=<old_name>           # find all usages
qartez_rename old_name=<old> new_name=<new>           # preview rename
qartez_rename old_name=<old> new_name=<new> apply=true # execute rename
```

### Finding test gaps

```
qartez_test_gaps mode=gaps              # untested files ranked by risk
qartez_test_gaps mode=map               # full test-to-source mapping
qartez_test_gaps mode=map file_path=src/auth.rs  # tests for one file
```

### Security review

```
qartez_security                         # full scan, sorted by risk
qartez_security min_severity=high       # only high/critical
qartez_security file_path=src/server/   # scope to a directory
```

## Token efficiency tips

### Use `format=concise` for scanning

When you need to scan many results to find the right one, use `concise`
format. It returns names and locations only, without full signatures and
context. Switch to `detailed` once you've found what you need.

### Use `token_budget` to control output size

Several tools accept a `token_budget` parameter (approximate token limit).
Set it lower when you need a quick overview, higher when you need completeness:

```
qartez_map token_budget=1000            # quick overview
qartez_refs symbol=Error token_budget=8000  # comprehensive reference list
```

### Batch reads with `qartez_read symbols=[...]`

Instead of multiple `qartez_read` calls for individual symbols, use the batch
`symbols` parameter to fetch several in one call:

```
qartez_read symbols=["parse_input", "validate_request", "handle_error"]
```

### Use `qartez_outline` before `qartez_read`

When you're not sure which symbol you need from a file, check the outline
first (cheap, returns names + line numbers), then read specific symbols.

## What qartez doesn't do

- **Type inference** — qartez uses tree-sitter (syntactic), not a type
  checker. It can't tell you the type of a variable or resolve generic type
  parameters.
- **Runtime behavior** — no execution, no dynamic dispatch resolution, no
  test coverage measurement. Analysis is purely static.
- **Cross-language call tracking** — imports between files of different
  languages (Python calling C extensions, JS loading WASM) are not detected.
  Each language is parsed independently.
- **Build system integration** — qartez doesn't need or use `cargo build`,
  `npm install`, etc. This means it works without a working build
  environment, but also means it can't resolve build-time generated code.

## Interaction with the modification guard

The MCP instructions include a modification guard: **call `qartez_impact`
before modifying any file with PageRank >= 0.05 or blast radius >= 10.**

This isn't just a suggestion — it's the single most effective way to avoid
cascading breakage. The impact report tells you:

- **Direct importers** — files that `import`/`use`/`require` this file
- **Transitive dependents** — files that depend on this file through a chain
- **Co-change partners** — files that historically change alongside this one
- **Test coverage** — whether test files import this file
- **Health score** — composite of complexity, coupling, and churn

Read the impact report before deciding your edit strategy. If the blast radius
is high, consider whether your change can be made in a backward-compatible
way, or whether you need to update dependents in the same change.
