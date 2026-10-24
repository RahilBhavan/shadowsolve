"""Deterministic dependency-free feasibility models."""

from dataclasses import dataclass
import math
import re
from typing import Any, Dict, Sequence, Tuple

from .split import ModelObservation, Partition


def _rows(partition: Partition) -> Tuple[ModelObservation, ...]:
    if getattr(partition, "role", None) != "train":
        raise ValueError("train partition required")
    rows = partition.observations
    if not rows:
        raise ValueError("empty partition")
    names = rows[0].feature_names
    width = len(names)
    if any(row.feature_names != names or len(row.features) != width for row in rows):
        raise ValueError("inconsistent feature schema")
    return tuple(
        sorted(
            rows,
            key=lambda row: (
                row.decision_timestamp,
                row.decision_block,
                row.auction_id,
                row.candidate_id,
                row.horizon,
            ),
        )
    )


def _features(features: Sequence[float], width: int) -> Tuple[float, ...]:
    try:
        valid = (
            type(features) in (tuple, list)
            and len(features) == width
            and all(type(value) in (int, float) and math.isfinite(value) for value in features)
        )
    except OverflowError:
        valid = False
    if not valid:
        raise ValueError("invalid features")
    return tuple(float(value) for value in features)


def _sigmoid(value: float) -> float:
    if value >= 0:
        exponential = math.exp(-value)
        return 1 / (1 + exponential)
    exponential = math.exp(value)
    return exponential / (1 + exponential)


def _finite(value: float) -> None:
    if not math.isfinite(value):
        raise ValueError("nonfinite model parameter")


def _feature_names(names: Tuple[str, ...]) -> bool:
    return type(names) is tuple and bool(names) and len(set(names)) == len(names) and all(
        type(name) is str and re.fullmatch(r"[A-Za-z][A-Za-z0-9_]{0,63}", name)
        for name in names
    )


@dataclass(frozen=True)
class Standardizer:
    means: Tuple[float, ...]
    scales: Tuple[float, ...]

    def __post_init__(self):
        if type(self.means) is not tuple or type(self.scales) is not tuple:
            raise ValueError("invalid standardizer")
        if not self.means or len(self.means) != len(self.scales):
            raise ValueError("invalid standardizer")
        if any(type(value) not in (int, float) or not math.isfinite(value) for value in self.means):
            raise ValueError("invalid means")
        if any(type(value) not in (int, float) or not math.isfinite(value) or value <= 0 for value in self.scales):
            raise ValueError("invalid scales")

    def transform(self, features: Sequence[float]) -> Tuple[float, ...]:
        values = _features(features, len(self.means))
        result = tuple(
            (value - mean) / scale
            for value, mean, scale in zip(values, self.means, self.scales)
        )
        if any(not math.isfinite(value) for value in result):
            raise ValueError("nonfinite standardized features")
        return result


@dataclass(frozen=True)
class BaseRateModel:
    rate: float
    feature_names: Tuple[str, ...]
    model_version: str = "1"

    def __post_init__(self):
        if type(self.rate) not in (int, float) or not math.isfinite(self.rate) or not 0 <= self.rate <= 1:
            raise ValueError("invalid base rate")
        if not _feature_names(self.feature_names):
            raise ValueError("invalid feature names")

    def predict_proba(self, features: Sequence[float]) -> float:
        _features(features, len(self.feature_names))
        return self.rate

    def parameters(self) -> Dict[str, Any]:
        return {
            "model_version": self.model_version,
            "feature_names": list(self.feature_names),
            "hyperparameters": {},
            "training_iterations": 1,
            "rate": self.rate,
        }


