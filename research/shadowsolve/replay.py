"""Deterministic, bounded constant-product future-state replay."""

from collections.abc import Sequence
from typing import Optional

from .types import Candidate, PoolState, ReplayLabel, U64_MAX, U128_MAX, require_uint


def _checked_add(left: int, right: int) -> int:
    value = left + right
    if value > U128_MAX:
        raise ValueError("u128 addition overflow")
    return value


def _checked_mul(left: int, right: int) -> int:
    value = left * right
    if value > U128_MAX:
        raise ValueError("u128 multiplication overflow")
    return value


def amount_out(amount_in: int, reserve_in: int, reserve_out: int, fee_bps: int) -> int:
    """Match the Rust constant-product integer arithmetic and overflow bounds."""
    require_uint(amount_in, "amount_in")
    require_uint(reserve_in, "reserve_in")
    require_uint(reserve_out, "reserve_out")
    require_uint(fee_bps, "fee_bps", 9_999)
    if amount_in == 0 or reserve_in == 0 or reserve_out == 0:
        raise ValueError("amount and reserves must be positive")
    net = _checked_mul(amount_in, 10_000 - fee_bps) // 10_000
    denominator = _checked_add(reserve_in, net)
    output = _checked_mul(reserve_out, net) // denominator
    if output >= reserve_out:
        raise ValueError("insufficient liquidity")
    return output


def _coherent_states(
    candidate: Candidate, states: Sequence[PoolState], block: int, block_hash: Optional[str] = None
) -> dict[str, PoolState]:
    if type(states) not in (tuple, list) or not states:
        raise ValueError("pool states must be a nonempty tuple or list")
    by_pool: dict[str, PoolState] = {}
    observed_hash: Optional[str] = None
    for state in states:
        if type(state) is not PoolState:
            raise ValueError("pool states must contain PoolState values")
        if state.concentrated:
            raise ValueError("concentrated liquidity is unsupported")
        if state.pool_id in by_pool:
            raise ValueError(f"duplicate pool state: {state.pool_id}")
        if state.network != candidate.network:
            raise ValueError("mixed or incorrect pool-state network")
        if state.block != block:
            raise ValueError("mixed or incorrect pool-state block")
        if observed_hash is not None and state.block_hash != observed_hash:
            raise ValueError("mixed pool-state block hashes")
        if block_hash is not None and state.block_hash != block_hash:
            raise ValueError("pool-state hash does not match as-of hash")
        observed_hash = state.block_hash
        by_pool[state.pool_id] = state
    route_pools = {route.pool_id for route in candidate.routes}
    supplied_pools = set(by_pool)
    if route_pools != supplied_pools:
        missing = sorted(route_pools - supplied_pools)
        extra = sorted(supplied_pools - route_pools)
        raise ValueError(f"pool-state set mismatch; missing={missing}, extra={extra}")
    return by_pool


def replay(candidate: Candidate, future_states: Sequence[PoolState], horizon: int) -> ReplayLabel:
    if type(candidate) is not Candidate:
        raise ValueError("candidate must be Candidate")
    require_uint(horizon, "horizon", U64_MAX)
    if horizon == 0 or candidate.decision_block > U64_MAX - horizon:
        raise ValueError("horizon must be positive and future block must fit u64")
    future_block = candidate.decision_block + horizon
    by_pool = _coherent_states(candidate, future_states, future_block)

    future_output = 0
    for route in candidate.routes:
        state = by_pool[route.pool_id]
        future_output = _checked_add(
            future_output,
            amount_out(route.sell_amount, state.reserve_in, state.reserve_out, state.fee_bps),
        )

    margin_remaining = future_output - candidate.committed_output
    initial_margin = candidate.auction_output - candidate.committed_output
    return ReplayLabel(
        auction_id=candidate.auction_id,
        candidate_id=candidate.candidate_id,
        network=candidate.network,
        decision_block=candidate.decision_block,
        decision_hash=candidate.decision_hash,
        horizon=horizon,
        future_block=future_block,
        future_block_hash=next(iter(by_pool.values())).block_hash,
        future_output=future_output,
        feasible=margin_remaining >= 0,
        margin_remaining=margin_remaining,
        margin_erosion=initial_margin - margin_remaining,
        score_decay=max(0, candidate.auction_output - future_output),
    )


def replay_horizons(
    candidate: Candidate,
    states_by_horizon: dict[int, Sequence[PoolState]],
    horizons: tuple[int, ...] = (1, 2, 3),
) -> tuple[ReplayLabel, ...]:
    if type(states_by_horizon) is not dict or type(horizons) is not tuple or not horizons:
        raise ValueError("states_by_horizon must be a dict and horizons a nonempty tuple")
    if len(set(horizons)) != len(horizons) or set(states_by_horizon) != set(horizons):
        raise ValueError("states must match each unique requested horizon exactly")
    return tuple(replay(candidate, states_by_horizon[horizon], horizon) for horizon in horizons)
