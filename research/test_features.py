import math
import pytest
from shadowsolve.features import decision_features, DecisionFeatures
from shadowsolve.types import Candidate, PoolState, Route

def candidate(cid="c", block=10, h="h10"):
    return Candidate("auction", cid, "mainnet", block, h, 175, 181, (Route("p", 100), Route("q", 50)))
def states(block=10, h="h10"):
    return [PoolState("p","mainnet",block,h,1000,2000,30), PoolState("q","mainnet",block,h,500,1000,20)]

def test_formulas_and_metadata_are_decision_anchored():
    f=decision_features(candidate(),states(),100,10,100,source_snapshot_hash="0"*64,state_snapshot_hash="1"*64)
    assert (f.initial_margin,f.route_count,f.split_count)==(6,2,1)
    assert f.margin_bps == pytest.approx(60000/175)
    assert f.aggregate_trade_liquidity_ratio == pytest.approx(150/1500)
    assert math.isfinite(f.aggregate_constant_product_price_impact)
    assert (f.decision_block,f.decision_hash,f.receive_block,f.receive_timestamp)==(10,"h10",10,100)
    assert f.source_snapshot_hash=="0"*64 and f.state_snapshot_hash=="1"*64

def test_rejects_future_receive_and_incoherent_snapshot():
    with pytest.raises(ValueError): decision_features(candidate(),states(),100,11,100,source_snapshot_hash='0'*64,state_snapshot_hash='1'*64)
    with pytest.raises(ValueError): decision_features(candidate(),states(),100,10,101,source_snapshot_hash='0'*64,state_snapshot_hash='1'*64)
    with pytest.raises(ValueError): decision_features(candidate(),states(10,"wrong"),source_snapshot_hash='0'*64,state_snapshot_hash='1'*64)

def test_rejects_invalid_arithmetic_and_nonfinite_values():
    with pytest.raises(ValueError): DecisionFeatures("a","c","n",1,"h",1,1,1,"s","t","v",2,1,0,1,0,0,0,1,0,())
    with pytest.raises(ValueError): DecisionFeatures("a","c","n",1,"h",1,1,1,1,"s","t","v",2,2,0,0,math.nan,0,0,0,1,0,())

def test_outcome_like_ids_are_rejected_without_banning_normal_ids():
    with pytest.raises(ValueError): decision_features(candidate("future-win"),states(),source_snapshot_hash='0'*64,state_snapshot_hash='1'*64)
    assert decision_features(candidate("candidate-2024"),states(),source_snapshot_hash='0'*64,state_snapshot_hash='1'*64).candidate_id=="candidate-2024"
