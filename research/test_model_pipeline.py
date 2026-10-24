import inspect
import json
import math

import pytest

from shadowsolve.calibrate import Calibrated, calibrate
from shadowsolve.evaluate import metrics, paired_policy, pinball_loss, decay_metrics, candidate_decision_value
from shadowsolve.decay import SAFE_TARGET_CEILING, fit_decay, DecayModel
from shadowsolve.split import ModelObservation, Partition, split_observations
from shadowsolve.train import (
    BaseRateModel,
    BoostedTreeModel,
    LogisticModel,
    Standardizer,
    fit_base_rate,
    fit_boosted_tree,
    fit_logistic,
)


def observation(
    auction_id,
    timestamp,
    *,
    candidate_id="candidate",
    horizon=1,
    features=(0.0,),
    feasible=False,
    score_decay=0.0,
    block=None,
    feature_names=("signal",),
    **policy,
):
    return ModelObservation(
        timestamp,
        timestamp if block is None else block,
        auction_id,
        candidate_id,
        horizon,
        features,
        feasible,
        score_decay,
        feature_names,
        **policy,
    )


def partition(role, rows):
    rows = tuple(rows)
    return Partition(
        role,
        rows,
        (10, 20),
        tuple(sorted({row.auction_id for row in rows})),
    )


def chronological_rows():
    return [
        observation(
            f"auction-{index}",
            index,
            features=(float(index),),
            feasible=index % 2 == 0,
        )
        for index in range(1, 7)
    ]


class SequenceModel:
    def __init__(self, probabilities):
        self.probabilities = probabilities

    def predict_proba(self, features):
        return self.probabilities[int(features[0])]

    def parameters(self):
        return {"feature_names": ["signal"]}


@pytest.mark.parametrize(
    "field,value",
    [
        ("decision_timestamp", True),
        ("decision_block", 1.5),
        ("auction_id", " auction"),
        ("candidate_id", "candidate/1"),
        ("horizon", 0),
        ("features", [1.0]),
        ("features", (True,)),
        ("feasible", 1),
        ("score_decay", float("nan")),
        ("predicted_validity", 1.01),
        ("gas_proxy", -0.01),
    ],
)
def test_observation_rejects_non_strict_types_and_ranges(field, value):
    values = {
        "decision_timestamp": 1,
        "decision_block": 1,
        "auction_id": "auction",
        "candidate_id": "candidate",
        "horizon": 1,
        "features": (1.0,),
        "feasible": True,
        "score_decay": 0.0,
        "feature_names": ("signal",),
        "predicted_validity": 0.5,
        "gas_proxy": 0.0,
    }
    values[field] = value
    with pytest.raises(ValueError):
        ModelObservation(**values)


@pytest.mark.parametrize(
    "names",
    [
        ("future_score",),
        ("FutureScore",),
        ("is_feasible",),
        ("target",),
        ("signal", "signal"),
        (" signal",),
        ("bad/name",),
    ],
)
def test_observation_rejects_leakage_and_invalid_feature_schemas(names):
    features = (1.0,) * len(names)
    with pytest.raises(ValueError):
        observation("auction", 1, features=features, feature_names=names)


def test_split_requires_unique_identities_and_balanced_horizons():
    balanced = [
        observation(auction, timestamp, horizon=horizon)
        for auction, timestamp in (("a", 1), ("b", 2), ("c", 3))
        for horizon in (1, 2)
    ]
    train, calibration, test = split_observations(balanced, cutoffs=(1, 2))
    assert [row.horizon for row in train.observations] == [1, 2]
    assert [row.horizon for row in calibration.observations] == [1, 2]
    assert [row.horizon for row in test.observations] == [1, 2]
    with pytest.raises(ValueError, match="duplicate"):
        split_observations(balanced + [balanced[0]], cutoffs=(1, 2))
    with pytest.raises(ValueError, match="imbalance"):
        split_observations(balanced[:-1], cutoffs=(1, 2))


