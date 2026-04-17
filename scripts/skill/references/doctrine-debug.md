# Doctrine: Debug

Goal: Trace from symptom to root cause using semantic graph traversal.

## Sequence

1. `qartez_find`
2. `qartez_calls`
3. `qartez_refs`
4. `qartez_read`
5. `qartez_context`
6. `qartez_deps`

## Output standard

- Show step-by-step trace with file/symbol anchors
- Mark inferred transitions explicitly
