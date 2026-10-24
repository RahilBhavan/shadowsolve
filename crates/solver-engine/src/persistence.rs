use domain::{Notification, Solution, SolveRequest};
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::watch;
use tokio::sync::Mutex;
use tokio_postgres::{Client, NoTls};

const REQUIRED_TABLES: &[(&str, &[&str])] = &[
    ("auctions", &["id", "payload"]),
    ("candidates", &["id", "auction_id", "payload"]),
    (
        "predictions",
        &["id", "auction_id", "candidate_id", "payload"],
    ),
    ("notifications", &["id", "payload"]),
    (
        "advisory_provenance",
        &[
            "advisory_id",
            "auction_id",
            "source_network",
            "block_number",
            "block_hash",
            "gas_units",
            "gas_price",
            "gross_output",
            "gas_cost",
            "net_surplus",
            "rejection_reason",
            "route",
            "assumptions",
            "rejection_metadata",
            "model_policy_metadata",
            "payload",
            "created_at",
        ],
    ),
];

pub struct AdvisoryRecord {
    pub advisory_id: String,
    pub auction_id: String,
    pub source_network: String,
    pub block_number: i64,
    pub block_hash: String,
    pub gas_units: i64,
    pub gas_price: String,
    pub gross_output: String,
    pub gas_cost: String,
    pub net_surplus: String,
    pub rejection_reason: Option<String>,
    pub route: Value,
    pub assumptions: Value,
    pub rejection_metadata: Value,
    pub model_policy_metadata: Value,
    pub payload: Value,
}

#[derive(Debug)]
pub enum PersistenceError {
    Database(tokio_postgres::Error),
    AdvisoryConflict,
    Serialization(serde_json::Error),
}

impl std::fmt::Display for PersistenceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(error) => error.fmt(formatter),
            Self::AdvisoryConflict => formatter.write_str("conflicting advisory provenance"),
            Self::Serialization(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for PersistenceError {}

impl From<tokio_postgres::Error> for PersistenceError {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Database(error)
    }
}
impl From<serde_json::Error> for PersistenceError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialization(error)
    }
}

#[derive(Clone)]
pub struct ProjectionRows {
    pub auctions: Vec<Value>,
    pub candidates: Vec<Value>,
    pub notifications: Vec<Value>,
}

#[derive(Clone)]
pub struct Persistence {
    client: Arc<Mutex<Client>>,
    alive: watch::Receiver<bool>,
}

