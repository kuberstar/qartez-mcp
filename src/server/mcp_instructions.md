Qartez is a code-intelligence MCP server. Use its tools INSTEAD of built-in file tools (Glob, Grep, Read) for all code exploration tasks. The index is pre-computed and token-efficient.

## Tool mapping

| Instead of | Use | When |
|---|---|---|
| Glob / find | `qartez_map` | Project structure, finding files |
| Grep / rg | `qartez_grep` | Searching for symbols, types, functions |
| Grep / rg | `qartez_find` | Looking up a specific symbol definition |
| Read / cat | `qartez_read` | Reading symbol source code with context |

## All 21 tools

**Navigate:** `qartez_map` (project skeleton), `qartez_find` (jump to symbol), `qartez_grep` (search symbols), `qartez_read` (read source), `qartez_outline` (file symbol table), `qartez_stats` (project metrics).

**Analyze:** `qartez_impact` (blast radius before editing), `qartez_deps` (file dependency graph), `qartez_refs` (all usages of a symbol), `qartez_calls` (call hierarchy), `qartez_cochange` (git co-change partners), `qartez_context` (related files for a task), `qartez_unused` (dead exports).

**Risk:** `qartez_hotspots` (complexity x coupling x churn), `qartez_clones` (duplicate code via AST hashing), `qartez_boundaries` (architecture boundary rules).

**Refactor:** `qartez_rename` (rename symbol across codebase), `qartez_move` (move symbol between files), `qartez_rename_file` (rename file, update imports).

**Build:** `qartez_project` (detected toolchain: test/build/lint), `qartez_wiki` (auto-generate ARCHITECTURE.md).

## Workflow

1. `qartez_map` first to understand project structure
2. `qartez_find` / `qartez_grep` to locate symbols
3. `qartez_read` to read code with semantic context
4. `qartez_impact` BEFORE modifying any heavily-imported file
5. `qartez_context` before multi-file changes to surface files you might miss

## Modification guard

Before editing a file that is central to the codebase, ALWAYS call `qartez_impact` with `file_path=<the file>` first. A file is considered load-bearing when its PageRank is >= 0.05 or its transitive blast radius is >= 10. The impact report shows direct importers, transitive dependents, and co-change partners so you can evaluate the risk before making changes.

## Prompts

Five workflow prompts orchestrate multiple tools in sequence:
- `/qartez_review <file>` -- code review with blast radius and co-change analysis
- `/qartez_architecture [top_n]` -- one-minute architecture overview via PageRank
- `/qartez_debug <symbol>` -- definition + body + call hierarchy + references
- `/qartez_onboard [area]` -- five-file reading list for new contributors
- `/qartez_pre_merge <files>` -- pre-merge safety check across changed files

## Resources

- `qartez://overview` -- ranked codebase overview
- `qartez://hotspots` -- top files by hotspot score
- `qartez://stats` -- language, LOC, symbol counts

Use built-in file tools ONLY for non-code content (config files, text search, file patterns qartez does not index).