@dataclass(frozen=True)
class BoostedTreeModel:
    feature_names: Tuple[str, ...]
    intercept: float
    learning_rate: float
    trees: Tuple[Tuple[int, float, float, float], ...]
    iterations: int

    def __post_init__(self):
        if (
            not _feature_names(self.feature_names)
        ):
            raise ValueError("invalid feature names")
        if type(self.iterations) is not int or not 0 <= self.iterations <= 64 or not 0 <= len(self.trees) <= 64:
            raise ValueError("invalid tree parameters")
        if type(self.learning_rate) not in (int, float) or not math.isfinite(self.learning_rate) or not 0 < self.learning_rate <= 1:
            raise ValueError("invalid tree parameters")
        if type(self.intercept) not in (int, float) or not math.isfinite(self.intercept):
            raise ValueError("invalid tree parameters")
        for tree in self.trees:
            if type(tree) not in (tuple, list) or len(tree) != 4 or type(tree[0]) is not int or not 0 <= tree[0] < len(self.feature_names) or any(type(v) not in (int, float) or not math.isfinite(v) for v in tree[1:]):
                raise ValueError("invalid tree parameters")

    def predict_proba(self, features: Sequence[float]) -> float:
        values = _features(features, len(self.feature_names))
        logit = self.intercept + self.learning_rate * sum(
            left if values[index] <= threshold else right
            for index, threshold, left, right in self.trees
        )
        _finite(logit)
        probability = _sigmoid(logit)
        _finite(probability)
        return probability

    def parameters(self) -> Dict[str, Any]:
        return {
            "model_version": "1", "feature_names": list(self.feature_names),
            "hyperparameters": {"learning_rate": self.learning_rate},
            "training_iterations": self.iterations, "intercept": self.intercept,
            "trees": [{"feature_index": i, "threshold": t, "left_value": l, "right_value": r}
                       for i, t, l, r in self.trees],
        }


@dataclass(frozen=True)
class LogisticModel:
    standardizer: Standardizer
    weights: Tuple[float, ...]
    intercept: float
    feature_names: Tuple[str, ...]
    iterations: int
    hyperparameters: Dict[str, float]

    def __post_init__(self):
        if (
            not _feature_names(self.feature_names)
            or len(self.weights) != len(self.feature_names)
            or len(self.standardizer.means) != len(self.feature_names)
            or any(type(value) not in (int, float) or not math.isfinite(value) for value in (*self.weights, self.intercept))
        ):
            raise ValueError("invalid logistic model")

    def predict_proba(self, features: Sequence[float]) -> float:
        transformed = self.standardizer.transform(features)
        if len(transformed) != len(self.feature_names) or len(self.weights) != len(transformed):
            raise ValueError("invalid logistic model")
        logit = self.intercept + sum(weight * value for weight, value in zip(self.weights, transformed))
        _finite(logit)
        probability = _sigmoid(logit)
        _finite(probability)
        return probability

    def parameters(self) -> Dict[str, Any]:
        return {
            "model_version": "1",
            "feature_names": list(self.feature_names),
            "hyperparameters": dict(self.hyperparameters),
            "training_iterations": self.iterations,
            "means": list(self.standardizer.means),
            "scales": list(self.standardizer.scales),
            "weights": list(self.weights),
            "intercept": self.intercept,
        }


def fit_base_rate(partition: Partition) -> BaseRateModel:
    rows = _rows(partition)
    return BaseRateModel(
        sum(row.feasible for row in rows) / len(rows), rows[0].feature_names
    )


