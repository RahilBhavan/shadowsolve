# Local stack runbook

**Last updated:** 2026-08-07

Prerequisites: Rust 1.97.0 with `rustfmt` and `clippy`, Python 3, Node.js, Docker, and Docker Compose.

```sh
cp .env.example .env
docker compose up --build
```

Dashboard: `http://localhost:${DASHBOARD_HOST_PORT:-5173}`. Solver exposes `GET /health`, `GET /ready`, `GET /metrics`, `GET /api/dashboard`, `POST /solve`, and `POST /notify`.

Ingestion is opt-in (`SHADOWSOLVE_INGESTION_ENABLED=true`). Default is off.

## Persistence and readiness

Fresh PostgreSQL volumes run migrations from `infra/postgres/migrations` in filename order. Existing volumes are **not** upgraded automatically. Apply migrations with:

```sh
bash scripts/migrate-postgres.sh
```

Safe stop (keep volumes): `docker compose down`. Destructive reset: `docker compose down -v` then `up --build`.

Solve plus advisory persistence and notification acceptance each use a database transaction. Advisory identity is stable on auction/network/finalized block/hash; conflicting retries are rejected; notifications are idempotent on `(schema_version, auction_id, candidate_id)`.

## Read-only production advisory ingestion

Set `SHADOWSOLVE_RPC_URL`, `SHADOWSOLVE_NETWORK`, `SHADOWSOLVE_INGESTION_POOLS`, `SHADOWSOLVE_FINALITY_DEPTH`, `SHADOWSOLVE_POLL_SECONDS`, and `SHADOWSOLVE_MAX_AGE_SECONDS`. Configured ingestion requires `DATABASE_URL`. Policy network/pool IDs must match ingestion configuration. Transport sends only `eth_getBlockByNumber` and read-only `eth_call` for `getReserves`; never provide a private key.

`/ready` returns 503 while starting or when state is stale, unsafe, unavailable, or noncanonical. A configured `/solve` must use the reported finalized height and allow-listed pool IDs.

Pinned mainnet example: [`docs/protocol-pins.md`](protocol-pins.md). Verify math offline with `python3 scripts/verify-pinned-pool.py --self-check`, or against a public RPC with `--rpc-url`.

## Models, policy, and fixture mode

Python exports hashed JSON artifacts. Rust loads `SHADOWSOLVE_MODEL_PATH` or falls back. Online features: `expected_surplus`, `gas_proxy`, `route_count`. `SHADOWSOLVE_DASHBOARD_MODE=fixture` serves deterministic dashboard data (UI-labeled) and does not persist solve/notification changes.

## Checks

```sh
cargo fmt --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
python3 tests/mock_rpc.py --self-check
python3 -m pytest
node src/self-check.js
for f in src/*.js; do node --check "$f"; done
python3 scripts/verify-pinned-pool.py --self-check
bash tests/e2e-shadow-stack.sh
```

CI runs unit/integration checks and Docker E2E on every push/PR.

Ceiling: one chain, allow-listed CP pools, one order, bounded routes/hops. Advisory-only.
