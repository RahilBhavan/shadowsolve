#!/usr/bin/env python3
"""Verify pinned constant-product pool math against mock or live read-only RPC."""

from __future__ import annotations

import argparse
import json
import sys
import urllib.request
from typing import Any

# Canonical Uniswap V2 USDC/WETH pair (token0=USDC, token1=WETH).
PINNED_POOL = "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"
GET_RESERVES = "0x0902f1ac"
FEE_BPS = 30


def amount_out(amount_in: int, reserve_in: int, reserve_out: int, fee_bps: int = FEE_BPS) -> int:
    """Mirror crates/pool-math constant-product integer math."""
    if amount_in <= 0 or reserve_in <= 0 or reserve_out <= 0 or fee_bps >= 10_000:
        raise ValueError("invalid swap inputs")
    net = amount_in * (10_000 - fee_bps) // 10_000
    out = reserve_out * net // (reserve_in + net)
    if out >= reserve_out:
        raise ValueError("insufficient liquidity")
    return out


def decode_reserves(data: str) -> tuple[int, int]:
    raw = data[2:] if data.startswith("0x") else data
    if len(raw) < 128:
        raise ValueError("invalid getReserves payload")
    reserve0 = int(raw[0:64], 16)
    reserve1 = int(raw[64:128], 16)
    return reserve0, reserve1


def rpc_call(url: str, method: str, params: list[Any]) -> Any:
    payload = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    request = urllib.request.Request(url, data=payload, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=20) as response:
        body = json.loads(response.read().decode())
    if "error" in body:
        raise RuntimeError(body["error"])
    return body["result"]


def self_check() -> None:
    # Deterministic CP identity used by the mock fixture (equal reserves).
    assert amount_out(100, 100_000, 100_000, 30) == 98
    assert amount_out(100, 1000, 1000, 30) == 90
    # Decode a packed ABI getReserves-style payload.
    payload = "0x" + f"{100000:064x}{200000:064x}{0:064x}"
    assert decode_reserves(payload) == (100000, 200000)
    print("verify-pinned-pool self-check passed")


def verify_live(url: str, pool: str, amount_in: int, sell_token0: bool, block: str) -> None:
    result = rpc_call(url, "eth_call", [{"to": pool, "data": GET_RESERVES}, block])
    reserve0, reserve1 = decode_reserves(result)
    if sell_token0:
        out = amount_out(amount_in, reserve0, reserve1)
        direction = "token0->token1"
    else:
        out = amount_out(amount_in, reserve1, reserve0)
        direction = "token1->token0"
    print(
        json.dumps(
            {
                "pool": pool,
                "block": block,
                "reserve0": str(reserve0),
                "reserve1": str(reserve1),
                "direction": direction,
                "amount_in": str(amount_in),
                "amount_out": str(out),
                "fee_bps": FEE_BPS,
                "note": "advisory math only; not an execution quote",
            },
            indent=2,
        )
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-check", action="store_true")
    parser.add_argument("--rpc-url", default="")
    parser.add_argument("--pool", default=PINNED_POOL)
    parser.add_argument("--amount-in", type=int, default=1_000_000)
    parser.add_argument("--sell-token0", action="store_true", default=True)
    parser.add_argument("--buy-token0", action="store_true", help="Sell token1 for token0")
    parser.add_argument("--block", default="finalized")
    args = parser.parse_args()
    if args.self_check:
        self_check()
        return 0
    if not args.rpc_url:
        print("Provide --rpc-url or use --self-check", file=sys.stderr)
        return 2
    sell_token0 = not args.buy_token0
    verify_live(args.rpc_url, args.pool, args.amount_in, sell_token0, args.block)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
