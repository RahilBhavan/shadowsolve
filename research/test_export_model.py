import copy
from hashlib import sha256
import json
import math
from pathlib import Path

import pytest

from shadowsolve.calibrate import Calibrated
from shadowsolve.export_model import export_model, load_model, predict
from shadowsolve.split import ModelObservation, Partition
from shadowsolve.train import (
    BaseRateModel,
    BoostedTreeModel,
    LogisticModel,
    Standardizer,
    fit_boosted_tree,
)


FIXTURE = Path(__file__).parents[1] / "fixtures" / "model-parity.json"
HASHES = {
    "feature_schema_hash": "0" * 64,
    "training_data_hash": "1" * 64,
    "calibration_data_hash": "2" * 64,
    "code_hash": "3" * 64,
    "config_hash": "4" * 64,
}


def _logistic():
    return LogisticModel(
        Standardizer((10.0, -2.0, 0.5), (2.0, 4.0, 0.25)),
        (1.25, -0.75, 2.0),
        -0.4,
        ("liquidity", "price_impact", "gas_ratio"),
        1,
        {},
    )


def _export(model, path, **kwargs):
    return export_model(
        model,
        path,
        **HASHES,
        created_at="2026-08-04T12:00:00Z",
        **kwargs,
    )


def _canonical(value):
    return json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=True,
        allow_nan=False,
    )


def _write_artifact(path, artifact, *, rehash=True):
    value = copy.deepcopy(artifact)
    if rehash:
        body = {key: item for key, item in value.items() if key != "artifact_sha256"}
        value["artifact_sha256"] = sha256(_canonical(body).encode()).hexdigest()
    path.write_text(_canonical(value), encoding="utf-8")
    return value


def _fixture_artifact():
    return json.loads(FIXTURE.read_text(encoding="utf-8"))["artifact"]


def test_checked_in_fixture_is_generated_from_canonical_calibrated_export(tmp_path):
    fixture = json.loads(FIXTURE.read_text(encoding="utf-8"))
    calibrated = Calibrated(_logistic(), 0.8, -0.15, "platt", {})
    artifact = _export(calibrated, tmp_path / "model.json")
    assert artifact == fixture["artifact"]
    for vector in fixture["vectors"]:
        assert predict(artifact, vector["features"]) == vector["probability"]


def test_export_is_canonical_deterministic_and_round_trips(tmp_path):
    first_path = tmp_path / "first.json"
    second_path = tmp_path / "second.json"
    assert _export(_logistic(), first_path) == _export(_logistic(), second_path)
    assert first_path.read_bytes() == second_path.read_bytes()
    assert first_path.read_bytes().endswith(b"\n")
    assert load_model(first_path) == _fixture_artifact() | {
        "calibration": {"method": "none", "slope": 1.0, "intercept": 0.0},
        "artifact_sha256": load_model(first_path)["artifact_sha256"],
    }


def _tree_partition(labels):
    rows = tuple(
        ModelObservation(i, i, f"auction-{i}", "candidate", 1, (0.0,), label, 0.0, ("x",))
        for i, label in enumerate(labels, 1)
    )
    return Partition("train", rows, (10, 20), tuple(row.auction_id for row in rows))


def test_fitted_constant_tree_exports_and_round_trips_extreme_predictions(tmp_path):
    model = fit_boosted_tree(_tree_partition([False, True, False, True]), iterations=8)
    assert model.trees == ()
    path = tmp_path / "constant-fit.json"
    _export(model, path)
    loaded = load_model(path)
    for value in (0.0, 1e308, -1e308):
        assert predict(path, {"x": value}) == pytest.approx(model.predict_proba((value,)))
        assert predict(loaded, {"x": value}) == pytest.approx(model.predict_proba((value,)))


@pytest.mark.parametrize("labels", [[True, True, True, True], [False, False, False, False]])
def test_all_same_label_tree_export_round_trip(labels, tmp_path):
    model = fit_boosted_tree(_tree_partition(labels), iterations=8)
    path = tmp_path / "same-label.json"
    _export(model, path)
    for value in (-1e3, 0.0, 1e3):
        result = predict(path, {"x": value})
        assert math.isfinite(result) and 0.0 <= result <= 1.0
        assert result == pytest.approx(model.predict_proba((value,)))