def test_split_uses_exact_inclusive_timestamp_boundaries():
    train, calibration, test = split_observations(
        chronological_rows(), cutoffs=(2, 4)
    )
    assert [part.role for part in (train, calibration, test)] == [
        "train",
        "calibration",
        "test",
    ]
    assert [row.decision_timestamp for row in train.observations] == [1, 2]
    assert [row.decision_timestamp for row in calibration.observations] == [3, 4]
    assert [row.decision_timestamp for row in test.observations] == [5, 6]
    assert train.auction_ids == ("auction-1", "auction-2")


def test_split_keeps_an_entire_timestamp_bucket_together():
    rows = [
        observation("a", 1, block=10),
        observation("b", 1, block=11),
        observation("c", 2, block=20),
        observation("d", 3, block=30),
    ]
    train, calibration, test = split_observations(rows, cutoffs=(1, 2))
    assert train.auction_ids == ("a", "b")
    assert calibration.auction_ids == ("c",)
    assert test.auction_ids == ("d",)


def test_split_is_input_order_invariant_and_requires_bucket_cutoffs():
    rows = chronological_rows()
    assert split_observations(rows, cutoffs=(2, 4)) == split_observations(
        reversed(rows), cutoffs=(2, 4)
    )
    with pytest.raises(ValueError, match="timestamp bucket"):
        split_observations(rows, cutoffs=(2, 99))


def test_training_calibration_and_evaluation_enforce_role_boundaries():
    train, calibration, test = split_observations(
        chronological_rows(), cutoffs=(2, 4)
    )
    model = fit_base_rate(train)
    with pytest.raises(ValueError, match="train"):
        fit_base_rate(calibration)
    with pytest.raises(ValueError, match="calibration"):
        calibrate(model, test)
    with pytest.raises(ValueError, match="test"):
        metrics(model, train)
    with pytest.raises(ValueError, match="test"):
        paired_policy(calibration)


def test_predict_proba_requires_exact_width_and_finite_numeric_features():
    train = split_observations(chronological_rows(), cutoffs=(2, 4))[0]
    for model in (fit_base_rate(train), fit_logistic(train, iterations=10)):
        probability = model.predict_proba((3.0,))
        assert math.isfinite(probability) and 0 <= probability <= 1
        for invalid in ((), (1.0, 2.0), (float("inf"),), (True,)):
            with pytest.raises(ValueError):
                model.predict_proba(invalid)


@pytest.mark.parametrize(
    "arguments",
    [
        {"l2": -1.0},
        {"learning_rate": 0.0},
        {"iterations": True},
        {"iterations": 0},
        {"iterations": 100001},
        {"tolerance": float("nan")},
        {"max_step": 0.0},
    ],
)
def test_logistic_optimizer_validation(arguments):
    train = split_observations(chronological_rows(), cutoffs=(2, 4))[0]
    with pytest.raises(ValueError, match="optimizer"):
        fit_logistic(train, **arguments)


def test_standardization_is_fit_on_train_only():
    rows = [
        observation("a", 1, features=(-2.0,)),
        observation("b", 2, features=(2.0,), feasible=True),
        observation("c", 3, features=(1000.0,), feasible=True),
        observation("d", 4, features=(-1000.0,)),
        observation("e", 5, features=(1e9,), feasible=True),
    ]
    train = split_observations(rows, cutoffs=(2, 4))[0]
    model = fit_logistic(train, iterations=5)
    assert model.standardizer.means == (0.0,)
    assert model.standardizer.scales == (2.0,)


def test_constant_features_get_unit_scale_and_zero_weight():
    rows = [
        observation(f"a-{index}", index, features=(7.0,), feasible=index % 2 == 0)
        for index in range(1, 5)
    ]
    model = fit_logistic(partition("train", rows), iterations=50)
    assert model.standardizer.means == (7.0,)
    assert model.standardizer.scales == (1.0,)
    assert model.weights == (0.0,)


