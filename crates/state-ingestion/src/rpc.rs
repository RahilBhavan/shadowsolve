use crate::{BlockHeader, Error, PoolSnapshot};
use reqwest::redirect::Policy;
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

const MAX_RPC_RESPONSE_BYTES: usize = 1024 * 1024;
const GET_RESERVES_SELECTOR: &str = "0x0902f1ac";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngestionConfig {
    pub rpc_url: String,
    pub network: String,
    pub start_block: u64,
    pub finality_depth: u64,
    pub poll_interval: Duration,
    pub max_age: u64,
    pub pools: Vec<String>,
}
impl IngestionConfig {
    pub fn from_env() -> Result<Self, Error> {
        let get = |name: &str| std::env::var(name).map_err(|_| Error::Missing(name.into()));
        let parse = |name: &str, default: u64| -> Result<u64, Error> {
            std::env::var(name).ok().map_or(Ok(default), |v| {
                v.parse().map_err(|_| Error::Invalid(name.into()))
            })
        };
        let pools = get("SHADOWSOLVE_INGESTION_POOLS")?
            .split(',')
            .map(str::trim)
            .filter(|pool| !pool.is_empty())
            .map(str::to_ascii_lowercase)
            .collect::<Vec<_>>();
        if pools.is_empty()
            || pools.iter().any(|pool| !valid_address(pool))
            || pools
                .iter()
                .enumerate()
                .any(|(index, pool)| pools[..index].contains(pool))
        {
            return Err(Error::Invalid("SHADOWSOLVE_INGESTION_POOLS".into()));
        }
        let poll_seconds = parse("SHADOWSOLVE_POLL_SECONDS", 5)?;
        let max_age = parse("SHADOWSOLVE_MAX_AGE_SECONDS", 60)?;
        if poll_seconds == 0
            || max_age == 0
            || poll_seconds > i64::MAX as u64
            || max_age > i64::MAX as u64
        {
            return Err(Error::Invalid("ingestion interval".into()));
        }
        Ok(Self {
            rpc_url: get("SHADOWSOLVE_RPC_URL")?,
            network: get("SHADOWSOLVE_NETWORK")?,
            start_block: parse("SHADOWSOLVE_START_BLOCK", 0)?,
            finality_depth: parse("SHADOWSOLVE_FINALITY_DEPTH", 12)?,
            poll_interval: Duration::from_secs(poll_seconds),
            max_age,
            pools,
        })
    }
}

#[allow(async_fn_in_trait)]
pub trait RpcTransport {
    async fn head(&self) -> Result<BlockHeader, Error>;
    async fn finalized(&self) -> Result<BlockHeader, Error>;
    async fn block(&self, number: u64) -> Result<BlockHeader, Error>;
    async fn pool_snapshot(&self, pool: &str, block: &BlockHeader) -> Result<PoolSnapshot, Error>;
}

pub struct HttpRpcTransport {
    client: Client,
    rpc_url: String,
    network: String,
    next_id: AtomicU64,
}

impl HttpRpcTransport {
    pub fn new(rpc_url: String, network: String) -> Result<Self, Error> {
        let parsed = reqwest::Url::parse(&rpc_url)
            .map_err(|_| Error::Invalid("SHADOWSOLVE_RPC_URL".into()))?;
        if !matches!(parsed.scheme(), "http" | "https")
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || network.trim().is_empty()
        {
            return Err(Error::Invalid("rpc configuration".into()));
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .redirect(Policy::none())
            .build()
            .map_err(|_| Error::Invalid("SHADOWSOLVE_RPC_URL".into()))?;
        Ok(Self {
            client,
            rpc_url,
            network,
            next_id: AtomicU64::new(1),
        })
    }

    async fn call(&self, method: &str, params: Value) -> Result<Value, Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let response = self
            .client
            .post(&self.rpc_url)
            .json(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .send()
            .await
            .map_err(|_| Error::Unavailable)?;
        if !response.status().is_success() {
            return Err(Error::Unavailable);
        }
        let bytes = response.bytes().await.map_err(|_| Error::Unavailable)?;
        if bytes.len() > MAX_RPC_RESPONSE_BYTES {
            return Err(Error::Invalid("rpc response size".into()));
        }
        let body: Value =
            serde_json::from_slice(&bytes).map_err(|_| Error::Invalid("rpc json".into()))?;
        if body.get("jsonrpc") != Some(&Value::String("2.0".into()))
            || body.get("id").and_then(Value::as_u64) != Some(id)
            || body.get("error").is_some()
        {
            return Err(Error::Unavailable);
        }
        body.get("result")
            .cloned()
            .filter(|value| !value.is_null())
            .ok_or(Error::Unavailable)
    }

    async fn tagged_block(&self, tag: Value) -> Result<BlockHeader, Error> {
        parse_block(
            self.call("eth_getBlockByNumber", json!([tag, false]))
                .await?,
        )
    }
}

impl RpcTransport for HttpRpcTransport {
    async fn head(&self) -> Result<BlockHeader, Error> {
        self.tagged_block(Value::String("latest".into())).await
    }

    async fn finalized(&self) -> Result<BlockHeader, Error> {
        self.tagged_block(Value::String("finalized".into())).await
    }