def test_intercept_only_boosted_tree_exports_and_predicts_at_extreme_endpoint(tmp_path):
    model = BoostedTreeModel(("x",), -0.25, 0.1, (), 0)
    artifact = _export(model, tmp_path / "tree.json")
    assert artifact["model"]["trees"] == []
    assert predict(tmp_path / "tree.json", {"x": 1e308}) == pytest.approx(model.predict_proba((1e308,)))


def test_base_rate_export_and_named_prediction(tmp_path):
    artifact = _export(BaseRateModel(0.25, ("x", "y")), tmp_path / "base.json")
    assert artifact["model"] == {
        "kind": "base-rate",
        "feature_names": ["x", "y"],
        "rate": 0.25,
    }
    assert predict(artifact, {"x": -1e300, "y": 1e300}) == 0.25


def test_calibrated_platt_export_matches_python_model(tmp_path):
    calibrated = Calibrated(_logistic(), 0.8, -0.15, "platt", {})
    artifact = _export(calibrated, tmp_path / "platt.json")
    features = {"gas_ratio": 0.75, "price_impact": 2.0, "liquidity": 12.0}
    assert artifact["calibration"] == {
        "method": "platt",
        "slope": 0.8,
        "intercept": -0.15,
    }
    ordered = tuple(features[name] for name in _logistic().feature_names)
    assert predict(artifact, features) == pytest.approx(
        calibrated.predict_proba(ordered), abs=1e-15
    )


@pytest.mark.parametrize("rate", [0.0, 1.0])
def test_calibrated_identity_export_preserves_endpoint_probabilities(tmp_path, rate):
    calibrated = Calibrated(
        BaseRateModel(rate, ("x",)), 1.0, 0.0, "identity", {"iterations": 0}
    )
    artifact = _export(calibrated, tmp_path / f"identity-{rate}.json")
    assert artifact["calibration"]["method"] == "none"
    assert predict(artifact, {"x": 0.0}) == rate


@pytest.mark.parametrize("features", [{"x": 1.0}, {"x": 1.0, "y": 2.0}])
def test_named_prediction_rejects_missing_and_extra_keys(features):
    with pytest.raises(ValueError, match="mapping mismatch"):
        predict(_fixture_artifact(), features)


@pytest.mark.parametrize("features", [[10.0, -2.0, 0.5], (10.0, -2.0, 0.5), None])
def test_named_prediction_rejects_positional_or_absent_inputs(features):
    with pytest.raises(ValueError, match="mapping mismatch"):
        predict(_fixture_artifact(), features)


def test_named_prediction_uses_schema_order_not_mapping_insertion_order():
    features = {"gas_ratio": 0.75, "price_impact": 2.0, "liquidity": 12.0}
    assert predict(_fixture_artifact(), features) == 0.8220063142137535


@pytest.mark.parametrize("bad", [True, float("nan"), float("inf"), "10", 10**400])
def test_named_prediction_rejects_non_strict_or_nonfinite_numbers(bad):
    features = {"liquidity": bad, "price_impact": -2.0, "gas_ratio": 0.5}
    with pytest.raises(ValueError, match="invalid features"):
        predict(_fixture_artifact(), features)


@pytest.mark.parametrize("location,field", [("model", "intercept"), ("calibration", "slope")])
def test_export_and_load_reject_oversized_integer_parameters(tmp_path, location, field):
    artifact = _fixture_artifact()
    artifact[location][field] = 10**400
    with pytest.raises(ValueError, match="invalid numeric"):
        _write_artifact(tmp_path / "oversized.json", artifact)
        load_model(tmp_path / "oversized.json")


def test_load_rejects_tampered_artifact_hash(tmp_path):
    artifact = _fixture_artifact()
    artifact["model"]["intercept"] = 123.0
    path = tmp_path / "tampered.json"
    _write_artifact(path, artifact, rehash=False)
    with pytest.raises(ValueError, match="hash mismatch"):
        load_model(path)


