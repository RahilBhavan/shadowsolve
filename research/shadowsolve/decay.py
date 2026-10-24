"""Deterministic train-only additive quantile stump decay models."""
from dataclasses import dataclass
import math
from typing import Any, Dict, Protocol, Sequence, Tuple
from .train import _features, _feature_names, _rows

SAFE_TARGET_CEILING = 1e150
MAX_TRAIN_ROWS = 4096
_HORIZONS = (1, 2, 3)
_QUANTILES = (0.5, 0.9)


def pinball_loss(actual: float, predicted: float, quantile: float) -> float:
    if (type(actual) not in (int, float) or type(predicted) not in (int, float)
            or type(quantile) not in (int, float)
            or not all(math.isfinite(v) for v in (actual, predicted, quantile))
            or not 0 < quantile < 1):
        raise ValueError("invalid pinball arguments")
    error = float(actual) - float(predicted)
    return quantile * error if error >= 0 else (quantile - 1) * error


def _quantile(values: Sequence[float], q: float) -> float:
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, max(0, math.ceil(q * len(ordered)) - 1))]


def _target(value: float) -> float:
    if type(value) not in (int, float) or not math.isfinite(value) or not 0 <= value <= SAFE_TARGET_CEILING:
        raise ValueError("invalid decay target")
    return float(value)


class DecayPredictor(Protocol):
    def predict(self, features: Sequence[float], horizon: int) -> Dict[str, float]: ...


@dataclass(frozen=True)
class DecayModel:
    feature_names: Tuple[str, ...]
    horizons: Tuple[int, ...]
    quantiles: Tuple[float, ...]
    intercepts: Tuple[Tuple[float, float], ...]
    trees: Tuple[Tuple[Tuple[int, float, float, float, float, float], ...], ...]
    iterations: int

    def __post_init__(self) -> None:
        if (not _feature_names(self.feature_names) or self.horizons != _HORIZONS
                or self.quantiles != _QUANTILES or type(self.iterations) is not int
                or not 0 <= self.iterations <= 64 or len(self.intercepts) != 3 or len(self.trees) != 3):
            raise ValueError("invalid decay model")
        for intercept, trees in zip(self.intercepts, self.trees):
            if len(intercept) != 2 or any(type(v) not in (int, float) or not math.isfinite(v) or v < 0 for v in intercept):
                raise ValueError("invalid decay model")
            for tree in trees:
                if len(tree) != 6 or type(tree[0]) is not int or not 0 <= tree[0] < len(self.feature_names) or any(not math.isfinite(v) for v in tree[1:]):
                    raise ValueError("invalid decay model")

    def predict(self, features: Sequence[float], horizon: int) -> Dict[str, float]:
        if type(horizon) is not int or horizon not in _HORIZONS:
            raise ValueError("invalid horizon")
        values = _features(features, len(self.feature_names))
        p50, p90 = self.intercepts[horizon - 1]
        for index, threshold, left50, right50, left90, right90 in self.trees[horizon - 1]:
            if values[index] <= threshold:
                p50 += left50; p90 += left90
            else:
                p50 += right50; p90 += right90
        p50 = max(0.0, min(SAFE_TARGET_CEILING, p50))
        p90 = max(p50, min(SAFE_TARGET_CEILING, p90))
        return {"p50": p50, "p90": p90, "tail_loss": p90, "uncertainty": p90 - p50}

    def parameters(self) -> Dict[str, Any]:
        return {"model_version": "1", "feature_names": list(self.feature_names), "horizons": list(self.horizons), "quantiles": list(self.quantiles), "intercepts": [list(x) for x in self.intercepts], "trees": [[list(t) for t in trees] for trees in self.trees], "training_iterations": self.iterations}


def fit_decay(partition: Any, *, learning_rate: float = 0.1, iterations: int = 8) -> DecayModel:
    rows = _rows(partition)
    if len(rows) > MAX_TRAIN_ROWS:
        raise ValueError("training partition too large")
    if type(iterations) is not int or not 0 <= iterations <= 64 or type(learning_rate) not in (int, float) or not math.isfinite(learning_rate) or not 0 < learning_rate <= 1:
        raise ValueError("invalid decay parameters")
    if any(row.horizon not in _HORIZONS for row in rows): raise ValueError("unsupported horizon")
    targets = [_target(row.score_decay) for row in rows]
    intercepts, all_trees = [], []
    for horizon in _HORIZONS:
        indices = [i for i, row in enumerate(rows) if row.horizon == horizon]
        if not indices: raise ValueError("missing horizon")
        base = [_quantile([targets[i] for i in indices], q) for q in _QUANTILES]
        predictions = [[base[k] for _ in indices] for k in range(2)]
        trees = []
        for _ in range(iterations):
            residual = [[targets[indices[j]] - predictions[k][j] for j in range(len(indices))] for k in range(2)]
            best = None
            for feature in range(len(rows[0].features)):
                for threshold in sorted({rows[i].features[feature] for i in indices}):
                    membership = tuple(rows[i].features[feature] <= threshold for i in indices)
                    if all(membership) or not any(membership): continue
                    branches = ([j for j, left in enumerate(membership) if left], [j for j, left in enumerate(membership) if not left])
                    values = tuple(_quantile([residual[k][j] for j in branch], _QUANTILES[k]) for branch in branches for k in range(2))
                    loss = math.fsum(pinball_loss(targets[indices[j]], predictions[k][j] + learning_rate * values[2 * (0 if membership[j] else 1) + k], _QUANTILES[k]) for j in range(len(indices)) for k in range(2))
                    candidate = (loss, feature, threshold, *values)
                    if best is None or candidate < best: best = candidate
            if best is None: break
            _, feature, threshold, ll50, ll90, rl50, rl90 = best
            membership = tuple(rows[i].features[feature] <= threshold for i in indices)
            trees.append((feature, threshold, learning_rate * ll50, learning_rate * rl50, learning_rate * ll90, learning_rate * rl90))
            for j, left in enumerate(membership):
                predictions[0][j] += learning_rate * (ll50 if left else rl50)
                predictions[1][j] += learning_rate * (ll90 if left else rl90)
        intercepts.append(tuple(base)); all_trees.append(tuple(trees))
    return DecayModel(rows[0].feature_names, _HORIZONS, _QUANTILES, tuple(intercepts), tuple(all_trees), iterations)

fit_quantile_decay = fit_decay
fit_decay_model = fit_decay