def test_boosted_tree_fit_is_deterministic_and_constant_features_are_intercept_only():
    rows = [observation(f"a-{i}", i, features=(7.0,), feasible=i % 2 == 0) for i in range(1, 5)]
    forward = fit_boosted_tree(partition("train", rows), iterations=8)
    reverse = fit_boosted_tree(partition("train", reversed(rows)), iterations=8)
    assert forward.parameters() == reverse.parameters()
    constant = fit_boosted_tree(partition("train", rows), iterations=8)
    assert constant.trees == ()
    assert 0 <= constant.predict_proba((1e308,)) <= 1


@pytest.mark.parametrize("name", ["bad/name", "bad-name", "bad.name", "x y", "é", "", "a" * 65])
def test_direct_models_reject_artifact_incompatible_feature_names(name):
    with pytest.raises(ValueError):
        BaseRateModel(0.5, (name,))
    with pytest.raises(ValueError):
        BoostedTreeModel((name,), 0.0, 0.1, (), 0)
    with pytest.raises(ValueError):
        LogisticModel(Standardizer((0.0,), (1.0,)), (0.0,), 0.0, (name,), 0, {})


def test_logistic_fit_is_exactly_deterministic_and_order_invariant():
    rows = chronological_rows()
    forward = fit_logistic(partition("train", rows), iterations=100).parameters()
    reverse = fit_logistic(
        partition("train", reversed(rows)), iterations=100
    ).parameters()
    assert forward == reverse


def test_logistic_learns_signal_direction_and_records_json_safe_parameters():
    rows = [
        observation(
            f"a-{index}",
            index,
            features=(value,),
            feasible=value > 0,
        )
        for index, value in enumerate((-2.0, -1.0, 1.0, 2.0), start=1)
    ]
    model = fit_logistic(partition("train", rows), l2=0.0, iterations=200)
    assert model.predict_proba((2.0,)) > model.predict_proba((-2.0,))
    json.dumps(model.parameters(), allow_nan=False)
    json.dumps(fit_base_rate(partition("train", rows)).parameters(), allow_nan=False)


@pytest.mark.parametrize(
    "arguments",
    [
        {"iterations": False},
        {"iterations": 0},
        {"learning_rate": 0.0},
        {"tolerance": float("inf")},
    ],
)
def test_calibration_validates_optimizer_arguments(arguments):
    rows = [observation("a", 11), observation("b", 12, feasible=True)]
    with pytest.raises(ValueError, match="calibration arguments"):
        calibrate(SequenceModel({0: 0.5}), partition("calibration", rows), **arguments)


@pytest.mark.parametrize("bad_probability", [True, -0.1, 1.1, float("nan")])
def test_calibration_rejects_invalid_base_probabilities(bad_probability):
    rows = [observation("a", 11)]
    with pytest.raises(ValueError, match="probability"):
        calibrate(
            SequenceModel({0: bad_probability}),
            partition("calibration", rows),
            iterations=1,
        )


def test_calibration_selects_lower_loss_and_records_selection_metadata():
    rows = [
        observation(f"a-{index}", 11 + index, feasible=True)
        for index in range(3)
    ]
    calibrated = calibrate(
        SequenceModel({0: 0.5}),
        partition("calibration", rows),
        iterations=20,
    )
    assert calibrated.method == "platt"
    assert calibrated.selection_losses["platt"] < calibrated.selection_losses["identity"]
    record = calibrated.parameters()
    assert record["selection_partition"] == "calibration"
    assert record["hyperparameters"] == {"method": "platt"}
    assert record["training_iterations"] == 20
    json.dumps(record, allow_nan=False)


def test_calibrated_identity_preserves_probability_and_validates_width():
    base = fit_base_rate(partition("train", [observation("a", 1, feasible=True)]))
    calibrated = Calibrated(base, 1.0, 0.0, "identity", {"iterations": 0})
    assert calibrated.predict_proba((0.0,)) == 1.0
    with pytest.raises(ValueError):
        calibrated.predict_proba(())


