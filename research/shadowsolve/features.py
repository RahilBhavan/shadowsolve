"""Immutable decision-time features with strict provenance and arithmetic validation."""
from collections.abc import Sequence
from dataclasses import dataclass
from typing import Optional
import math
import re
from .replay import _coherent_states, amount_out
from .types import Candidate, PoolState, U64_MAX, U128_MAX, require_text, require_uint


def _finite(value: object, name: str) -> float:
    if type(value) not in (int, float) or not math.isfinite(value):
        raise ValueError(f"{name} must be finite")
    return float(value)

@dataclass(frozen=True)
class RouteFeature:
    pool_id: str; sell_amount: int; reserve_in: int; reserve_out: int; fee_bps: int
    def __post_init__(self):
        require_text(self.pool_id, "pool_id")
        for v,n in ((self.sell_amount,"sell_amount"),(self.reserve_in,"reserve_in"),(self.reserve_out,"reserve_out")):
            require_uint(v,n)
            if v == 0: raise ValueError(f"{n} must be positive")
        require_uint(self.fee_bps,"fee_bps",9999)

@dataclass(frozen=True)
class DecisionFeatures:
    auction_id: str; candidate_id: str; network: str
    decision_block: int; decision_hash: str; decision_timestamp: int
    receive_block: int; receive_timestamp: int
    source_snapshot_hash: str; state_snapshot_hash: str; feature_schema_version: str
    committed_output: int; auction_output: int; gas_proxy: int; initial_margin: int; margin_bps: float
    aggregate_trade_liquidity_ratio: float; aggregate_constant_product_price_impact: float
    route_count: int; split_count: int; routes: tuple[RouteFeature,...]
    raw_unit: str = "network-pair-native"
    raw_unit_config: str = "mvp-network-pair"
    def __post_init__(self):
        for v,n in ((self.auction_id,"auction_id"),(self.candidate_id,"candidate_id"),(self.network,"network"),(self.decision_hash,"decision_hash"),(self.source_snapshot_hash,"source_snapshot_hash"),(self.state_snapshot_hash,"state_snapshot_hash"),(self.feature_schema_version,"feature_schema_version"),(self.raw_unit,"raw_unit"),(self.raw_unit_config,"raw_unit_config")): require_text(v,n)
        if self.raw_unit != "network-pair-native" or self.raw_unit_config != "mvp-network-pair": raise ValueError("unsupported raw-unit metadata")
        if not re.fullmatch(r"[0-9a-f]{64}", self.source_snapshot_hash) or not re.fullmatch(r"[0-9a-f]{64}", self.state_snapshot_hash): raise ValueError("snapshot hashes must be lowercase SHA-256")
        for value,name in ((self.auction_id,"auction_id"),(self.candidate_id,"candidate_id")):
            if not re.fullmatch(r"[A-Za-z0-9._~-]+", value): raise ValueError(f"{name} has invalid identifier grammar")
        for v,n,m in ((self.decision_block,"decision_block",U64_MAX),(self.receive_block,"receive_block",U64_MAX),(self.decision_timestamp,"decision_timestamp",U64_MAX),(self.receive_timestamp,"receive_timestamp",U64_MAX),(self.committed_output,"committed_output",U128_MAX),(self.auction_output,"auction_output",U128_MAX),(self.gas_proxy,"gas_proxy",U128_MAX),(self.initial_margin,"initial_margin",U128_MAX),(self.route_count,"route_count",U64_MAX),(self.split_count,"split_count",U64_MAX)): require_uint(v,n,m)
        if self.receive_block > self.decision_block or self.receive_timestamp > self.decision_timestamp: raise ValueError("feature receive cannot be in the future")
        if type(self.routes) is not tuple or not self.routes or any(type(r) is not RouteFeature for r in self.routes): raise ValueError("routes must be a nonempty tuple of RouteFeature values")
        if self.auction_output < self.committed_output or self.initial_margin != self.auction_output-self.committed_output: raise ValueError("invalid margin")
        if self.route_count != len(self.routes) or self.split_count != max(0,len(self.routes)-1): raise ValueError("route counts do not match routes")
        margin_bps = self.initial_margin / self.committed_output * 10000 if self.committed_output else 0
        ratio = sum(r.sell_amount for r in self.routes) / sum(r.reserve_in for r in self.routes)
        expected_impact = sum(1-(amount_out(r.sell_amount,r.reserve_in,r.reserve_out,r.fee_bps)/(r.sell_amount*r.reserve_out/r.reserve_in)) for r in self.routes)/len(self.routes)
        for v,n in ((self.margin_bps,"margin_bps"),(self.aggregate_trade_liquidity_ratio,"aggregate_trade_liquidity_ratio"),(self.aggregate_constant_product_price_impact,"aggregate_constant_product_price_impact")): _finite(v,n)
        if self.aggregate_trade_liquidity_ratio < 0 or self.aggregate_constant_product_price_impact < 0 or self.aggregate_constant_product_price_impact > 1: raise ValueError("aggregate metrics outside bounds")
        if not math.isclose(self.margin_bps,margin_bps,rel_tol=1e-12) or not math.isclose(self.aggregate_trade_liquidity_ratio,ratio,rel_tol=1e-12) or not math.isclose(self.aggregate_constant_product_price_impact,expected_impact,rel_tol=1e-12): raise ValueError("derived feature mismatch")
        if len({r.pool_id for r in self.routes}) != len(self.routes): raise ValueError("routes must use unique pools")
    @property
    def as_of_block(self): return self.decision_block
    @property
    def as_of_hash(self): return self.decision_hash

