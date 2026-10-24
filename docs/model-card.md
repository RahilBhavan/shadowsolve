# ShadowSolve model card

**Last updated:** 2026-08-07

## Intended use

Offline research on replayed candidate feasibility and dry-run online ranking. Supported artifacts are validated base-rate, standardized logistic, or bounded deterministic shallow boosted-tree models, with identity or Platt calibration. Artifacts are hashed JSON records containing feature-schema, data, code, configuration, version, and artifact hashes.

## Training and serving

Training uses chronological train rows; calibration uses calibration rows; final metrics and paired policy summaries use test rows. Splits preserve complete auction timestamp buckets. Boosted trees are bounded additive decision stumps and do not discover routes. Constant-feature fits can export intercept-only artifacts. The Rust solver validates artifact hashes, schema hash, feature width, named feature order, and finite arithmetic, then serves predictions online or uses deterministic static fallback.

Walk-forward evaluation fits and calibrates base-rate, logistic, and boosted-tree models on each fold. See [`reports/walk-forward/`](../reports/walk-forward/).

The actual online feature schema is exactly:

- `expected_surplus`
- `gas_proxy`
- `route_count`

The checked online fixture is `fixtures/online-model.json`. The artifact's presence is not evidence that the model is trained on representative production data.

## Evaluation and limitations

Reported metrics are Brier, log loss, AUPRC where positives exist, feasibility base rate, score-decay summaries, and paired static-versus-risk policy results. These are replay results, not production performance estimates. Features are fixed-route, directional constant-product calculations and use only the supplied gas proxy. The model does not execute, discover unrestricted routes, alter state, or verify external chain data beyond configured read-only ingestion.

There is no claim of causal validity or live profitability.
