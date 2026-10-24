use crate::{validate_cursor, BlockHeader, CanonicalChain, Error, IngestionCursor, PoolSnapshot};
use serde_json::Value;
use tokio_postgres::{Client, NoTls, Transaction};

pub struct PostgresRepository {
    client: Client,
}

pub struct Persist<'a> {
    pub network: &'a str,
    pub blocks: &'a [BlockHeader],
    pub snapshots: &'a [PoolSnapshot],
    pub cursor: &'a IngestionCursor,
}

impl PostgresRepository {
    pub async fn connect(url: &str) -> Result<Self, Error> {
        let (client, connection) = tokio_postgres::connect(url, NoTls)
            .await
            .map_err(classify_database_error)?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(Self { client })
    }

    pub async fn persist(
        &mut self,
        chain: &CanonicalChain,
        input: Persist<'_>,
    ) -> Result<(), Error> {
        validate_ingestion(chain, &input)?;
        let transaction = self
            .client
            .transaction()
            .await
            .map_err(classify_database_error)?;
        persist_transaction(&transaction, &input).await?;
        transaction.commit().await.map_err(classify_database_error)
    }
}

fn validate_ingestion(chain: &CanonicalChain, input: &Persist<'_>) -> Result<(), Error> {
    if input.network.trim().is_empty() || input.cursor.network != input.network {
        return Err(Error::Invalid("network".into()));
    }
    validate_cursor(chain, input.cursor)?;
    if input.blocks.is_empty() {
        return Err(Error::Invalid("blocks".into()));
    }
    let mut previous: Option<&BlockHeader> = None;
    for block in input.blocks {
        block.validate()?;
        if chain.canonical_hash(block.number) != Some(block.hash.as_str()) {
            return Err(Error::Invalid("noncanonical block".into()));
        }
        if let Some(parent) = previous {
            if parent.number.checked_add(1) != Some(block.number)
                || block.parent_hash != parent.hash
            {
                return Err(Error::IncompleteReplacement);
            }
        }
        previous = Some(block);
    }
    if previous != chain.head() {
        return Err(Error::IncompleteReplacement);
    }
    for snapshot in input.snapshots {
        snapshot.validate()?;
        if snapshot.network != input.network {
            return Err(Error::Invalid("network".into()));
        }
    }
    Ok(())
}

async fn persist_transaction(
    transaction: &Transaction<'_>,
    input: &Persist<'_>,
) -> Result<(), Error> {
    for block in input.blocks {
        put_block(transaction, input.network, block).await?;
    }
    for snapshot in input.snapshots {
        put_snapshot(transaction, snapshot).await?;
    }
    put_cursor(transaction, input.cursor).await?;

    let first_number = checked_i64(input.blocks[0].number, "number")?;
    transaction
        .execute(
            "UPDATE blocks SET canonical=false WHERE network=$1 AND canonical AND number >= $2",
            &[&input.network, &first_number],
        )
        .await
        .map_err(classify_database_error)?;
    for block in input.blocks {
        transaction
            .execute(
                "UPDATE blocks SET canonical=true WHERE network=$1 AND hash=$2",
                &[&input.network, &block.hash],
            )
            .await
            .map_err(classify_database_error)?;
    }
    Ok(())
}

async fn put_block(
    transaction: &Transaction<'_>,
    network: &str,
    block: &BlockHeader,
) -> Result<(), Error> {
    let number = checked_i64(block.number, "number")?;
    let timestamp = checked_i64(block.timestamp, "timestamp")?;
    transaction
        .execute(
            "INSERT INTO blocks(network,number,hash,parent_hash,timestamp,canonical) VALUES($1,$2,$3,$4,$5,false) ON CONFLICT (network,hash) DO NOTHING",
            &[&network, &number, &block.hash, &block.parent_hash, &timestamp],
        )
        .await
        .map_err(classify_database_error)?;
    let row = transaction
        .query_one(
            "SELECT number,parent_hash,timestamp FROM blocks WHERE network=$1 AND hash=$2",
            &[&network, &block.hash],
        )
        .await
        .map_err(classify_database_error)?;
    if !block_row_matches(
        row.get(0),
        row.get::<_, String>(1).as_str(),
        row.get(2),
        block,
    )? {
        return Err(Error::Conflict("immutable block".into()));
    }
    Ok(())
}