def metric_rows(targets, decays=None):
    if decays is None:
        decays = [0.0] * len(targets)
    return [
        observation(
            f"a-{index}",
            21 + index,
            features=(float(index),),
            feasible=target,
            score_decay=decays[index],
        )
        for index, target in enumerate(targets)
    ]


def test_metrics_exact_brier_and_log_loss():
    result = metrics(
        SequenceModel({0: 0.25, 1: 0.75}),
        partition("test", metric_rows([False, True])),
    )
    assert result["brier"] == pytest.approx(0.0625, abs=1e-15)
    assert result["log_loss"] == pytest.approx(-math.log(0.75), abs=1e-15)
    assert result["feasibility_base_rate"] == 0.5


def test_auprc_groups_probability_ties_exactly():
    result = metrics(
        SequenceModel({0: 0.9, 1: 0.8, 2: 0.8, 3: 0.1}),
        partition("test", metric_rows([True, True, False, False])),
    )
    assert result["auprc"] == pytest.approx(5 / 6, abs=1e-15)


def test_auprc_is_one_for_all_positive_labels():
    result = metrics(
        SequenceModel({0: 0.1, 1: 0.1, 2: 0.9}),
        partition("test", metric_rows([True, True, True])),
    )
    assert result["auprc"] == 1.0
    assert result["auprc_defined"] is True


def test_auprc_is_explicitly_undefined_without_positive_labels():
    result = metrics(
        SequenceModel({0: 0.9, 1: 0.1}),
        partition("test", metric_rows([False, False])),
    )
    assert result["auprc"] is None
    assert result["auprc_defined"] is False


def test_tail_metrics_use_ceil_and_the_worst_high_decay_tail():
    decays = [float(value) for value in range(1, 12)]
    result = metrics(
        SequenceModel({index: 0.5 for index in range(11)}),
        partition("test", metric_rows([False] * 11, decays)),
        alpha=0.25,
    )
    assert result["worst_decile_mean_decay"] == 10.5
    assert result["cvar"] == 10.0


def policy_row(auction, candidate, **values):
    defaults = {
        "features": (0.0,),
        "static_score": 0.0,
        "risk_value": 0.0,
        "predicted_validity": 1.0,
        "future_feasible_score": 0.0,
        "gas_proxy": 0.0,
        "score_decay": 0.0,
        "feasible": True,
    }
    defaults.update(values)
    return observation(auction, 21, candidate_id=candidate, **defaults)


def paired_rows():
    return [
        policy_row(
            "a",
            "static",
            static_score=10.0,
            risk_value=1.0,
            predicted_validity=0.2,
            future_feasible_score=100.0,
            gas_proxy=10.0,
            score_decay=4.0,
            feasible=False,
        ),
        policy_row(
            "a",
            "risk",
            static_score=5.0,
            risk_value=10.0,
            predicted_validity=0.8,
            future_feasible_score=90.0,
            gas_proxy=5.0,
            score_decay=1.0,
        ),
        policy_row(
            "b",
            "static",
            static_score=10.0,
            risk_value=1.0,
            predicted_validity=0.2,
            future_feasible_score=50.0,
            score_decay=6.0,
        ),
        policy_row(
            "b",
            "risk",
            static_score=5.0,
            risk_value=10.0,
            predicted_validity=0.6,
            future_feasible_score=40.0,
            score_decay=8.0,
            feasible=False,
        ),
    ]


def test_paired_policy_records_exact_static_and_risk_selections():
    result = paired_policy(
        partition("test", paired_rows()), min_validity_probability=0.7
    )
    first, second = result["selections"]
    assert (first["auction_id"], first["static_candidate"], first["risk_candidate"]) == (
        "a",
        "static",
        "risk",
    )
    assert first["static_gas_adjusted_value"] == 90.0
    assert first["risk_gas_adjusted_value"] == 85.0
    assert first["score_sacrifice"] == 10.0
    assert first["invalidation"] is False
    assert "static_invalidation" not in first
    assert "risk_invalidation" not in first
    assert second["status"] == "no-eligible"
    assert second["risk_candidate"] is None


