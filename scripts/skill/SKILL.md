---
name: qartez
description: >-
  Semantic code intelligence skill for qartez MCP. Use qartez tools for code
  exploration, impact analysis, and guarded refactors; treat built-in code-tool
  denials as routing signals and recover via qartez replacements.
allowed-tools:
  - mcp__qartez__qartez_map
  - mcp__qartez__qartez_find
  - mcp__qartez__qartez_grep
  - mcp__qartez__qartez_read
  - mcp__qartez__qartez_outline
  - mcp__qartez__qartez_stats
  - mcp__qartez__qartez_impact
  - mcp__qartez__qartez_deps
  - mcp__qartez__qartez_refs
  - mcp__qartez__qartez_calls
  - mcp__qartez__qartez_cochange
  - mcp__qartez__qartez_context
  - mcp__qartez__qartez_unused
  - mcp__qartez__qartez_hotspots
  - mcp__qartez__qartez_clones
  - mcp__qartez__qartez_boundaries
  - mcp__qartez__qartez_rename
  - mcp__qartez__qartez_move
  - mcp__qartez__qartez_rename_file
  - mcp__qartez__qartez_project
  - mcp__qartez__qartez_wiki
  - mcp__qartez__qartez_diff_impact
  - mcp__qartez__qartez_trend
  - mcp__qartez__qartez_hierarchy
---

# Qartez — Semantic Code Intelligence Skill

Qartez provides semantic, graph-aware code intelligence for any repository. It replaces naive file-system tools (glob, grep, read) with structured analysis grounded in a pre-built language-aware index.

---

## 1. When Qartez Is Authoritative

Use qartez for **all** code-related exploration and analysis work.

Use built-in tools only for non-code content or when qartez cannot handle the request.

| Built-in intent | Qartez replacement |
|---|---|
| `glob` / `ls` / find | `qartez_map` |
| `grep` / `rg` | `qartez_grep` or `qartez_find` |
| `read` / `cat` on source | `qartez_read` |
| outline/API surface read | `qartez_outline` |
| dependency inspection | `qartez_deps` |
| call chain tracing | `qartez_calls` / `qartez_refs` |

---

## 2. Runtime States

Read `.qartez/status.json` first when available.

| State | Required behavior |
|---|---|
| `READY` | Use qartez-first workflow normally |
| `INDEXING` | Use `qartez_map`; defer symbol/code queries |
| `UNAVAILABLE` | Report capability mismatch; do not silently continue |
| `CAPABILITY_MISMATCH` | Report immediately to parent/orchestrator |
| `STALE` | Proceed with warning |
| `RISK_ACK_REQUIRED` | Call `qartez_impact`, then retry edit |

---

## 3. Denied-tool Recovery Protocol

A denied built-in code tool call is a **routing signal**, not completion.

Guards emit JSON payloads. Parse `replacement` and retry immediately.

```json
{
  "qartez": {
    "type": "DENIED_BUILTIN_CODE_TOOL",
    "tool_attempted": "read",
    "replacement": ["qartez_read", "qartez_outline"],
    "reason_code": "SOURCE_CODE_REQUIRES_QARTEZ",
    "retryable": true,
    "state": "READY"
  }
}
```

If `reason_code` is `INDEX_NOT_READY`, wait until READY and retry.

---

## 4. Subagent / Delegated Sessions

For delegated sessions (e.g., `<!-- OMO_INTERNAL_INITIATOR -->`):

1. Start with qartez tools directly.
2. Treat denials as routing signals and recover via replacement tool.
3. If qartez tools are visible but execution fails, report `CAPABILITY_MISMATCH`.
4. End with the execution footer template below.

---

## 5. Operating Modes

### Explore mode (read-only)

Primary tools: `qartez_map`, `qartez_find`, `qartez_grep`, `qartez_read`, `qartez_outline`, `qartez_refs`, `qartez_calls`, `qartez_deps`, `qartez_context`, `qartez_stats`.

### Change mode (edit-authorized)

Risk tools and transforms: `qartez_impact`, `qartez_diff_impact`, `qartez_cochange`, `qartez_unused`, `qartez_rename`, `qartez_move`, `qartez_rename_file`.

---

## 6. Workflows

- Explore: `qartez_map → qartez_find → qartez_outline → qartez_read → qartez_refs/calls → qartez_deps`
- Debug: `qartez_find → qartez_calls → qartez_refs → qartez_read → qartez_context → qartez_deps`
- Review before edit: `qartez_outline → qartez_impact → qartez_cochange → qartez_deps → qartez_context`
- Refactor: `qartez_find → qartez_refs → qartez_impact → qartez_deps → qartez_rename/move → re-verify refs`
- Pre-merge: `qartez_diff_impact → qartez_hotspots → qartez_cochange → qartez_boundaries → qartez_context`

---

## 7. Modification Guard

If edit is blocked on a load-bearing file:

1. Call `qartez_impact` with `file_path`.
2. Review blast radius.
3. Retry edit (ack window defaults to 10 minutes).

---

## 8. Subagent Execution Footer

Delegated sessions should end with:

```md
<!-- QARTEZ_EXECUTION_REPORT
State: <READY|INDEXING|CAPABILITY_MISMATCH|UNAVAILABLE|STALE>
Tools_used: <comma-separated list>
Builtin_denials: <count by type or none>
Recovery_actions: <what was retried or none>
Change_mode_used: <yes|no>
Risk_ack_issued: <yes, file list|no>
Residual_uncertainty: <text or none>
Missing_capability: <text or none>
-->
```

---

## 9. References

- `references/runtime-contract.md`
- `references/subagent-contract.md`
- `references/host-matrix.md`
- `references/confidence-model.md`
- `references/doctrine-explore.md`
- `references/doctrine-debug.md`
- `references/doctrine-review.md`
- `references/doctrine-refactor.md`
- `references/doctrine-premerge.md`
- `references/tools.md`
- `references/guard.md`
