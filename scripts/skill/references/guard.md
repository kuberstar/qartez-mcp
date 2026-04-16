# Modification Guard Reference

## What It Does

The `qartez-guard` hook is a PreToolUse hook that intercepts Edit, Write, and MultiEdit tool calls. Before allowing a modification to proceed, it checks whether the target file is "load-bearing" (central to the codebase based on graph metrics). If the file exceeds the configured thresholds, the edit is blocked with an error message explaining exactly which thresholds were triggered.

This prevents accidental breakage of critical files by requiring you to explicitly assess the impact before editing.

## Thresholds

A file is considered load-bearing when either condition is true:

| Metric | Default threshold | Meaning |
|---|---|---|
| PageRank | >= 0.05 | The file is in the top tier of importance by dependency graph centrality |
| Blast radius | >= 10 | At least 10 files depend on this file transitively |

Both thresholds are evaluated independently. If either one fires, the edit is blocked.

## How to Acknowledge

When the guard blocks an edit, the error message names the file and the thresholds that fired. To acknowledge the risk and proceed:

```
Call qartez_impact with file_path=<the blocked file>
```

This does two things:
1. Shows you the full impact analysis so you understand the consequences of your edit
2. Grants edit permission for that specific file for a limited time window

After calling `qartez_impact`, retry the edit. It will succeed.

## Acknowledgment TTL

Each acknowledgment lasts **10 minutes** (600 seconds) by default. After the TTL expires, you must call `qartez_impact` again to re-acknowledge. This prevents stale acknowledgments from persisting across long conversations where context may have shifted.

## Environment Variable Overrides

Configure the guard behavior using these environment variables:

| Variable | Default | Description |
|---|---|---|
| `QARTEZ_GUARD_PAGERANK_MIN` | `0.05` | Minimum PageRank score that triggers the guard |
| `QARTEZ_GUARD_BLAST_MIN` | `10` | Minimum transitive blast radius that triggers the guard |
| `QARTEZ_GUARD_ACK_TTL_SECS` | `600` | Seconds an acknowledgment remains valid |
| `QARTEZ_GUARD_DISABLE` | `0` | Set to `1` to disable the guard entirely |

### Examples

Lower the PageRank threshold to catch more files:
```bash
export QARTEZ_GUARD_PAGERANK_MIN=0.02
```

Increase the blast radius threshold to be less strict:
```bash
export QARTEZ_GUARD_BLAST_MIN=20
```

Extend acknowledgment validity to 30 minutes:
```bash
export QARTEZ_GUARD_ACK_TTL_SECS=1800
```

Disable the guard for a session (use with caution):
```bash
export QARTEZ_GUARD_DISABLE=1
```

## Typical Workflow

1. Attempt to edit a load-bearing file
2. Guard blocks the edit with a message like: "Blocked: `src/core/engine.rs` has PageRank 0.12 (>= 0.05) and blast radius 34 (>= 10). Call qartez_impact to acknowledge."
3. Call `qartez_impact file_path=src/core/engine.rs`
4. Review the impact analysis output
5. Retry the edit, which now succeeds
6. If you spend more than 10 minutes before your next edit on the same file, repeat from step 3
