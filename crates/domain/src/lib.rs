use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Amount(pub u128);
impl Serialize for Amount {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        s.serialize_str(&self.0.to_string())
    }
}
impl<'de> Deserialize<'de> for Amount {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let x = String::deserialize(d)?;
        if x != "0" && x.starts_with('0') {
            return Err(serde::de::Error::custom("non-canonical amount"));
        }
        if x.is_empty() || !x.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(serde::de::Error::custom("invalid amount"));
        }
        x.parse().map(Amount).map_err(serde::de::Error::custom)
    }
}
impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Auction {
    pub id: String,
    pub block: u64,
    pub tokens: Vec<String>,
    pub orders: Vec<Order>,
    pub state: State,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct State {
    pub observed_block: u64,
    pub deadline_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Order {
    pub id: String,
    pub sell_token: String,
    pub buy_token: String,
    pub sell_amount: Amount,
    pub min_buy_amount: Amount,
    pub kind: OrderKind,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OrderKind {
    #[default]
    Sell,
    Buy,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Pool {
    pub id: String,
    pub token0: String,
    pub token1: String,
    pub reserve0: Amount,
    pub reserve1: Amount,
    pub fee_bps: u32,
    #[serde(default)]
    pub concentrated: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SolveRequest {
    pub schema_version: String,
    pub auction: Auction,
    #[serde(default)]
    pub pools: Vec<Pool>,
    #[serde(default)]
    pub split_ratios: Vec<u32>,
    #[serde(default)]
    pub deadline_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Solution {
    pub auction_id: String,
    pub candidates: Vec<Candidate>,
    pub reasons: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<AdvisoryProvenance>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AdvisoryProvenance {
    pub advisory_id: String,
    pub source_network: String,
    pub source_block_number: u64,
    pub source_block_hash: String,
    pub verification: String,
    pub gross_output: Amount,
    pub gas_units: u64,
    pub gas_price: String,
    pub gas_conversion: String,
    pub gas_cost: Amount,
    pub signed_net_surplus: i128,
    pub route: Vec<Route>,
    pub assumptions: Vec<String>,
    pub rejection_reason: Option<String>,
    pub model_policy_metadata: Value,
    pub timestamp_ms: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Candidate {
    pub id: String,
    pub auction_id: String,
    pub routes: Vec<Route>,
    pub expected_surplus: Amount,
    pub gas_proxy: u64,
    pub score: u128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub predicted_validity: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_value: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eligible: Option<bool>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub pool_id: String,
    pub sell_amount: Amount,
    pub buy_amount: Amount,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Notification {
    pub schema_version: String,
    pub auction_id: String,
    pub outcome: String,
    #[serde(default)]
    pub candidate_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::Amount;

    #[test]
    fn amount_serializes_canonically_and_preserves_u128_bounds() {
        for value in [0, 1, u128::MAX] {
            let amount = Amount(value);
            let json = serde_json::to_string(&amount).unwrap();
            assert_eq!(json, format!("\"{value}\""));
            assert_eq!(serde_json::from_str::<Amount>(&json).unwrap(), amount);
        }
    }

    #[test]
    fn amount_rejects_noncanonical_or_out_of_bounds_strings() {
        for value in [
            "",
            "00",
            "01",
            "-1",
            "1.0",
            "340282366920938463463374607431768211456",
        ] {
            assert!(serde_json::from_str::<Amount>(&format!("\"{value}\"")).is_err());
        }
    }
}
