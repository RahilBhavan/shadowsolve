# Protocol pins

**Last updated:** 2026-08-07

## Pinned Uniswap V2 pool (Ethereum mainnet)

| Field | Value |
|---|---|
| Venue | Uniswap V2 |
| Pool | `0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc` |
| token0 | USDC (`0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48`) |
| token1 | WETH (`0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2`) |
| fee_bps | 30 (0.30%) |
| concentrated | false |
| getReserves selector | `0x0902f1ac` |

Pool orientation is canonical and position-sensitive: under this pool ID, `token0`/`token1` must match USDC/WETH exactly because `reserve0`/`reserve1` are position-dependent.

Example policy file: [`tests/fixtures/protocol-policy.mainnet-example.json`](../tests/fixtures/protocol-policy.mainnet-example.json).

The E2E stack uses a symbolic fixture, not the mainnet pin:

- Policy file [`tests/fixtures/protocol-policy.json`](../tests/fixtures/protocol-policy.json) uses pool id `symbolic-weth-usdc-30`
- Compose E2E ingestion/solve requests use pool address `0x0000000000000000000000000000000000000001`

Do not confuse either fixture identifier with the mainnet Uniswap V2 pin above.

## Verification

```sh
# Offline math self-check (CI)
python3 scripts/verify-pinned-pool.py --self-check

# Optional live read-only check against a public RPC
SHADOWSOLVE_RPC_URL=https://… python3 scripts/verify-pinned-pool.py \
  --rpc-url "$SHADOWSOLVE_RPC_URL" --block finalized
```

Live verification compares advisory constant-product math to `getReserves` at the requested block. It does not sign, submit, or execute trades.
