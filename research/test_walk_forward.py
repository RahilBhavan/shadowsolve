import json
from pathlib import Path

import pytest

from shadowsolve.split import ModelObservation, build_walk_forward_folds
from shadowsolve.walk_forward import read_walk_forward_report, walk_forward_report, write_walk_forward_report


def row(t, auction=None, candidate="c", feasible=False, feature=None):
    auction = auction or f"a{t}"
    return ModelObservation(t, t, auction, candidate, 1, (float(t if feature is None else feature),), feasible, 0.1, ("signal",))


def rows(count=5):
    return [row(t, feasible=t % 2 == 0) for t in range(1, count + 1)]


def test_folds_isolate_complete_timestamp_and_auction_buckets():
    values = [row(1, "a", "x"), row(1, "a", "y"), row(1, "b"), row(2, "c"), row(3, "d")]
    folds = build_walk_forward_folds(values)
    fold = folds[0]
    assert {x.decision_timestamp for x in fold.train.observations} == {1}
    assert {x.decision_timestamp for x in fold.calibration.observations} == {2}
    assert {x.decision_timestamp for x in fold.test.observations} == {3}
    assert set(fold.train.auction_ids).isdisjoint(fold.calibration.auction_ids + fold.test.auction_ids)
    with pytest.raises(ValueError):
        build_walk_forward_folds(values[:2])


def test_report_is_deterministic_and_has_fitted_provenance():
    report = walk_forward_report(rows(), logistic_kwargs={"iterations": 10}, boosted_tree_kwargs={"iterations": 4})
    shuffled = walk_forward_report(list(reversed(rows())), logistic_kwargs={"iterations": 10}, boosted_tree_kwargs={"iterations": 4})
    assert report == shuffled
    json.dumps(report, sort_keys=True, separators=(",", ":"), allow_nan=False)
    for fold in report["folds"]:
        assert {"base_rate", "logistic", "boosted_tree"} <= set(fold["models"])
        assert {"base_model_sha256", "calibration_parameters_sha256", "feature_schema_sha256",
                "train_data_sha256", "calibration_data_sha256", "split_sha256",
                "boosted_tree_base_model_sha256", "boosted_tree_final_model_sha256"} <= set(fold["provenance"])
    assert report["config_sha256"] == report["config_sha256"]
    assert {"base_rate", "logistic", "boosted_tree"} <= set(report["aggregate"])


def test_auprc_aggregate_tracks_defined_folds():
    report = walk_forward_report(rows())
    for model in report["aggregate"].values():
        assert model["auprc_defined_fold_count"] + model["auprc_undefined_fold_count"] == report["fold_count"]
        assert model["auprc_defined"] == (model["auprc"] is not None)


def test_publication_manifest_is_content_hash_and_idempotent(tmp_path):
    root = tmp_path / "report.json"
    first = write_walk_forward_report(root, rows())
    before = root.read_bytes(), root.with_suffix(".json.manifest.json").read_bytes()
    second = write_walk_forward_report(root, list(reversed(rows())))
    assert first == second
    assert before == (root.read_bytes(), root.with_suffix(".json.manifest.json").read_bytes())
    manifest = json.loads(before[1])
    content = root.with_name(manifest["report"])
    assert content.read_bytes() == root.read_bytes()
    assert manifest["sha256"] == first["sha256"]
    assert read_walk_forward_report(root.with_suffix(".json.manifest.json")) == walk_forward_report(rows())


@pytest.mark.parametrize("mutation", [
    lambda r: r.update(schema_version="bad"),
    lambda r: r.update(extra=True),
    lambda r: r.pop("fold_count"),
    lambda r: r.update(fold_count="1"),
    lambda r: r["config"].update(policy_threshold=float("nan")),
    lambda r: r.update(feature_schema_sha256="A" * 64),
    lambda r: r["folds"][0].update(index="0"),
    lambda r: r["folds"][0]["test"].update(candidates=0),
    lambda r: r["folds"][0]["models"]["base_rate"]["metrics"].update(count="1"),
    lambda r: r["folds"][0]["provenance"].pop("split_sha256"),
    lambda r: r["folds"][0]["provenance"].update(split_sha256="0" * 63),
    lambda r: r["folds"][0]["models"]["base_rate"]["metrics"].update(count=-1),
    lambda r: r["folds"][0]["paired"]["static"].update(eligible_count="bad"),
    lambda r: r["folds"][0]["paired"]["risk"].update(selected_count=-1),
    lambda r: r["aggregate"]["base_rate"].update(auprc_defined_fold_count=-1),
    lambda r: r["aggregate"]["base_rate"].update(auprc_defined_fold_count=99),
])
def test_publication_rejects_malformed_reports(tmp_path, mutation):
    root = tmp_path / "report.json"
    write_walk_forward_report(root, rows())
    manifest_path = root.with_suffix(".json.manifest.json")
    manifest = json.loads(manifest_path.read_bytes())
    report_path = root.with_name(manifest["report"])
    report = json.loads(report_path.read_bytes())
    mutation(report)
    data = json.dumps(report, sort_keys=True, separators=(",", ":"), allow_nan=True).encode() + b"\\n"
    report_path.write_bytes(data)
    manifest.update(bytes=len(data), sha256=__import__("hashlib").sha256(data).hexdigest())
    manifest_path.write_text(json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\\n")
    with pytest.raises(ValueError):
        read_walk_forward_report(manifest_path)


def test_publication_rejects_collisions_and_symlink_targets(tmp_path):
    root = tmp_path / "report.json"
    with pytest.raises(ValueError):
        write_walk_forward_report(root, rows(), manifest_path=root)
    link = tmp_path / "link.json"
    link.symlink_to(root)
    with pytest.raises(ValueError):
        write_walk_forward_report(link, rows())


def test_nonfinite_configuration_is_rejected(tmp_path):
    with pytest.raises((ValueError, TypeError)):
        walk_forward_report(rows(), policy_threshold=float("nan"))
    with pytest.raises((ValueError, TypeError)):
        write_walk_forward_report(tmp_path / "report.json", rows(), logistic_kwargs={"l2": float("nan")})


def test_hashes_ignore_last_bit_float_noise():
    from shadowsolve.walk_forward import _hash

    value = {"weights": [0.1 + 0.2, -0.0, 1.0 / 3.0], "bias": 2.5}
    noisy = {"weights": [0.30000000000000004 + 1e-16, 0.0, 1.0 / 3.0 + 3e-17], "bias": 2.5}
    assert _hash(value) == _hash(noisy)
    assert _hash(value) != _hash({"weights": [0.3 + 1e-6, 0.0, 1.0 / 3.0], "bias": 2.5})


def test_published_report_reproduces_from_the_publisher(tmp_path):
    import importlib.util

    root = Path(__file__).resolve().parents[1]
    spec = importlib.util.spec_from_file_location("publish_walk_forward", root / "scripts" / "publish-walk-forward.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    manifest = write_walk_forward_report(tmp_path / "report.json", module.observations(),
                                         logistic_kwargs={"iterations": 40}, boosted_tree_kwargs={"iterations": 8},
                                         policy_threshold=0.5)
    published = json.loads((root / "reports" / "walk-forward" / "report.json.manifest.json").read_text())
    assert manifest["sha256"] == published["sha256"]
    assert manifest["bytes"] == published["bytes"]
