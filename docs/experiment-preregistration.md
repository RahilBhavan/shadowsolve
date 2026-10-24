# Experiment preregistration

**Last updated:** 2026-08-07

## Question

Among replayed candidates, does the risk policy's validity threshold and `risk_value` selection differ from selecting the maximum `static_score` within the same auction in future feasibility and score-decay outcomes?

## Locked procedure

1. Publish a manifest-backed dataset with explicit provenance hashes and expected horizons.
2. Validate identity uniqueness, decision-state coherence, label equations, and complete horizons.
3. Split complete auction timestamp buckets chronologically into train, calibration, and test; do not tune on test.
4. Fit the declared base-rate, logistic, and bounded boosted-tree models on train only.
5. Select identity versus Platt calibration using calibration log loss only.
6. Report declared-horizon test metrics and the `paired_policy` summary at one declared validity threshold.

Primary paired quantities are invalidation rate, mean future feasible score, mean gas-adjusted value, mean score decay, worst-decile decay, CVaR decay, and overall mean score sacrifice. Empty risk-eligible auctions remain in selection output and are excluded from risk-policy means as implemented.

## Exclusions and status

No rerouting beyond the allow-listed bounded solver, optimization for execution, signing, settlement, unrestricted route discovery, concentrated-liquidity, multi-order, causal, or post-hoc horizon claims are permitted. Quantile decay modeling and automated walk-forward evaluation (including boosted-tree) are implemented. Model artifact export, online Rust serving, and policy/fixture modes are implemented. Published replay walk-forward evidence lives under `reports/walk-forward/`; it is not a live profitability claim.
