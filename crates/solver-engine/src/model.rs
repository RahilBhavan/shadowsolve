use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const MAX_ARTIFACT_BYTES: usize = 1 << 20;
#[derive(Debug, PartialEq)]
pub enum ModelError {
    Json(String),
    Invalid(String),
    HashMismatch,
}
impl From<serde_json::Error> for ModelError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e.to_string())
    }
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    artifact_version: String,
    model: Model,
    calibration: Calibration,
    model_version: String,
    feature_schema_hash: String,
    training_data_hash: String,
    calibration_data_hash: String,
    code_hash: String,
    config_hash: String,
    created_at: String,
    artifact_sha256: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Model {
    #[serde(rename = "base-rate")]
    Base {
        feature_names: Vec<String>,
        rate: f64,
    },
    #[serde(rename = "logistic")]
    Logistic {
        feature_names: Vec<String>,
        means: Vec<f64>,
        scales: Vec<f64>,
        weights: Vec<f64>,
        intercept: f64,
    },
    #[serde(rename = "boosted-tree")]
    BoostedTree {
        feature_names: Vec<String>,
        intercept: f64,
        learning_rate: f64,
        trees: Vec<Tree>,
    },
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Tree {
    feature_index: usize,
    threshold: f64,
    left_value: f64,
    right_value: f64,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Calibration {
    method: String,
    slope: f64,
    intercept: f64,
}
#[derive(Debug, Clone, PartialEq)]
pub enum FallbackReason {
    InvalidArtifact,
    SchemaMismatch,
    HashMismatch,
    FeatureWidth,
}
#[derive(Debug)]
pub enum Compatibility {
    Compatible(Box<ModelArtifact>),
    StaticFallback(FallbackReason),
}
#[derive(Debug)]
pub struct ModelArtifact {
    artifact: Artifact,
}
impl ModelArtifact {
    pub fn load(bytes: &[u8]) -> Result<Self, ModelError> {
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ModelError::Invalid("artifact size".into()));
        }
        let a: Artifact = serde_json::from_slice(bytes)?;
        validate(&a)?;
        let v = serde_json::to_value(&a)?;
        let mut o = v
            .as_object()
            .ok_or_else(|| ModelError::Invalid("artifact object".into()))?
            .clone();
        let h = o
            .remove("artifact_sha256")
            .ok_or_else(|| ModelError::Invalid("artifact hash".into()))?;
        let expected = h
            .as_str()
            .ok_or_else(|| ModelError::Invalid("artifact hash".into()))?;
        let canonical = serde_json::to_string(&o)?;
        if sha256(canonical.as_bytes()) != expected {
            return Err(ModelError::HashMismatch);
        }
        Ok(Self { artifact: a })
    }
    pub fn predict(&self, features: &[(&str, f64)]) -> Result<f64, ModelError> {
        let names = match &self.artifact.model {
            Model::Base { feature_names, .. }
            | Model::Logistic { feature_names, .. }
            | Model::BoostedTree { feature_names, .. } => feature_names,
        };
        if features.len() != names.len()
            || features
                .iter()
                .zip(names)
                .any(|((name, value), expected)| *name != expected || !value.is_finite())
        {
            return Err(ModelError::Invalid("named features".into()));
        }
        self.predict_positional(&features.iter().map(|(_, value)| *value).collect::<Vec<_>>())
    }

    pub fn predict_named(&self, features: &[(&str, f64)]) -> Result<f64, ModelError> {
        self.predict(features)
    }

    fn predict_positional(&self, features: &[f64]) -> Result<f64, ModelError> {
        let m = &self.artifact.model;
        let n = match m {
            Model::Base { feature_names, .. }
            | Model::Logistic { feature_names, .. }
            | Model::BoostedTree { feature_names, .. } => feature_names.len(),
        };
        if features.len() != n || features.iter().any(|x| !x.is_finite()) {
            return Err(ModelError::Invalid("features".into()));
        }
        let p = match m {
            Model::Base { rate, .. } => *rate,
            Model::Logistic {
                means,
                scales,
                weights,
                intercept,
                ..
            } => {
                let mut logit = *intercept;
                for (((x, m), s), w) in features.iter().zip(means).zip(scales).zip(weights) {
                    let centered = x - m;
                    let standardized = centered / s;
                    let contribution = w * standardized;
                    if !centered.is_finite()
                        || !standardized.is_finite()
                        || !contribution.is_finite()
                    {
                        return Err(ModelError::Invalid("nonfinite logit".into()));
                    }
                    logit += contribution;
                    if !logit.is_finite() {
                        return Err(ModelError::Invalid("nonfinite logit".into()));
                    }
                }
                sigmoid(logit)
            }
            Model::BoostedTree {
                intercept,
                learning_rate,
                trees,
                ..
            } => {
                let mut total = 0.0;
                for tree in trees {
                    let contribution = if features[tree.feature_index] <= tree.threshold {
                        tree.left_value
                    } else {
                        tree.right_value
                    };
                    if !contribution.is_finite() {
                        return Err(ModelError::Invalid("nonfinite tree".into()));
                    }
                    total += contribution;
                    if !total.is_finite() {
                        return Err(ModelError::Invalid("nonfinite tree".into()));
                    }
                }
                let scaled = *learning_rate * total;
                let logit = *intercept + scaled;
                if !scaled.is_finite() || !logit.is_finite() {
                    return Err(ModelError::Invalid("nonfinite logit".into()));
                }
                sigmoid(logit)
            }
        };
        if !p.is_finite() || !(0.0..=1.0).contains(&p) {
            return Err(ModelError::Invalid("nonfinite probability".into()));
        }
        let c = &self.artifact.calibration;
        if c.method == "none" {
            return Ok(p);
        }
        let q = p.clamp(1e-15, 1.0 - 1e-15);
        let z = c.intercept + c.slope * logit(q);
        if !z.is_finite() {
            return Err(ModelError::Invalid("nonfinite calibrated logit".into()));
        }
        let result = sigmoid(z);
        if !result.is_finite() || !(0.0..=1.0).contains(&result) {
            return Err(ModelError::Invalid("nonfinite probability".into()));
        }
        Ok(result)
    }

    fn feature_width(&self) -> usize {
        match &self.artifact.model {
            Model::Base { feature_names, .. }
            | Model::Logistic { feature_names, .. }
            | Model::BoostedTree { feature_names, .. } => feature_names.len(),
        }
    }
}
pub fn compatibility(bytes: &[u8]) -> Compatibility {
    match ModelArtifact::load(bytes) {
        Ok(a) => Compatibility::Compatible(Box::new(a)),
        Err(ModelError::HashMismatch) => {
            Compatibility::StaticFallback(FallbackReason::HashMismatch)
        }
        Err(_) => Compatibility::StaticFallback(FallbackReason::InvalidArtifact),
    }
}

