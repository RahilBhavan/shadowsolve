import json
import subprocess

import pytest
from shadowsolve.contracts import load_contract, validate

FIXTURES = {
    "auction": {"id":"a","block":1,"tokens":["WETH","USDC"],"orders":[{"id":"o","sell_token":"WETH","buy_token":"USDC","sell_amount":"10","min_buy_amount":"1","kind":"sell"}],"state":{"observed_block":1,"deadline_ms":1000}},
    "candidate": {"id":"c","auction_id":"a","routes":[{"pool_id":"p","sell_amount":"10","buy_amount":"9"}],"expected_surplus":"8","gas_proxy":100,"score":7,"predicted_validity":0.875,"risk_value":0.125,"eligible":True},
    "prediction": {"id":"p","label":"x","probability":"0"},
    "notification": {"schema_version":"1","auction_id":"a","outcome":"solved","candidate_id":None},
    "replay-label": {
        "id":"a:c:1","auction_id":"a","candidate_id":"c","network":"mainnet",
        "decision_block":"10","decision_hash":"h10","horizon":"1","future_block":"11",
        "future_block_hash":"h11","future_output":"9","feasible":True,
        "margin_remaining":"1","margin_erosion":"0","score_decay":"0",
    },
}

def test_contracts_load_and_validate():
    for name, value in FIXTURES.items(): assert validate(value, load_contract(name))

def test_rust_serialized_candidate_contract():
    output = subprocess.run(
        ["cargo", "run", "--quiet", "-p", "shadowsolve-solver-engine", "--", "--emit-contract-fixture"],
        capture_output=True,
        text=True,
        check=True,
    )
    solution = json.loads(output.stdout)
    assert set(solution) == {"auction_id", "candidates", "reasons"}
    candidate = solution["candidates"][0]
    assert validate(candidate, load_contract("candidate"))
    assert {"predicted_validity", "risk_value", "eligible"} <= candidate.keys()
    assert 0 <= candidate["predicted_validity"] <= 1
    assert isinstance(candidate["risk_value"], (int, float))
    assert isinstance(candidate["eligible"], bool)
    assert isinstance(candidate["expected_surplus"], str)
    assert all(isinstance(route[key], str) for route in candidate["routes"] for key in ("sell_amount", "buy_amount"))


def test_static_fallback_candidate_and_notifications():
    candidate = dict(FIXTURES["candidate"])
    for key in ("predicted_validity", "risk_value", "eligible"):
        candidate.pop(key)
    assert validate(candidate, load_contract("candidate"))
    assert validate({"schema_version":"1","auction_id":"a","outcome":"ok"}, load_contract("notification"))
    assert validate(dict(FIXTURES["notification"], message="retry later"), load_contract("notification"))


def test_contract_rejects_boundaries():
    schema = load_contract("auction")
    bad = dict(FIXTURES["auction"], extra=1)
    assert not validate(bad, schema)
    bad = dict(FIXTURES["auction"], orders=[dict(FIXTURES["auction"]["orders"][0], kind="unsupported")])
    assert not validate(bad, schema)
    bad["orders"][0]["sell_amount"] = "-1"
    assert not validate(bad, schema)
    for amount in ("00", "01"):
        bad = dict(FIXTURES["auction"], orders=[dict(FIXTURES["auction"]["orders"][0], sell_amount=amount)])
        assert not validate(bad, schema)
    for amount in ("0", "1", "10"):
        good = dict(FIXTURES["auction"], orders=[dict(FIXTURES["auction"]["orders"][0], sell_amount=amount)])
        assert validate(good, schema)

    candidate_schema = load_contract("candidate")
    bad = dict(FIXTURES["candidate"], extra=1)
    assert not validate(bad, candidate_schema)
    bad = dict(FIXTURES["candidate"], predicted_validity=1.01)
    assert not validate(bad, candidate_schema)
    for amount in ("00", "01"):
        bad = dict(FIXTURES["candidate"], expected_surplus=amount)
        assert not validate(bad, candidate_schema)
    for amount in ("0", "1", "10"):
        good = dict(FIXTURES["candidate"], expected_surplus=amount)
        assert validate(good, candidate_schema)
    bad = dict(FIXTURES["candidate"], risk_value=-1)
    assert not validate(bad, candidate_schema)

    notification_schema = load_contract("notification")
    bad = dict(FIXTURES["notification"], extra=1)
    assert not validate(bad, notification_schema)
    bad = dict(FIXTURES["notification"], message="x" * 129)
    assert not validate(bad, notification_schema)
    bad = dict(FIXTURES["notification"], candidate_id="")
    assert not validate(bad, notification_schema)
