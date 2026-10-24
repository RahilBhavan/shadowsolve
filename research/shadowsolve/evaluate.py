"""Test-only metrics and paired policy evaluation."""

import math
import json
import os
import tempfile
from hashlib import sha256
from pathlib import Path
from typing import Any, Dict, List, Optional, Protocol, Sequence, Tuple

from .calibrate import calibrate
from .split import build_walk_forward_folds
from .train import fit_base_rate, fit_logistic
from .decay import pinball_loss as _decay_pinball_loss

from .split import ModelObservation, Partition


def pinball_loss(actual: float, predicted: float, quantile: float) -> float:
    if type(actual) not in (int, float) or type(predicted) not in (int, float) or type(quantile) not in (int, float) or not all(math.isfinite(v) for v in (actual, predicted, quantile)) or not 0 < quantile < 1:
        raise ValueError("invalid pinball arguments")
    return _decay_pinball_loss(float(actual), float(predicted), float(quantile))


def decay_metrics(models, partition: Partition, *, evaluation_horizon: Optional[int] = None) -> Dict[str, Any]:
    rows = _test(partition)
    if evaluation_horizon is None:
        horizons = {row.horizon for row in rows}
        if len(horizons) != 1: raise ValueError("evaluation horizon required")
        evaluation_horizon = next(iter(horizons))
    if type(evaluation_horizon) is not int or evaluation_horizon <= 0: raise ValueError("invalid horizon")
    selected = [row for row in rows if row.horizon == evaluation_horizon]
    if not selected: raise ValueError("missing horizon")
    model = models[evaluation_horizon] if isinstance(models, dict) else models
    predictions = [model.predict(row.features, evaluation_horizon) for row in selected]
    actual = [row.score_decay for row in selected]
    p50 = [p["p50"] for p in predictions]; p90 = [p["p90"] for p in predictions]
    return {"horizon": evaluation_horizon, "count": len(selected), "p50_loss": math.fsum(pinball_loss(y,p,.5) for y,p in zip(actual,p50))/len(actual), "p90_loss": math.fsum(pinball_loss(y,p,.9) for y,p in zip(actual,p90))/len(actual), "p50_coverage": math.fsum(y <= p for y,p in zip(actual,p50))/len(actual), "p90_coverage": math.fsum(y <= p for y,p in zip(actual,p90))/len(actual), "actual_mean": math.fsum(actual)/len(actual), "p50_mean": math.fsum(p50)/len(p50), "p90_mean": math.fsum(p90)/len(p90), "uncertainty_mean": math.fsum(p90[i]-p50[i] for i in range(len(p90)))/len(p90), "realized_mean": math.fsum(actual)/len(actual), "current_upper_tail": max(actual)}


def candidate_decision_value(validity: float, static_score: float, tail_penalty: float, p90: float) -> float:
    if any(type(v) not in (int, float) or not math.isfinite(v) for v in (validity, static_score, tail_penalty, p90)) or validity < 0 or tail_penalty < 0 or p90 < 0:
        raise ValueError("invalid decision arguments")
    return max(0.0, validity * static_score - tail_penalty * p90)


class ProbabilityModel(Protocol):
    def predict_proba(self, features: Sequence[float]) -> float:
        ...


def _test(partition: Partition) -> Tuple[ModelObservation, ...]:
    if getattr(partition, "role", None) != "test":
        raise ValueError("test partition required")
    if not partition.observations:
        raise ValueError("empty test partition")
    return partition.observations


def _tail(values: Sequence[float], fraction: float) -> float:
    count = max(1, math.ceil(len(values) * fraction))
    return math.fsum(sorted(values)[-count:]) / count


def metrics(
    model: ProbabilityModel, partition: Partition, alpha: float = 0.1
) -> Dict[str, Any]:
    if (
        type(alpha) not in (int, float)
        or not math.isfinite(alpha)
        or not 0 < alpha <= 1
    ):
        raise ValueError("invalid alpha")
    rows = _test(partition)
    probabilities = []
    for row in rows:
        probability = model.predict_proba(row.features)
        if (
            type(probability) not in (int, float)
            or not math.isfinite(probability)
            or not 0 <= probability <= 1
        ):
            raise ValueError("invalid probability")
        probabilities.append(float(probability))

    targets = [int(row.feasible) for row in rows]
    brier = math.fsum(
        (probability - target) ** 2
        for probability, target in zip(probabilities, targets)
    ) / len(targets)
    log_loss = -math.fsum(
        target * math.log(max(1e-15, probability))
        + (1 - target) * math.log(max(1e-15, 1 - probability))
        for probability, target in zip(probabilities, targets)
    ) / len(targets)

    positive_count = sum(targets)
    groups = []
    for probability in sorted(set(probabilities), reverse=True):
        indices = [
            index
            for index, candidate in enumerate(probabilities)
            if candidate == probability
        ]
        groups.append(
            (sum(targets[index] for index in indices), len(indices))
        )
    if positive_count == 0:
        auprc = None
    else:
        seen = 0
        hits = 0
        auprc = 0.0
        for group_hits, group_size in groups:
            seen += group_size
            hits += group_hits
            auprc += (hits / seen) * (group_hits / positive_count)

    decays = [row.score_decay for row in rows]
    return {
        "count": len(rows),
        "positive_count": positive_count,
        "brier": brier,
        "log_loss": log_loss,
        "auprc": auprc,
        "auprc_defined": auprc is not None,
        "feasibility_base_rate": positive_count / len(targets),
        "mean_score_decay": math.fsum(decays) / len(decays),
        "worst_decile_mean_decay": _tail(decays, 0.1),
        "cvar": _tail(decays, alpha),
    }


