# Qartez Runtime Contract

Defines qartez runtime states, denial payload schemas, and environment variables.

## States

| State | Meaning |
|---|---|
| `READY` | Index is available and queries should run normally |
| `INDEXING` | Index build is in progress |
| `UNAVAILABLE` | Index or runtime unavailable |
| `CAPABILITY_MISMATCH` | Tool visible but not executable in current host/session |
| `STALE` | Index present but potentially stale |
| `RISK_ACK_REQUIRED` | Edit blocked pending `qartez_impact` acknowledgment |

## Status file schema (`.qartez/status.json`)

```json
{
  "schema_version": "1",
  "state": "READY",
  "project_root": "/absolute/path",
  "index_version": "2026-04-17T12:34:56Z",
  "languages": ["ts", "rs"],
  "file_count": 1382,
  "is_stale": false,
  "stale_threshold_secs": 300,
  "last_error": null,
  "started_at": "2026-04-17T12:30:00Z",
  "ready_at": "2026-04-17T12:34:56Z",
  "progress_hint": null
}
```

## Guard denial payloads

### Built-in code tool denied

```json
{
  "qartez": {
    "type": "DENIED_BUILTIN_CODE_TOOL",
    "tool_attempted": "read",
    "file_path": "src/server/mod.ts",
    "replacement": ["qartez_read", "qartez_outline"],
    "reason_code": "SOURCE_CODE_REQUIRES_QARTEZ",
    "retryable": true,
    "state": "READY"
  }
}
```

### Index not ready

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

### Load-bearing edit requires risk ack

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

## Environment variables

| Variable | Default | Purpose |
|---|---|---|
| `QARTEZ_GUARD_DISABLE` | unset | Set `1` to disable guard logic |
| `QARTEZ_INDEX_PATH` | `.qartez` | Override index directory |
| `QARTEZ_STATUS_PATH` | `.qartez/status.json` | Override status file path |
| `QARTEZ_ACK_TTL_SECS` | `600` | Guard acknowledgment TTL seconds |
| `QARTEZ_GUARD_PAGERANK_MIN` | `0.05` | Load-bearing PageRank threshold |
| `QARTEZ_GUARD_BLAST_MIN` | `10` | Load-bearing blast radius threshold |
