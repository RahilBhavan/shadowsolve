"""Strict immutable types for the bounded ShadowSolve replay slice."""

from dataclasses import dataclass
from typing import Mapping


U64_MAX = 2**64 - 1
U128_MAX = 2**128 - 1


def require_text(value: object, name: str) -> str:
    if type(value) is not str or not value or value != value.strip():
        raise ValueError(f"{name} must be a nonempty, trimmed string")
    return value


def require_uint(value: object, name: str, maximum: int = U128_MAX) -> int:
    if type(value) is not int or not 0 <= value <= maximum:
        raise ValueError(f"{name} must be an integer from 0 through {maximum}")
    return value


def decimal_to_uint(value: object, name: str = "value", maximum: int = U128_MAX) -> int:
    """Parse the canonical unsigned decimal representation used by JSON contracts."""
    if type(value) is not str or not value or not value.isascii() or not value.isdecimal():
        raise ValueError(f"{name} must be an unsigned decimal string")
    if len(value) > 1 and value[0] == "0":
        raise ValueError(f"{name} must be canonical (no leading zeroes)")
    parsed = int(value)
    return require_uint(parsed, name, maximum)


def _exact_mapping(value: object, fields: frozenset[str], name: str) -> Mapping[str, object]:
    if type(value) is not dict or frozenset(value) != fields:
        raise ValueError(f"{name} must contain exactly: {', '.join(sorted(fields))}")
    return value


@dataclass(frozen=True)
class Route:
    pool_id: str
    sell_amount: int

    def __post_init__(self) -> None:
        require_text(self.pool_id, "pool_id")
        require_uint(self.sell_amount, "sell_amount")
        if self.sell_amount == 0:
            raise ValueError("sell_amount must be positive")

    @classmethod
    def from_mapping(cls, value: object) -> "Route":
        row = _exact_mapping(value, frozenset(("pool_id", "sell_amount")), "route")
        return cls(
            require_text(row["pool_id"], "pool_id"),
            decimal_to_uint(row["sell_amount"], "sell_amount"),
        )


@dataclass(frozen=True)
class Candidate:
    auction_id: str
    candidate_id: str
    network: str
    decision_block: int
    decision_hash: str
    committed_output: int
    auction_output: int
    routes: tuple[Route, ...]

    def __post_init__(self) -> None:
        for value, name in (
            (self.auction_id, "auction_id"),
            (self.candidate_id, "candidate_id"),
            (self.network, "network"),
            (self.decision_hash, "decision_hash"),
        ):
            require_text(value, name)
        require_uint(self.decision_block, "decision_block", U64_MAX)
        require_uint(self.committed_output, "committed_output")
        require_uint(self.auction_output, "auction_output")
        if type(self.routes) is not tuple or not self.routes:
            raise ValueError("routes must be a nonempty tuple")
        if any(type(route) is not Route for route in self.routes):
            raise ValueError("routes must contain Route values")
        if len({route.pool_id for route in self.routes}) != len(self.routes):
            raise ValueError("routes must use unique pools")
        if self.auction_output < self.committed_output:
            raise ValueError("auction_output cannot be below commitment")

    @classmethod
    def from_mapping(cls, value: object) -> "Candidate":
        fields = frozenset(
            (
                "auction_id", "candidate_id", "network", "decision_block", "decision_hash",
                "committed_output", "auction_output", "routes",
            )
        )
        row = _exact_mapping(value, fields, "candidate")
        raw_routes = row["routes"]
        if type(raw_routes) is not list:
            raise ValueError("routes must be a list")
        return cls(
            auction_id=require_text(row["auction_id"], "auction_id"),
            candidate_id=require_text(row["candidate_id"], "candidate_id"),
            network=require_text(row["network"], "network"),
            decision_block=decimal_to_uint(row["decision_block"], "decision_block", U64_MAX),
            decision_hash=require_text(row["decision_hash"], "decision_hash"),
            committed_output=decimal_to_uint(row["committed_output"], "committed_output"),
            auction_output=decimal_to_uint(row["auction_output"], "auction_output"),
            routes=tuple(Route.from_mapping(route) for route in raw_routes),
        )


@dataclass(frozen=True)
class PoolState:
    pool_id: str
    network: str
    block: int
    block_hash: str
    reserve_in: int
    reserve_out: int
    fee_bps: int
    concentrated: bool = False

    def __post_init__(self) -> None:
        for value, name in (
            (self.pool_id, "pool_id"),
            (self.network, "network"),
            (self.block_hash, "block_hash"),
        ):
            require_text(value, name)
        require_uint(self.block, "block", U64_MAX)
        require_uint(self.reserve_in, "reserve_in")
        require_uint(self.reserve_out, "reserve_out")
        require_uint(self.fee_bps, "fee_bps", 9_999)
        if self.reserve_in == 0 or self.reserve_out == 0:
            raise ValueError("reserves must be positive")
        if type(self.concentrated) is not bool:
            raise ValueError("concentrated must be boolean")

    @classmethod
    def from_mapping(cls, value: object) -> "PoolState":
        fields = frozenset(
            ("pool_id", "network", "block", "block_hash", "reserve_in", "reserve_out", "fee_bps", "concentrated")
        )
        row = _exact_mapping(value, fields, "pool state")
        return cls(
            pool_id=require_text(row["pool_id"], "pool_id"),
            network=require_text(row["network"], "network"),
            block=decimal_to_uint(row["block"], "block", U64_MAX),
            block_hash=require_text(row["block_hash"], "block_hash"),
            reserve_in=decimal_to_uint(row["reserve_in"], "reserve_in"),
            reserve_out=decimal_to_uint(row["reserve_out"], "reserve_out"),
            fee_bps=decimal_to_uint(row["fee_bps"], "fee_bps", 9_999),
            concentrated=row["concentrated"],
        )


@dataclass(frozen=True)
class ReplayLabel:
    auction_id: str
    candidate_id: str
    network: str
    decision_block: int
    decision_hash: str
    horizon: int
    future_block: int
    future_block_hash: str
    future_output: int
    feasible: bool
    margin_remaining: int
    margin_erosion: int
    score_decay: int

    def __post_init__(self) -> None:
        for value, name in (
            (self.auction_id, "auction_id"),
            (self.candidate_id, "candidate_id"),
            (self.network, "network"),
            (self.decision_hash, "decision_hash"),
            (self.future_block_hash, "future_block_hash"),
        ):
            require_text(value, name)
        require_uint(self.decision_block, "decision_block", U64_MAX)
        require_uint(self.horizon, "horizon", U64_MAX)
        require_uint(self.future_block, "future_block", U64_MAX)
        require_uint(self.future_output, "future_output")
        require_uint(self.score_decay, "score_decay")
        if self.horizon == 0 or self.future_block != self.decision_block + self.horizon:
            raise ValueError("future_block must equal decision_block plus a positive horizon")
        if type(self.feasible) is not bool:
            raise ValueError("feasible must be boolean")
        for value, name in (
            (self.margin_remaining, "margin_remaining"),
            (self.margin_erosion, "margin_erosion"),
        ):
            if type(value) is not int or not -U128_MAX <= value <= U128_MAX:
                raise ValueError(f"{name} is outside signed replay bounds")
        if self.feasible != (self.margin_remaining >= 0):
            raise ValueError("feasible must match margin_remaining")
