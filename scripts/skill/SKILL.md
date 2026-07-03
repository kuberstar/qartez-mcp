---
name: qartez
description: >-
  Semantic code intelligence skill for qartez MCP. Use qartez tools for code
  exploration, impact analysis, and guarded refactors; treat built-in code-tool
  denials as routing signals and recover via qartez replacements.
---

# Qartez - Semantic Code Intelligence Skill

Qartez provides semantic, graph-aware code intelligence for any repository. It replaces naive file-system tools (glob, grep, read) with structured analysis grounded in a pre-built language-aware index.

This applies equally to **application code** and to **infrastructure-as-code / DevOps repos**. Qartez indexes Kubernetes/Kustomize/Helm YAML, Terraform/OpenTofu HCL, ArgoCD manifests, Dockerfiles, Ansible, and CI configs, and builds a cross-file dependency graph over them (Kustomize `resources`/`bases`/`components`/patches, Terraform local `module` sources, Helm `Chart.yaml` dependencies, ArgoCD `Application` source paths). Use the same qartez-first workflow when the task is "deploy X", "wire up this k8s app", "trace which overlay a manifest belongs to", or "what breaks if I change this base" - see `references/doctrine-infra.md`.

---

## 1. When Qartez Is Authoritative

Use qartez for **all** code-related exploration and analysis work.

Use built-in tools only for non-code content or when qartez cannot handle the request.

| Built-in intent | Qartez replacement |
|---|---|
| `glob` / `ls` / find | `qartez_map` |
| `grep` / `rg` | `qartez_grep` or `qartez_find` |
| `read` / `cat` on source (exploration) | `qartez_read` |
| outline/API surface read | `qartez_outline` |

`qartez_read` is for **exploration**. When you are about to make a non-symbol `Edit` (imports, partial-line tweaks, config, multi-block edits), a built-in `Read` of the exact target range first is the correct path - the harness must record it in `readFileState` before `Edit`/`Write` succeeds. `qartez_read` does not populate that state. For symbol-level changes, prefer the qartez mutators (Section 5), which need no prior `Read`.
| dependency inspection | `qartez_deps` |
| call chain tracing | `qartez_calls` / `qartez_refs` |

**Infra-as-code counts as code here.** A DevOps task in an infra repo (Kubernetes/Kustomize/Helm/Terraform/ArgoCD/GitOps) is qartez-first exactly like an app-code task. `qartez_map` for the manifest layout, `qartez_find`/`qartez_grep` to locate a resource / module / `kustomization.yaml`, `qartez_deps` to see which overlays and bases a manifest pulls in, and `qartez_impact` before editing a shared base or module. Do **not** fall back to raw `grep`/`find` just because the files are YAML/HCL. Full playbook: `references/doctrine-infra.md`.

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

**Prefer qartez mutators for structural/symbol edits.** `qartez_replace_symbol`, `qartez_insert_after_symbol`, `qartez_insert_before_symbol`, `qartez_rename`, `qartez_move`, `qartez_safe_delete`, and `qartez_rename_file` operate on the index directly. They need **no prior built-in `Read`** (they bypass `readFileState`) and they are **not** subject to the load-bearing ack gate (the guard matches only `Edit|Write|MultiEdit`). Use them to replace a whole function/struct/class body, add a sibling symbol, rename, relocate, or delete - friction is zero.

**Use built-in `Edit` only for non-symbol regions:** import blocks, partial-line edits, config/manifest values, or multi-block changes inside one symbol. For those, do a built-in `Read` of the target range immediately before the `Edit` (see Section 7).

---

## 6. Workflows

- Explore: `qartez_map → qartez_find → qartez_outline → qartez_read → qartez_refs/calls → qartez_deps`
- Debug: `qartez_find → qartez_calls → qartez_refs → qartez_read → qartez_context → qartez_deps`
- Review before edit: `qartez_outline → qartez_impact → qartez_cochange → qartez_deps → qartez_context`
- Refactor: `qartez_find → qartez_refs → qartez_impact → qartez_deps → qartez_rename/move → re-verify refs`
- Pre-merge: `qartez_diff_impact → qartez_hotspots → qartez_cochange → qartez_boundaries → qartez_context`
- Infra/DevOps: `qartez_map → qartez_find (resource/module/kustomization) → qartez_deps (overlays+bases) → qartez_impact (shared base/module) → edit → qartez_diff_impact`

---

## 7. Modification Guard and Edit Prep

### Symbol-level change (preferred, zero friction)

`qartez_map`/`qartez_find`/`qartez_read`/`qartez_impact` to understand, then apply the change with a qartez mutator (`qartez_replace_symbol`, `qartez_insert_after_symbol`, `qartez_insert_before_symbol`, `qartez_rename`, `qartez_move`, `qartez_safe_delete`). No built-in `Read` and no ack are needed.

### Non-symbol `Edit` (imports, partial lines, config, multi-block)

Canonical flow:

1. `qartez_map`/`qartez_find`/`qartez_read`/`qartez_impact` to locate and understand the region.
2. **Proactively** run a built-in `Read` of the exact target line range. This is the required step that records the file in `readFileState`; `qartez_read` does not. The read-guard hook explicitly endorses this `Read` - it is not a mistake.
3. Built-in `Edit`.

### If edit is blocked on a load-bearing file

1. Call `qartez_impact` with `file_path`.
2. Review blast radius.
3. Retry edit (ack window defaults to 10 minutes). Note: the qartez mutators above are not subject to this gate.

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
- `references/doctrine-infra.md`
- `references/tools.md`
- `references/guard.md`