def test_paired_policy_aggregates_over_the_same_paired_cohort():
    result = paired_policy(
        partition("test", paired_rows()), min_validity_probability=0.7
    )
    assert result["static"] == {
        "eligible_count": 1,
        "selected_count": 1,
        "no_eligible_count": 1,
        "invalidation_rate": 1.0,
        "mean_future_feasible_score": 100.0,
        "mean_gas_adjusted_value": 90.0,
        "mean_decay": 4.0,
        "worst_decile_decay": 4.0,
        "cvar_decay": 4.0,
    }
    assert result["risk"] == {
        "eligible_count": 1,
        "selected_count": 1,
        "no_eligible_count": 1,
        "invalidation_rate": 0.0,
        "mean_future_feasible_score": 90.0,
        "mean_gas_adjusted_value": 85.0,
        "mean_decay": 1.0,
        "worst_decile_decay": 1.0,
        "cvar_decay": 1.0,
    }
    assert result["overall_mean_score_sacrifice"] == 10.0


def test_paired_policy_no_eligible_has_null_risk_aggregates():
    result = paired_policy(
        partition("test", paired_rows()), min_validity_probability=0.99
    )
    assert result["risk"]["selected_count"] == 0
    assert result["risk"]["no_eligible_count"] == 2
    assert result["risk"]["invalidation_rate"] is None
    assert result["risk"]["mean_decay"] is None
    assert result["static"]["selected_count"] == 0
    assert result["static"]["mean_decay"] is None
    assert result["overall_mean_score_sacrifice"] is None


def test_paired_policy_threshold_is_inclusive_and_horizon_is_explicit():
    rows = [
        policy_row("a", "h1", horizon=1, predicted_validity=0.7, risk_value=2.0),
        policy_row("a", "h2", horizon=2, predicted_validity=1.0, risk_value=3.0),
    ]
    test = partition("test", rows)
    with pytest.raises(ValueError, match="horizon"):
        paired_policy(test, min_validity_probability=0.7)
    result = paired_policy(
        test, min_validity_probability=0.7, evaluation_horizon=1
    )
    assert result["selections"][0]["risk_candidate"] == "h1"
    with pytest.raises(ValueError, match="horizon"):
        paired_policy(test, evaluation_horizon=0)


def test_paired_policy_is_input_order_invariant_with_deterministic_ties():
    rows = paired_rows() + [
        policy_row(
            "c", "a", static_score=1.0, risk_value=1.0, predicted_validity=1.0
        ),
        policy_row(
            "c", "z", static_score=1.0, risk_value=1.0, predicted_validity=1.0
        ),
    ]
    forward = paired_policy(partition("test", rows), 0.7)
    reverse = paired_policy(partition("test", reversed(rows)), 0.7)
    assert forward == reverse
    assert forward["selections"][-1]["static_candidate"] == "z"
    assert forward["selections"][-1]["risk_candidate"] == "z"


@pytest.mark.parametrize("features", [(10**400,), (-(10**400),)])
def test_predict_rejects_oversized_integer_input(features):
    model = fit_base_rate(partition("train", chronological_rows()))
    with pytest.raises(ValueError):
        model.predict_proba(features)


