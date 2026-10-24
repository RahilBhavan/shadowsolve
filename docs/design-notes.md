# Design notes

Three design questions, each answered with pointers to the code.

## 1. Reorg fail-closed

**Question:** What happens if the RPC finalized branch jumps incompatibly?

**Answer:** Ingestion maintains a bounded canonical chain. A finalized reorg that cannot connect to the retained branch marks the process **unsafe**. `/ready` returns 503; configured `/solve` fails closed until restart rebuilds state. E2E switches mock-rpc to `reorg` and asserts the close.

**Code anchors:** `crates/state-ingestion`, `tests/e2e-shadow-stack.sh`, `/ready` projection in `crates/solver-engine/src/main.rs`.

## 2. Model hash / schema fallback

**Question:** How do you prevent a bad model artifact from silently ranking wrong?

**Answer:** Python exports hashed JSON. Rust loads the artifact, verifies content hash, online feature schema hash, width, named order, and finite arithmetic. Any failure uses **deterministic static fallback** and surfaces `modelFallback` on the dashboard.

**Code anchors:** `crates/solver-engine/src/model.rs`, `fixtures/online-model.json`, `research/shadowsolve/export_model.py`.

## 3. Paired static vs risk policy

**Question:** What does the walk-forward experiment actually measure?

**Answer:** Preregistered comparison of max `static_score` vs risk policy (`predicted_validity` threshold + `risk_value`) on chronological folds. Metrics: invalidation, future feasible score, decay tails/CVaR, score sacrifice. Boosted-tree folds are included. Published numbers under `reports/walk-forward/` are a **synthetic replay / methodology smoke test**, not live alpha or profitability.

**Code anchors:** `docs/experiment-preregistration.md`, `research/shadowsolve/walk_forward.py`, `reports/walk-forward/`.
