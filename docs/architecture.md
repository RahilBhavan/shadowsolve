# Architecture

**Last updated:** 2026-08-07

ShadowSolve is a single product with three cooperating surfaces:

```text
Browser dashboard          Solver HTTP service           Research Python
  index.html + src/*.js      crates/solver-engine          research/shadowsolve
  GET /api/dashboard         /solve /notify /ready         replay → train → export
  fixture or live mode       PostgreSQL provenance         walk-forward reports
                             state-ingestion (opt-in)
```

The dashboard is a dependency-free ES-module UI. It reads the solver’s live projection or an explicit fixture projection (`?fixture=1` / `SHADOWSOLVE_DASHBOARD_MODE=fixture`).

## Solve path

1. Validate request (one order, bounded pools, deadline).
2. If ingestion is configured, admit only fresh finalized state; replace request reserves with finalized snapshots for allow-listed pool IDs.
3. Generate candidates:
   - Direct constant-product hops matching sell→buy
   - Optional two-pool parallel split when `split_ratios` is set
   - Bounded multi-hop discovery (`discover_routes`, max 2 hops) over allow-listed pools
4. Re-validate each candidate (parallel-split or sequential multi-hop arithmetic).
5. Rank with the online model when the artifact schema matches; otherwise deterministic static fallback.
6. Persist auction / candidates / predictions / advisory provenance transactionally when PostgreSQL is configured.

## Ingestion

`state-ingestion` is an HTTPS-capable, read-only Ethereum JSON-RPC transport. The worker calls only `eth_getBlockByNumber` and `eth_call` with the constant-product `getReserves` selector, validates a bounded finalized canonical chain, and atomically persists headers, snapshots, and a cursor. A finalized reorg outside the retained branch marks the process unsafe until restart.

## Models

Python research produces replay datasets, hashed artifacts, and walk-forward reports. The Rust solver loads compatible artifacts from `SHADOWSOLVE_MODEL_PATH` and validates hashes, schema, feature width, and finite arithmetic.

Online feature schema: `expected_surplus`, `gas_proxy`, `route_count`.

## Persistence

PostgreSQL stores auctions, candidates, predictions, notifications, and advisory provenance. Without `DATABASE_URL`, a bounded atomic JSON notification fallback is used; configured ingestion requires PostgreSQL. Fresh-volume migrations are mounted by Compose; existing volumes need `scripts/migrate-postgres.sh` or a deliberate `docker compose down -v` reset.