async fn put_snapshot(transaction: &Transaction<'_>, snapshot: &PoolSnapshot) -> Result<(), Error> {
    let timestamp = checked_i64(snapshot.timestamp, "timestamp")?;
    let payload = snapshot_payload(snapshot)?;
    transaction
        .execute(
            "INSERT INTO pool_snapshots(network,pool,block_hash,timestamp,payload) VALUES($1,$2,$3,$4,$5) ON CONFLICT (network,pool,block_hash) DO NOTHING",
            &[
                &snapshot.network,
                &snapshot.pool,
                &snapshot.block_hash,
                &timestamp,
                &payload,
            ],
        )
        .await
        .map_err(classify_database_error)?;
    let row = transaction
        .query_one(
            "SELECT timestamp,payload FROM pool_snapshots WHERE network=$1 AND pool=$2 AND block_hash=$3",
            &[&snapshot.network, &snapshot.pool, &snapshot.block_hash],
        )
        .await
        .map_err(classify_database_error)?;
    if !snapshot_row_matches(row.get(0), &row.get::<_, Value>(1), snapshot)? {
        return Err(Error::Conflict("immutable snapshot".into()));
    }
    Ok(())
}

async fn put_cursor(transaction: &Transaction<'_>, cursor: &IngestionCursor) -> Result<(), Error> {
    let next_block = checked_i64(cursor.next_block, "next_block")?;
    let row = transaction
        .query_opt(
            "SELECT next_block,block_hash FROM ingestion_cursors WHERE network=$1 AND source=$2",
            &[&cursor.network, &cursor.source],
        )
        .await
        .map_err(classify_database_error)?;
    if let Some(row) = row {
        let old_next: i64 = row.get(0);
        let old_hash: String = row.get(1);
        if cursor_row_matches(old_next, &old_hash, cursor)? {
            return Ok(());
        }
        if old_next > next_block {
            return Err(Error::Stale("cursor".into()));
        }
        if old_next == next_block {
            return Err(Error::Conflict("cursor".into()));
        }
        transaction
            .execute(
                "UPDATE ingestion_cursors SET next_block=$3,block_hash=$4 WHERE network=$1 AND source=$2",
                &[&cursor.network, &cursor.source, &next_block, &cursor.block_hash],
            )
            .await
            .map_err(classify_database_error)?;
    } else {
        transaction
            .execute(
                "INSERT INTO ingestion_cursors(network,source,next_block,block_hash) VALUES($1,$2,$3,$4)",
                &[&cursor.network, &cursor.source, &next_block, &cursor.block_hash],
            )
            .await
            .map_err(classify_database_error)?;
    }
    Ok(())
}

fn checked_i64(value: u64, name: &str) -> Result<i64, Error> {
    i64::try_from(value).map_err(|_| Error::Overflow(name.into()))
}

fn snapshot_payload(snapshot: &PoolSnapshot) -> Result<Value, Error> {
    serde_json::from_slice(&snapshot.data).map_err(|_| Error::Invalid("data".into()))
}

fn block_row_matches(
    number: i64,
    parent_hash: &str,
    timestamp: i64,
    block: &BlockHeader,
) -> Result<bool, Error> {
    Ok(number == checked_i64(block.number, "number")?
        && parent_hash == block.parent_hash
        && timestamp == checked_i64(block.timestamp, "timestamp")?)
}

fn snapshot_row_matches(
    timestamp: i64,
    payload: &Value,
    snapshot: &PoolSnapshot,
) -> Result<bool, Error> {
    Ok(timestamp == checked_i64(snapshot.timestamp, "timestamp")?
        && payload == &snapshot_payload(snapshot)?)
}

#[allow(dead_code)]
fn cursor_row_matches(
    next_block: i64,
    block_hash: &str,
    cursor: &IngestionCursor,
) -> Result<bool, Error> {
    Ok(next_block == checked_i64(cursor.next_block, "next_block")?
        && block_hash == cursor.block_hash)
}