@pytest.mark.parametrize("location", ["top", "model", "calibration"])
def test_load_rejects_extra_fields_at_every_schema_level(tmp_path, location):
    artifact = _fixture_artifact()
    target = artifact if location == "top" else artifact[location]
    target["unexpected"] = 1
    path = tmp_path / f"extra-{location}.json"
    _write_artifact(path, artifact)
    with pytest.raises(ValueError):
        load_model(path)


@pytest.mark.parametrize("value", ["A" * 64, "0" * 63, "g" * 64, 0])
def test_load_rejects_malformed_metadata_hashes(tmp_path, value):
    artifact = _fixture_artifact()
    artifact["code_hash"] = value
    path = tmp_path / "bad-hash.json"
    _write_artifact(path, artifact)
    with pytest.raises(ValueError, match="invalid hash"):
        load_model(path)


@pytest.mark.parametrize(
    "value", ["2026-08-04", "2026-02-30T12:00:00Z", "2026-08-04T24:00:00Z"]
)
def test_load_rejects_noncanonical_or_impossible_timestamps(tmp_path, value):
    artifact = _fixture_artifact()
    artifact["created_at"] = value
    path = tmp_path / "bad-time.json"
    _write_artifact(path, artifact)
    with pytest.raises(ValueError, match="created_at"):
        load_model(path)


@pytest.mark.parametrize(
    "names", [[], ["x", "x", "z"], ["1bad", "price_impact", "gas_ratio"], ["x-y", "p", "g"]]
)
def test_load_rejects_empty_duplicate_or_invalid_feature_names(tmp_path, names):
    artifact = _fixture_artifact()
    artifact["model"]["feature_names"] = names
    path = tmp_path / "bad-names.json"
    _write_artifact(path, artifact)
    with pytest.raises(ValueError, match="feature names|width"):
        load_model(path)


@pytest.mark.parametrize("field", ["means", "scales", "weights"])
def test_load_rejects_logistic_array_width_mismatch(tmp_path, field):
    artifact = _fixture_artifact()
    artifact["model"][field].pop()
    path = tmp_path / f"bad-{field}.json"
    _write_artifact(path, artifact)
    with pytest.raises(ValueError, match="width"):
        load_model(path)


def test_load_rejects_nonpositive_logistic_scale(tmp_path):
    artifact = _fixture_artifact()
    artifact["model"]["scales"][1] = 0.0
    path = tmp_path / "bad-scale.json"
    _write_artifact(path, artifact)
    with pytest.raises(ValueError, match="scales"):
        load_model(path)


@pytest.mark.parametrize(
    "calibration",
    [
        {"method": "none", "slope": 0.5, "intercept": 0.0},
        {"method": "identity", "slope": 1.0, "intercept": 0.0},
        {"method": "platt", "slope": math.inf, "intercept": 0.0},
    ],
)
def test_load_rejects_invalid_calibration_contract(tmp_path, calibration):
    artifact = _fixture_artifact()
    artifact["calibration"] = calibration
    path = tmp_path / "bad-calibration.json"
    if math.isfinite(calibration["slope"]):
        _write_artifact(path, artifact)
    else:
        path.write_text(json.dumps(artifact), encoding="utf-8")
    with pytest.raises(ValueError):
        load_model(path)


@pytest.mark.parametrize("calibration", [{}, False, 0])
def test_export_rejects_falsey_calibration_values(tmp_path, calibration):
    with pytest.raises(ValueError, match="invalid calibration"):
        _export(_logistic(), tmp_path / "bad-calibration.json", calibration=calibration)


def test_export_omitted_calibration_uses_identity(tmp_path):
    artifact = _export(_logistic(), tmp_path / "default-calibration.json")
    assert artifact["calibration"] == {"method": "none", "slope": 1.0, "intercept": 0.0}


def test_export_rejects_unsupported_model_without_creating_file(tmp_path):
    path = tmp_path / "unsupported.json"
    with pytest.raises(ValueError, match="unsupported model"):
        _export(object(), path)
    assert not path.exists()


def test_export_creates_parent_directories_and_loads_from_path(tmp_path):
    path = tmp_path / "nested" / "models" / "model.json"
    artifact = _export(_logistic(), path)
    assert path.is_file()
    features = {"liquidity": 10.0, "price_impact": -2.0, "gas_ratio": 0.5}
    assert predict(path, features) == predict(artifact, features)
