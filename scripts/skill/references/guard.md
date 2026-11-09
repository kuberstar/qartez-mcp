# Modification Guard Reference

## Purpose

Qartez guard hooks prevent unsafe built-in code-tool usage and enforce risk acknowledgment before load-bearing edits.

## Denial contracts

Both shell and OpenCode guards emit a machine-readable JSON payload in denial text:

### 1) Built-in code tool denial

```json
{
  "qartez": {
    "type": "DENIED_BUILTIN_CODE_TOOL",
    "tool_attempted": "read",
    "file_path": "src/server/mod.rs",
    "replacement": ["qartez_read", "qartez_outline"],
    "reason_code": "SOURCE_CODE_REQUIRES_QARTEZ",
    "retryable": true,
    "state": "READY"
  }
}
```

### 2) Index not ready denial

```json
{
  "qartez": {
    "type": "DENIED_BUILTIN_CODE_TOOL",
    "tool_attempted": "grep",
    "replacement": [],
    "reason_code": "INDEX_NOT_READY",
    "retryable": false,
    "state": "INDEXING"
  }
}
```

### 3) Risk acknowledgment required

```json
{
  "qartez": {
    "type": "RISK_ACK_REQUIRED",
    "tool_attempted": "edit",
    "file_path": "src/server/mod.rs",
    "replacement": ["qartez_impact"],
    "reason_code": "LOAD_BEARING_FILE",
    "pagerank": 0.12,
    "blast_radius": 34,
    "retryable": true,
    "ack_ttl_secs": 600
  }
}
```

### 4) Shell exploration of source code (Claude Code `Bash`)

The `qartez-guard` hook on `Bash|mcp__qartez__.*` counts consecutive shell reads and searches of source code inside the project (`grep`/`rg`, `sed -n`, `cat`/`head`/`tail`, `awk`, `find -name '*.rs'`). Pipe filters (`cmd | grep x`), non-code files (Markdown, config, logs), files outside the project and vendored/build directories never count. A qartez tool call resets the count.

At the threshold the hook denies one call with the exact replacement, for example `qartez_read file_path="src/main.rs" start_line=120 limit=61`, and the block holds: a plain retry or the next shell read of code is denied too, until a qartez call. If the shell really is the right tool (a non-code file, filtering output, a file outside the project), prefix the command with `QARTEZ_SHELL=1 `; that, or three denials in a row, lets it through and starts the cooldown, and an unresolved block expires after 15 minutes. A command that also writes, builds or installs is never denied; it gets the same advice as context instead.

## Recovery behavior

1. Parse payload JSON from denial.
2. If `type=DENIED_BUILTIN_CODE_TOOL` and `retryable=true`, retry with `replacement`.
3. If `reason_code=INDEX_NOT_READY`, wait for status `READY`.
4. If `type=RISK_ACK_REQUIRED`, call `qartez_impact` for the file, then retry edit.

## Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `QARTEZ_GUARD_DISABLE` | unset | Set `1` to disable guard checks |
| `QARTEZ_INDEX_PATH` | `.qartez` | Override index directory |
| `QARTEZ_STATUS_PATH` | `.qartez/status.json` | Override status file path |
| `QARTEZ_ACK_TTL_SECS` | `600` | Ack TTL for risk-ack windows |
| `QARTEZ_GUARD_ACK_TTL_SECS` | `600` | Backward-compatible ack TTL key |
| `QARTEZ_GUARD_PAGERANK_MIN` | `0.05` | PageRank threshold |
| `QARTEZ_GUARD_BLAST_MIN` | `10` | Blast-radius threshold |
| `QARTEZ_GUARD_BASH_STREAK` | `3` | Consecutive shell reads/searches of code before the shell guard steps in; `0` disables it |
| `QARTEZ_GUARD_BASH_COOLDOWN_SECS` | `120` | Quiet seconds after a `QARTEZ_SHELL=1` read or a give-up |

## Source-file extension coverage

`ts, tsx, js, jsx, mjs, cjs, rs, go, py, rb, java, kt, swift, c, cc, cpp, h, hpp, cs, fs, elm, hs, vue, svelte, sh, bash`
