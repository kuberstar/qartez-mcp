# Qartez Confidence Model

Defines confidence levels for qartez outputs and required caveat labeling.

## Levels

| Level | Meaning | Typical tools |
|---|---|---|
| Exact | Derived directly from indexed symbols/edges | `qartez_find`, `qartez_map`, `qartez_read`, `qartez_outline`, `qartez_stats` |
| Strong | High-confidence static analysis with known dynamic blind spots | `qartez_refs`, `qartez_calls`, `qartez_deps`, `qartez_impact`, `qartez_context` |
| Heuristic | Best-effort; requires manual verification | `qartez_unused`, `qartez_clones` |
| Historical Correlation | Based on git co-occurrence/churn, not causality | `qartez_cochange`, `qartez_hotspots`, `qartez_trend` |
| Inferred | Structural inference/modeling | `qartez_wiki`, boundary suggestions |

## Blind spots

- Dynamic dispatch and reflection
- Runtime string-based imports
- External consumers outside the indexed repository

## Output labeling standard

Use labels when presenting non-exact outputs:

```text
[CONFIDENCE: HEURISTIC] ...
[CONFIDENCE: HISTORICAL] ...
[CONFIDENCE: INFERRED] ...
```
