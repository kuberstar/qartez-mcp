Qartez is a code-intelligence MCP server. Use its tools INSTEAD of built-in file tools (Glob, Grep, Read) for all code exploration tasks. The index is pre-computed and token-efficient.

## Tool mapping

| Instead of | Use | When |
|---|---|---|
| Glob / find | `qartez_map` | Project structure, finding files |
| Grep / rg | `qartez_grep` | Searching for symbols, types, functions |
| Grep / rg | `qartez_find` | Looking up a specific symbol definition |
| Read / cat | `qartez_read` | Reading symbol source code with context |

## Tool tiers

Qartez organizes its 37 tools into tiers. Core tools are always available. Additional tiers can be unlocked on demand via `qartez_tools`.

### Core (always available)

Navigate and assess before editing:
- `qartez_map` - project skeleton ranked by importance (start here)
- `qartez_find` - jump to a symbol definition by name
- `qartez_grep` - search indexed symbols by name, kind, or file
- `qartez_read` - read symbol source code with line numbers
- `qartez_outline` - file symbol table (table of contents)
- `qartez_impact` - blast radius before editing (MUST call before modifying load-bearing files)
- `qartez_deps` - file dependency graph
- `qartez_stats` - project metrics (LOC, languages, symbols)

### Analysis (unlock via `qartez_tools enable: ["analysis"]`)

Deep investigation for debugging, review, and architecture:
- `qartez_refs` - all usages of a symbol across the codebase
- `qartez_calls` - call hierarchy (callers and callees)
- `qartez_cochange` - files that historically change together in git
- `qartez_context` - related files for a task (surfaces files you might miss)
- `qartez_unused` - dead exports and unreferenced symbols
- `qartez_diff_impact` - blast radius of a git diff
- `qartez_hotspots` - complexity x coupling x churn ranking
- `qartez_clones` - duplicate code via AST hashing
- `qartez_health` - prioritized fix list cross-referencing hotspots + smells
- `qartez_refactor_plan` - ordered refactor steps for one file with safety + CC-impact annotations
- `qartez_boundaries` - architecture boundary rule violations
- `qartez_hierarchy` - type/trait inheritance hierarchy
- `qartez_trend` - symbol complexity trend over git history
- `qartez_security` - scan for OWASP-style vulnerabilities and insecure patterns
- `qartez_semantic` - semantic code search via embedding similarity (requires `semantic` feature)

### Refactor (unlock via `qartez_tools enable: ["refactor"]`)

Codebase-wide rename, move, replace, insert, and safe-delete operations:
- `qartez_rename` - rename a symbol across all files
- `qartez_move` - move a symbol between files
- `qartez_rename_file` - rename a file and update all imports
- `qartez_replace_symbol` - replace a symbol's whole line range with new source
- `qartez_insert_before_symbol` - splice new code immediately before an anchor symbol
- `qartez_insert_after_symbol` - splice new code immediately after an anchor symbol
- `qartez_safe_delete` - delete a symbol after reporting every file that still imports it

### Meta (unlock via `qartez_tools enable: ["meta"]`)

Build toolchain and documentation:
- `qartez_project` - detected toolchain (test/build/lint commands)
- `qartez_wiki` - auto-generate ARCHITECTURE.md

### Discovery tool

- `qartez_tools` - list available tiers, enable or disable tools on demand. Call with no arguments to see what is available. Use `enable: ["all"]` to unlock everything at once.

## Workflow

1. `qartez_map` first to understand project structure
2. `qartez_find` / `qartez_grep` to locate symbols
3. `qartez_read` to read code with semantic context
4. `qartez_impact` BEFORE modifying any heavily-imported file
5. `qartez_context` before multi-file changes to surface files you might miss
6. Unlock `analysis` or `refactor` tiers when you need deeper capabilities

## Modification guard

Before editing a file that is central to the codebase, ALWAYS call `qartez_impact` with `file_path=<the file>` first. A file is considered load-bearing when its PageRank is >= 0.05 or its transitive blast radius is >= 10. The impact report shows direct importers, transitive dependents, and co-change partners so you can evaluate the risk before making changes.

## Prompts

Five workflow prompts orchestrate multiple tools in sequence:
- `/qartez_review <file>` - code review with blast radius and co-change analysis
- `/qartez_architecture [top_n]` - one-minute architecture overview via PageRank
- `/qartez_debug <symbol>` - definition + body + call hierarchy + references
- `/qartez_onboard [area]` - five-file reading list for new contributors
- `/qartez_pre_merge <files>` - pre-merge safety check across changed files

## Resources

- `qartez://overview` - ranked codebase overview
- `qartez://hotspots` - top files by hotspot score
- `qartez://stats` - language, LOC, symbol counts

Use built-in file tools ONLY for non-code content (config files, text search, file patterns qartez does not index).