    async fn block(&self, number: u64) -> Result<BlockHeader, Error> {
        self.tagged_block(Value::String(format!("0x{number:x}")))
            .await
    }

    async fn pool_snapshot(&self, pool: &str, block: &BlockHeader) -> Result<PoolSnapshot, Error> {
        if !valid_address(pool) {
            return Err(Error::Invalid("pool".into()));
        }
        let result = self
            .call(
                "eth_call",
                json!([{"to":pool,"data":GET_RESERVES_SELECTOR}, format!("0x{:x}", block.number)]),
            )
            .await?;
        let encoded = result
            .as_str()
            .ok_or_else(|| Error::Invalid("eth_call result".into()))?;
        let (reserve0, reserve1) = parse_reserves(encoded)?;
        Ok(PoolSnapshot {
            network: self.network.clone(),
            pool: pool.into(),
            block_hash: block.hash.clone(),
            timestamp: block.timestamp,
            data: serde_json::to_vec(&json!({
                "reserve0": reserve0.to_string(),
                "reserve1": reserve1.to_string(),
            }))
            .map_err(|_| Error::Internal)?,
        })
    }
}

fn valid_address(value: &str) -> bool {
    value.len() == 42
        && value.starts_with("0x")
        && value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn valid_hash(value: &str) -> bool {
    value.len() == 66
        && value.starts_with("0x")
        && value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn parse_quantity(value: &Value, name: &str) -> Result<u64, Error> {
    let encoded = value
        .as_str()
        .and_then(|value| value.strip_prefix("0x"))
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Error::Invalid(name.into()))?;
    u64::from_str_radix(encoded, 16).map_err(|_| Error::Invalid(name.into()))
}

fn parse_block(value: Value) -> Result<BlockHeader, Error> {
    let block = BlockHeader {
        number: parse_quantity(&value["number"], "block number")?,
        hash: value["hash"]
            .as_str()
            .filter(|value| valid_hash(value))
            .ok_or_else(|| Error::Invalid("block hash".into()))?
            .into(),
        parent_hash: value["parentHash"]
            .as_str()
            .filter(|value| valid_hash(value))
            .ok_or_else(|| Error::Invalid("parent hash".into()))?
            .into(),
        timestamp: parse_quantity(&value["timestamp"], "block timestamp")?,
    };
    block.validate()?;
    Ok(block)
}

fn parse_reserves(value: &str) -> Result<(u128, u128), Error> {
    let encoded = value
        .strip_prefix("0x")
        .filter(|value| value.len() >= 128 && value.len() % 2 == 0)
        .ok_or_else(|| Error::Invalid("getReserves result".into()))?;
    let parse_word = |word: &str| {
        if !word.bytes().all(|byte| byte.is_ascii_hexdigit())
            || word[..32].bytes().any(|byte| byte != b'0')
        {
            return Err(Error::Overflow("reserve".into()));
        }
        u128::from_str_radix(&word[32..], 16).map_err(|_| Error::Invalid("reserve".into()))
    };
    Ok((parse_word(&encoded[..64])?, parse_word(&encoded[64..128])?))
}

pub struct ReadOnlyAdapter<T> {
    pub config: IngestionConfig,
    pub transport: T,
}
impl<T: RpcTransport> ReadOnlyAdapter<T> {
    pub async fn ingest_once(
        &self,
    ) -> Result<(BlockHeader, BlockHeader, Vec<PoolSnapshot>), Error> {
        let head = self.transport.head().await?;
        let finalized = self.transport.finalized().await?;
        if finalized.number < self.config.start_block
            || finalized.number > head.number
            || finalized.timestamp > head.timestamp
            || head.number.saturating_sub(finalized.number) < self.config.finality_depth
        {
            return Err(Error::Unsafe);
        }
        let mut snapshots = Vec::with_capacity(self.config.pools.len());
        for pool in &self.config.pools {
            snapshots.push(self.transport.pool_snapshot(pool, &finalized).await?);
        }
        if snapshots.iter().any(|snapshot| {
            snapshot.network != self.config.network
                || snapshot.block_hash != finalized.hash
                || snapshot.timestamp != finalized.timestamp
        }) {
            return Err(Error::Stale("snapshot block".into()));
        }
        Ok((head, finalized, snapshots))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_block_and_reserve_boundaries() {
        let block = parse_block(json!({
            "number":"0x2a", "hash":format!("0x{:064x}", 42),
            "parentHash":format!("0x{:064x}", 41), "timestamp":"0x64"
        }))
        .unwrap();
        assert_eq!(block.number, 42);
        assert_eq!(block.timestamp, 100);
        let encoded = format!("0x{:064x}{:064x}{:064x}", 12u128, 34u128, 0u128);
        assert_eq!(parse_reserves(&encoded), Ok((12, 34)));
        assert!(parse_reserves("0x01").is_err());
        let overflow = format!("0x1{:063x}{:064x}", 0, 1);
        assert!(matches!(parse_reserves(&overflow), Err(Error::Overflow(_))));
    }

    #[test]
    fn pool_addresses_are_strict() {
        assert!(valid_address("0x0000000000000000000000000000000000000001"));
        assert!(!valid_address("pool-1"));
        assert!(!valid_address("0x000000000000000000000000000000000000000g"));
    }
}
