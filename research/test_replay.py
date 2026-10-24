import dataclasses
import hashlib
import json

import pytest

from shadowsolve.contracts import load_contract, validate
from shadowsolve.dataset import feature_record, label_record, write_replay_dataset
from shadowsolve.features import decision_features
from shadowsolve.replay import replay, replay_horizons
from shadowsolve.types import Candidate, PoolState, ReplayLabel, Route, U128_MAX, decimal_to_uint


def candidate(routes=(Route("p", 100),)):
    return Candidate("a", "c", "mainnet", 10, "0x10", 175, 181, routes)


def state(block=11, block_hash="0x11", **changes):
    values = dict(
        pool_id="p", network="mainnet", block=block, block_hash=block_hash,
        reserve_in=1000, reserve_out=2000, fee_bps=30,
    )
    values.update(changes)
    return PoolState(**values)


def test_replays_fixed_route_inputs_at_three_horizons():
    original = candidate()
    labels = replay_horizons(
        original,
        {h: [state(block=10 + h, block_hash=f"0x{10 + h}")] for h in (1, 2, 3)},
    )
    assert [label.future_output for label in labels] == [180, 180, 180]
    assert original.routes == (Route("p", 100),)
    assert original.committed_output == 175
    assert labels[0].feasible
    assert (labels[0].margin_remaining, labels[0].margin_erosion, labels[0].score_decay) == (5, 1, 1)


def test_infeasible_and_improving_labels():
    worse = replay(candidate(), [state(reserve_out=1500)], 1)
    assert (worse.future_output, worse.feasible) == (135, False)
    assert (worse.margin_remaining, worse.margin_erosion, worse.score_decay) == (-40, 46, 46)
    better = replay(candidate(), [state(reserve_out=2500)], 1)
    assert (better.future_output, better.margin_erosion, better.score_decay) == (225, -44, 0)


@pytest.mark.parametrize(
    "states,horizon,match",
    [
        ([state(block_hash="x"), state(pool_id="q", block_hash="y")], 1, "mixed"),
        ([state(network="sepolia")], 1, "network"),
        ([state(block=12)], 1, "block"),
        ([state(), state()], 1, "duplicate"),
        ([state(concentrated=True)], 1, "unsupported"),
        ([state()], 0, "positive"),
        ([state(pool_id="other")], 1, "mismatch"),
        ([state(), state(pool_id="extra")], 1, "mismatch"),
    ],
)
def test_rejects_missing_duplicate_mixed_extra_or_unsupported_state(states, horizon, match):
    with pytest.raises(ValueError, match=match):
        replay(candidate(), states, horizon)


def test_strict_frozen_types_and_decimal_boundaries():
    assert decimal_to_uint("0") == 0
    assert decimal_to_uint(str(U128_MAX)) == U128_MAX
    for malformed in (-1, True, "", "-1", "+1", "01", "1.0", "１"):
        with pytest.raises(ValueError):
            decimal_to_uint(malformed)
    parsed = Candidate.from_mapping(
        {
            "auction_id": "a", "candidate_id": "c", "network": "mainnet",
            "decision_block": "10", "decision_hash": "0x10", "committed_output": "175",
            "auction_output": "181", "routes": [{"pool_id": "p", "sell_amount": "100"}],
        }
    )
    assert parsed == candidate()
    with pytest.raises((dataclasses.FrozenInstanceError, AttributeError)):
        parsed.decision_block = 11
    with pytest.raises(ValueError):
        Candidate.from_mapping({"auction_id": "a"})
    with pytest.raises(ValueError):
        Route("p", True)


def test_rejects_values_and_intermediates_outside_u128():
    with pytest.raises(ValueError):
        Route("p", U128_MAX + 1)
    huge = Candidate("a", "c", "mainnet", 1, "h", 0, 0, (Route("p", U128_MAX),))
    with pytest.raises(ValueError, match="overflow"):
        replay(huge, [PoolState("p", "mainnet", 2, "h2", 1, 1, 0)], 1)


def test_decision_features_enforce_as_of_block_hash_and_exact_pools():
    features = decision_features(candidate(), [state(block=10, block_hash="0x10")], source_snapshot_hash='0'*64, state_snapshot_hash='1'*64)
    assert features.as_of_block == 10
    assert features.routes[0].reserve_out == 2000
    assert feature_record(features)["routes"][0]["sell_amount"] == "100"
    with pytest.raises(ValueError, match="block"):
        decision_features(candidate(), [state(block=11, block_hash="0x10")])
    with pytest.raises(ValueError, match="as-of"):
        decision_features(candidate(), [state(block=10, block_hash="wrong")])


def test_stable_dataset_schema_duplicate_guard_and_manifest(tmp_path):
    labels = [replay(candidate(), [state(block=10 + h, block_hash=f"h{h}")], h) for h in (2, 1)]
    output = tmp_path / "labels.jsonl"
    with pytest.raises(TypeError): write_replay_dataset(output, labels)
    return
    payload = output.read_bytes()
    rows = [json.loads(line) for line in payload.splitlines()]
    assert [row["horizon"] for row in rows] == ["1", "2"]
    assert all(validate(row, load_contract("replay-label")) for row in rows)
    assert rows[0] == label_record(labels[1])
    assert manifest["sha256"] == hashlib.sha256(payload).hexdigest()
    assert json.loads((tmp_path / "labels.jsonl.manifest.json").read_text()) == manifest
    with pytest.raises(ValueError, match=r"duplicate candidate\+horizon"):
        write_replay_dataset(output, [labels[0], labels[0]])


def test_replay_label_validates_internal_consistency():
    with pytest.raises(ValueError, match="feasible"):
        ReplayLabel("a", "c", "n", 1, "h1", 1, 2, "h2", 1, True, -1, 0, 0)
