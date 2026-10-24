# ShadowSolve methodology

**Last updated:** 2026-08-07

`write_replay_dataset` publishes manifest-backed JSONL and CSV. Rows join one `DecisionFeatures` record to one `ReplayLabel` for each requested horizon, with explicit provenance hashes and deterministic ordering. Replay fixes every candidate route and sell amount, requires coherent future states, and validates constant-product integer equations for future output, feasibility, margin remaining, margin erosion, and score decay.

Decision features use only decision-block state and include route reserves, fees, sell amounts, margin, liquidity ratio, and price impact. Receive metadata cannot be later than decision metadata. Snapshot hashes must be explicit lowercase SHA-256 values.

Implemented model training includes a feasibility base rate, standardized logistic regression, and a bounded deterministic shallow boosted-tree ensemble. Calibration uses calibration rows only and selects identity or Platt scaling by log loss. Artifacts are exported as validated content-addressed JSON and can be loaded and served by Rust online when the exact feature schema and width match; otherwise the solver uses static fallback. Online features are exactly `expected_surplus`, `gas_proxy`, and `route_count`.

Chronological splits preserve complete auction timestamp buckets across train, calibration, and test. Metrics include Brier, log loss, AUPRC when applicable, feasibility base rate, score-decay summaries, CVaR, and paired static-versus-risk policy summaries. Quantile decay modeling and automated walk-forward evaluation (including boosted-tree folds) are implemented. Walk-forward publication is manifest-backed and crash-safe.

SQL checks inspect published files and report violations without repairing data. No claim is made about causality, live profitability, or commercial searcher performance.

For configured online advisories, the worker admits only fresh finalized state whose header and pool snapshots share one canonical block hash. The request must name that finalized height and use only configured pool addresses; ingested reserves replace caller reserves before constant-product solving. Provenance records the source network/block/hash, selected route, gross output, fixed gas proxy/cost, signed surplus over the order minimum, explicit assumptions and rejection reasons, and model/policy metadata in the same PostgreSQL transaction as solve rows. Retries with the same advisory identity and payload are idempotent.

Candidate generation includes direct hops, optional two-pool parallel splits, and allow-listed multi-hop discovery bounded to two hops. This online method remains limited to one chain, allow-listed constant-product pools, and one order. It is advisory-only; concentrated liquidity, multi-order solving, signing, and execution are deferred.
