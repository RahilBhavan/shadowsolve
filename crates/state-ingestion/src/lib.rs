use sha2::{Digest, Sha256};

pub mod postgres;
pub mod rpc;
use std::collections::{BTreeMap, HashMap};
use std::fmt;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct BlockHeader {
    pub number: u64,
    pub hash: String,
    pub parent_hash: String,
    pub timestamp: u64,
}
impl BlockHeader {
    pub fn validate(&self) -> Result<(), Error> {
        nonempty(&self.hash, "hash")?;
        nonempty(&self.parent_hash, "parent_hash")?;
        signed(self.number, "number")?;
        signed(self.timestamp, "timestamp")
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct PoolKey {
    pub network: String,
    pub pool: String,
    pub block_hash: String,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolSnapshot {
    pub network: String,
    pub pool: String,
    pub block_hash: String,
    pub timestamp: u64,
    pub data: Vec<u8>,
}
impl PoolSnapshot {
    pub fn validate(&self) -> Result<(), Error> {
        nonempty(&self.network, "network")?;
        nonempty(&self.pool, "pool")?;
        nonempty(&self.block_hash, "block_hash")?;
        signed(self.timestamp, "timestamp")?;
        let value: serde_json::Value =
            serde_json::from_slice(&self.data).map_err(|_| Error::Invalid("data".into()))?;
        if !value.as_object().is_some_and(|object| !object.is_empty()) {
            return Err(Error::Invalid("data".into()));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IngestionCursor {
    pub network: String,
    pub source: String,
    pub next_block: u64,
    pub block_hash: String,
}
impl IngestionCursor {
    pub fn validate(&self) -> Result<(), Error> {
        nonempty(&self.network, "network")?;
        nonempty(&self.source, "source")?;
        nonempty(&self.block_hash, "block_hash")?;
        signed(self.next_block, "next_block")
    }
}
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Error {
    Invalid(String),
    Reorg,
    IncompleteReplacement,
    Unsafe,
    Missing(String),
    Stale(String),
    Conflict(String),
    Overflow(String),
    Unavailable,
    Internal,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
fn nonempty(s: &str, n: &str) -> Result<(), Error> {
    if s.trim().is_empty() {
        Err(Error::Invalid(n.into()))
    } else {
        Ok(())
    }
}
fn signed(v: u64, n: &str) -> Result<(), Error> {
    if v > i64::MAX as u64 {
        Err(Error::Overflow(n.into()))
    } else {
        Ok(())
    }
}

pub(crate) fn validate_cursor(
    chain: &CanonicalChain,
    cursor: &IngestionCursor,
) -> Result<(), Error> {
    cursor.validate()?;
    if (!chain.network.is_empty() && chain.network != cursor.network)
        || chain.is_unsafe()
        || cursor.next_block == 0
        || chain.canonical_hash(cursor.next_block - 1) != Some(cursor.block_hash.as_str())
    {
        return Err(Error::Invalid("noncanonical cursor".into()));
    }
    Ok(())
}

pub struct CanonicalChain {
    window: usize,
    network: String,
    blocks: BTreeMap<u64, BlockHeader>,
    unsafe_state: bool,
}
impl CanonicalChain {
    pub fn new(window: usize) -> Result<Self, Error> {
        if window == 0 {
            Err(Error::Invalid("window".into()))
        } else {
            Ok(Self {
                window,
                network: String::new(),
                blocks: BTreeMap::new(),
                unsafe_state: false,
            })
        }
    }
    pub fn with_network(window: usize, network: impl Into<String>) -> Result<Self, Error> {
        let mut chain = Self::new(window)?;
        chain.network = network.into();
        nonempty(&chain.network, "network")?;
        Ok(chain)
    }

    pub fn head(&self) -> Option<&BlockHeader> {
        self.blocks.values().next_back()
    }
    pub fn is_unsafe(&self) -> bool {
        self.unsafe_state
    }
    pub fn network(&self) -> &str {
        &self.network
    }
    pub fn reset(&mut self) {
        self.blocks.clear();
        self.unsafe_state = false;
    }
    pub fn replace_branch(&mut self, branch: Vec<BlockHeader>) -> Result<(), Error> {
        if self.unsafe_state {
            return Err(Error::Unsafe);
        }
        if branch.is_empty() {
            return Err(Error::Invalid("branch".into()));
        }
        for b in &branch {
            b.validate()?;
        }
        let ancestor = self
            .blocks
            .values()
            .find(|b| b.hash == branch[0].parent_hash)
            .ok_or(Error::Reorg)?;
        let mut expected = ancestor
            .number
            .checked_add(1)
            .ok_or_else(|| Error::Overflow("number".into()))?;
        let mut parent_hash = ancestor.hash.as_str();
        for b in &branch {
            if b.number != expected || b.parent_hash != parent_hash {
                return Err(Error::IncompleteReplacement);
            }
            parent_hash = &b.hash;
            expected = expected
                .checked_add(1)
                .ok_or_else(|| Error::Overflow("number".into()))?;
        }
        let mut next = self.blocks.clone();
        next.retain(|n, _| *n <= ancestor.number);
        for b in branch {
            next.insert(b.number, b);
        }
        self.blocks = next;
        self.trim();
        Ok(())
    }

    pub fn apply(&mut self, b: BlockHeader) -> Result<(), Error> {
        if self.unsafe_state {
            return Err(Error::Unsafe);
        }
        b.validate()?;
        if self.blocks.is_empty() {
            self.blocks.insert(b.number, b);
            return Ok(());
        }
        if let Some(old) = self.blocks.get(&b.number) {
            if old == &b {
                return Ok(());
            }
        }
        if let Some(h) = self.head() {
            let next = h
                .number
                .checked_add(1)
                .ok_or_else(|| Error::Overflow("number".into()))?;
            if b.number == next && b.parent_hash == h.hash {
                self.blocks.insert(b.number, b);
                self.trim();
                return Ok(());
            }
        }
        let ancestor = self
            .blocks
            .iter()
            .find(|(_, x)| x.hash == b.parent_hash)
            .map(|(n, _)| *n);
        if let Some(a) = ancestor {
            let expected = a
                .checked_add(1)
                .ok_or_else(|| Error::Overflow("number".into()))?;
            if b.number == expected {
                self.blocks.retain(|n, _| *n <= a);
                self.blocks.insert(b.number, b);
                self.trim();
                return Ok(());
            }
            self.unsafe_state = true;
            return Err(Error::IncompleteReplacement);
        }
        self.unsafe_state = true;
        Err(Error::Reorg)
    }
    fn trim(&mut self) {
        while self.blocks.len() > self.window {
            let n = *self.blocks.keys().next().unwrap();
            self.blocks.remove(&n);
        }
    }
    pub fn canonical_hash(&self, n: u64) -> Option<&str> {
        self.blocks.get(&n).map(|b| b.hash.as_str())
    }
}
#[derive(Default)]
pub struct MemoryRepository {
    snapshots: HashMap<PoolKey, PoolSnapshot>,
    cursors: HashMap<(String, String), IngestionCursor>,
}
impl MemoryRepository {
    pub fn put_snapshot(&mut self, s: PoolSnapshot) -> Result<(), Error> {
        s.validate()?;
        let k = PoolKey {
            network: s.network.clone(),
            pool: s.pool.clone(),
            block_hash: s.block_hash.clone(),
        };
        if let Some(old) = self.snapshots.get(&k) {
            if old != &s {
                return Err(Error::Conflict("immutable snapshot".into()));
            }
        } else {
            self.snapshots.insert(k, s);
        }
        Ok(())
    }
    pub fn put_cursor(&mut self, chain: &CanonicalChain, c: IngestionCursor) -> Result<(), Error> {
        validate_cursor(chain, &c)?;
        let k = (c.network.clone(), c.source.clone());
        if let Some(old) = self.cursors.get(&k) {
            if old.next_block == c.next_block && old.block_hash == c.block_hash {
                return Ok(());
            }
            if c.next_block < old.next_block {
                return Err(Error::Stale("cursor".into()));
            }
            if c.next_block == old.next_block {
                return Err(Error::Conflict("cursor".into()));
            }
        }
        self.cursors.insert(k, c);
        Ok(())
    }
    pub fn coherent(
        &self,
        chain: &CanonicalChain,
        network: &str,
        pools: &[String],
        block: &BlockHeader,
        max_age: u64,
        now: u64,
    ) -> Result<Vec<&PoolSnapshot>, Error> {
        if chain.is_unsafe() {
            return Err(Error::Unsafe);
        }
        nonempty(network, "network")?;
        if !chain.network.is_empty() && chain.network != network {
            return Err(Error::Invalid("network".into()));
        }
        block.validate()?;
        if chain.canonical_hash(block.number) != Some(block.hash.as_str()) {
            return Err(Error::Invalid("noncanonical block".into()));
        }
        if pools.is_empty() {
            return Err(Error::Invalid("pools".into()));
        }
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        for p in pools {
            nonempty(p, "pool")?;
            if !seen.insert(p) {
                return Err(Error::Invalid("duplicate pool".into()));
            }
            let k = PoolKey {
                network: network.into(),
                pool: p.clone(),
                block_hash: block.hash.clone(),
            };
            let s = self
                .snapshots
                .get(&k)
                .ok_or_else(|| Error::Missing(p.clone()))?;
            if s.network != network || now.checked_sub(s.timestamp).is_none_or(|a| a > max_age) {
                return Err(Error::Stale(p.clone()));
            }
            out.push(s);
        }
        Ok(out)
    }
}
pub struct ArchiveManifest;
impl ArchiveManifest {
    pub fn record(
        network: &str,
        date: &str,
        pair: &str,
        kind: &str,
        payload: &[u8],
    ) -> Result<(String, String), Error> {
        for (v, n) in [
            (network, "network"),
            (date, "date"),
            (pair, "pair"),
            (kind, "type"),
        ] {
            if v.is_empty()
                || !v
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
                || v == "."
                || v == ".."
            {
                return Err(Error::Invalid(n.into()));
            }
        }
        let hash = hex::encode(Sha256::digest(payload));
        Ok((format!("{network}/{date}/{pair}/{kind}/{hash}.json"), hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn b(n: u64, h: &str, p: &str) -> BlockHeader {
        BlockHeader {
            number: n,
            hash: h.into(),
            parent_hash: p.into(),
            timestamp: 1,
        }
    }
    fn s(h: &str, d: &[u8]) -> PoolSnapshot {
        PoolSnapshot {
            network: "net".into(),
            pool: "pool".into(),
            block_hash: h.into(),
            timestamp: 10,
            data: d.into(),
        }
    }

    #[test]
    fn chain_duplicate_replacement_and_reset() {
        let mut c = CanonicalChain::new(4).unwrap();
        c.apply(b(1, "a", "root")).unwrap();
        c.apply(b(2, "b", "a")).unwrap();
        c.apply(b(2, "b", "a")).unwrap();
        c.apply(b(2, "c", "a")).unwrap();
        assert_eq!(c.canonical_hash(2), Some("c"));
        assert_eq!(c.apply(b(4, "x", "c")), Err(Error::IncompleteReplacement));
        assert_eq!(c.apply(b(3, "d", "c")), Err(Error::Unsafe));
        c.reset();
        assert!(!c.is_unsafe());
    }
    #[test]
    fn json_boundaries_and_coherent_canonical() {
        assert!(s("a", br#"{"#).validate().is_err());
        assert!(s("a", br#"[]"#).validate().is_err());
        assert!(s("a", br#"{}"#).validate().is_err());
        assert!(s("a", br#"{"x":1}"#).validate().is_ok());
        assert_eq!(signed(i64::MAX as u64, "x"), Ok(()));
        assert!(signed(i64::MAX as u64 + 1, "x").is_err());
        let mut c = CanonicalChain::new(2).unwrap();
        c.apply(b(1, "a", "root")).unwrap();
        let mut r = MemoryRepository::default();
        r.put_snapshot(s("a", br#"{"x":1}"#)).unwrap();
        let pools = vec!["pool".into()];
        assert!(r
            .coherent(&c, "net", &pools, &b(1, "a", "root"), 1, 10)
            .is_ok());
        assert!(r
            .coherent(&c, "net", &pools, &b(1, "f", "root"), 1, 10)
            .is_err());
        let mut cross_network = CanonicalChain::with_network(2, "other").unwrap();
        cross_network.apply(b(1, "a", "root")).unwrap();
        assert!(r
            .coherent(&cross_network, "net", &pools, &b(1, "a", "root"), 1, 10)
            .is_err());
    }
    #[test]
    fn branch_replacement_is_atomic() {
        let mut c = CanonicalChain::new(8).unwrap();
        c.apply(b(1, "a1", "root")).unwrap();
        c.apply(b(2, "a2", "a1")).unwrap();
        c.apply(b(3, "a3", "a2")).unwrap();
        c.replace_branch(vec![b(2, "b2", "a1"), b(3, "b3", "b2")])
            .unwrap();
        assert_eq!(c.canonical_hash(2), Some("b2"));
        assert_eq!(c.canonical_hash(3), Some("b3"));
        assert_eq!(c.head().unwrap().hash, "b3");
        assert!(!c.is_unsafe());
        assert_eq!(
            c.replace_branch(vec![b(2, "bad", "a1"), b(4, "bad4", "bad")]),
            Err(Error::IncompleteReplacement)
        );
        assert_eq!(c.canonical_hash(2), Some("b2"));
        assert!(!c.is_unsafe());
        assert_eq!(
            c.replace_branch(vec![b(2, "bad", "a1"), b(3, "bad3", "wrong")]),
            Err(Error::IncompleteReplacement)
        );
        assert_eq!(c.canonical_hash(2), Some("b2"));
        assert_eq!(c.canonical_hash(3), Some("b3"));
    }

    #[test]
    fn archive_components_are_portable() {
        for value in [
            "", ".", "..", "a/b", "a\\\\b", "a\\nb", "a\\0b", "a\\tb", "a b", "é",
        ] {
            assert!(ArchiveManifest::record(value, "2026", "pair", "kind", b"").is_err());
        }
    }

    #[test]
    fn cursor_requires_canonical_block_and_network() {
        let mut chain = CanonicalChain::with_network(4, "net").unwrap();
        chain.apply(b(0, "a", "root")).unwrap();
        let mut r = MemoryRepository::default();
        let cursor = IngestionCursor {
            network: "net".into(),
            source: "s".into(),
            next_block: 1,
            block_hash: "a".into(),
        };
        r.put_cursor(&chain, cursor.clone()).unwrap();
        assert!(r
            .put_cursor(
                &chain,
                IngestionCursor {
                    block_hash: "orphan".into(),
                    ..cursor.clone()
                }
            )
            .is_err());
        assert!(r
            .put_cursor(
                &chain,
                IngestionCursor {
                    network: "other".into(),
                    ..cursor
                }
            )
            .is_err());
    }

    #[test]
    fn immutable_cursor_snapshot_archive() {
        let mut r = MemoryRepository::default();
        r.put_snapshot(s("a", br#"{"x":1}"#)).unwrap();
        assert!(r.put_snapshot(s("a", br#"{"x":2}"#)).is_err());
        let c = IngestionCursor {
            network: "n".into(),
            source: "s".into(),
            next_block: 1,
            block_hash: "a".into(),
        };
        let mut chain = CanonicalChain::new(4).unwrap();
        chain.apply(b(0, "a", "root")).unwrap();
        r.put_cursor(&chain, c.clone()).unwrap();
        assert!(r
            .put_cursor(&chain, IngestionCursor { next_block: 0, ..c })
            .is_err());
        let (p, h) = ArchiveManifest::record("n", "2026", "pair", "snapshot", b"x").unwrap();
        assert!(p.ends_with(&format!("/{h}.json")));
        assert!(ArchiveManifest::record("n", "../x", "p", "k", b"").is_err());
    }
}