def decision_features(candidate: Candidate, decision_states: Sequence[PoolState], decision_timestamp: int = 0, feature_receive_block: Optional[int] = None, feature_receive_timestamp: Optional[int] = None, *, source_snapshot_hash: Optional[str] = None, state_snapshot_hash: Optional[str] = None, feature_schema_version: str = "1", gas_proxy: int = 0, raw_unit: str = "network-pair-native", raw_unit_config: str = "mvp-network-pair") -> DecisionFeatures:
    if type(candidate) is not Candidate: raise ValueError("candidate must be Candidate")
    if not re.fullmatch(r"[A-Za-z0-9._~-]+", candidate.candidate_id): raise ValueError("candidate_id has invalid identifier grammar")
    if any(token in candidate.candidate_id.lower().replace("-",".").replace("_",".").split(".") for token in ("win","loss","fail","future","outcome","label")): raise ValueError("candidate_id must not encode outcome")
    rb = candidate.decision_block if feature_receive_block is None else feature_receive_block
    rt = decision_timestamp if feature_receive_timestamp is None else feature_receive_timestamp
    require_uint(decision_timestamp,"decision_timestamp",U64_MAX); require_uint(rb,"receive_block",U64_MAX); require_uint(rt,"receive_timestamp",U64_MAX)
    states = _coherent_states(candidate, decision_states, candidate.decision_block, candidate.decision_hash)
    routes=tuple(RouteFeature(r.pool_id,r.sell_amount,states[r.pool_id].reserve_in,states[r.pool_id].reserve_out,states[r.pool_id].fee_bps) for r in candidate.routes)
    if source_snapshot_hash is None or state_snapshot_hash is None or not re.fullmatch(r"[0-9a-f]{64}", source_snapshot_hash) or not re.fullmatch(r"[0-9a-f]{64}", state_snapshot_hash): raise ValueError("snapshot hashes must be explicit lowercase SHA-256")
    margin=candidate.auction_output-candidate.committed_output
    ratio=sum(r.sell_amount for r in routes)/sum(r.reserve_in for r in routes)
    impact=sum(1-(amount_out(r.sell_amount,r.reserve_in,r.reserve_out,r.fee_bps)/(r.sell_amount*r.reserve_out/r.reserve_in)) for r in routes)/len(routes)
    return DecisionFeatures(candidate.auction_id,candidate.candidate_id,candidate.network,candidate.decision_block,candidate.decision_hash,decision_timestamp,rb,rt,source_snapshot_hash,state_snapshot_hash,feature_schema_version,candidate.committed_output,candidate.auction_output,gas_proxy,margin,(margin/candidate.committed_output*10000 if candidate.committed_output else 0),ratio,impact,len(routes),max(0,len(routes)-1),routes,raw_unit,raw_unit_config)