fn classify_database_error(error: tokio_postgres::Error) -> Error {
    classify_sqlstate(error.code().map(|code| code.code()), error.is_closed())
}

fn classify_sqlstate(code: Option<&str>, closed: bool) -> Error {
    if closed {
        return Error::Unavailable;
    }
    match code {
        Some("23505" | "23P01") => Error::Conflict("database constraint".into()),
        Some(code) if code.starts_with("22") || code.starts_with("23") => {
            Error::Invalid("database constraint".into())
        }
        Some(code) if code.starts_with("08") || code.starts_with("53") => Error::Unavailable,
        Some("40001" | "40P01" | "57P01" | "57P02" | "57P03") => Error::Unavailable,
        _ => Error::Internal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(number: u64, hash: &str, parent_hash: &str) -> BlockHeader {
        BlockHeader {
            number,
            hash: hash.into(),
            parent_hash: parent_hash.into(),
            timestamp: number,
        }
    }

    fn snapshot(network: &str, payload: &[u8]) -> PoolSnapshot {
        PoolSnapshot {
            network: network.into(),
            pool: "pool".into(),
            block_hash: "a".into(),
            timestamp: 1,
            data: payload.into(),
        }
    }

    fn cursor(network: &str) -> IngestionCursor {
        IngestionCursor {
            network: network.into(),
            source: "source".into(),
            next_block: 3,
            block_hash: "b".into(),
        }
    }

    #[test]
    fn row_comparisons_distinguish_idempotence_from_conflict() {
        let block = block(1, "a", "root");
        assert!(block_row_matches(1, "root", 1, &block).unwrap());
        assert!(!block_row_matches(2, "root", 1, &block).unwrap());
        let snapshot = snapshot("net", br#"{"x":1}"#);
        assert!(snapshot_row_matches(1, &serde_json::json!({"x": 1}), &snapshot).unwrap());
        assert!(!snapshot_row_matches(1, &serde_json::json!({"x": 2}), &snapshot).unwrap());
        let cursor = cursor("net");
        assert!(cursor_row_matches(3, "b", &cursor).unwrap());
        assert!(!cursor_row_matches(2, "b", &cursor).unwrap());
    }

    #[test]
    fn batch_validation_requires_one_canonical_suffix_and_network() {
        let mut chain = CanonicalChain::with_network(4, "net").unwrap();
        chain.apply(block(1, "a", "root")).unwrap();
        chain.apply(block(2, "b", "a")).unwrap();
        let blocks = [block(1, "a", "root"), block(2, "b", "a")];
        let snapshots = [snapshot("net", br#"{"x":1}"#)];
        let cursor = cursor("net");
        let input = Persist {
            network: "net",
            blocks: &blocks,
            snapshots: &snapshots,
            cursor: &cursor,
        };
        assert_eq!(validate_ingestion(&chain, &input), Ok(()));

        let wrong_network = [snapshot("other", br#"{"x":1}"#)];
        let input = Persist {
            snapshots: &wrong_network,
            ..input
        };
        assert_eq!(
            validate_ingestion(&chain, &input),
            Err(Error::Invalid("network".into()))
        );
        let incomplete = [block(1, "a", "root")];
        let input = Persist {
            blocks: &incomplete,
            snapshots: &snapshots,
            ..input
        };
        assert_eq!(
            validate_ingestion(&chain, &input),
            Err(Error::IncompleteReplacement)
        );
    }

    #[test]
    fn database_errors_are_sanitized_and_classified() {
        assert_eq!(
            classify_sqlstate(Some("23505"), false),
            Error::Conflict("database constraint".into())
        );
        assert_eq!(
            classify_sqlstate(Some("23503"), false),
            Error::Invalid("database constraint".into())
        );
        assert_eq!(classify_sqlstate(Some("08006"), false), Error::Unavailable);
        assert_eq!(classify_sqlstate(None, true), Error::Unavailable);
        assert_eq!(classify_sqlstate(Some("XX000"), false), Error::Internal);
    }
}
