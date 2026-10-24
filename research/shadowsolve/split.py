"""Validated chronological partitions for ShadowSolve."""

from dataclasses import dataclass
import math
import re
from typing import Iterable, Optional, Tuple, Sequence, List, Dict


_SAFE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]*$")
_LEAK = re.compile(r"outcome|label|future|target|feasible", re.I)


def _number(value: object, *, nonnegative: bool = False) -> None:
    if (
        type(value) not in (int, float)
        or not math.isfinite(value)
        or (nonnegative and value < 0)
    ):
        raise ValueError("invalid numeric value")


def _identifier(value: object) -> str:
    if type(value) is not str or not _SAFE.fullmatch(value):
        raise ValueError("invalid identifier")
    return value


@dataclass(frozen=True)
class ModelObservation:
    decision_timestamp: int
    decision_block: int
    auction_id: str
    candidate_id: str
    horizon: int
    features: Tuple[float, ...]
    feasible: bool
    score_decay: float
    feature_names: Tuple[str, ...]
    static_score: float = 0.0
    risk_value: float = 0.0
    predicted_validity: float = 1.0
    future_feasible_score: float = 0.0
    gas_proxy: float = 0.0

    def __post_init__(self) -> None:
        for value in (self.decision_timestamp, self.decision_block):
            if type(value) is not int or value < 0:
                raise ValueError("invalid decision identity")
        if type(self.horizon) is not int or self.horizon <= 0:
            raise ValueError("invalid horizon")
        _identifier(self.auction_id)
        _identifier(self.candidate_id)
        if type(self.features) is not tuple or not self.features:
            raise ValueError("features must be a non-empty tuple")
        if any(
            type(value) not in (int, float) or not math.isfinite(value)
            for value in self.features
        ):
            raise ValueError("invalid features")
        if (
            type(self.feature_names) is not tuple
            or len(self.feature_names) != len(self.features)
            or len(set(self.feature_names)) != len(self.feature_names)
            or any(
                type(name) is not str
                or not _SAFE.fullmatch(name)
                or _LEAK.search(name)
                for name in self.feature_names
            )
        ):
            raise ValueError("invalid feature schema")
        if type(self.feasible) is not bool:
            raise ValueError("feasible must be bool")
        for value in (
            self.score_decay,
            self.static_score,
            self.risk_value,
            self.predicted_validity,
            self.future_feasible_score,
            self.gas_proxy,
        ):
            _number(value)
        if (
            self.score_decay < 0
            or self.gas_proxy < 0
            or not 0 <= self.predicted_validity <= 1
        ):
            raise ValueError("invalid policy value")


Observation = ModelObservation


@dataclass(frozen=True)
class WalkForwardFold:
    index: int
    train: "Partition"
    calibration: "Partition"
    test: "Partition"


def build_walk_forward_folds(
    observations: Iterable[ModelObservation], *, min_train_auctions: int = 1,
    min_calibration_auctions: int = 1, min_test_auctions: int = 1,
) -> Tuple[WalkForwardFold, ...]:
    raw = tuple(observations)
    if any(type(value) is not int or value < 1 for value in
           (min_train_auctions, min_calibration_auctions, min_test_auctions)):
        raise ValueError("minimum auction counts must be positive integers")
    if not raw or any(type(row) is not ModelObservation for row in raw):
        raise ValueError("insufficient observations")
    identities = [(row.auction_id, row.candidate_id, row.horizon) for row in raw]
    if len(set(identities)) != len(identities):
        raise ValueError("duplicate candidate identity")
    by_auction = {}
    for row in raw:
        by_auction.setdefault(row.auction_id, []).append(row)
    if any(len({(r.decision_timestamp, r.decision_block) for r in values}) != 1 for values in by_auction.values()):
        raise ValueError("auction spans decision points")
    horizons = {row.horizon for row in raw}
    if any({row.horizon for row in values} != horizons for values in by_auction.values()):
        raise ValueError("horizon imbalance")
    timestamps = sorted({row.decision_timestamp for row in raw})
    folds = []
    for calibration_index in range(1, len(timestamps) - 1):
        train_ts = timestamps[:calibration_index]
        cal_ts = (timestamps[calibration_index],)
        test_ts = (timestamps[calibration_index + 1],)
        train = tuple(row for row in raw if row.decision_timestamp in train_ts)
        cal = tuple(row for row in raw if row.decision_timestamp in cal_ts)
        test = tuple(row for row in raw if row.decision_timestamp in test_ts)
        counts = [len({row.auction_id for row in group}) for group in (train, cal, test)]
        if all(count >= minimum for count, minimum in zip(counts, (min_train_auctions, min_calibration_auctions, min_test_auctions))):
            folds.append(WalkForwardFold(len(folds), _make_partition("train", train, timestamps[calibration_index]), _make_partition("calibration", cal, timestamps[calibration_index], timestamps[calibration_index + 1]), _make_partition("test", test, timestamps[calibration_index + 1])))
    if not folds:
        raise ValueError("insufficient complete timestamp buckets for walk-forward folds")
    return tuple(folds)


