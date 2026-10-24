# Limitations

**Last updated:** 2026-08-07

This slice supports one configured chain, an explicit allow-list of directional constant-product pools with basis-point fees, one order, bounded routes/hops (direct, optional parallel split, and allow-listed multi-hop discovery up to two hops), and bounded integer arithmetic. Its read-only JSON-RPC worker obtains finalized headers and `getReserves`, validates canonicality, and persists state atomically. It does not discover arbitrary pools outside the allow-list or prove that configured token orientation/fee metadata is correct.

Replay labels are fixed-route observations at explicit horizons, not causal estimates, profitability guarantees, or live-feasibility evidence. A configured RPC block hash establishes source provenance and internal coherence, not independent truth; provider quality and constant-product ABI correctness remain assumptions.

Model artifact export, validated online Rust serving, calibration, deterministic fallback, and boosted-tree walk-forward evaluation are implemented. The online schema is only `expected_surplus`, `gas_proxy`, and `route_count`. Gas is represented by the typed advisory quote fields (units, price, conversion, and cost), but remains a fixed buy-token raw-unit proxy, not a live fee/conversion quote. Independent continuous live verification against commercial searcher profitability remains out of scope. Published walk-forward evidence under [`reports/walk-forward/`](../reports/walk-forward/) is a deterministic synthetic replay / methodology smoke test, not live market performance.

Explicit non-goals are concentrated liquidity, multi-order optimization, unrestricted route discovery, private keys, signing, execution, settlement, and transaction submission. The service is advisory-only.
