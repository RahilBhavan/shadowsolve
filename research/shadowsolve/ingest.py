"""Strict stdlib JSONL adapter for replay dataset publication."""
import json
from pathlib import Path
from typing import Optional

from .dataset import write_replay_dataset
from .features import decision_features
from .replay import replay_horizons
from .types import Candidate, PoolState, decimal_to_uint


def _unique_object_pairs(pairs):
    value = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate JSON object key: {key}")
        value[key] = item
    return value


def read_jsonl(path):
    records = []
    try:
        handle = open(path, encoding="utf-8")
    except OSError as exc:
        raise ValueError("unable to read JSONL input") from exc
    with handle:
        for line_number, line in enumerate(handle, 1):
            if not line.strip():
                raise ValueError(f"JSONL line {line_number} is blank")
            try:
                value = json.loads(line, object_pairs_hook=_unique_object_pairs)
            except json.JSONDecodeError as exc:
                raise ValueError(f"JSONL line {line_number} is malformed JSON") from exc
            except ValueError as exc:
                raise ValueError(f"JSONL line {line_number} is invalid: {exc}") from exc
            if type(value) is not dict:
                raise ValueError(f"JSONL line {line_number} must be a JSON object")
            records.append(value)
    return records


def candidate_from_record(record):
    return Candidate.from_mapping(record)


def pool_state_from_record(record):
    return PoolState.from_mapping(record)


def ingest(candidates_path, decision_states_path, future_states_path, output_path,
           manifest_path=None, *, decision_timestamp=0, feature_receive_block=None,
           feature_receive_timestamp=None, source_snapshot_hash=None,
           state_snapshot_hash=None, feature_schema_version="1", gas_proxy=0,
           raw_unit="network-pair-native", raw_unit_config="mvp-network-pair",
           expected_horizons=(1, 2, 3), provenance=None):
    candidates = sorted((candidate_from_record(r) for r in read_jsonl(candidates_path)),
                       key=lambda c: (c.auction_id, c.candidate_id))
    if len({(c.auction_id, c.candidate_id) for c in candidates}) != len(candidates):
        raise ValueError("duplicate candidate identity")
    decisions = [pool_state_from_record(r) for r in read_jsonl(decision_states_path)]
    decisions.sort(key=lambda s: s.pool_id)
    if len({s.pool_id for s in decisions}) != len(decisions):
        raise ValueError("duplicate decision pool")
    future = {}
    for record in read_jsonl(future_states_path):
        if set(record) != {"horizon", "state"}:
            raise ValueError("future state must contain exactly horizon and state")
        horizon = decimal_to_uint(record["horizon"], "horizon")
        if horizon not in expected_horizons:
            raise ValueError("future horizon is not expected")
        state = pool_state_from_record(record["state"])
        key = (horizon, state.pool_id)
        if key in future:
            raise ValueError("duplicate future horizon and pool")
        future[key] = state
    expected_horizons = tuple(sorted(expected_horizons))
    present = {h for h, _ in future}
    if present != set(expected_horizons):
        raise ValueError("missing or extra future horizons")
    by_horizon = {h: tuple(sorted((s for (hh, _), s in future.items() if hh == h), key=lambda s: s.pool_id))
                  for h in expected_horizons}
    features = tuple(decision_features(
        c, decisions, decision_timestamp, feature_receive_block, feature_receive_timestamp,
        source_snapshot_hash=source_snapshot_hash, state_snapshot_hash=state_snapshot_hash,
        feature_schema_version=feature_schema_version, gas_proxy=gas_proxy,
        raw_unit=raw_unit, raw_unit_config=raw_unit_config) for c in candidates)
    labels = tuple(label for c in candidates for label in replay_horizons(c, by_horizon, expected_horizons))
    return write_replay_dataset(output_path, features, labels, manifest_path,
                                expected_horizons=expected_horizons, provenance=provenance)