def fit_boosted_tree(
    partition: Partition,
    *,
    learning_rate: float = 0.1,
    iterations: int = 8,
) -> BoostedTreeModel:
    rows = _rows(partition)
    if type(iterations) is not int or not 0 <= iterations <= 64 or type(learning_rate) not in (int, float) or not math.isfinite(learning_rate) or not 0 < learning_rate <= 1:
        raise ValueError("invalid tree parameters")
    targets = [int(row.feasible) for row in rows]
    rate = sum(targets) / len(targets)
    intercept = math.log(rate / (1 - rate)) if 0 < rate < 1 else (-20.0 if rate == 0 else 20.0)
    residual = [target - _sigmoid(intercept) for target in targets]
    trees = []
    width = len(rows[0].features)
    for _ in range(iterations):
        best = None
        for index in range(width):
            values = sorted({row.features[index] for row in rows})
            for threshold in values:
                left = [i for i, row in enumerate(rows) if row.features[index] <= threshold]
                right = [i for i, row in enumerate(rows) if row.features[index] > threshold]
                if not right:
                    continue
                lv = math.fsum(residual[i] for i in left) / len(left)
                rv = math.fsum(residual[i] for i in right) / len(right)
                error = math.fsum((residual[i] - (lv if i in left else rv)) ** 2 for i in range(len(rows)))
                candidate = (error, index, threshold, lv, rv)
                if best is None or candidate < best:
                    best = candidate
        if best is None:
            break
        _, index, threshold, lv, rv = best
        trees.append((index, threshold, lv, rv))
        for i, row in enumerate(rows):
            residual[i] -= learning_rate * (lv if row.features[index] <= threshold else rv)
    model = BoostedTreeModel(rows[0].feature_names, intercept, learning_rate, tuple(trees), len(trees))
    for value in (model.intercept, model.learning_rate, *(v for tree in model.trees for v in tree)):
        _finite(value)
    return model


def fit_logistic(
    partition: Partition,
    *,
    l2: float = 1.0,
    learning_rate: float = 0.1,
    iterations: int = 1000,
    tolerance: float = 1e-9,
    max_step: float = 1.0,
) -> LogisticModel:
    rows = _rows(partition)
    width = len(rows[0].features)
    numeric = (l2, learning_rate, tolerance, max_step)
    if (
        type(iterations) is not int
        or not 1 <= iterations <= 100000
        or any(type(value) not in (int, float) or not math.isfinite(value) for value in numeric)
        or l2 < 0
        or learning_rate <= 0
        or tolerance <= 0
        or max_step <= 0
    ):
        raise ValueError("invalid optimizer parameters")

    means = tuple(
        math.fsum(row.features[index] for row in rows) / len(rows)
        for index in range(width)
    )
    scales = tuple(
        math.sqrt(
            math.fsum(
                (row.features[index] - means[index]) ** 2 for row in rows
            )
            / len(rows)
        )
        or 1.0
        for index in range(width)
    )
    inputs = [
        tuple(
            (value - mean) / scale
            for value, mean, scale in zip(row.features, means, scales)
        )
        for row in rows
    ]
    targets = [int(row.feasible) for row in rows]
    weights = [0.0] * width
    intercept = 0.0
    used = 0
    for used in range(1, iterations + 1):
        probabilities = [
            _sigmoid(
                intercept
                + sum(weight * value for weight, value in zip(weights, features))
            )
            for features in inputs
        ]
        intercept_gradient = math.fsum(
            probability - target
            for probability, target in zip(probabilities, targets)
        ) / len(targets)
        gradients = [
            math.fsum(
                (probabilities[index] - targets[index]) * inputs[index][column]
                for index in range(len(targets))
            )
            / len(targets)
            + l2 * weights[column]
            for column in range(width)
        ]
        largest_gradient = max(
            abs(intercept_gradient), *(abs(gradient) for gradient in gradients)
        )
        if largest_gradient < tolerance:
            break
        factor = min(1.0, max_step / largest_gradient)
        intercept -= learning_rate * intercept_gradient * factor
        weights = [
            weight - learning_rate * gradient * factor
            for weight, gradient in zip(weights, gradients)
        ]

    for value in (*weights, intercept, *means, *scales):
        _finite(value)
    return LogisticModel(
        Standardizer(means, scales),
        tuple(weights),
        intercept,
        rows[0].feature_names,
        used,
        {
            "l2": l2,
            "learning_rate": learning_rate,
            "tolerance": tolerance,
            "max_step": max_step,
        },
    )
