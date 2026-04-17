# Qartez Subagent Contract

Defines delegated-session expectations for qartez-aware execution.

## Delegation detection

Treat a session as delegated when any of the following apply:

- Prompt/session contains `<!-- OMO_INTERNAL_INITIATOR -->`
- Session was created via a task/delegate mechanism

## Entry protocol

1. Check `.qartez/status.json`.
2. If `READY` or `STALE`, proceed qartez-first.
3. If `INDEXING`, avoid symbol-heavy queries until ready.
4. If `UNAVAILABLE`/`CAPABILITY_MISMATCH`, report immediately.

## Execution protocol

- Start with qartez tools; do not lead with built-in code tools.
- On denial, parse JSON payload and retry with `replacement`.
- On `RISK_ACK_REQUIRED`, call `qartez_impact`, then retry edit.
- Do not mark task complete on denial alone.

## Parent-agent responsibilities

- Include qartez preamble in delegated prompts.
- Parse denial payloads from subagent logs and inject recovery instructions.
- Parse and enforce execution footer contract.

## Suggested delegated-task preamble

```text
QARTEZ ROUTING: use qartez tools for all code work.
glob/ls/find -> qartez_map
grep/rg -> qartez_grep or qartez_find
read on source -> qartez_read or qartez_outline
If built-in tool is denied, parse payload.replacement and retry immediately.
Check .qartez/status.json before execution.
End with QARTEZ_EXECUTION_REPORT footer.
```

## Execution footer template

```md
<!-- QARTEZ_EXECUTION_REPORT
State: <READY|INDEXING|CAPABILITY_MISMATCH|UNAVAILABLE|STALE>
Tools_used: <comma-separated list>
Builtin_denials: <count by type or none>
Recovery_actions: <retried mappings or none>
Change_mode_used: <yes|no>
Risk_ack_issued: <yes, paths|no>
Residual_uncertainty: <text or none>
Missing_capability: <text or none>
-->
```

## oh-my-openagent advisory integration notes

- Add delegate-task recovery loop: detect denial JSON payload and append retry instruction.
- Expand skill-reminder targeting to delegated sessions.
- Lower reminder threshold for short-lived spawned sessions.
- Track upstream OpenCode MCP permission inheritance issue: #16491.
