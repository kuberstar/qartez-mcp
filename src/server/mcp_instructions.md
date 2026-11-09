Qartez is a pre-computed code index. For reading or searching SOURCE CODE, call qartez instead of shell `grep`/`rg`, `sed -n`, `cat`/`head`/`tail`, `find`, or the built-in Grep/Glob/Read tools: one call returns the exact symbol, with line numbers, for a fraction of the tokens.

## Instead of the shell

| Instead of | Use |
|---|---|
| `rg`/`grep` for a name, type or function | `qartez_find` (definition) or `qartez_grep` (symbols by name/kind/file) |
| `rg`/`grep` for a string inside code | `qartez_grep` with `search_bodies=true` |
| `sed -n 120,180p f.rs`, `cat f.ts` | `qartez_read` with `symbol_name`, or `file_path` + `start_line`/`limit` |
| `find`/`ls` to learn the layout | `qartez_map` (files ranked by importance) |
| reading a file to see what it holds | `qartez_outline` |

The shell stays right for non-code content (Markdown, YAML/JSON/TOML config, logs, data), for filtering a command's output, and for files outside the project.

## Before editing

Call `qartez_impact file_path=<file>` before modifying a load-bearing file (PageRank >= 0.05 or transitive blast radius >= 10). It lists direct importers, transitive dependents and co-change partners.

## Workflow

1. `qartez_map` for structure
2. `qartez_find` / `qartez_grep` to locate symbols
3. `qartez_read` to read them
4. `qartez_impact` before editing a heavily imported file
5. `qartez_context` before a multi-file change, to surface files you might miss

## More tools

- Analysis: `qartez_refs` (all usages), `qartez_calls` (call hierarchy), `qartez_deps`, `qartez_cochange`, `qartez_context`, `qartez_diff_impact`, `qartez_unused`, `qartez_hotspots`, `qartez_health`, `qartez_smells`, `qartez_clones`, `qartez_refactor_plan`, `qartez_boundaries`, `qartez_hierarchy`, `qartez_trend`, `qartez_blame`, `qartez_security`, `qartez_semantic`, `qartez_path`, `qartez_stats`.
- Refactor: `qartez_rename`, `qartez_move`, `qartez_rename_file`, `qartez_replace_symbol`, `qartez_insert_before_symbol`, `qartez_insert_after_symbol`, `qartez_safe_delete`. They edit by symbol and need no prior read of the file.
- Meta: `qartez_project` (build/test/lint commands), `qartez_wiki`, `qartez_workspace`, `qartez_add_root`, `qartez_list_roots`.
- `qartez_tools` lists the tiers; when a tool is missing (progressive mode, `QARTEZ_PROGRESSIVE=1`), enable it with `qartez_tools enable: ["analysis"]` or `["all"]`.

Prompts: `/qartez_review <file>`, `/qartez_architecture`, `/qartez_debug <symbol>`, `/qartez_onboard`, `/qartez_pre_merge <files>`, `/qartez_arch_review`. Resources: `qartez://overview`, `qartez://hotspots`, `qartez://stats`.