def test_direct_model_construction_rejects_invalid_parameters():
    with pytest.raises(ValueError):
        Standardizer((0.0,), (1.0, 2.0))
    for means, scales in [((0.0,), (0.0,)), ((0.0,), (float("inf"),)), ((float("nan"),), (1.0,))]:
        with pytest.raises(ValueError):
            Standardizer(means, scales)
    standardizer = Standardizer((0.0,), (1.0,))
    for weights, intercept, names in [
        ((), 0.0, ("x",)),
        ((float("inf"),), 0.0, ("x",)),
        ((0.0,), float("nan"), ("x",)),
        ((0.0,), 0.0, ("bad/name",)),
    ]:
        with pytest.raises(ValueError):
            LogisticModel(standardizer, weights, intercept, names, 1, {})
    for names in [(1,), (), ("x", "x"), ("bad/name",)]:
        with pytest.raises(ValueError):
            BoostedTreeModel(names, 0.0, 0.1, (), 0)


def test_public_model_pipeline_apis_have_complete_annotations():
    public = (
        split_observations,
        fit_base_rate,
        fit_logistic,
        calibrate,
        metrics,
        paired_policy,
    )
    for function in public:
        signature = inspect.signature(function)
        assert signature.return_annotation is not inspect.Signature.empty
        assert all(
            parameter.annotation is not inspect.Parameter.empty
            for parameter in signature.parameters.values()
        )


def decay_rows(values=(0.0, 1.0, 2.0)):
    return [observation(f"d-{h}-{i}", i + 1, horizon=h, features=(float(i),), score_decay=value)
            for h in (1, 2, 3) for i, value in enumerate(values)]


def test_decay_pinball_coverage_and_decision_value():
    assert pinball_loss(2.0, 1.0, 0.5) == 0.5
    assert pinball_loss(1.0, 2.0, 0.9) == pytest.approx(0.1)
    assert candidate_decision_value(0.8, 10.0, 2.0, 3.0) == 2.0
    assert candidate_decision_value(0.1, 1.0, 2.0, 3.0) == 0.0


def test_decay_fit_is_deterministic_horizon_isolated_and_crossing_safe():
    rows = decay_rows()
    first = fit_decay(partition("train", rows), iterations=4)
    second = fit_decay(partition("train", reversed(rows)), iterations=4)
    assert first.parameters() == second.parameters()
    for horizon in (1, 2, 3):
        prediction = first.predict((1.0,), horizon)
        assert prediction["p90"] >= prediction["p50"] >= 0
        assert prediction["tail_loss"] == prediction["p90"]


def test_decay_zero_iterations_constant_features_and_validation():
    rows = [observation(f"d-{h}-{i}", i + 1, horizon=h, features=(7.0,), score_decay=float(i))
            for h in (1, 2, 3) for i in range(3)]
    model = fit_decay(partition("train", rows), iterations=0)
    assert model.iterations == 0
    assert all(not model.trees[h - 1] for h in (1, 2, 3))
    with pytest.raises(ValueError): fit_decay(partition("train", rows), iterations=65)
    with pytest.raises(ValueError): fit_decay(partition("train", rows), learning_rate=0.0)
    with pytest.raises(ValueError, match="missing"):
        fit_decay(partition("train", [row for row in rows if row.horizon != 3]), iterations=0)
    over = list(rows); over[0] = observation("over", 1, horizon=1, features=(7.0,), score_decay=SAFE_TARGET_CEILING * 2)
    with pytest.raises(ValueError): fit_decay(partition("train", over), iterations=0)


def test_decay_metrics_requires_explicit_multi_horizon_and_uses_model():
    model = fit_decay(partition("train", decay_rows()), iterations=0)
    test = partition("test", decay_rows())
    with pytest.raises(ValueError, match="horizon"):
        decay_metrics(model, test)
    result = decay_metrics(model, test, evaluation_horizon=2)
    assert result["count"] == 3 and result["p90_coverage"] >= result["p50_coverage"]


