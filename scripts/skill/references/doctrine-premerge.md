# Doctrine: Pre-merge Risk Assessment

Goal: Produce a risk-labeled merge recommendation from semantic impact signals.

## Sequence

1. `qartez_diff_impact`
2. `qartez_hotspots`
3. `qartez_cochange`
4. `qartez_boundaries`
5. `qartez_context` (highest blast-radius files)
6. `qartez_refs` (key changed symbols)

## Report template

```text
PRE-MERGE RISK REPORT
Changed files: <n>
Overall blast radius: <n>
Risk: LOW | MEDIUM | HIGH | CRITICAL

Hotspots touched: ...
Missing co-change candidates: ...
Boundary violations: ...
Unaccounted consumers: ...

Recommendation: SAFE TO MERGE | REVIEW REQUIRED | BLOCK
```
