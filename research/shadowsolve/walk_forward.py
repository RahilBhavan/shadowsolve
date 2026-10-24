"""Deterministic walk-forward evaluation and manifest-committed publication."""
import json, math, os, tempfile
from hashlib import sha256
from pathlib import Path
from typing import Any, Iterable
from .split import ModelObservation, build_walk_forward_folds
from .train import fit_base_rate, fit_logistic, fit_boosted_tree
from .calibrate import calibrate
from .evaluate import metrics, paired_policy


FLOAT_DECIMALS = 10


def _normalize(value: Any) -> Any:
    """Round floats so hashes survive last-bit libm differences across Python versions and platforms."""
    if isinstance(value, float):
        rounded = round(value, FLOAT_DECIMALS) if math.isfinite(value) else value
        return rounded + 0.0  # turns -0.0 into 0.0
    if isinstance(value, dict):
        return {key: _normalize(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_normalize(item) for item in value]
    return value


def _canonical(value: Any) -> bytes:
    return (json.dumps(_normalize(value), sort_keys=True, separators=(",", ":"), ensure_ascii=True, allow_nan=False) + "\n").encode()


def _hash(value: Any) -> str:
    return sha256(_canonical(value)).hexdigest()


def _row_identity(row: ModelObservation) -> dict:
    return {"decision_timestamp": row.decision_timestamp, "decision_block": row.decision_block,
            "auction_id": row.auction_id, "candidate_id": row.candidate_id, "horizon": row.horizon,
            "features": list(row.features), "feasible": row.feasible, "score_decay": row.score_decay,
            "feature_names": list(row.feature_names), "static_score": row.static_score,
            "risk_value": row.risk_value, "predicted_validity": row.predicted_validity,
            "future_feasible_score": row.future_feasible_score, "gas_proxy": row.gas_proxy}


def _partition_data(partition, *, include_outcomes=True):
    rows = partition.observations
    result = []
    for row in rows:
        item = _row_identity(row)
        if not include_outcomes:
            item.pop("feasible", None)
            item.pop("score_decay", None)
            item.pop("future_feasible_score", None)
        result.append(item)
    return result


def _validate_report(report):
    def obj(value, keys):
        if type(value) is not dict or set(value) != set(keys): raise ValueError("invalid report shape")
    def string(value, *, hash_value=False):
        if not isinstance(value, str) or (hash_value and (len(value) != 64 or any(c not in "0123456789abcdef" for c in value))): raise ValueError("invalid report value")
    def number(value, nullable=False):
        if value is None and nullable: return
        if type(value) not in (int, float) or not math.isfinite(value): raise ValueError("invalid report number")
    obj(report, {"schema_version", "config", "config_sha256", "feature_schema", "feature_schema_sha256", "fold_count", "folds", "aggregate"})
    if report["schema_version"] != "walk-forward-v1": raise ValueError("invalid report schema")
    string(report["config_sha256"], hash_value=True); string(report["feature_schema_sha256"], hash_value=True)
    if type(report["fold_count"]) is not int or report["fold_count"] != len(report["folds"]): raise ValueError("invalid fold count")
    if type(report["feature_schema"]) is not list or any(not isinstance(x, str) for x in report["feature_schema"]): raise ValueError("invalid feature schema")
    obj(report["config"], {"min_train_auctions", "min_calibration_auctions", "min_test_auctions", "logistic_kwargs", "boosted_tree_kwargs", "calibration_kwargs", "policy_threshold", "evaluation_horizon"})
    for key in ("min_train_auctions", "min_calibration_auctions", "min_test_auctions"):
        if type(report["config"][key]) is not int: raise ValueError("invalid config")
    number(report["config"]["policy_threshold"]); obj(report["config"]["logistic_kwargs"], set(report["config"]["logistic_kwargs"])); obj(report["config"]["boosted_tree_kwargs"], set(report["config"]["boosted_tree_kwargs"])); obj(report["config"]["calibration_kwargs"], set(report["config"]["calibration_kwargs"]))
    if report["config"]["evaluation_horizon"] is not None and type(report["config"]["evaluation_horizon"]) is not int: raise ValueError("invalid config")
    metric_keys = {"count", "positive_count", "brier", "log_loss", "auprc", "auprc_defined", "feasibility_base_rate", "mean_score_decay", "worst_decile_mean_decay", "cvar"}
    policy_keys = {"eligible_count", "selected_count", "no_eligible_count", "invalidation_rate", "mean_future_feasible_score", "mean_gas_adjusted_value", "mean_decay", "worst_decile_decay", "cvar_decay"}
    selection_keys = {"auction_id", "static_candidate", "risk_candidate", "status", "static_decay", "risk_decay", "static_future_feasible_score", "risk_future_feasible_score", "static_gas_adjusted_value", "risk_gas_adjusted_value", "score_sacrifice", "invalidation"}
    prov_keys = {"base_model_sha256", "calibration_parameters_sha256", "feature_schema_sha256", "train_data_sha256", "calibration_data_sha256", "split_sha256", "final_model_sha256", "logistic_base_model_sha256", "logistic_calibration_parameters_sha256", "logistic_final_model_sha256", "boosted_tree_base_model_sha256", "boosted_tree_calibration_parameters_sha256", "boosted_tree_final_model_sha256"}
    aggregate_keys = {"brier", "log_loss", "feasibility_base_rate", "mean_score_decay", "worst_decile_mean_decay", "cvar", "auprc_defined_fold_count", "auprc_undefined_fold_count", "auprc_defined", "auprc"}
    for fold in report["folds"]:
        obj(fold, {"index", "train", "calibration", "test", "models", "paired", "provenance"})
        if type(fold["index"]) is not int: raise ValueError("invalid fold")
        for part in ("train", "calibration", "test"):
            obj(fold[part], {"timestamps", "auctions", "candidates"})
            if type(fold[part]["timestamps"]) is not list or any(type(x) is not int for x in fold[part]["timestamps"]): raise ValueError("invalid partition")
            if type(fold[part]["auctions"]) is not int or type(fold[part]["candidates"]) is not int or fold[part]["candidates"] < fold[part]["auctions"]: raise ValueError("invalid partition")
        obj(fold["models"], {"base_rate", "logistic", "boosted_tree"})
        for model in fold["models"].values():
            obj(model, {"parameters_sha256", "metrics"}); string(model["parameters_sha256"], hash_value=True); obj(model["metrics"], metric_keys)
            if any(type(model["metrics"][key]) is not int or model["metrics"][key] < 0 for key in ("count", "positive_count")) or type(model["metrics"]["auprc_defined"]) is not bool: raise ValueError("invalid metrics")
            for key in metric_keys - {"count", "positive_count", "auprc", "auprc_defined"}: number(model["metrics"][key])
            number(model["metrics"]["auprc"], nullable=True)
            if model["metrics"]["auprc_defined"] != (model["metrics"]["auprc"] is not None): raise ValueError("invalid auprc")
        obj(fold["provenance"], prov_keys)
        if any(not isinstance(v, str) or len(v) != 64 or any(c not in "0123456789abcdef" for c in v) for v in fold["provenance"].values()): raise ValueError("invalid provenance")
        obj(fold["paired"], {"selections", "static", "risk", "overall_mean_score_sacrifice"});
        if type(fold["paired"]["selections"]) not in (list, tuple): raise ValueError("invalid paired")
        for selection in fold["paired"]["selections"]: obj(selection, selection_keys)
        for policy in ("static", "risk"):
            obj(fold["paired"][policy], policy_keys)
            if any(type(fold["paired"][policy][key]) is not int or fold["paired"][policy][key] < 0 for key in ("eligible_count", "selected_count", "no_eligible_count")): raise ValueError("invalid policy counts")
            [number(fold["paired"][policy][key], nullable=True) for key in policy_keys - {"eligible_count", "selected_count", "no_eligible_count"}]
        number(fold["paired"]["overall_mean_score_sacrifice"], nullable=True)
    obj(report["aggregate"], {"base_rate", "logistic", "boosted_tree"})
    for model in report["aggregate"].values():
        obj(model, aggregate_keys)
        for key in aggregate_keys - {"auprc_defined_fold_count", "auprc_undefined_fold_count", "auprc_defined", "auprc"}: number(model[key])
        if (any(type(model[k]) is not int or not 0 <= model[k] <= report["fold_count"] for k in ("auprc_defined_fold_count", "auprc_undefined_fold_count"))
                or model["auprc_defined_fold_count"] + model["auprc_undefined_fold_count"] != report["fold_count"]
                or type(model["auprc_defined"]) is not bool or model["auprc_defined"] != (model["auprc"] is not None)): raise ValueError("invalid aggregate auprc")
        number(model["auprc"], nullable=True)
    return report


def walk_forward_report(observations: Iterable[ModelObservation], *, min_train_auctions=1,
                        min_calibration_auctions=1, min_test_auctions=1,
                        logistic_kwargs=None, boosted_tree_kwargs=None, calibration_kwargs=None,
                        policy_threshold=0.0, evaluation_horizon=None) -> dict:
    rows = tuple(observations)
    if any(type(row) is not ModelObservation for row in rows):
        raise ValueError("observations must contain ModelObservation values")
    folds = build_walk_forward_folds(rows, min_train_auctions=min_train_auctions,
                                     min_calibration_auctions=min_calibration_auctions,
                                     min_test_auctions=min_test_auctions)
    names = rows[0].feature_names
    if any(row.feature_names != names for row in rows):
        raise ValueError("inconsistent feature schema")
    config = {"min_train_auctions": min_train_auctions, "min_calibration_auctions": min_calibration_auctions,
              "min_test_auctions": min_test_auctions, "logistic_kwargs": logistic_kwargs or {},
              "boosted_tree_kwargs": boosted_tree_kwargs or {},
              "calibration_kwargs": calibration_kwargs or {}, "policy_threshold": policy_threshold,
              "evaluation_horizon": evaluation_horizon}
    records = []
    for fold in folds:
        base = fit_base_rate(fold.train)
        logistic = fit_logistic(fold.train, **(logistic_kwargs or {}))
        tree = fit_boosted_tree(fold.train, **(boosted_tree_kwargs or {}))
        calibrated_base = calibrate(base, fold.calibration, **(calibration_kwargs or {}))
        calibrated_logistic = calibrate(logistic, fold.calibration, **(calibration_kwargs or {}))
        calibrated_tree = calibrate(tree, fold.calibration, **(calibration_kwargs or {}))
        split = {"index": fold.index, "train_cutoff": list(fold.train.cutoffs),
                 "calibration_cutoff": list(fold.calibration.cutoffs), "test_cutoff": list(fold.test.cutoffs),
                 "train_auctions": list(fold.train.auction_ids), "calibration_auctions": list(fold.calibration.auction_ids),
                 "test_auctions": list(fold.test.auction_ids)}
        provenance = {"base_model_sha256": _hash(base.parameters()),
                      "calibration_parameters_sha256": _hash({"model": base.parameters(), "config": calibration_kwargs or {}, "result": calibrated_base.parameters()}),
                      "feature_schema_sha256": _hash(list(names)),
                      "train_data_sha256": _hash(_partition_data(fold.train)),
                      "calibration_data_sha256": _hash(_partition_data(fold.calibration)),
                      "split_sha256": _hash(split),
                      "final_model_sha256": _hash(calibrated_base.parameters()),
                      "logistic_base_model_sha256": _hash(logistic.parameters()),
                      "logistic_calibration_parameters_sha256": _hash({"model": logistic.parameters(), "config": calibration_kwargs or {}, "result": calibrated_logistic.parameters()}),
                      "logistic_final_model_sha256": _hash(calibrated_logistic.parameters()),
                      "boosted_tree_base_model_sha256": _hash(tree.parameters()),
                      "boosted_tree_calibration_parameters_sha256": _hash({"model": tree.parameters(), "config": calibration_kwargs or {}, "result": calibrated_tree.parameters()}),
                      "boosted_tree_final_model_sha256": _hash(calibrated_tree.parameters())}
        model_results = {}
        for name, model in (("base_rate", calibrated_base), ("logistic", calibrated_logistic), ("boosted_tree", calibrated_tree)):
            model_results[name] = {"parameters_sha256": _hash(model.parameters()), "metrics": metrics(model, fold.test)}
        paired = paired_policy(fold.test, policy_threshold, evaluation_horizon)
        paired["selections"] = list(paired["selections"])
        records.append({"index": fold.index, "train": {"timestamps": sorted({r.decision_timestamp for r in fold.train.observations}), "auctions": len(fold.train.auction_ids), "candidates": len(fold.train.observations)},
                        "calibration": {"timestamps": sorted({r.decision_timestamp for r in fold.calibration.observations}), "auctions": len(fold.calibration.auction_ids), "candidates": len(fold.calibration.observations)},
                        "test": {"timestamps": sorted({r.decision_timestamp for r in fold.test.observations}), "auctions": len(fold.test.auction_ids), "candidates": len(fold.test.observations)},
                        "models": model_results, "paired": paired, "provenance": provenance})
    aggregate = {}
    total = sum(r["test"]["candidates"] for r in records)
    for model in ("base_rate", "logistic", "boosted_tree"):
        aggregate[model] = {key: sum(r["models"][model]["metrics"][key] * r["test"]["candidates"] for r in records) / total
                            for key in ("brier", "log_loss", "feasibility_base_rate", "mean_score_decay", "worst_decile_mean_decay", "cvar")}
        defined = [r for r in records if r["models"][model]["metrics"]["auprc_defined"]]
        aggregate[model]["auprc_defined_fold_count"] = len(defined)
        aggregate[model]["auprc_undefined_fold_count"] = len(records) - len(defined)
        aggregate[model]["auprc_defined"] = bool(defined)
        aggregate[model]["auprc"] = (sum(r["models"][model]["metrics"]["auprc"] * r["test"]["candidates"] for r in defined) /
                                      sum(r["test"]["candidates"] for r in defined) if defined else None)
    return _validate_report(_normalize({"schema_version": "walk-forward-v1", "config": config, "config_sha256": _hash(config),
                                        "feature_schema": list(names), "feature_schema_sha256": _hash(list(names)),
                                        "fold_count": len(records), "folds": records, "aggregate": aggregate}))


def _resolved_boundary(root):
    boundary = root.parent.resolve(strict=False)
    current = root.parent
    while True:
        if current.is_symlink():
            raise ValueError("symlinked output parent")
        if current == boundary or current.parent == current:
            break
        current = current.parent
    return boundary


def _safe_target(path, boundary):
    if path.parent.resolve(strict=False) != boundary:
        raise ValueError("output target outside publication directory")
    current = path.parent
    while True:
        if current.is_symlink():
            raise ValueError("symlinked output parent")
        if current == boundary or current.parent == current:
            break
        current = current.parent
    if os.path.lexists(path) and (path.is_symlink() or path.is_dir()):
        raise ValueError("invalid output target")


def _replace(source, target):
    os.replace(source, target)


def _strict_object_pairs(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key")
        result[key] = value
    return result


def _reject_constant(value):
    raise ValueError("non-finite JSON value")


def read_walk_forward_report(manifest_path):
    manifest = Path(manifest_path)
    boundary = manifest.parent.resolve(strict=False)
    _safe_target(manifest, boundary)
    try:
        value = json.loads(manifest.read_bytes(), object_pairs_hook=_strict_object_pairs,
                           parse_constant=_reject_constant)
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ValueError("invalid publication manifest") from exc
    if not isinstance(value, dict) or value.get("schema_version") != "walk-forward-v1":
        raise ValueError("invalid publication manifest schema")
    required = {"schema_version", "report", "bytes", "sha256"}
    if set(value) != required or not isinstance(value["report"], str):
        raise ValueError("invalid publication manifest keys")
    size, digest = value["bytes"], value["sha256"]
    if isinstance(size, bool) or not isinstance(size, int) or size < 0 or size > 64 * 1024 * 1024:
        raise ValueError("invalid publication byte count")
    if not isinstance(digest, str) or len(digest) != 64 or any(c not in "0123456789abcdef" for c in digest):
        raise ValueError("invalid publication hash")
    target = manifest.parent / value["report"]
    if Path(value["report"]).is_absolute() or target.resolve(strict=False).parent != boundary:
        raise ValueError("invalid publication report path")
    _safe_target(target, boundary)
    try:
        with target.open("rb") as stream:
            data = stream.read(size + 1)
    except OSError as exc:
        raise ValueError("unreadable publication report") from exc
    if len(data) != size or sha256(data).hexdigest() != digest:
        raise ValueError("publication report integrity check failed")
    try:
        report = json.loads(data, object_pairs_hook=_strict_object_pairs, parse_constant=_reject_constant)
    except (json.JSONDecodeError, ValueError) as exc:
        raise ValueError("invalid publication report") from exc
    if not isinstance(report, dict) or report.get("schema_version") != value["schema_version"]:
        raise ValueError("invalid publication report schema")
    return _validate_report(report)


def write_walk_forward_report(path, observations: Iterable[ModelObservation], *, manifest_path=None, **kwargs):
    report = walk_forward_report(observations, **kwargs); data = _canonical(report)
    root = Path(path); boundary = _resolved_boundary(root)
    manifest = Path(manifest_path) if manifest_path else root.with_suffix(root.suffix + ".manifest.json")
    if manifest.is_absolute() and manifest.parent.resolve(strict=False) != boundary:
        raise ValueError("manifest outside publication directory")
    digest = sha256(data).hexdigest(); content = root.with_name(root.stem + "-" + digest + root.suffix)
    targets = [root, content, manifest]
    if len({p.resolve(strict=False) for p in targets}) != len(targets): raise ValueError("report output targets collide")
    for target in targets: _safe_target(target, boundary)
    root.parent.mkdir(parents=True, exist_ok=True)
    staged = []
    try:
        for target, payload in ((content, data), (root, data), (manifest, _canonical({"schema_version": report["schema_version"], "report": content.name, "bytes": len(data), "sha256": digest}))):
            fd, temporary = tempfile.mkstemp(prefix="." + target.name + ".", dir=target.parent)
            with os.fdopen(fd, "wb") as stream: stream.write(payload); stream.flush(); os.fsync(stream.fileno())
            staged.append((temporary, target))
        _replace(staged[0][0], staged[0][1])
        _replace(staged[1][0], staged[1][1])
        _replace(staged[2][0], staged[2][1])
    finally:
        for temporary, _ in staged:
            if os.path.exists(temporary): os.unlink(temporary)
    return {"schema_version": report["schema_version"], "report": content.name, "bytes": len(data), "sha256": digest}
