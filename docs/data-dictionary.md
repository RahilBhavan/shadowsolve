# ShadowSolve data dictionary

**Last updated:** 2026-08-07

## API and persistence records

| Record | Stored fields / purpose |
|---|---|
| Auction | Incoming auction payload, keyed by auction id. |
| Candidate | Solver candidate payload linked to an auction. Routes contain pool id and string decimal amounts (parallel split legs or sequential multi-hop legs). |
| Prediction | Candidate prediction payload linked to auction and candidate. |
| Notification | JSON payload keyed by `(schema_version, auction_id, candidate_id)` for idempotent acceptance. |
| Block | State-ingestion network, number, hash, parent hash, timestamp, and canonical flag. |
| Pool snapshot | Immutable network/pool/block-hash payload and timestamp. |
| Ingestion cursor | Network/source next block and its canonical block hash. |
| Advisory provenance | Network/block/hash, route, gross out, gas proxy/cost, surplus, assumptions, model/policy metadata. |

PostgreSQL is the primary solver store. The fallback JSON file stores notifications only and is bounded by record and byte limits.

## Online model features

The Rust online schema has exactly three ordered named features:

1. `expected_surplus`
2. `gas_proxy`
3. `route_count`

The model artifact also records calibration, model/version, feature-schema hash, training/calibration/data/code/config hashes, creation time, and artifact hash. A compatible artifact may be base-rate, logistic, or bounded shallow boosted-tree; incompatible artifacts use static fallback.

## Replay dataset

`write_replay_dataset` emits one row per `DecisionFeatures` record and requested positive horizon. JSONL is the canonical hashed representation; CSV is a flattened presentation. Decision features include decision provenance, route reserves/fees/sell amounts, committed and auction output, gas proxy, margin, liquidity ratio, price impact, route count, split count, snapshot hashes, schema version, and raw-unit metadata. Replay labels include future state provenance, future output, feasibility, margin remaining, margin erosion, and score decay. Fixed routes and constant-product integer arithmetic are used; model-only observations are not inferred from the replay CSV.