impl Persistence {
    #[allow(dead_code)]
    pub async fn connect(url: &str) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
        let (alive_tx, alive) = watch::channel(true);
        tokio::spawn(async move {
            if let Err(error) = connection.await {
                eprintln!("postgres connection failed: {error}");
            }
            let _ = alive_tx.send(false);
        });
        for (table, columns) in REQUIRED_TABLES {
            let exists = client.query_one("SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_schema='public' AND table_name=$1)", &[table]).await?.get::<_, bool>(0);
            if !exists {
                return Err(format!("required postgres table missing: {table}").into());
            }
            for column in *columns {
                let present = client.query_one("SELECT EXISTS (SELECT 1 FROM information_schema.columns WHERE table_schema='public' AND table_name=$1 AND column_name=$2)", &[table, column]).await?.get::<_, bool>(0);
                if !present {
                    return Err(
                        format!("required postgres column missing: {table}.{column}").into(),
                    );
                }
            }
        }
        Ok(Self {
            client: Arc::new(Mutex::new(client)),
            alive,
        })
    }

    pub fn is_alive(&self) -> bool {
        *self.alive.borrow()
    }

    pub async fn notification(&self, n: &Notification) -> Result<bool, tokio_postgres::Error> {
        let mut client = self.client.lock().await;
        let tx = client.transaction().await?;
        let id = serde_json::to_string(&(
            n.schema_version.as_str(),
            n.auction_id.as_str(),
            n.candidate_id.as_deref(),
        ))
        .unwrap_or_else(|_| "[]".into());
        let inserted = tx
            .execute(
                "INSERT INTO notifications (id,payload) VALUES ($1,$2) ON CONFLICT (id) DO NOTHING",
                &[&id, &serde_json::to_value(n).unwrap_or(Value::Null)],
            )
            .await?;
        tx.commit().await?;
        Ok(inserted == 1)
    }

    pub async fn persist_solve(
        &self,
        request: &SolveRequest,
        solution: &Solution,
        advisory: Option<&AdvisoryRecord>,
    ) -> Result<(), PersistenceError> {
        let mut client = self.client.lock().await;
        let tx = client.transaction().await?;
        let auction = serde_json::to_value(&request.auction)?;
        tx.execute("INSERT INTO auctions (id,payload) VALUES ($1,$2) ON CONFLICT (id) DO UPDATE SET payload=EXCLUDED.payload", &[&request.auction.id, &auction]).await?;
        for candidate in &solution.candidates {
            let payload = serde_json::to_value(candidate)?;
            tx.execute(
                "INSERT INTO candidates (auction_id,payload) VALUES ($1,$2) ON CONFLICT (auction_id,payload) DO NOTHING",
                &[&request.auction.id, &payload],
            ).await?;
            let candidate_id: i64 = tx
                .query_one(
                    "SELECT id FROM candidates WHERE auction_id=$1 AND payload=$2",
                    &[&request.auction.id, &payload],
                )
                .await?
                .get(0);
            let prediction_id =
                serde_json::to_string(&(request.auction.id.as_str(), candidate.id.as_str()))
                    .unwrap_or_else(|_| "[]".into());
            tx.execute("INSERT INTO predictions (id,auction_id,candidate_id,payload) VALUES ($1,$2,$3,$4) ON CONFLICT (id) DO UPDATE SET candidate_id=EXCLUDED.candidate_id,payload=EXCLUDED.payload", &[&prediction_id, &request.auction.id, &candidate_id, &payload]).await?;
        }
        if let Some(advisory) = advisory {
            let written = tx.execute(
                "INSERT INTO advisory_provenance (advisory_id,auction_id,source_network,block_number,block_hash,gas_units,gas_price,gross_output,gas_cost,net_surplus,rejection_reason,route,assumptions,rejection_metadata,model_policy_metadata,payload) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16) ON CONFLICT (advisory_id) DO UPDATE SET payload=advisory_provenance.payload WHERE advisory_provenance.payload=EXCLUDED.payload",
                &[&advisory.advisory_id,&advisory.auction_id,&advisory.source_network,&advisory.block_number,&advisory.block_hash,&advisory.gas_units,&advisory.gas_price,&advisory.gross_output,&advisory.gas_cost,&advisory.net_surplus,&advisory.rejection_reason,&advisory.route,&advisory.assumptions,&advisory.rejection_metadata,&advisory.model_policy_metadata,&advisory.payload],
            ).await?;
            if written != 1 {
                return Err(PersistenceError::AdvisoryConflict);
            }
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn latest_advisory(&self) -> Result<Option<Value>, tokio_postgres::Error> {
        let client = self.client.lock().await;
        Ok(client
            .query_opt(
                "SELECT payload FROM advisory_provenance ORDER BY id DESC LIMIT 1",
                &[],
            )
            .await?
            .map(|row| row.get(0)))
    }

    pub async fn load_projection(&self) -> Result<ProjectionRows, tokio_postgres::Error> {
        let client = self.client.lock().await;
        let mut auctions: Vec<Value> = client
            .query(
                "SELECT a.payload FROM auctions a ORDER BY (SELECT MAX(c.id) FROM candidates c WHERE c.auction_id=a.id) DESC NULLS LAST,a.id DESC LIMIT 100",
                &[],
            )
            .await?
            .into_iter()
            .map(|r| r.get(0))
            .collect();
        let mut candidates: Vec<Value> = client
            .query(
                "SELECT payload FROM candidates ORDER BY id DESC LIMIT 500",
                &[],
            )
            .await?
            .into_iter()
            .map(|r| r.get(0))
            .collect();
        let mut notifications: Vec<Value> = client
            .query(
                "SELECT payload FROM notifications ORDER BY id DESC LIMIT 500",
                &[],
            )
            .await?
            .into_iter()
            .map(|r| r.get(0))
            .collect();
        auctions.reverse();
        candidates.reverse();
        notifications.reverse();
        Ok(ProjectionRows {
            auctions,
            candidates,
            notifications,
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tuple_ids_do_not_collide_when_components_contain_colons() {
        let first = serde_json::to_string(&("a:b", "c")).unwrap();
        let second = serde_json::to_string(&("a", "b:c")).unwrap();
        assert_ne!(first, second);
    }
}
