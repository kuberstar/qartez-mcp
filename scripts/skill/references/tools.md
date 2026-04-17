# Qartez Tools Reference

Canonical built-in → qartez routing map and tool catalog for this skill.

## Routing map

| Built-in tool | Qartez replacement |
|---|---|
| `glob` / `ls` / find | `qartez_map` |
| `grep` / `rg` | `qartez_grep` (pattern search), `qartez_find` (exact symbol) |
| `read` / `cat` on source | `qartez_read`, `qartez_outline` |
| risk check before edit | `qartez_impact` |

## Core navigation & reading

- `qartez_map` — structural overview, ranked by importance
- `qartez_find` — exact symbol definition lookup
- `qartez_grep` — indexed symbol/body search
- `qartez_read` — semantic symbol/file reads
- `qartez_outline` — file symbol inventory
- `qartez_stats` — repository metrics

## Analysis & risk

- `qartez_impact` — blast radius and risk ack trigger
- `qartez_deps` — importers/imports graph view
- `qartez_refs` — usages/references
- `qartez_calls` — caller/callee hierarchy
- `qartez_cochange` — historical co-change partners
- `qartez_context` — smart related-file context set
- `qartez_unused` — candidate dead exports
- `qartez_hotspots` — complexity × coupling × churn
- `qartez_clones` — structural duplication
- `qartez_boundaries` — architecture-boundary checks

## Refactor operations

- `qartez_rename`
- `qartez_move`
- `qartez_rename_file`

## Project/meta

- `qartez_project` — build/test/lint detection and execution
- `qartez_wiki` — architecture document generation
- `qartez_diff_impact` — pre-merge diff risk
- `qartez_trend` — metric trends from git history
- `qartez_hierarchy` — type/module hierarchy exploration

## Confidence reminders

- Prefer exact/strong tools for critical decisions.
- Treat `qartez_unused`, `qartez_clones` as heuristic.
- Treat `qartez_cochange`, `qartez_hotspots`, `qartez_trend` as historical correlation.
