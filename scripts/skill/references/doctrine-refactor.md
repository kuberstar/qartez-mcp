# Doctrine: Refactor

Goal: Execute semantically safe refactors with full verification.

## Phase 1 — Understand

1. `qartez_find`
2. `qartez_refs`
3. `qartez_impact`
4. `qartez_cochange`
5. `qartez_deps`
6. `qartez_boundaries`

## Phase 2 — Execute

7. `qartez_rename` / `qartez_move` / `qartez_rename_file`

## Phase 3 — Verify

8. `qartez_refs` (new symbol/path)
9. `qartez_find` (old symbol/path should be absent)

## Dead-code branch

Use `qartez_unused` then verify each candidate with `qartez_refs` before deletion.
