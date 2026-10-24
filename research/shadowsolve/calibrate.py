"""Calibration using calibration data only."""

from dataclasses import dataclass
import math
from typing import Any, Dict, Protocol, Sequence

from .split import Partition
from .train import _sigmoid


class ProbabilityModel(Protocol):
    def predict_proba(self, features: Sequence[float]) -> float:
        ...

    def parameters(self) -> Dict[str, Any]:
        ...


def _loss(targets: Sequence[int], probabilities: Sequence[float]) -> float:
    return -math.fsum(
        target * math.log(max(1e-15, probability))
        + (1 - target) * math.log(max(1e-15, 1 - probability))
        for target, probability in zip(targets, probabilities)
    ) / len(targets)


@dataclass(frozen=True)
class Calibrated:
    model: ProbabilityModel
    slope: float
    intercept: float
    method: str
    selection_losses: Dict[str, Any]

    def predict_proba(self, features: Sequence[float]) -> float:
        probability = self.model.predict_proba(features)
        if (
            type(probability) not in (int, float)
            or not math.isfinite(probability)
            or not 0 <= probability <= 1
        ):
            raise ValueError("invalid model probability")
        if self.method == "identity":
            return float(probability)
        clipped = max(1e-15, min(1 - 1e-15, probability))
        logit = math.log(clipped / (1 - clipped))
        calibrated = _sigmoid(self.intercept + self.slope * logit)
        if not math.isfinite(calibrated):
            raise ValueError("invalid calibrated probability")
        return calibrated

    def parameters(self) -> Dict[str, Any]:
        base_parameters = self.model.parameters()
        feature_names = base_parameters.get("feature_names")
        if not isinstance(feature_names, list) or not all(
            type(name) is str for name in feature_names
        ):
            raise ValueError("invalid model parameters")
        return {
            "model_version": "calibrated-1",
            "feature_names": list(feature_names),
            "hyperparameters": {"method": self.method},
            "training_iterations": self.selection_losses.get("iterations", 0),
            "slope": self.slope,
            "intercept": self.intercept,
            "selection_losses": dict(self.selection_losses),
            "selection_partition": "calibration",
        }


def calibrate(
    model: ProbabilityModel,
    partition: Partition,
    *,
    iterations: int = 1000,
    learning_rate: float = 0.1,
    tolerance: float = 1e-9,
) -> Calibrated:
    if getattr(partition, "role", None) != "calibration":
        raise ValueError("calibration partition required")
    if (
        type(iterations) is not int
        or not 1 <= iterations <= 100000
        or any(
            type(value) not in (int, float)
            or not math.isfinite(value)
            or value <= 0
            for value in (learning_rate, tolerance)
        )
    ):
        raise ValueError("invalid calibration arguments")

    rows = tuple(
        sorted(
            partition.observations,
            key=lambda row: (
                row.decision_timestamp,
                row.decision_block,
                row.auction_id,
                row.candidate_id,
                row.horizon,
            ),
        )
    )
    targets = [int(row.feasible) for row in rows]
    raw = []
    for row in rows:
        probability = model.predict_proba(row.features)
        if (
            type(probability) not in (int, float)
            or not math.isfinite(probability)
            or not 0 <= probability <= 1
        ):
            raise ValueError("invalid model probability")
        raw.append(float(probability))

    logits = []
    for probability in raw:
        clipped = max(1e-15, min(1 - 1e-15, probability))
        logits.append(math.log(clipped / (1 - clipped)))
    slope = 1.0
    intercept = 0.0
    used = 0
    for used in range(1, iterations + 1):
        probabilities = [
            _sigmoid(intercept + slope * logit) for logit in logits
        ]
        slope_gradient = math.fsum(
            (probability - target) * logit
            for probability, target, logit in zip(
                probabilities, targets, logits
            )
        ) / len(targets)
        intercept_gradient = math.fsum(
            probability - target
            for probability, target in zip(probabilities, targets)
        ) / len(targets)
        largest_gradient = max(abs(slope_gradient), abs(intercept_gradient))
        if largest_gradient < tolerance:
            break
        factor = min(1.0, 1.0 / largest_gradient)
        slope -= learning_rate * slope_gradient * factor
        intercept -= learning_rate * intercept_gradient * factor

    fitted_probabilities = [
        _sigmoid(intercept + slope * logit) for logit in logits
    ]
    identity_loss = _loss(targets, raw)
    platt_loss = _loss(targets, fitted_probabilities)
    if not all(
        math.isfinite(value)
        for value in (slope, intercept, identity_loss, platt_loss)
    ):
        raise ValueError("nonfinite calibration result")
    losses = {
        "identity": identity_loss,
        "platt": platt_loss,
        "iterations": used,
    }
    if platt_loss < identity_loss:
        return Calibrated(model, slope, intercept, "platt", losses)
    return Calibrated(model, 1.0, 0.0, "identity", losses)