def _policy_summary(
    selections: Sequence[Dict[str, Any]], policy: str
) -> Dict[str, Any]:
    selected = [
        selection
        for selection in selections
        if selection["risk_candidate"] is not None
    ]
    decays = [selection[policy + "_decay"] for selection in selected]
    invalidations = [
        selection["_" + policy + "_invalidation"] for selection in selected
    ]
    scores = [
        selection[policy + "_future_feasible_score"] for selection in selected
    ]
    gas_values = [
        selection[policy + "_gas_adjusted_value"] for selection in selected
    ]
    return {
        "eligible_count": len(selected),
        "selected_count": len(selected),
        "no_eligible_count": len(selections) - len(selected),
        "invalidation_rate": (
            math.fsum(invalidations) / len(invalidations)
            if invalidations
            else None
        ),
        "mean_future_feasible_score": (
            math.fsum(scores) / len(scores) if scores else None
        ),
        "mean_gas_adjusted_value": (
            math.fsum(gas_values) / len(gas_values) if gas_values else None
        ),
        "mean_decay": math.fsum(decays) / len(decays) if decays else None,
        "worst_decile_decay": _tail(decays, 0.1) if decays else None,
        "cvar_decay": _tail(decays, 0.1) if decays else None,
    }


def paired_policy(
    partition: Partition,
    min_validity_probability: float = 0.0,
    evaluation_horizon: Optional[int] = None,
    decay_model: Any = None,
    tail_penalty: float = 0.0,
) -> Dict[str, Any]:
    rows = _test(partition)
    if (
        type(min_validity_probability) not in (int, float)
        or not math.isfinite(min_validity_probability)
        or not 0 <= min_validity_probability <= 1
    ):
        raise ValueError("invalid threshold")
    if evaluation_horizon is not None and (
        type(evaluation_horizon) is not int or evaluation_horizon <= 0
    ):
        raise ValueError("invalid horizon")
    if type(tail_penalty) not in (int, float) or not math.isfinite(tail_penalty) or tail_penalty < 0:
        raise ValueError("invalid tail penalty")

    groups: Dict[str, List[ModelObservation]] = {}
    for row in rows:
        groups.setdefault(row.auction_id, []).append(row)
    selections = []
    for auction_id, auction_rows in sorted(groups.items()):
        horizons = {row.horizon for row in auction_rows}
        if evaluation_horizon is None and len(horizons) != 1:
            raise ValueError("evaluation horizon required")
        if evaluation_horizon is not None:
            auction_rows = [
                row for row in auction_rows if row.horizon == evaluation_horizon
            ]
            if not auction_rows:
                continue
        if len({row.candidate_id for row in auction_rows}) != len(auction_rows):
            raise ValueError("duplicate candidate")

        static = max(
            auction_rows, key=lambda row: (row.static_score, row.candidate_id)
        )
        eligible = [
            row
            for row in auction_rows
            if row.predicted_validity >= min_validity_probability
        ]
        def decision(row):
            p90 = decay_model.predict(row.features, row.horizon)["p90"] if decay_model is not None else 0.0
            return candidate_decision_value(row.predicted_validity, row.static_score, tail_penalty, p90)
        risk = (max(eligible, key=lambda row: ((decision(row) if decay_model is not None else row.risk_value), row.candidate_id)) if eligible else None)
        selections.append(
            {
                "auction_id": auction_id,
                "static_candidate": static.candidate_id,
                "risk_candidate": risk.candidate_id if risk else None,
                "status": "selected" if risk else "no-eligible",
                "static_decay": static.score_decay,
                "risk_decay": risk.score_decay if risk else None,
                "static_future_feasible_score": static.future_feasible_score,
                "risk_future_feasible_score": (
                    risk.future_feasible_score if risk else None
                ),
                "static_gas_adjusted_value": (
                    static.future_feasible_score - static.gas_proxy
                ),
                "risk_gas_adjusted_value": (
                    risk.future_feasible_score - risk.gas_proxy if risk else None
                ),
                "score_sacrifice": (
                    static.future_feasible_score - risk.future_feasible_score
                    if risk
                    else None
                ),
                "_static_invalidation": not static.feasible,
                "_risk_invalidation": not risk.feasible if risk else None,
                "invalidation": not risk.feasible if risk else None,
            }
        )

    paired = [
        selection
        for selection in selections
        if selection["risk_candidate"] is not None
    ]
    sacrifices = [selection["score_sacrifice"] for selection in paired]
    public_selections = tuple(
        {
            key: value
            for key, value in selection.items()
            if not key.startswith("_")
        }
        for selection in selections
    )
    return {
        "selections": public_selections,
        "static": _policy_summary(selections, "static"),
        "risk": _policy_summary(selections, "risk"),
        "overall_mean_score_sacrifice": (
            math.fsum(sacrifices) / len(sacrifices) if sacrifices else None
        ),
    }


def walk_forward_report(*args, **kwargs):
    from .walk_forward import walk_forward_report as implementation
    return implementation(*args, **kwargs)


def write_walk_forward_report(*args, **kwargs):
    from .walk_forward import write_walk_forward_report as implementation
    return implementation(*args, **kwargs)
