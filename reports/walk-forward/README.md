# Walk-forward publication

**Preregistration:** [`docs/experiment-preregistration.md`](../../docs/experiment-preregistration.md)

This directory contains a deterministic, content-addressed walk-forward report over a synthetic high-fidelity constant-product feature stream. It is **replay evidence**, not a claim of live profitability, causal validity, or production alpha.

## Artifacts

| File | Role |
|---|---|
| `report.json` | Current published report |
| `report.json.manifest.json` | Manifest with bytes + sha256 |
| `report-<sha256>.json` | Content-addressed copy |

Re-publish (the sha256 below was reproduced on Python 3.12, 3.13 and 3.14; floats are rounded to 10 decimals before hashing):

```sh
python3 scripts/publish-walk-forward.py
```

## Ablation (aggregate over folds)

Published report sha256: `245c39b4d19a5b4804fd0c5c83fd79bfc9f62a36a16f33086f8252764670a4a3`

| Model | Brier | Log loss | Feasibility base rate |
|---|---:|---:|---:|
| base_rate | 0.2721 | 1.0452 | 0.6500 |
| logistic | 0.3856 | 1.7581 | 0.6500 |
| boosted_tree | 0.2778 | 1.0672 | 0.6500 |

These folds use a deliberately separable synthetic stream so CI clones stay deterministic. Treat them as a methodology smoke test, not market evidence. Re-open `report.json` → `aggregate` after regenerating if you change the publisher.

Paired static-versus-risk policy summaries remain under each fold’s `paired` object.

## Honesty bounds

- Online feature schema remains `expected_surplus`, `gas_proxy`, `route_count`
- Gas is a fixed raw-unit proxy
- Dataset is synthetic CP-style features for reproducibility in CI/clone environments without archive RPC
- Do not cite these numbers as live MEV performance