def _make_partition(role: str, rows: Sequence[ModelObservation], lower: int, upper: Optional[int] = None) -> "Partition":
    if not rows:
        raise ValueError("empty fold partition")
    cutoff = (lower, upper if upper is not None else lower + 1)
    return Partition(role, tuple(sorted(rows, key=lambda r: (r.decision_timestamp, r.decision_block, r.auction_id, r.candidate_id, r.horizon))), cutoff, tuple(sorted({r.auction_id for r in rows})))


@dataclass(frozen=True)
class Partition:
    role: str
    observations: Tuple[ModelObservation, ...]
    cutoffs: Tuple[int, int]
    auction_ids: Tuple[str, ...]

    def __post_init__(self) -> None:
        if (
            self.role not in ("train", "calibration", "test")
            or type(self.observations) is not tuple
            or not self.observations
            or any(type(row) is not ModelObservation for row in self.observations)
        ):
            raise ValueError("invalid partition")
        if (
            type(self.cutoffs) is not tuple
            or len(self.cutoffs) != 2
            or any(type(value) is not int or value < 0 for value in self.cutoffs)
            or self.cutoffs[0] >= self.cutoffs[1]
        ):
            raise ValueError("invalid cutoffs")
        if (
            type(self.auction_ids) is not tuple
            or tuple(sorted(self.auction_ids)) != self.auction_ids
            or len(set(self.auction_ids)) != len(self.auction_ids)
        ):
            raise ValueError("invalid auction metadata")
        if set(self.auction_ids) != {row.auction_id for row in self.observations}:
            raise ValueError("inconsistent metadata")


def split_observations(
    observations: Iterable[ModelObservation],
    train_fraction: float = 0.6,
    calibration_fraction: float = 0.2,
    *,
    cutoffs: Optional[Tuple[int, int]] = None,
) -> Tuple[Partition, Partition, Partition]:
    """Split complete timestamp buckets into train, calibration, and test roles."""

    raw = tuple(observations)
    if not raw or any(type(row) is not ModelObservation for row in raw):
        raise ValueError("invalid observations")
    identities = [
        (row.auction_id, row.candidate_id, row.horizon) for row in raw
    ]
    if len(set(identities)) != len(identities):
        raise ValueError("duplicate identity")

    by_auction = {}
    for row in raw:
        by_auction.setdefault(row.auction_id, []).append(row)
    for rows in by_auction.values():
        if len({(row.decision_timestamp, row.decision_block) for row in rows}) != 1:
            raise ValueError("auction spans decision points")
    horizons = {row.horizon for row in raw}
    if any({row.horizon for row in rows} != horizons for rows in by_auction.values()):
        raise ValueError("horizon imbalance")

    timestamps = sorted({rows[0].decision_timestamp for rows in by_auction.values()})
    if cutoffs is None:
        for fraction in (train_fraction, calibration_fraction):
            if type(fraction) is not float or not math.isfinite(fraction) or fraction <= 0:
                raise ValueError("invalid fractions")
        if train_fraction + calibration_fraction >= 1 or len(timestamps) < 3:
            raise ValueError("invalid fractions or timestamp buckets")
        count = len(timestamps)
        first = max(1, min(count - 2, round(count * train_fraction)))
        second = max(
            first + 1,
            min(count - 1, round(count * (train_fraction + calibration_fraction))),
        )
        cutoffs = (timestamps[first - 1], timestamps[second - 1])
    if (
        type(cutoffs) is not tuple
        or len(cutoffs) != 2
        or any(type(value) is not int or value < 0 for value in cutoffs)
        or cutoffs[0] >= cutoffs[1]
    ):
        raise ValueError("invalid cutoffs")
    if cutoffs[0] not in timestamps or cutoffs[1] not in timestamps:
        raise ValueError("cutoff must be a timestamp bucket")

    ordered = tuple(
        sorted(
            raw,
            key=lambda row: (
                row.decision_timestamp,
                row.decision_block,
                row.auction_id,
                row.candidate_id,
                row.horizon,
            ),
        )
    )
    result = []
    boundaries = (
        ("train", None, cutoffs[0]),
        ("calibration", cutoffs[0], cutoffs[1]),
        ("test", cutoffs[1], None),
    )
    for role, lower, upper in boundaries:
        auction_ids = tuple(
            sorted(
                auction_id
                for auction_id, rows in by_auction.items()
                if (lower is None or rows[0].decision_timestamp > lower)
                and (upper is None or rows[0].decision_timestamp <= upper)
            )
        )
        partition_rows = tuple(
            row for row in ordered if row.auction_id in auction_ids
        )
        if not partition_rows:
            raise ValueError("empty partition")
        result.append(Partition(role, partition_rows, cutoffs, auction_ids))
    return tuple(result)
