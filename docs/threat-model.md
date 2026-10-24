# Threat model

**Last updated:** 2026-08-07

## Assets

- Advisory solve outputs and provenance rows
- Finalized pool reserve snapshots
- Model artifacts and walk-forward reports
- Operator machine / Compose loopback endpoints

## Trust boundaries

| Boundary | Assumption | Failure mode |
|---|---|---|
| Ethereum JSON-RPC provider | Returns coherent finalized headers and `getReserves` for allow-listed pools | Stale/reorg/non-canonical → fail-closed `/ready`, solves rejected |
| Protocol policy pins | token0/token1 orientation and fee_bps match reality | Wrong pin → incorrect advisory surplus; operator error |
| Model artifact | Content hash and schema match | Tamper/mismatch → deterministic static fallback |
| Dashboard consumers | Read-only observers | Misreading fixture panels as live research evidence |

## Explicit non-capabilities (by design)

- No private keys, wallets, signing, `eth_sendRawTransaction`, or settlement controls
- No open proxy for arbitrary `eth_call` outside allow-listed pool `getReserves`
- No unrestricted route discovery beyond allow-listed pools and two hops
- Advisory outputs are not execution instructions

## Misuse scenarios

1. **Treating advisories as executable trades** — Mitigated by UI guardrails, provenance assumptions fields, and documentation.
2. **Feeding signing material into env** — Forbidden; CI greps for wallet/signer patterns.
3. **Publicly exposing RPC worker without allow-list** — Compose binds loopback by default; do not publish ports without auth/rate limits.
4. **Fixture dashboard as live evidence** — UI banners fixture mode; live panels hide empty research sections.

## Residual risks

- Malicious or buggy RPC can lie within the fail-closed envelope until independent verification.
- Fixed gas proxy can under/overstate net surplus versus real fee markets.
- Synthetic walk-forward datasets demonstrate methodology, not alpha.