pub fn compatibility_for_schema(
    bytes: &[u8],
    expected_feature_schema_hash: &str,
    expected_feature_width: usize,
) -> Compatibility {
    match ModelArtifact::load(bytes) {
        Ok(a) if a.artifact.feature_schema_hash != expected_feature_schema_hash => {
            Compatibility::StaticFallback(FallbackReason::SchemaMismatch)
        }
        Ok(a) if a.feature_width() != expected_feature_width => {
            Compatibility::StaticFallback(FallbackReason::FeatureWidth)
        }
        Ok(a) => Compatibility::Compatible(Box::new(a)),
        Err(ModelError::HashMismatch) => {
            Compatibility::StaticFallback(FallbackReason::HashMismatch)
        }
        Err(_) => Compatibility::StaticFallback(FallbackReason::InvalidArtifact),
    }
}

fn validate(a: &Artifact) -> Result<(), ModelError> {
    if a.artifact_version != "1" || a.model_version != "1" || !valid_timestamp(&a.created_at) {
        return Err(ModelError::Invalid("version".into()));
    }
    for h in [
        &a.feature_schema_hash,
        &a.training_data_hash,
        &a.calibration_data_hash,
        &a.code_hash,
        &a.config_hash,
        &a.artifact_sha256,
    ] {
        if h.len() != 64
            || !h
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !(c as char).is_ascii_uppercase())
        {
            return Err(ModelError::Invalid("hash".into()));
        }
    }
    if !["none", "platt"].contains(&a.calibration.method.as_str())
        || !a.calibration.slope.is_finite()
        || !a.calibration.intercept.is_finite()
        || (a.calibration.method == "none"
            && (a.calibration.slope != 1.0 || a.calibration.intercept != 0.0))
    {
        return Err(ModelError::Invalid("calibration".into()));
    }
    let names = match &a.model {
        Model::Base {
            feature_names,
            rate,
        } => {
            if !rate.is_finite() || !(0.0..=1.0).contains(rate) {
                return Err(ModelError::Invalid("rate".into()));
            }
            feature_names
        }
        Model::Logistic {
            feature_names,
            means,
            scales,
            weights,
            intercept,
        } => {
            if means.len() != feature_names.len()
                || scales.len() != means.len()
                || weights.len() != means.len()
                || !intercept.is_finite()
                || scales.iter().any(|x| *x <= 0.0 || !x.is_finite())
                || means.iter().chain(weights).any(|x| !x.is_finite())
            {
                return Err(ModelError::Invalid("width".into()));
            }
            feature_names
        }
        Model::BoostedTree {
            feature_names,
            intercept,
            learning_rate,
            trees,
        } => {
            if !intercept.is_finite()
                || !learning_rate.is_finite()
                || *learning_rate <= 0.0
                || *learning_rate > 1.0
                || trees.len() > 64
                || trees.iter().any(|t| {
                    t.feature_index >= feature_names.len()
                        || !t.threshold.is_finite()
                        || !t.left_value.is_finite()
                        || !t.right_value.is_finite()
                })
            {
                return Err(ModelError::Invalid("boosted tree".into()));
            }
            feature_names
        }
    };
    if names.is_empty()
        || names.iter().any(|x| !valid_feature_name(x))
        || names.iter().collect::<HashSet<_>>().len() != names.len()
    {
        return Err(ModelError::Invalid("features".into()));
    }
    Ok(())
}