def test_decay_strict_pinball_and_row_ceiling():
    for value in (True, float("nan"), float("inf"), -float("inf")):
        with pytest.raises(ValueError): pinball_loss(value, 0.0, 0.5)
    for value in (True, float("nan"), float("inf"), -float("inf")):
        with pytest.raises(ValueError): pinball_loss(0.0, value, 0.5)
    for value in (True, 0.0, 1.0, float("nan")):
        with pytest.raises(ValueError): pinball_loss(0.0, 0.0, value)
    rows = decay_rows((0.0,))
    exact = (rows * 1366)[:4096]
    assert fit_decay(partition("train", exact), iterations=0)
    with pytest.raises(ValueError, match="large"):
        fit_decay(partition("train", exact + rows[:1]), iterations=0)



def test_decay_one_iteration_stores_asymmetric_quantile_stump_and_predicts_branches():
    values = [(0, 0), (1, 10), (2, 1), (3, 20), (4, 2), (5, 30)]
    rows = [
        observation(f"x-{index}", index + 1, horizon=1, features=(float(x),), score_decay=y)
        for index, (x, y) in enumerate(values)
    ] + [
        observation(f"h-{horizon}", 100 + horizon, horizon=horizon, features=(0.0,), score_decay=0.0)
        for horizon in (2, 3)
    ]
    model = fit_decay(partition("train", rows), learning_rate=1.0, iterations=1)
    assert model.trees[0] == ((0, 4.0, 0.0, 28.0, -10.0, 0.0),)
    assert model.predict((-1.0,), 1)["p50"] == 2.0
    assert model.predict((-1.0,), 1)["p90"] == 20.0
    assert model.predict((10.0,), 1)["p50"] == 30.0
    assert model.predict((10.0,), 1)["p90"] == 30.0


def test_decay_multiple_iterations_selects_scored_thresholds_and_final_predictions():
    values = [(0, 0), (1, 10), (2, 1), (3, 20), (4, 2), (5, 30)]
    rows = [
        observation(f"x-{index}", index + 1, horizon=1, features=(float(x),), score_decay=y)
        for index, (x, y) in enumerate(values)
    ] + [
        observation(f"h-{horizon}", 100 + horizon, horizon=horizon, features=(0.0,), score_decay=0.0)
        for horizon in (2, 3)
    ]
    model = fit_decay(partition("train", rows), learning_rate=1.0, iterations=2)
    assert model.trees[0] == (
        (0, 4.0, 0.0, 28.0, -10.0, 0.0),
        (0, 2.0, -1.0, 0.0, -10.0, 0.0),
    )
    assert model.predict((-1.0,), 1)["p50"] == 1.0
    assert model.predict((-1.0,), 1)["p90"] == 10.0
    assert model.predict((10.0,), 1)["p50"] == 30.0
    assert model.predict((10.0,), 1)["p90"] == 30.0


def test_paired_policy_tail_penalty_uses_decay_prediction_without_selection_leakage():
    class SuppliedDecay:
        def predict(self, features, horizon):
            p90 = {0.0: 10.0, 1.0: 0.0}[features[0]]
            return {"p50": 0.0, "p90": p90, "tail_loss": p90, "uncertainty": p90}

    rows = [
        policy_row("a", "high", features=(0.0,), static_score=10.0, risk_value=10.0),
        policy_row("a", "low", features=(1.0,), static_score=9.0, risk_value=9.0),
    ]
    model = SuppliedDecay()
    no_penalty = paired_policy(partition("test", rows), decay_model=model, tail_penalty=0.0)
    penalty = paired_policy(partition("test", rows), decay_model=model, tail_penalty=1.0)
    assert no_penalty["selections"][0]["risk_candidate"] == "high"
    assert penalty["selections"][0]["risk_candidate"] == "low"

    changed = [
        policy_row("a", "high", features=(0.0,), static_score=10.0, risk_value=10.0, score_decay=99.0, feasible=False),
        policy_row("a", "low", features=(1.0,), static_score=9.0, risk_value=9.0, score_decay=88.0, feasible=False),
    ]
    changed_result = paired_policy(partition("test", changed), decay_model=model, tail_penalty=1.0)
    assert changed_result["selections"][0]["risk_candidate"] == penalty["selections"][0]["risk_candidate"]
