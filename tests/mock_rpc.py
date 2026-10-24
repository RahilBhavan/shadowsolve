#!/usr/bin/env python3
"""Deterministic read-only Ethereum JSON-RPC fixture for the shadow-stack E2E."""

from __future__ import annotations

import argparse
import json
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

POOL = "0x0000000000000000000000000000000000000001"
mode = "ready"
# One wall-clock base for the process so ready/reorg stay fresh and identical
# across mode toggles; stale subtracts an hour without changing block hashes.
base_timestamp = int(time.time())
mode_timestamp = base_timestamp


def set_mode(next_mode: str) -> None:
    """Switch fixture mode without rewriting canonical block identities."""
    global mode, mode_timestamp
    mode = next_mode
    mode_timestamp = base_timestamp - (3600 if next_mode == "stale" else 0)


def block(number: int, current_mode: str) -> dict[str, str]:
    del current_mode
    suffix = "b" if mode == "reorg" and number == 18 else "a"
    parent_suffix = "a"
    return {
        "number": hex(number),
        "hash": f"0x{suffix}{number:063x}",
        "parentHash": f"0x{parent_suffix}{number - 1:063x}",
        "timestamp": hex(mode_timestamp),
    }


class Handler(BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        if self.path == "/health":
            payload = b"ok"
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return
        if self.path in {"/mode/ready", "/mode/stale", "/mode/reorg"}:
            set_mode(self.path.rsplit("/", 1)[1])
            self.send_response(204)
            self.end_headers()
            return
        self.send_error(404)

    def do_POST(self) -> None:
        try:
            length = int(self.headers.get("Content-Length", "0"))
            request = json.loads(self.rfile.read(length))
            method = request["method"]
            params = request.get("params", [])
            if method == "eth_getBlockByNumber":
                tag = params[0]
                number = 20 if tag == "latest" else 18 if tag == "finalized" else int(tag, 16)
                result = block(number, mode)
            elif method == "eth_call" and params[0] == {"to": POOL, "data": "0x0902f1ac"}:
                result = "0x" + f"{100000:064x}{100000:064x}{0:064x}"
            elif method == "shadowsolve_poolSnapshot" and params[0] == POOL:
                tag = params[1] if len(params) > 1 else "latest"
                number = 20 if tag == "latest" else 18 if tag == "finalized" else int(tag, 16)
                result = {
                    "pool": POOL,
                    "blockHash": block(number, mode)["hash"],
                    "reserve0": "100000",
                    "reserve1": "100000",
                }
            else:
                raise ValueError("unsupported read method")
            response = {"jsonrpc": "2.0", "id": request["id"], "result": result}
            payload = json.dumps(response, separators=(",", ":")).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
        except (KeyError, TypeError, ValueError, json.JSONDecodeError) as error:
            payload = json.dumps({"error": str(error)}).encode()
            self.send_response(400)
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    def log_message(self, _format: str, *_args: object) -> None:
        return


def self_check() -> None:
    set_mode("ready")
    assert block(20, "ready")["number"] == "0x14"
    ready_hash = block(18, "ready")["hash"]
    ready_ts = int(block(18, "ready")["timestamp"], 16)
    assert block(20, "ready")["timestamp"] == block(18, "ready")["timestamp"]
    set_mode("reorg")
    assert block(18, "reorg")["hash"] != ready_hash
    set_mode("stale")
    stale_ts = int(block(18, "stale")["timestamp"], 16)
    assert stale_ts < ready_ts
    set_mode("ready")
    assert block(18, "ready")["hash"] == ready_hash
    assert int(block(18, "ready")["timestamp"], 16) == ready_ts


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("port", nargs="?", type=int, default=18545)
    parser.add_argument("--host", default="127.0.0.1")
    parser.add_argument("--self-check", action="store_true")
    args = parser.parse_args()
    if args.self_check:
        self_check()
    else:
        ThreadingHTTPServer((args.host, args.port), Handler).serve_forever()