fn valid_feature_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphabetic()
        && bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

fn valid_timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 20
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'Z'
        || bytes
            .iter()
            .enumerate()
            .any(|(i, byte)| ![4, 7, 10, 13, 16, 19].contains(&i) && !byte.is_ascii_digit())
    {
        return false;
    }
    let number =
        |start: usize| u32::from(bytes[start] - b'0') * 10 + u32::from(bytes[start + 1] - b'0');
    let year = number(0) * 100 + number(2);
    let month = number(5);
    let day = number(8);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day) && number(11) <= 23 && number(14) <= 59 && number(17) <= 59
}
fn logit(p: f64) -> f64 {
    if p <= 0.0 {
        f64::NEG_INFINITY
    } else if p >= 1.0 {
        f64::INFINITY
    } else {
        (p / (1.0 - p)).ln()
    }
}
fn sigmoid(x: f64) -> f64 {
    if x >= 0.0 {
        1.0 / (1.0 + (-x).exp())
    } else {
        let e = x.exp();
        e / (1.0 + e)
    }
}
fn sha256(data: &[u8]) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let k: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut m = data.to_vec();
    let l = (m.len() as u64) * 8;
    m.push(128);
    while m.len() % 64 != 56 {
        m.push(0)
    }
    m.extend_from_slice(&l.to_be_bytes());
    for c in m.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([c[i * 4], c[i * 4 + 1], c[i * 4 + 2], c[i * 4 + 3]])
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1)
        }
        let (mut a, mut b, mut c0, mut d, mut e, mut f, mut g, mut q) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = q
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(k[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c0) ^ (b & c0);
            let t2 = s0.wrapping_add(maj);
            (a, b, c0, d, e, f, g, q) = (t1.wrapping_add(t2), a, b, c0, d.wrapping_add(t1), e, f, g)
        }
        for (i, v) in [a, b, c0, d, e, f, g, q].iter().enumerate() {
            h[i] = h[i].wrapping_add(*v)
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::{json, Value};
    use std::collections::BTreeMap;

    const FIXTURE: &str = include_str!("../../../fixtures/model-parity.json");
    const ONLINE_FIXTURE: &str = include_str!("../../../fixtures/online-model.json");

    #[derive(Deserialize)]
    struct ParityFixture {
        artifact: Value,
        vectors: Vec<ParityVector>,
    }

    #[derive(Deserialize)]
    struct ParityVector {
        features: BTreeMap<String, f64>,
        probability: f64,
    }

    fn fixture() -> ParityFixture {
        serde_json::from_str(FIXTURE).unwrap()
    }

    fn artifact_bytes(mut artifact: Value, rehash: bool) -> Vec<u8> {
        if rehash {
            let object = artifact.as_object_mut().unwrap();
            object.remove("artifact_sha256");
            let canonical = serde_json::to_string(object).unwrap();
            object.insert(
                "artifact_sha256".into(),
                Value::String(sha256(canonical.as_bytes())),
            );
        }
        serde_json::to_vec(&artifact).unwrap()
    }

    fn loaded_fixture() -> ModelArtifact {
        ModelArtifact::load(&artifact_bytes(fixture().artifact, false)).unwrap()
    }

    #[test]
    fn sha256_matches_empty_known_vector() {
        assert_eq!(
            sha256(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn sha256_matches_abc_known_vector() {
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_matches_multiblock_known_vector() {
        assert_eq!(
            sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn sha256_matches_million_a_known_vector() {
        assert_eq!(
            sha256(&vec![b'a'; 1_000_000]),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn fixture_probabilities_match_python_within_one_e_minus_twelve() {
        let fixture = fixture();
        let model = ModelArtifact::load(&artifact_bytes(fixture.artifact, false)).unwrap();
        let names = match &model.artifact.model {
            Model::Base { feature_names, .. }
            | Model::Logistic { feature_names, .. }
            | Model::BoostedTree { feature_names, .. } => feature_names,
        };
        for vector in fixture.vectors {
            let features: Vec<_> = names.iter().map(|name| vector.features[name]).collect();
            let actual = model.predict_positional(&features).unwrap();
            assert!(
                (actual - vector.probability).abs() <= 1e-12,
                "expected {}, got {actual}",
                vector.probability
            );
        }
    }

    #[test]
    fn fixture_loads_with_python_canonical_hash() {
        assert!(matches!(
            compatibility(&artifact_bytes(fixture().artifact, false)),
            Compatibility::Compatible(_)
        ));
    }

    #[test]
    fn online_fixture_matches_schema_and_python_prediction() {
        let artifact: Value = serde_json::from_str(ONLINE_FIXTURE).unwrap();
        let bytes = serde_json::to_vec(&artifact).unwrap();
        let model = match compatibility_for_schema(
            &bytes,
            crate::ONLINE_FEATURE_SCHEMA_HASH,
            crate::ONLINE_FEATURE_SCHEMA.len(),
        ) {
            Compatibility::Compatible(model) => model,
            other => panic!("unexpected compatibility: {other:?}"),
        };
        assert!(
            (model
                .predict_named(&[
                    ("expected_surplus", 8.0),
                    ("gas_proxy", 100.0),
                    ("route_count", 2.0)
                ])
                .unwrap()
                - 0.4625701546562505)
                .abs()
                < 1e-15
        );
        assert!(matches!(
            compatibility_for_schema(&bytes, &"0".repeat(64), 3),
            Compatibility::StaticFallback(FallbackReason::SchemaMismatch)
        ));
    }

    #[test]
    fn oversized_artifact_uses_invalid_artifact_fallback() {
        assert!(matches!(
            compatibility(&vec![b' '; MAX_ARTIFACT_BYTES + 1]),
            Compatibility::StaticFallback(FallbackReason::InvalidArtifact)
        ));
    }

    #[test]
    fn malformed_json_uses_invalid_artifact_fallback() {
        assert!(matches!(
            compatibility(b"not json"),
            Compatibility::StaticFallback(FallbackReason::InvalidArtifact)
        ));
    }

    #[test]
    fn tampering_uses_hash_mismatch_fallback() {
        let mut artifact = fixture().artifact;
        artifact["model"]["intercept"] = json!(123.0);
        assert!(matches!(
            compatibility(&artifact_bytes(artifact, false)),
            Compatibility::StaticFallback(FallbackReason::HashMismatch)
        ));
    }

    #[test]
    fn schema_hash_mismatch_uses_invalid_artifact_fallback() {
        let artifact = artifact_bytes(fixture().artifact, false);
        assert!(matches!(
            compatibility_for_schema(&artifact, &"f".repeat(64), 3),
            Compatibility::StaticFallback(FallbackReason::SchemaMismatch)
        ));
    }

    #[test]
    fn feature_width_mismatch_uses_width_fallback() {
        let artifact = artifact_bytes(fixture().artifact, false);
        assert!(matches!(
            compatibility_for_schema(&artifact, &"0".repeat(64), 2),
            Compatibility::StaticFallback(FallbackReason::FeatureWidth)
        ));
    }

    #[test]
    fn exact_schema_and_width_are_compatible() {
        let artifact = artifact_bytes(fixture().artifact, false);
        assert!(matches!(
            compatibility_for_schema(&artifact, &"0".repeat(64), 3),
            Compatibility::Compatible(_)
        ));
    }

    #[test]
    fn unknown_fields_are_rejected_at_all_schema_levels() {
        for location in ["top", "model", "calibration"] {
            let mut artifact = fixture().artifact;
            let target = if location == "top" {
                artifact.as_object_mut().unwrap()
            } else {
                artifact[location].as_object_mut().unwrap()
            };
            target.insert("unexpected".into(), json!(1));
            assert!(matches!(
                ModelArtifact::load(&artifact_bytes(artifact, true)),
                Err(ModelError::Json(_))
            ));
        }
    }

    #[test]
    fn logistic_array_width_mismatches_are_rejected() {
        for field in ["means", "scales", "weights"] {
            let mut artifact = fixture().artifact;
            artifact["model"][field].as_array_mut().unwrap().pop();
            assert!(matches!(
                ModelArtifact::load(&artifact_bytes(artifact, true)),
                Err(ModelError::Invalid(message)) if message == "width"
            ));
        }
    }

    #[test]
    fn invalid_scales_and_feature_names_are_rejected() {
        let mut bad_scale = fixture().artifact;
        bad_scale["model"]["scales"][0] = json!(0.0);
        assert!(ModelArtifact::load(&artifact_bytes(bad_scale, true)).is_err());

        for names in [
            json!([]),
            json!(["x", "x", "z"]),
            json!(["1bad", "p", "g"]),
            json!(["x-y", "p", "g"]),
        ] {
            let mut artifact = fixture().artifact;
            artifact["model"]["feature_names"] = names;
            assert!(ModelArtifact::load(&artifact_bytes(artifact, true)).is_err());
        }
    }

    #[test]
    fn invalid_versions_timestamps_and_hashes_are_rejected() {
        for (field, value) in [
            ("artifact_version", "2"),
            ("model_version", "2"),
            ("created_at", "2026-02-30T12:00:00Z"),
            (
                "code_hash",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            ),
        ] {
            let mut artifact = fixture().artifact;
            artifact[field] = json!(value);
            assert!(ModelArtifact::load(&artifact_bytes(artifact, true)).is_err());
        }
    }

    #[test]
    fn nonneutral_none_calibration_is_rejected() {
        let mut artifact = fixture().artifact;
        artifact["calibration"] = json!({"method": "none", "slope": 0.5, "intercept": 0.0});
        assert!(ModelArtifact::load(&artifact_bytes(artifact, true)).is_err());
    }

    #[test]
    fn named_prediction_matches_positional_and_requires_exact_order() {
        let model = loaded_fixture();
        let named = [
            ("liquidity", 12.0),
            ("price_impact", 2.0),
            ("gas_ratio", 0.75),
        ];
        assert_eq!(
            model.predict(&named).unwrap(),
            model.predict_positional(&[12.0, 2.0, 0.75]).unwrap()
        );
        for bad in [
            [
                ("price_impact", 2.0),
                ("liquidity", 12.0),
                ("gas_ratio", 0.75),
            ],
            [("wrong", 12.0), ("price_impact", 2.0), ("gas_ratio", 0.75)],
        ] {
            assert!(model.predict_named(&bad).is_err());
        }
        assert!(model.predict_named(&named[..2]).is_err());
        assert!(model
            .predict_named(&[
                ("liquidity", 12.0),
                ("price_impact", 2.0),
                ("gas_ratio", f64::NAN)
            ])
            .is_err());
    }

    #[test]
    fn prediction_rejects_wrong_width_and_nonfinite_features() {
        let model = loaded_fixture();
        assert!(matches!(
            model.predict_positional(&[1.0, 2.0]),
            Err(ModelError::Invalid(_))
        ));
        assert!(matches!(
            model.predict_positional(&[1.0, f64::NAN, 3.0]),
            Err(ModelError::Invalid(_))
        ));
    }

    #[test]
    fn zero_tree_intercept_only_artifact_loads_and_predicts() {
        let mut artifact = fixture().artifact;
        artifact["model"] = json!({"kind":"boosted-tree","feature_names":["x"],"intercept":-0.25,"learning_rate":0.1,"trees":[]});
        artifact["calibration"] = json!({"method":"none","slope":1.0,"intercept":0.0});
        let model = ModelArtifact::load(&artifact_bytes(artifact, true)).unwrap();
        let probability = model.predict_positional(&[f64::MAX]).unwrap();
        assert!((probability - sigmoid(-0.25)).abs() < 1e-15);
    }

    #[test]
    fn base_rate_identity_preserves_zero_and_one_extremes() {
        for rate in [0.0, 1.0] {
            let mut artifact = fixture().artifact;
            artifact["model"] = json!({
                "kind": "base-rate",
                "feature_names": ["x"],
                "rate": rate
            });
            artifact["calibration"] = json!({"method": "none", "slope": 1.0, "intercept": 0.0});
            let model = ModelArtifact::load(&artifact_bytes(artifact, true)).unwrap();
            assert_eq!(model.predict_positional(&[f64::MAX]).unwrap(), rate);
        }
    }

    #[test]
    fn extreme_logistic_arithmetic_is_invalid() {
        for (means, scales, weights, features) in [
            (vec![-f64::MAX], vec![1.0], vec![1.0], vec![f64::MAX]),
            (vec![0.0], vec![1.0], vec![f64::MAX], vec![2.0]),
            (
                vec![0.0, 0.0],
                vec![1.0, 1.0],
                vec![f64::MAX, f64::MAX],
                vec![1.0, 1.0],
            ),
        ] {
            let mut artifact = fixture().artifact;
            let names = if features.len() == 2 {
                json!(["x", "y"])
            } else {
                json!(["x"])
            };
            artifact["model"] = json!({"kind":"logistic","feature_names": names, "means": means, "scales": scales, "weights": weights, "intercept": 0.0});
            let model = ModelArtifact::load(&artifact_bytes(artifact, true)).unwrap();
            assert!(matches!(
                model.predict_positional(&features),
                Err(ModelError::Invalid(_))
            ));
        }
    }

    #[test]
    fn extreme_boosted_tree_arithmetic_is_invalid() {
        for trees in [
            json!([{"feature_index":0,"threshold":0.0,"left_value":f64::MAX,"right_value":f64::MAX}]),
            json!([
                {"feature_index":0,"threshold":0.0,"left_value":f64::MAX,"right_value":f64::MAX},
                {"feature_index":0,"threshold":0.0,"left_value":f64::MAX,"right_value":f64::MAX}
            ]),
        ] {
            let mut artifact = fixture().artifact;
            let intercept = if trees.as_array().unwrap().len() == 1 {
                f64::MAX
            } else {
                0.0
            };
            artifact["model"] = json!({"kind":"boosted-tree","feature_names":["x"],"intercept":intercept,"learning_rate":1.0,"trees":trees});
            let model = ModelArtifact::load(&artifact_bytes(artifact, true)).unwrap();
            assert!(matches!(
                model.predict_positional(&[0.0]),
                Err(ModelError::Invalid(_))
            ));
        }
    }

    #[test]
    fn genuinely_finite_large_logit_returns_endpoint_probability() {
        let mut artifact = fixture().artifact;
        artifact["model"] = json!({"kind":"logistic","feature_names":["x"],"means":[0.0],"scales":[1.0],"weights":[0.0],"intercept":1e308});
        artifact["calibration"] = json!({"method":"none","slope":1.0,"intercept":0.0});
        let model = ModelArtifact::load(&artifact_bytes(artifact, true)).unwrap();
        assert_eq!(model.predict_positional(&[0.0]).unwrap(), 1.0);
    }

    #[test]
    fn sigmoid_is_stable_at_extreme_logits() {
        assert_eq!(sigmoid(1000.0), 1.0);
        assert!(sigmoid(-1000.0) < 1e-300);
        assert_eq!(logit(0.5), 0.0);
    }
}
