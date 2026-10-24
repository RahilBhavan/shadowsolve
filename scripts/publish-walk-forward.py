#!/usr/bin/env python3
"""Publish a deterministic walk-forward report into reports/walk-forward/."""

from __future__ import annotations

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "research"))

from shadowsolve.split import ModelObservation  # noqa: E402
from shadowsolve.walk_forward import write_walk_forward_report  # noqa: E402


def observations() -> list[ModelObservation]:
    """High-fidelity synthetic CP-style features; not live profitability evidence."""
    names = ("expected_surplus", "gas_proxy", "route_count")
    rows: list[ModelObservation] = []
    # Twelve chronological auctions with two candidates each → enough folds.
    for t in range(1, 13):
        for candidate_index, bias in enumerate((0.0, 1.5)):
            surplus = 120.0 + 15.0 * t + bias * 40.0
            gas = 100.0 + 20.0 * candidate_index
            routes = 1.0 + candidate_index
            # Mix feasible / infeasible across time and candidates for nonzero metrics.
            feasible = (surplus - gas) >= (80.0 + 5.0 * t) if candidate_index == 0 else (t % 3 != 0)
            decay = max(0.0, 0.04 * t + (0.0 if feasible else 0.15))
            score = surplus - gas
            rows.append(
                ModelObservation(
                    t,
                    t,
                    f"auction-{t}",
                    f"cand-{candidate_index}",
                    1,
                    (surplus, gas, routes),
                    feasible,
                    decay,
                    names,
                    static_score=score,
                    risk_value=score * (0.7 if feasible else 0.25),
                    predicted_validity=0.8 if feasible else 0.35,
                    future_feasible_score=score * (0.95 if feasible else 0.1),
                    gas_proxy=gas,
                )
            )
    return rows


def main() -> int:
    out = ROOT / "reports" / "walk-forward" / "report.json"
    out.parent.mkdir(parents=True, exist_ok=True)
    manifest = write_walk_forward_report(
        out,
        observations(),
        logistic_kwargs={"iterations": 40},
        boosted_tree_kwargs={"iterations": 8},
        policy_threshold=0.5,
    )
    print(json.dumps(manifest, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
