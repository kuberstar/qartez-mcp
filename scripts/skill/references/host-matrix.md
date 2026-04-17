# Qartez Host Matrix

Current/target parity for qartez guard behavior across hosts.

| Behavior | Claude hook (`qartez-guard.sh`) | OpenCode plugin (`opencode-plugin.ts`) | Target |
|---|---|---|---|
| Block `glob` on code tasks | Yes | Yes | Parity |
| Block `grep` on code tasks | Yes | Yes | Parity |
| Block `read` on source files | Yes | Yes | Parity |
| Emit structured denial payload JSON | Yes | Yes | Parity |
| Emit `INDEX_NOT_READY` when indexing | Yes | Yes | Parity |
| Load-bearing edit risk ack protocol | Yes | Yes | Parity |
| `QARTEZ_GUARD_DISABLE` support | Yes | Yes | Parity |
| `QARTEZ_INDEX_PATH` / `QARTEZ_STATUS_PATH` support | Yes | Yes | Parity |
| `QARTEZ_ACK_TTL_SECS` support | Yes | Yes | Parity |

## Delegation compatibility

| Path | Status | Notes |
|---|---|---|
| Claude direct session | Supported | PreToolUse + SessionStart hooks |
| OpenCode direct session | Supported | Plugin enforces tool routing |
| OpenCode spawned child sessions | Partial | Permission inheritance issue upstream (#16491) |

## Remediation notes

- Maintain structured payload contract in both hosts.
- Keep status lifecycle in session-start hook for host-agnostic readiness.
- Document OpenCode #16491 and retain defensive subagent recovery protocol.
