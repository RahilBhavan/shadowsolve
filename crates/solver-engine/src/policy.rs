use serde::Deserialize;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read};

pub const MAX_POLICY_BYTES: usize = 64 * 1024;
const MAX_TEXT: usize = 128;
const MAX_ITEMS: usize = 64;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtocolPolicy {
    pub network: String,
    pub allowed_tokens: BTreeSet<String>,
    pub allowed_pools: Vec<PolicyPool>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyPool {
    pub id: String,
    pub token0: String,
    pub token1: String,
    pub fee_bps: u32,
    pub concentrated: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPolicy {
    network: String,
    allowed_tokens: Vec<String>,
    allowed_pools: Vec<PolicyPool>,
}

pub fn load(path: impl AsRef<std::path::Path>) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let mut bytes = Vec::with_capacity(MAX_POLICY_BYTES + 1);
    file.by_ref()
        .take((MAX_POLICY_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() > MAX_POLICY_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid policy size",
        ));
    }
    Ok(bytes)
}

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_TEXT && value.is_ascii()
}

impl ProtocolPolicy {
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.is_empty() || bytes.len() > MAX_POLICY_BYTES {
            return Err("invalid policy size".into());
        }
        let raw: RawPolicy = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        if !valid_text(&raw.network)
            || raw.allowed_tokens.is_empty()
            || raw.allowed_tokens.len() > MAX_ITEMS
            || raw.allowed_pools.is_empty()
            || raw.allowed_pools.len() > MAX_ITEMS
            || raw.allowed_tokens.iter().any(|x| !valid_text(x))
        {
            return Err("invalid policy fields or counts".into());
        }
        let tokens: BTreeSet<_> = raw.allowed_tokens.iter().cloned().collect();
        if tokens.len() != raw.allowed_tokens.len() {
            return Err("duplicate policy token".into());
        }
        let mut pool_ids = BTreeSet::new();
        if raw.allowed_pools.iter().any(|x| {
            !valid_text(&x.id)
                || !valid_text(&x.token0)
                || !valid_text(&x.token1)
                || !pool_ids.insert(x.id.clone())
                || x.token0 == x.token1
                || x.fee_bps >= 10000
                || !tokens.contains(&x.token0)
                || !tokens.contains(&x.token1)
        }) {
            return Err("invalid policy pool".into());
        }
        Ok(Self {
            network: raw.network,
            allowed_tokens: tokens,
            allowed_pools: raw.allowed_pools,
        })
    }
}
