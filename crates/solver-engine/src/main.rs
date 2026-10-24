use axum::{
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use domain::{
    AdvisoryProvenance, Amount, Candidate, Notification, Pool, Route, Solution, SolveRequest,
};
use serde_json::Value;
use state_ingestion::{
    postgres::{Persist as IngestionPersist, PostgresRepository},
    rpc::{HttpRpcTransport, IngestionConfig, ReadOnlyAdapter, RpcTransport},
    BlockHeader, CanonicalChain, Error as IngestionError, IngestionCursor, PoolSnapshot,
};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Semaphore;

mod persistence;
use persistence::{AdvisoryRecord, Persistence, ProjectionRows};

const MAX_NOTIFICATION_TEXT: usize = 128;
const MAX_REPOSITORY_RECORDS: usize = 10_000;
const MAX_REPOSITORY_BYTES: usize = 10 * 1024 * 1024;
const NOTIFY_CONCURRENCY: usize = 4;
const SUPPORTED_SCHEMA_VERSION: &str = "1";
const SUPPORTED_OUTCOMES: &[&str] = &["accepted", "rejected", "failed", "solved", "ok"];
const DASHBOARD_STALE_MS: u64 = 300_000;
const MAX_INGESTION_CATCHUP_BLOCKS: u64 = 256;

#[derive(Clone)]
struct IngestionHandle {
    state: Arc<Mutex<IngestionState>>,
}

struct IngestionState {
    configured: bool,
    network: Option<String>,
    max_age: u64,
    status: String,
    head: Option<BlockHeader>,
    finalized: Option<BlockHeader>,
    snapshots: Vec<PoolSnapshot>,
    last_success: Option<u64>,
    unsafe_state: bool,
    canonical: bool,
}

impl IngestionHandle {
    fn disabled() -> Self {
        Self {
            state: Arc::new(Mutex::new(IngestionState {
                configured: false,
                network: None,
                max_age: 0,
                status: "disabled".into(),
                head: None,
                finalized: None,
                snapshots: Vec::new(),
                last_success: None,
                unsafe_state: false,
                canonical: false,
            })),
        }
    }

    fn configured(config: &IngestionConfig) -> Self {
        Self {
            state: Arc::new(Mutex::new(IngestionState {
                configured: true,
                network: Some(config.network.clone()),
                max_age: config.max_age,
                status: "starting".into(),
                head: None,
                finalized: None,
                snapshots: Vec::new(),
                last_success: None,
                unsafe_state: false,
                canonical: false,
            })),
        }
    }

    fn status(&self) -> Value {
        let now = current_epoch_seconds();
        let Ok(state) = self.state.lock() else {
            return serde_json::json!({"configured":true,"status":"unavailable","fresh":false,"unsafe":true,"canonical":false});
        };
        let fresh = ingestion_fresh(&state, now);
        let status = if state.status == "ready" && !fresh {
            "stale"
        } else {
            &state.status
        };
        serde_json::json!({
            "configured": state.configured,
            "status": status,
            "network": state.network,
            "head": state.head.as_ref().map(|block| block.number),
            "finalized": state.finalized.as_ref().map(|block| block.number),
            "hash": state.finalized.as_ref().map(|block| block.hash.clone()),
            "fresh": fresh,
            "unsafe": state.unsafe_state,
            "canonical": state.canonical,
            "lastSuccess": state.last_success,
            "maxAgeSeconds": state.max_age,
        })
    }

    fn prepare(
        &self,
        request: &SolveRequest,
    ) -> Result<(SolveRequest, IngestionSource), &'static str> {
        let now = current_epoch_seconds();
        let state = self.state.lock().map_err(|_| "ingestion unavailable")?;
        if !state.configured {
            return Err("ingestion disabled");
        }
        let finalized = state.finalized.as_ref().ok_or("ingestion unavailable")?;
        if !ingestion_fresh(&state, now) {
            return Err("ingestion stale, unsafe, or noncanonical");
        }
        if request.auction.block != finalized.number
            || request.auction.state.observed_block != finalized.number
        {
            return Err("solve block is not the canonical finalized block");
        }
        let mut grounded = request.clone();
        for pool in &mut grounded.pools {
            let snapshot = state
                .snapshots
                .iter()
                .find(|snapshot| snapshot.pool.eq_ignore_ascii_case(&pool.id))
                .ok_or("solve pool is not ingestion allow-listed")?;
            apply_snapshot(pool, snapshot)?;
        }
        Ok((
            grounded,
            IngestionSource {
                network: state.network.clone().ok_or("ingestion unavailable")?,
                head: state.head.as_ref().ok_or("ingestion unavailable")?.number,
                block: finalized.clone(),
            },
        ))
    }
}

#[derive(Clone)]
struct IngestionSource {
    network: String,
    head: u64,
    block: BlockHeader,
}

fn apply_snapshot(pool: &mut Pool, snapshot: &PoolSnapshot) -> Result<(), &'static str> {
    let value: Value =
        serde_json::from_slice(&snapshot.data).map_err(|_| "invalid ingested pool snapshot")?;
    let parse = |name: &str| {
        value[name]
            .as_str()
            .and_then(|number| number.parse::<u128>().ok())
            .filter(|number| *number > 0)
            .ok_or("invalid ingested pool reserves")
    };
    pool.reserve0 = Amount(parse("reserve0")?);
    pool.reserve1 = Amount(parse("reserve1")?);
    Ok(())
}

fn current_epoch_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(u64::MAX, |duration| duration.as_secs())
}

fn ingestion_fresh(state: &IngestionState, now: u64) -> bool {
    state.configured
        && state.status == "ready"
        && state.canonical
        && !state.unsafe_state
        && state
            .last_success
            .is_some_and(|t| now.saturating_sub(t) <= state.max_age)
        && state.finalized.as_ref().is_some_and(|b| {
            b.timestamp <= now.saturating_add(state.max_age)
                && now.saturating_sub(b.timestamp) <= state.max_age
        })
}

struct DashboardProjection {
    revision: u64,
    updates: u64,
    last_update: Option<u64>,
    auctions: Vec<Value>,
    candidates: Vec<Value>,
    failures: Vec<Value>,
    notifications: Vec<Value>,
    route: Value,
    latest_advisory: Option<Value>,
}
fn unavailable_route() -> Value {
    serde_json::json!({"name":"Unavailable","commitmentMargin":"Unavailable","status":"Unavailable","legs":[]})
}

impl DashboardProjection {
    fn new() -> Self {
        Self {
            revision: 0,
            updates: 0,
            last_update: None,
            auctions: Vec::new(),
            candidates: Vec::new(),
            failures: Vec::new(),
            notifications: Vec::new(),
            route: unavailable_route(),
            latest_advisory: None,
        }
    }
    fn push(v: &mut Vec<Value>, value: Value, cap: usize) {
        v.push(value);
        if v.len() > cap {
            v.remove(0);
        }
    }
    fn candidate_value(candidate: &Candidate, model: bool) -> Value {
        serde_json::json!({"id":candidate.id,"auctionId":candidate.auction_id,"name":candidate.id,"score":candidate.score.to_string(),"predictedValidity":candidate.predicted_validity.map_or_else(|| "Unavailable".into(), |v| v.to_string()),"decisionValue":candidate.risk_value.map_or_else(|| "Unavailable".into(), |v| v.to_string()),"routes":candidate.routes,"eligible":candidate.eligible.map_or_else(|| "Unavailable".into(), |v| v.to_string()),"modelAvailable":model})
    }
    fn solve(&mut self, r: &SolveRequest, s: &Solution, model: bool) {
        self.solve_at(r, s, model, current_epoch_ms());
    }
    fn solve_at(&mut self, r: &SolveRequest, s: &Solution, model: bool, now: u64) {
        let pair = r
            .auction
            .orders
            .first()
            .map(|o| format!("{}/{}", o.sell_token, o.buy_token))
            .unwrap_or_default();
        let selected = s.candidates.first();
        Self::push(
            &mut self.auctions,
            serde_json::json!({"id":r.auction.id,"pair":pair,"block":r.auction.block,"status":"open","candidate":selected.map_or_else(|| "Unavailable".into(), |c| c.id.clone()),"value":selected.map_or_else(|| "Unavailable".into(), |c| c.score.to_string())}),
            100,
        );
        for c in &s.candidates {
            Self::push(&mut self.candidates, Self::candidate_value(c, model), 500);
        }
        self.latest_advisory = s
            .provenance
            .as_ref()
            .and_then(|p| serde_json::to_value(p).ok());
        self.route = selected.map_or_else(unavailable_route, |c| serde_json::json!({
            "name": c.id,
            "commitmentMargin": c.risk_value.map_or_else(|| "Unavailable".into(), |v| v.to_string()),
            "status": if c.eligible == Some(true) { "eligible" } else { "Unavailable" },
            "legs": c.routes.iter().map(|route| serde_json::json!({
                "pool": route.pool_id,
                "name": route.pool_id,
                "sellAmount": route.sell_amount.to_string(),
                "buyAmount": route.buy_amount.to_string(),
            })).collect::<Vec<_>>(),
        }));
        self.revision += 1;
        self.updates += 1;
        self.last_update = Some(now);
    }
    fn notify(&mut self, n: &Notification) {
        self.notify_at(n, current_epoch_ms());
    }
    fn notify_at(&mut self, n: &Notification, now: u64) {
        Self::push(
            &mut self.notifications,
            serde_json::to_value(n).unwrap_or(Value::Null),
            500,
        );
        if let Some(a) = self.auctions.iter_mut().find(|a| a["id"] == n.auction_id) {
            a["status"] = Value::String(n.outcome.clone());
        }
        if matches!(n.outcome.as_str(), "failed" | "rejected") {
            Self::push(
                &mut self.failures,
                serde_json::json!({"id":n.auction_id,"cause":n.outcome,"message":n.message}),
                500,
            );
        }
        self.revision += 1;
        self.updates += 1;
        self.last_update = Some(now);
    }
    fn restore(
        &mut self,
        rows: ProjectionRows,
        model: bool,
        now: u64,
    ) -> Result<(), serde_json::Error> {
        let ProjectionRows {
            auctions,
            candidates,
            notifications,
        } = rows;
        let candidates = candidates
            .into_iter()
            .map(serde_json::from_value)
            .collect::<Result<Vec<Candidate>, _>>()?;
        for auction in auctions {
            let request = SolveRequest {
                schema_version: SUPPORTED_SCHEMA_VERSION.into(),
                auction: serde_json::from_value(auction)?,
                pools: Vec::new(),
                split_ratios: Vec::new(),
                deadline_ms: 0,
            };
            let candidates = candidates
                .iter()
                .filter(|candidate| candidate.auction_id == request.auction.id)
                .cloned()
                .collect();
            self.solve_at(
                &request,
                &Solution {
                    auction_id: request.auction.id.clone(),
                    candidates,
                    reasons: Vec::new(),
                    provenance: None,
                },
                model,
                now,
            );
        }
        self.candidates.clear();
        for candidate in &candidates {
            Self::push(
                &mut self.candidates,
                Self::candidate_value(candidate, model),
                500,
            );
        }
        for value in notifications {
            self.notify_at(&serde_json::from_value(value)?, now);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct NotificationIdentity {
    schema_version: String,
    auction_id: String,
    candidate_id: Option<String>,
}

fn notification_identity(n: &Notification) -> NotificationIdentity {
    NotificationIdentity {
        schema_version: n.schema_version.clone(),
        auction_id: n.auction_id.clone(),
        candidate_id: n.candidate_id.clone(),
    }
}

fn validate_notification(n: &Notification) -> Result<(), RepositoryError> {
    if n.schema_version != SUPPORTED_SCHEMA_VERSION
        || !SUPPORTED_OUTCOMES.contains(&n.outcome.as_str())
        || n.auction_id.is_empty()
        || n.auction_id.len() > MAX_NOTIFICATION_TEXT
        || n.candidate_id
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > MAX_NOTIFICATION_TEXT)
        || n.message
            .as_ref()
            .is_some_and(|s| s.len() > MAX_NOTIFICATION_TEXT)
    {
        return Err(RepositoryError::InvalidNotification);
    }
    Ok(())
}

fn current_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(u64::MAX, |d| d.as_millis().min(u64::MAX as u128) as u64)
}

#[derive(Debug)]
enum RepositoryError {
    Io(io::Error),
    Serialization(serde_json::Error),
    Capacity,
    DuplicateNotification,
    InvalidNotification,
}
impl std::fmt::Display for RepositoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "repository I/O error: {e}"),
            Self::Serialization(e) => write!(f, "repository serialization error: {e}"),
            Self::Capacity => write!(f, "notification repository capacity exceeded"),
            Self::DuplicateNotification => write!(f, "duplicate notification in repository"),
            Self::InvalidNotification => write!(f, "invalid notification"),
        }
    }
}
impl From<io::Error> for RepositoryError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<serde_json::Error> for RepositoryError {
    fn from(e: serde_json::Error) -> Self {
        Self::Serialization(e)
    }
}
impl std::error::Error for RepositoryError {}

struct Repository {
    records: BTreeMap<NotificationIdentity, Notification>,
    path: Option<String>,
    bytes: usize,
}
impl Repository {
    fn load(path: Option<String>) -> Result<Self, RepositoryError> {
        let mut records = BTreeMap::new();
        let mut bytes = 0;
        if let Some(ref p) = path {
            let data = match fs::read_to_string(p) {
                Ok(x) => x,
                Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
                Err(e) => return Err(e.into()),
            };
            bytes = data.len();
            if bytes > MAX_REPOSITORY_BYTES {
                return Err(RepositoryError::Capacity);
            }
            for line in data.lines() {
                let n: Notification = serde_json::from_str(line)?;
                validate_notification(&n)?;
                if records.insert(notification_identity(&n), n).is_some() {
                    return Err(RepositoryError::DuplicateNotification);
                }
            }
        }
        if records.len() > MAX_REPOSITORY_RECORDS {
            return Err(RepositoryError::Capacity);
        }
        Ok(Self {
            records,
            path,
            bytes,
        })
    }
    fn accept(&mut self, n: Notification) -> Result<bool, RepositoryError> {
        validate_notification(&n)?;
        let key = notification_identity(&n);
        if self.records.contains_key(&key) {
            return Ok(false);
        }
        let line = format!("{}\n", serde_json::to_string(&n)?);
        if self.records.len() >= MAX_REPOSITORY_RECORDS
            || self.bytes + line.len() > MAX_REPOSITORY_BYTES
        {
            return Err(RepositoryError::Capacity);
        }
        if let Some(ref path) = self.path {
            let tmp = format!(
                "{}.tmp.{}.{}",
                path,
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            let result = (|| {
                let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
                for old in self.records.values() {
                    writeln!(f, "{}", serde_json::to_string(old)?)?;
                }
                f.write_all(line.as_bytes())?;
                f.flush()?;
                f.sync_all()?;
                fs::rename(&tmp, path)
            })();
            if let Err(e) = result {
                let cleanup = fs::remove_file(&tmp);
                if let Err(cleanup) = cleanup {
                    if cleanup.kind() != io::ErrorKind::NotFound {
                        eprintln!("temporary-file cleanup failed: {cleanup}");
                    }
                }
                return Err(e.into());
            }
        }
        self.records.insert(key, n);
        self.bytes += line.len();
        Ok(true)
    }
    fn notifications(&self) -> Vec<Value> {
        self.records
            .values()
            .filter_map(|n| serde_json::to_value(n).ok())
            .collect()
    }
}
struct Counters {
    solves: AtomicU64,
    empty: AtomicU64,
    generated: AtomicU64,
    predictions: AtomicU64,
    fallbacks: AtomicU64,
    accepted: AtomicU64,
    duplicate: AtomicU64,
}
struct App {
    repository: Arc<Mutex<Repository>>,
    counters: Arc<Counters>,
    model: Option<Arc<shadowsolve_solver_engine::model::ModelArtifact>>,
    policy: Option<Arc<shadowsolve_solver_engine::policy::ProtocolPolicy>>,
    fallback: String,
    last: Arc<Mutex<Option<Solution>>>,
    notify_permits: Arc<Semaphore>,
    dashboard_mode: String,
    projection: Arc<Mutex<DashboardProjection>>,
    persistence: Option<Persistence>,
    ingestion: IngestionHandle,
}
impl Clone for App {
    fn clone(&self) -> Self {
        Self {
            repository: self.repository.clone(),
            counters: self.counters.clone(),
            model: self.model.clone(),
            policy: self.policy.clone(),
            fallback: self.fallback.clone(),
            last: self.last.clone(),
            notify_permits: self.notify_permits.clone(),
            dashboard_mode: self.dashboard_mode.clone(),
            projection: self.projection.clone(),
            persistence: self.persistence.clone(),
            ingestion: self.ingestion.clone(),
        }
    }
}

fn model_evaluated(a: &App) -> bool {
    a.counters.predictions.load(Ordering::Relaxed) > 0
}

fn prometheus_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

async fn health() -> &'static str {
    "ok"
}
async fn ready(State(a): State<App>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let ingestion = a.ingestion.status();
    if ingestion["configured"] == true && ingestion["fresh"] != true {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(
                serde_json::json!({"ready":false,"error":"ingestion stale, unsafe, or noncanonical","ingestion":ingestion}),
            ),
        ));
    }
    let p = a.projection.lock().map_err(|_| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"ready":false,"error":"projection unavailable"})),
        )
    })?;
    Ok(Json(
        serde_json::json!({"ready":a.persistence.as_ref().is_none_or(Persistence::is_alive),"persistenceMode":if a.persistence.is_some(){"postgres"}else{"json_fallback"},"modelEvaluated":model_evaluated(&a),"modelAvailable":a.model.is_some(),"notificationRepositoryInitialized":true,"staticFallbackAllowed":true,"modelFallback":a.model.is_none(),"modelFallbackReason":a.fallback,"protocolPolicyConfigured":a.policy.is_some(),"protocolPolicyNetwork":a.policy.as_ref().map(|p| p.network.clone()),"projectionInitialized":true,"projectionRevision":p.revision,"projectionHasData":p.last_update.is_some(),"ingestion":ingestion,"latestAdvisory":p.latest_advisory}),
    ))
}
async fn metrics(State(a): State<App>) -> String {
    let p = a.projection.lock();
    let (revision, updates, last_update, stale, latest_advisory) = match p {
        Ok(p) => (
            p.revision,
            p.updates,
            p.last_update.unwrap_or(0),
            p.last_update.map_or(1, |t| {
                (current_epoch_ms().saturating_sub(t) > DASHBOARD_STALE_MS) as u64
            }),
            p.latest_advisory.clone(),
        ),
        Err(_) => (0, 0, 0, 1, None),
    };
    let ingestion = a.ingestion.status();
    let network = prometheus_label(ingestion["network"].as_str().unwrap_or(""));
    let hash = prometheus_label(ingestion["hash"].as_str().unwrap_or(""));
    let advisory_id = prometheus_label(
        latest_advisory
            .as_ref()
            .and_then(|value| value["advisory_id"].as_str())
            .unwrap_or(""),
    );
    let advisory_network = prometheus_label(
        latest_advisory
            .as_ref()
            .and_then(|value| value["source_network"].as_str())
            .unwrap_or(""),
    );
    let advisory_hash = prometheus_label(
        latest_advisory
            .as_ref()
            .and_then(|value| value["source_block_hash"].as_str())
            .unwrap_or(""),
    );
    let advisory_block = latest_advisory
        .as_ref()
        .and_then(|value| value["source_block_number"].as_u64())
        .unwrap_or(0);
    format!("# TYPE shadowsolve_solves_total counter\nshadowsolve_solves_total {}\nshadowsolve_validation_empty_total {}\nshadowsolve_generated_candidates_total {}\nshadowsolve_model_predictions_total {}\nshadowsolve_model_fallback_uses_total {}\nshadowsolve_notifications_accepted_total {}\nshadowsolve_notifications_duplicate_total {}\nshadowsolve_model_fallback {}\nshadowsolve_dashboard_revision {}\nshadowsolve_dashboard_update_count {}\nshadowsolve_dashboard_last_update_ms {}\nshadowsolve_dashboard_stale {}\nshadowsolve_ingestion_configured {}\nshadowsolve_ingestion_fresh {}\nshadowsolve_ingestion_unsafe {}\nshadowsolve_ingestion_canonical {}\nshadowsolve_ingestion_head {}\nshadowsolve_ingestion_finalized {}\nshadowsolve_ingestion_info{{network=\"{}\",hash=\"{}\"}} 1\nshadowsolve_latest_advisory_info{{advisory_id=\"{}\",network=\"{}\",block_hash=\"{}\"}} {}\n",a.counters.solves.load(Ordering::Relaxed),a.counters.empty.load(Ordering::Relaxed),a.counters.generated.load(Ordering::Relaxed),a.counters.predictions.load(Ordering::Relaxed),a.counters.fallbacks.load(Ordering::Relaxed),a.counters.accepted.load(Ordering::Relaxed),a.counters.duplicate.load(Ordering::Relaxed),if a.fallback.is_empty(){0}else{1},revision,updates,last_update,stale,ingestion["configured"].as_bool().unwrap_or(false) as u8,ingestion["fresh"].as_bool().unwrap_or(false) as u8,ingestion["unsafe"].as_bool().unwrap_or(false) as u8,ingestion["canonical"].as_bool().unwrap_or(false) as u8,ingestion["head"].as_u64().unwrap_or(0),ingestion["finalized"].as_u64().unwrap_or(0),network,hash,advisory_id,advisory_network,advisory_hash,advisory_block)
}

fn build_provenance(
    app: &App,
    request: &SolveRequest,
    solution: &Solution,
    source: Option<&IngestionSource>,
) -> Result<AdvisoryProvenance, &'static str> {
    let candidate = solution.candidates.first();
    let gross_output = if let Some(candidate) = candidate {
        candidate
            .routes
            .iter()
            .try_fold(0u128, |total, route| total.checked_add(route.buy_amount.0))
            .ok_or("advisory economics overflow")?
    } else {
        0
    };
    let gas_units = candidate.map_or(0, |candidate| candidate.gas_proxy);
    let net_surplus = candidate.map_or(Ok(0), |candidate| {
        i128::try_from(candidate.score).map_err(|_| "advisory economics overflow")
    })?;
    let (network, head, block_number, block_hash, verification, timestamp_ms) = source.map_or_else(
        || {
            (
                "fixture".into(),
                request.auction.block,
                request.auction.block,
                "fixture-unverified".into(),
                "fixture_unverified".into(),
                current_epoch_ms(),
            )
        },
        |source| {
            (
                source.network.clone(),
                source.head,
                source.block.number,
                source.block.hash.clone(),
                "canonical_finalized_rpc".into(),
                source.block.timestamp.saturating_mul(1000),
            )
        },
    );
    let advisory_id = serde_json::to_string(&(
        request.auction.id.as_str(),
        network.as_str(),
        block_number,
        block_hash.as_str(),
    ))
    .map_err(|_| "advisory identity serialization failed")?;
    Ok(AdvisoryProvenance {
        advisory_id,
        source_network: network,
        source_block_number: block_number,
        source_block_hash: block_hash,
        verification,
        gross_output: Amount(gross_output),
        gas_units,
        gas_price: "1 proxy-unit".into(),
        gas_conversion: "gas_proxy is denominated in buy-token raw units".into(),
        gas_cost: Amount(gas_units as u128),
        signed_net_surplus: net_surplus,
        route: candidate.map_or_else(Vec::new, |candidate| candidate.routes.clone()),
        assumptions: vec![
            "read-only advisory; no signing, submission, or execution".into(),
            "one chain, one order, allow-listed constant-product pools, at most two routes".into(),
            "gas_proxy is a fixed buy-token raw-unit cost, not a live gas quote".into(),
            format!("source head {head}; source state finalized at block {block_number}"),
        ],
        rejection_reason: solution.reasons.first().cloned(),
        model_policy_metadata: serde_json::json!({
            "modelAvailable": app.model.is_some(),
            "modelEvaluated": candidate.is_some_and(|candidate| candidate.predicted_validity.is_some()),
            "modelSchemaHash": shadowsolve_solver_engine::ONLINE_FEATURE_SCHEMA_HASH,
            "modelFallbackReason": app.fallback,
            "policyConfigured": app.policy.is_some(),
            "policyNetwork": app.policy.as_ref().map(|policy| policy.network.clone()),
        }),
        timestamp_ms,
    })
}

fn advisory_record(
    request: &SolveRequest,
    solution: &Solution,
    provenance: &AdvisoryProvenance,
) -> Result<AdvisoryRecord, &'static str> {
    let block_number =
        i64::try_from(provenance.source_block_number).map_err(|_| "advisory block overflow")?;
    let gas_units = i64::try_from(provenance.gas_units).map_err(|_| "advisory gas overflow")?;
    let payload = serde_json::to_value(provenance).map_err(|_| "advisory serialization failed")?;
    Ok(AdvisoryRecord {
        advisory_id: provenance.advisory_id.clone(),
        auction_id: request.auction.id.clone(),
        source_network: provenance.source_network.clone(),
        block_number,
        block_hash: provenance.source_block_hash.clone(),
        gas_units,
        gas_price: provenance.gas_price.clone(),
        gross_output: provenance.gross_output.to_string(),
        gas_cost: provenance.gas_cost.to_string(),
        net_surplus: provenance.signed_net_surplus.to_string(),
        rejection_reason: provenance.rejection_reason.clone(),
        route: serde_json::to_value(&provenance.route)
            .map_err(|_| "advisory route serialization failed")?,
        assumptions: serde_json::to_value(&provenance.assumptions)
            .map_err(|_| "advisory assumptions serialization failed")?,
        rejection_metadata: serde_json::json!({"reasons":solution.reasons}),
        model_policy_metadata: provenance.model_policy_metadata.clone(),
        payload,
    })
}

async fn run_ingestion_worker(
    config: IngestionConfig,
    database_url: String,
    handle: IngestionHandle,
) {
    let adapter = match HttpRpcTransport::new(config.rpc_url.clone(), config.network.clone()) {
        Ok(transport) => Arc::new(ReadOnlyAdapter {
            config: config.clone(),
            transport,
        }),
        Err(error) => {
            mark_ingestion_error(&handle, &error);
            return;
        }
    };
    let mut repository = match PostgresRepository::connect(&database_url).await {
        Ok(repository) => repository,
        Err(error) => {
            mark_ingestion_error(&handle, &error);
            return;
        }
    };
    let window = usize::try_from(config.finality_depth.saturating_mul(2).max(64)).unwrap_or(256);
    let mut chain = match CanonicalChain::with_network(window, config.network.clone()) {
        Ok(chain) => chain,
        Err(error) => {
            mark_ingestion_error(&handle, &error);
            return;
        }
    };
    let mut interval = tokio::time::interval(config.poll_interval);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        let polled = match adapter.ingest_once().await {
            Ok(polled) => polled,
            Err(error) => {
                mark_ingestion_error(&handle, &error);
                continue;
            }
        };
        let (head, finalized, snapshots) = polled;
        let now = current_epoch_seconds();
        if finalized.timestamp > now.saturating_add(config.max_age)
            || now.saturating_sub(finalized.timestamp) > config.max_age
        {
            mark_ingestion_error(&handle, &IngestionError::Stale("block timestamp".into()));
            continue;
        }
        let current = chain.head().map(|block| block.number);
        if current.is_some_and(|number| number > finalized.number)
            || current.is_some_and(|number| {
                finalized.number.saturating_sub(number) > MAX_INGESTION_CATCHUP_BLOCKS
            })
        {
            mark_ingestion_error(&handle, &IngestionError::Unsafe);
            continue;
        }
        let mut blocks = Vec::new();
        if let Some(current) = current {
            if current < finalized.number {
                let end = finalized.number;
                let mut failed = false;
                for number in (current + 1)..end {
                    match adapter.transport.block(number).await {
                        Ok(block) => blocks.push(block),
                        Err(error) => {
                            failed = true;
                            mark_ingestion_error(&handle, &error);
                            break;
                        }
                    }
                }
                if failed {
                    continue;
                }
            }
        }
        blocks.push(finalized.clone());
        let mut chain_error = None;
        for block in &blocks {
            if let Err(error) = chain.apply(block.clone()) {
                chain_error = Some(error);
                break;
            }
        }
        if let Some(error) = chain_error {
            mark_ingestion_error(&handle, &error);
            continue;
        }
        let cursor = IngestionCursor {
            network: config.network.clone(),
            source: "json-rpc-http".into(),
            next_block: match finalized.number.checked_add(1) {
                Some(number) => number,
                None => {
                    mark_ingestion_error(&handle, &IngestionError::Overflow("block".into()));
                    continue;
                }
            },
            block_hash: finalized.hash.clone(),
        };
        if let Err(error) = repository
            .persist(
                &chain,
                IngestionPersist {
                    network: &config.network,
                    blocks: &blocks,
                    snapshots: &snapshots,
                    cursor: &cursor,
                },
            )
            .await
        {
            mark_ingestion_error(&handle, &error);
            continue;
        }
        if let Ok(mut state) = handle.state.lock() {
            state.status = "ready".into();
            state.head = Some(head);
            state.finalized = Some(finalized);
            state.snapshots = snapshots;
            state.last_success = Some(now);
            state.unsafe_state = false;
            state.canonical = true;
        }
    }
}

fn mark_ingestion_error(handle: &IngestionHandle, error: &IngestionError) {
    let Ok(mut state) = handle.state.lock() else {
        return;
    };
    match error {
        IngestionError::Unsafe | IngestionError::Reorg => {
            state.status = "unsafe".into();
            state.unsafe_state = true;
            state.canonical = false;
        }
        IngestionError::IncompleteReplacement
        | IngestionError::Invalid(_)
        | IngestionError::Conflict(_) => {
            state.status = "noncanonical".into();
            state.unsafe_state = true;
            state.canonical = false;
        }
        IngestionError::Stale(_) => state.status = "stale".into(),
        _ => state.status = "unavailable".into(),
    }
}

async fn solve(
    State(a): State<App>,
    Json(r): Json<SolveRequest>,
) -> Result<Json<Solution>, (StatusCode, Json<Value>)> {
    if r.schema_version != SUPPORTED_SCHEMA_VERSION {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":"unsupported solve schema version"})),
        ));
    }
    let ingestion_configured = a.ingestion.status()["configured"] == true;
    let (r, source) = if ingestion_configured {
        let (request, source) = a.ingestion.prepare(&r).map_err(|error| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":error,"ingestion":a.ingestion.status()})),
            )
        })?;
        (request, Some(source))
    } else {
        (r, None)
    };
    a.counters.solves.fetch_add(1, Ordering::Relaxed);
    let mut s = shadowsolve_solver_engine::solve_at_with_policy(
        &r,
        current_epoch_ms(),
        a.policy.as_deref(),
    );
    if s.candidates.is_empty() {
        a.counters.empty.fetch_add(1, Ordering::Relaxed);
    }
    a.counters
        .generated
        .fetch_add(s.candidates.len() as u64, Ordering::Relaxed);
    let min = std::env::var("SHADOWSOLVE_MIN_VALIDITY")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
        .unwrap_or(0.0);
    if let Some(m) = a.model.as_ref() {
        s = shadowsolve_solver_engine::rank_solution_for_request(s, &r, Some(m), min);
        if !s.candidates.is_empty() && s.candidates.iter().all(|c| c.predicted_validity.is_some()) {
            a.counters
                .predictions
                .fetch_add(s.candidates.len() as u64, Ordering::Relaxed);
        } else {
            a.counters.fallbacks.fetch_add(1, Ordering::Relaxed);
        }
    } else {
        a.counters
            .fallbacks
            .fetch_add(s.candidates.len() as u64, Ordering::Relaxed);
        s = shadowsolve_solver_engine::rank_solution_for_request(s, &r, None, 0.0)
    }
    let provenance = build_provenance(&a, &r, &s, source.as_ref()).map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":error})),
        )
    })?;
    s.provenance = Some(provenance.clone());
    let advisory = source
        .as_ref()
        .map(|_| advisory_record(&r, &s, &provenance))
        .transpose()
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error":error})),
            )
        })?;
    if let Some(db) = &a.persistence {
        db.persist_solve(&r, &s, advisory.as_ref())
            .await
            .map_err(|_| {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error":"persistence operation failed"})),
                )
            })?;
    }
    if let Ok(mut last) = a.last.lock() {
        *last = Some(s.clone())
    }
    if a.dashboard_mode == "live" {
        if let Ok(mut p) = a.projection.lock() {
            p.solve(&r, &s, a.model.is_some());
        }
    }
    Ok(Json(s))
}
#[derive(serde::Serialize)]
struct NotifyResponse {
    notification: Notification,
    accepted: bool,
    duplicate: bool,
}
async fn notify(
    State(a): State<App>,
    Json(n): Json<Notification>,
) -> Result<(StatusCode, Json<NotifyResponse>), (StatusCode, Json<Value>)> {
    validate_notification(&n).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":"invalid notification"})),
        )
    })?;
    let permit = a.notify_permits.clone().try_acquire_owned().map_err(|_| {
        (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({"error":"notification capacity temporarily unavailable"})),
        )
    })?;
    let repo = a.repository.clone();
    let n2 = n.clone();
    if let Some(db) = &a.persistence {
        let accepted = db.notification(&n).await.map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error":"postgres notification operation failed"})),
            )
        })?;
        if accepted {
            a.counters.accepted.fetch_add(1, Ordering::Relaxed);
            if a.dashboard_mode == "live" {
                if let Ok(mut p) = a.projection.lock() {
                    p.notify(&n);
                }
            }
        } else {
            a.counters.duplicate.fetch_add(1, Ordering::Relaxed);
        }
        return Ok((
            if accepted {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            },
            Json(NotifyResponse {
                notification: n,
                accepted,
                duplicate: !accepted,
            }),
        ));
    }
    let result = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        repo.lock()
            .map_err(|_| RepositoryError::Io(io::Error::other("repository lock poisoned")))?
            .accept(n2)
    })
    .await
    .map_err(|_| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":"repository operation failed"})),
        )
    })?;
    let accepted = match result {
        Ok(v) => v,
        Err(RepositoryError::Capacity) => {
            return Err((
                StatusCode::INSUFFICIENT_STORAGE,
                Json(serde_json::json!({"error":"notification repository capacity exceeded"})),
            ))
        }
        Err(_e) => {
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error":"notification repository operation failed"})),
            ))
        }
    };
    if accepted {
        a.counters.accepted.fetch_add(1, Ordering::Relaxed);
        if a.dashboard_mode == "live" {
            if let Ok(mut p) = a.projection.lock() {
                p.notify(&n);
            }
        }
    } else {
        a.counters.duplicate.fetch_add(1, Ordering::Relaxed);
    };
    Ok((
        if accepted {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        },
        Json(NotifyResponse {
            notification: n,
            accepted,
            duplicate: !accepted,
        }),
    ))
}
async fn dashboard(State(a): State<App>) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    if a.dashboard_mode == "live" {
        let p = a.projection.lock().map_err(|_| {
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({"error":"dashboard unavailable"})),
            )
        })?;
        let now = current_epoch_ms();
        let stale = p
            .last_update
            .is_none_or(|t| now.saturating_sub(t) > DASHBOARD_STALE_MS);
        let ingestion = a.ingestion.status();
        return Ok(Json(
            serde_json::json!({"meta":{"source":"live","mode":"process_projection","revision":p.revision,"hasData":p.last_update.is_some(),"asOf":p.last_update,"stale":stale,"staleAfterMs":DASHBOARD_STALE_MS,"persistenceMode":if a.persistence.is_some(){"postgres"}else{"json_fallback"},"persistenceStatus":if a.persistence.as_ref().is_none_or(Persistence::is_alive){"ready"}else{"Unavailable"},"modelAvailable":a.model.is_some(),"modelEvaluated":model_evaluated(&a),"modelFallback":a.model.is_none(),"modelVersion":"unknown","modelFallbackReason":a.fallback,"modelSchemaHash":shadowsolve_solver_engine::ONLINE_FEATURE_SCHEMA_HASH,"chain":ingestion},"auctions":p.auctions,"candidates":p.candidates,"notifications":p.notifications,"route":p.route,"latestAdvisory":p.latest_advisory,"horizons":[],"poolDrift":"Unavailable","calibration":"Unavailable","experiment":"Unavailable","failures":p.failures,"policy":{"configured":a.policy.is_some(),"network":a.policy.as_ref().map(|x|x.network.clone())}}),
        ));
    }
    let mut v: Value = serde_json::from_str(include_str!("../../../fixtures/dashboard.json"))
        .unwrap_or_else(|_| serde_json::json!({}));
    if let Some(m) = v.get_mut("meta").and_then(Value::as_object_mut) {
        m.insert("modelEvaluated".into(), Value::Bool(model_evaluated(&a)));
        m.insert("modelAvailable".into(), Value::Bool(a.model.is_some()));
        m.insert("modelFallback".into(), Value::Bool(a.model.is_none()));
        m.insert(
            "modelFallbackReason".into(),
            Value::String(a.fallback.clone()),
        );
        m.insert(
            "modelSchemaHash".into(),
            Value::String(shadowsolve_solver_engine::ONLINE_FEATURE_SCHEMA_HASH.into()),
        );
    }
    Ok(Json(v))
}

fn router(app: App) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/metrics", get(metrics))
        .route("/api/dashboard", get(dashboard))
        .route("/solve", post(solve))
        .route("/notify", post(notify))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .with_state(app)
}

#[cfg(test)]
fn test_app(fallback: &str) -> App {
    test_app_with(fallback, NOTIFY_CONCURRENCY, None)
}

#[cfg(test)]
fn test_app_with(fallback: &str, permits: usize, path: Option<String>) -> App {
    App {
        repository: Arc::new(Mutex::new(Repository::load(path).unwrap())),
        counters: Arc::new(Counters {
            solves: AtomicU64::new(0),
            empty: AtomicU64::new(0),
            generated: AtomicU64::new(0),
            predictions: AtomicU64::new(0),
            fallbacks: AtomicU64::new(0),
            accepted: AtomicU64::new(0),
            duplicate: AtomicU64::new(0),
        }),
        model: None,
        policy: None,
        fallback: fallback.into(),
        last: Arc::new(Mutex::new(None)),
        notify_permits: Arc::new(Semaphore::new(permits)),
        dashboard_mode: "live".into(),
        projection: Arc::new(Mutex::new(DashboardProjection::new())),
        persistence: None,
        ingestion: IngestionHandle::disabled(),
    }
}
fn healthcheck_response(response: &[u8]) -> bool {
    let Some(separator) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let (head, body) = response.split_at(separator);
    let body = &body[4..];
    // Liveness only: process is up. Fail-closed readiness stays on /ready.
    let status_ok = head
        .split(|&byte| byte == b' ')
        .nth(1)
        .is_some_and(|status| status == b"200");
    status_ok && body.starts_with(b"ok")
}

fn healthcheck_address(port: Option<&str>) -> Option<SocketAddr> {
    let port = port.unwrap_or("8080").parse::<u16>().ok()?;
    (port != 0).then(|| SocketAddr::from(([127, 0, 0, 1], port)))
}

fn run_healthcheck() -> bool {
    let port = std::env::var("SHADOWSOLVE_HEALTH_PORT").ok();
    let Some(address) = healthcheck_address(port.as_deref()) else {
        return false;
    };
    let timeout = std::time::Duration::from_secs(1);
    let Ok(mut stream) = TcpStream::connect_timeout(&address, timeout) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    if stream.write_all(b"GET /health HTTP/1.0\r\n\r\n").is_err() {
        return false;
    }
    let mut response = Vec::new();
    stream.read_to_end(&mut response).is_ok() && healthcheck_response(&response)
}

#[tokio::main]
async fn run_server() -> Result<(), Box<dyn std::error::Error>> {
    let bind = std::env::var("SOLVER_BIND").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let dashboard_mode =
        std::env::var("SHADOWSOLVE_DASHBOARD_MODE").unwrap_or_else(|_| "live".into());
    if dashboard_mode != "live" && dashboard_mode != "fixture" {
        return Err("invalid SHADOWSOLVE_DASHBOARD_MODE".into());
    }
    let path = (dashboard_mode != "fixture")
        .then(|| {
            std::env::var("NOTIFICATION_REPOSITORY_PATH")
                .ok()
                .filter(|p| !p.is_empty())
        })
        .flatten();
    let (model, fallback) = match std::env::var("SHADOWSOLVE_MODEL_PATH")
        .ok()
        .filter(|p| !p.is_empty())
    {
        None => (None, "no_model_configured".into()),
        Some(p) => match fs::read(p) {
            Ok(b) => match shadowsolve_solver_engine::model::compatibility_for_schema(
                &b,
                shadowsolve_solver_engine::ONLINE_FEATURE_SCHEMA_HASH,
                3,
            ) {
                shadowsolve_solver_engine::model::Compatibility::Compatible(m) => {
                    (Some(Arc::new(*m)), String::new())
                }
                shadowsolve_solver_engine::model::Compatibility::StaticFallback(r) => {
                    (None, format!("incompatible_model_{r:?}").to_lowercase())
                }
            },
            Err(_) => (None, "configured_model_failure".into()),
        },
    };
    let policy = match std::env::var("SHADOWSOLVE_PROTOCOL_POLICY_PATH")
        .ok()
        .filter(|p| !p.is_empty())
    {
        None => None,
        Some(p) => Some(Arc::new(
            shadowsolve_solver_engine::policy::ProtocolPolicy::parse(
                &shadowsolve_solver_engine::policy::load(p)?,
            )
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
        )),
    };
    let database_url = std::env::var("DATABASE_URL").ok().filter(|v| !v.is_empty());
    let persistence = match database_url.as_deref() {
        Some(_url) if dashboard_mode == "fixture" => None,
        Some(url) => Some(Persistence::connect(url).await.map_err(io::Error::other)?),
        None => None,
    };
    let repository = Repository::load(path)?;
    let projection = Arc::new(Mutex::new(DashboardProjection::new()));
    if let Some(db) = &persistence {
        let rows = db.load_projection().await.map_err(io::Error::other)?;
        projection
            .lock()
            .map_err(|_| io::Error::other("projection lock poisoned"))?
            .restore(rows, model.is_some(), current_epoch_ms())
            .map_err(io::Error::other)?;
        let latest_advisory = db.latest_advisory().await.map_err(io::Error::other)?;
        projection
            .lock()
            .map_err(|_| io::Error::other("projection lock poisoned"))?
            .latest_advisory = latest_advisory;
    } else {
        let notifications = repository.notifications();
        projection
            .lock()
            .map_err(|_| io::Error::other("projection lock poisoned"))?
            .restore(
                ProjectionRows {
                    auctions: Vec::new(),
                    candidates: Vec::new(),
                    notifications,
                },
                model.is_some(),
                current_epoch_ms(),
            )
            .map_err(io::Error::other)?;
    }
    let ingestion_enabled = std::env::var("SHADOWSOLVE_INGESTION_ENABLED")
        .ok()
        .is_some_and(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
    let ingestion_config = (dashboard_mode != "fixture" && ingestion_enabled)
        .then(IngestionConfig::from_env)
        .transpose()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let ingestion = ingestion_config
        .as_ref()
        .map_or_else(IngestionHandle::disabled, IngestionHandle::configured);
    if let (Some(config), Some(policy)) = (ingestion_config.as_ref(), policy.as_ref()) {
        if policy.network != config.network
            || config.pools.iter().any(|pool| {
                !policy
                    .allowed_pools
                    .iter()
                    .any(|allowed| allowed.id.eq_ignore_ascii_case(pool))
            })
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "ingestion network/pools do not match protocol policy",
            )
            .into());
        }
    }
    if let Some(config) = ingestion_config {
        let url = database_url.clone().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "configured ingestion requires DATABASE_URL",
            )
        })?;
        tokio::spawn(run_ingestion_worker(config, url, ingestion.clone()));
    }
    let app = App {
        repository: Arc::new(Mutex::new(repository)),
        counters: Arc::new(Counters {
            solves: AtomicU64::new(0),
            empty: AtomicU64::new(0),
            generated: AtomicU64::new(0),
            predictions: AtomicU64::new(0),
            fallbacks: AtomicU64::new(0),
            accepted: AtomicU64::new(0),
            duplicate: AtomicU64::new(0),
        }),
        model,
        policy,
        fallback,
        last: Arc::new(Mutex::new(None)),
        notify_permits: Arc::new(Semaphore::new(NOTIFY_CONCURRENCY)),
        dashboard_mode,
        projection,
        persistence,
        ingestion,
    };
    axum::serve(tokio::net::TcpListener::bind(bind).await?, router(app)).await?;
    Ok(())
}

fn contract_fixture() -> Solution {
    Solution {
        auction_id: "auction-fixture".into(),
        candidates: vec![Candidate {
            id: "candidate-fixture".into(),
            auction_id: "auction-fixture".into(),
            routes: vec![Route {
                pool_id: "pool-fixture".into(),
                sell_amount: Amount(10),
                buy_amount: Amount(9),
            }],
            expected_surplus: Amount(8),
            gas_proxy: 100,
            score: 7,
            predicted_validity: Some(0.875),
            risk_value: Some(0.125),
            eligible: Some(true),
        }],
        reasons: vec![],
        provenance: None,
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--emit-contract-fixture") {
        println!("{}", serde_json::to_string(&contract_fixture()).unwrap());
        return;
    }
    if std::env::args().nth(1).as_deref() == Some("--healthcheck") {
        if !run_healthcheck() {
            eprintln!("healthcheck failed");
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = run_server() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_fixture_has_serialized_model_metadata_and_string_amounts() {
        let value = serde_json::to_value(contract_fixture()).unwrap();
        let candidate = &value["candidates"][0];
        assert_eq!(candidate["predicted_validity"], 0.875);
        assert_eq!(candidate["risk_value"], 0.125);
        assert_eq!(candidate["eligible"], true);
        assert_eq!(candidate["expected_surplus"], "8");
        assert_eq!(candidate["routes"][0]["sell_amount"], "10");
        assert_eq!(candidate["routes"][0]["buy_amount"], "9");
    }

    #[test]
    fn persisted_rows_reconstruct_projection_and_replay_notification() {
        let auction = serde_json::json!({"id":"a","block":7,"tokens":[],"orders":[],"state":{"observed_block":7,"deadline_ms":0}});
        let mut candidate = serde_json::to_value(&contract_fixture().candidates[0]).unwrap();
        candidate["auction_id"] = Value::String("a".into());
        let mut projection = DashboardProjection::new();
        projection.restore(ProjectionRows { auctions: vec![auction], candidates: vec![candidate], notifications: vec![serde_json::json!({"schema_version":"1","auction_id":"a","outcome":"failed","message":"x"})] }, false, 42).unwrap();
        assert_eq!(projection.auctions[0]["status"], "failed");
        assert_eq!(projection.candidates.len(), 1);
        assert_eq!(projection.failures[0]["cause"], "failed");
        assert_eq!(projection.revision, 2);
        assert_eq!(projection.last_update, Some(42));
    }

    #[test]
    fn projection_cap_keeps_the_most_recent_values() {
        let mut values = Vec::new();
        for value in 0..4 {
            DashboardProjection::push(&mut values, Value::from(value), 3);
        }
        assert_eq!(values, vec![Value::from(1), Value::from(2), Value::from(3)]);
    }

    #[test]
    fn json_fallback_notifications_replay_on_restart() {
        let path = std::env::temp_dir().join(format!("solver-restart-{}", current_epoch_ms()));
        let mut repository = Repository::load(Some(path.to_string_lossy().into_owned())).unwrap();
        let n = Notification {
            schema_version: "1".into(),
            auction_id: "restart".into(),
            outcome: "failed".into(),
            candidate_id: None,
            message: Some("persisted".into()),
        };
        assert!(repository.accept(n).unwrap());
        let mut projection = DashboardProjection::new();
        projection
            .restore(
                ProjectionRows {
                    auctions: Vec::new(),
                    candidates: Vec::new(),
                    notifications: repository.notifications(),
                },
                false,
                9,
            )
            .unwrap();
        assert_eq!(projection.failures[0]["id"], "restart");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn healthcheck_address_parses_strict_nonzero_u16_port() {
        assert_eq!(healthcheck_address(None).unwrap().port(), 8080);
        assert_eq!(healthcheck_address(Some("1234")).unwrap().port(), 1234);
        assert!(healthcheck_address(Some("0")).is_none());
        assert!(healthcheck_address(Some("8080 ")).is_none());
        assert!(healthcheck_address(Some("65536")).is_none());
        assert!(healthcheck_address(Some("not-a-port")).is_none());
    }

    #[test]
    fn healthcheck_response_requires_http_200_and_ok_body() {
        assert!(healthcheck_response(b"HTTP/1.0 200 OK\r\n\r\nok"));
        assert!(healthcheck_response(
            b"HTTP/1.0 200 OK\r\nContent-Type: text/plain\r\n\r\nok\n"
        ));
        assert!(!healthcheck_response(
            b"HTTP/1.0 503 Service Unavailable\r\n\r\nok"
        ));
        assert!(!healthcheck_response(
            b"HTTP/1.0 200 OK\r\n\r\n{\"ready\":true}"
        ));
        assert!(!healthcheck_response(b"HTTP/1.0 200 OK\r\n\r\n"));
    }

    #[test]
    fn configured_ingestion_grounds_reserves_and_fails_closed() {
        let now = current_epoch_seconds();
        let pool = "0x0000000000000000000000000000000000000001";
        let handle = IngestionHandle {
            state: Arc::new(Mutex::new(IngestionState {
                configured: true,
                network: Some("net".into()),
                max_age: 60,
                status: "ready".into(),
                head: Some(BlockHeader {
                    number: 20,
                    hash: "head".into(),
                    parent_hash: "parent".into(),
                    timestamp: now,
                }),
                finalized: Some(BlockHeader {
                    number: 18,
                    hash: "final".into(),
                    parent_hash: "parent".into(),
                    timestamp: now,
                }),
                snapshots: vec![PoolSnapshot {
                    network: "net".into(),
                    pool: pool.into(),
                    block_hash: "final".into(),
                    timestamp: now,
                    data: br#"{"reserve0":"1000","reserve1":"2000"}"#.to_vec(),
                }],
                last_success: Some(now),
                unsafe_state: false,
                canonical: true,
            })),
        };
        let request: SolveRequest = serde_json::from_value(serde_json::json!({
            "schema_version":"1",
            "auction":{"id":"a","block":18,"tokens":["A","B"],"orders":[{"id":"o","sell_token":"A","buy_token":"B","sell_amount":"10","min_buy_amount":"1","kind":"sell"}],"state":{"observed_block":18,"deadline_ms":u64::MAX}},
            "pools":[{"id":pool,"token0":"A","token1":"B","reserve0":"1","reserve1":"1","fee_bps":30}],
            "deadline_ms":u64::MAX
        }))
        .unwrap();
        let (grounded, source) = handle.prepare(&request).unwrap();
        assert_eq!(grounded.pools[0].reserve0, Amount(1000));
        assert_eq!(grounded.pools[0].reserve1, Amount(2000));
        assert_eq!(source.block.number, 18);
        let mut wrong_block = request;
        wrong_block.auction.block = 17;
        assert!(handle.prepare(&wrong_block).is_err());
        handle.state.lock().unwrap().unsafe_state = true;
        assert_eq!(handle.status()["fresh"], false);
        assert!(handle.prepare(&wrong_block).is_err());
    }
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;

    struct TestTask;
    impl TestTask {
        fn abort(self) {}
    }

    async fn start(app: App) -> (Router, TestTask) {
        (router(app), TestTask)
    }

    async fn request(app: &Router, method: &str, path: &str, body: &str) -> (u16, String) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status().as_u16();
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    fn notification(id: &str) -> String {
        format!(r#"{{"schema_version":"1","auction_id":"{id}","outcome":"ok"}}"#)
    }

    #[tokio::test]
    async fn endpoints_and_notify_behave_over_http() {
        let (address, task) = start(test_app("fallback")).await;
        for path in ["/health", "/ready", "/metrics", "/api/dashboard"] {
            let (status, body) = request(&address, "GET", path, "").await;
            assert_eq!(status, 200);
            assert!(!body.is_empty());
        }
        let (status, body) = request(&address, "POST", "/notify", &notification("a")).await;
        assert_eq!(status, 202);
        assert!(body.contains("\"accepted\":true"));
        let (status, body) = request(&address, "POST", "/notify", &notification("a")).await;
        assert_eq!(status, 200);
        assert!(body.contains("\"duplicate\":true"));
        let (status, _) = request(&address, "POST", "/notify", &notification("b")).await;
        assert_eq!(status, 202);
        let (_, metrics) = request(&address, "GET", "/metrics", "").await;
        assert!(metrics.contains("shadowsolve_notifications_accepted_total 2"));
        assert!(metrics.contains("shadowsolve_notifications_duplicate_total 1"));
        task.abort();
    }

    #[tokio::test]
    async fn malformed_and_oversized_requests_are_safe() {
        let (address, task) = start(test_app("")).await;
        let (status, body) = request(&address, "POST", "/notify", "{").await;
        assert_eq!(status, 400);
        assert!(!body.is_empty());
        let (status, body) = request(
            &address,
            "POST",
            "/notify",
            r#"{"schema_version":"1","auction_id":"unknown","outcome":"ok","extra":true}"#,
        )
        .await;
        assert!((400..=422).contains(&status));
        assert!(!body.is_empty());
        let huge = format!(
            "{{\"auction_id\":\"{}\",\"outcome\":\"ok\"}}",
            "x".repeat(256 * 1024)
        );
        let (status, _) = request(&address, "POST", "/notify", &huge).await;
        assert_eq!(status, 413);
        for body in ["{}", "not-json"] {
            let (status, body) = request(&address, "POST", "/solve", body).await;
            assert!((400..=422).contains(&status));
            assert!(!body.is_empty());
        }
        let (status, body) = request(&address, "POST", "/solve", &"x".repeat(256 * 1024 + 1)).await;
        assert_eq!(status, 413);
        assert!(!body.is_empty());
        task.abort();
    }

    #[test]
    fn repository_rejects_duplicate_persisted_identities() {
        let path =
            std::env::temp_dir().join(format!("shadowsolve-duplicate-{}.json", std::process::id()));
        let record = |outcome: &str, message: &str| {
            format!(
                r#"{{"schema_version":"1","auction_id":"duplicate","outcome":"{outcome}","message":"{message}"}}"#
            )
        };
        fs::write(
            &path,
            format!(
                "{}\n{}\n",
                record("ok", "first"),
                record("failed", "second")
            ),
        )
        .unwrap();
        assert!(matches!(
            Repository::load(Some(path.to_string_lossy().into_owned())),
            Err(RepositoryError::DuplicateNotification)
        ));
        fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn notify_saturation_returns_too_many_requests() {
        let app = test_app_with("", 0, None);
        let (address, task) = start(app).await;
        let (status, body) = request(&address, "POST", "/notify", &notification("full")).await;
        assert_eq!(status, 429);
        assert!(body.contains("notification capacity temporarily unavailable"));
        task.abort();
    }

    #[tokio::test]
    async fn repository_io_failure_returns_sanitized_error() {
        let app = test_app("");
        let path = format!("/this/path/does/not/exist/{}.json", std::process::id());
        app.repository.lock().unwrap().path = Some(path);
        let (address, task) = start(app).await;
        let (status, body) = request(&address, "POST", "/notify", &notification("io")).await;
        assert_eq!(status, 500);
        assert_eq!(
            body,
            r#"{"error":"notification repository operation failed"}"#
        );
        assert!(!body.contains("does not exist"));
        assert!(!body.contains("/this/path"));
        task.abort();
    }

    #[tokio::test]
    async fn duplicate_at_repository_capacity_is_still_allowed() {
        let app = test_app("");
        let duplicate = notification("same");
        {
            let mut repo = app.repository.lock().unwrap();
            repo.accept(serde_json::from_str(&duplicate).unwrap())
                .unwrap();
            for i in 1..MAX_REPOSITORY_RECORDS {
                repo.accept(Notification {
                    schema_version: "1".into(),
                    auction_id: format!("cap-{i}"),
                    outcome: "ok".into(),
                    candidate_id: None,
                    message: None,
                })
                .unwrap();
            }
        }
        let (address, task) = start(app).await;
        let (status, _) = request(&address, "POST", "/notify", &duplicate).await;
        assert_eq!(status, 200);
        let (status, body) = request(&address, "POST", "/notify", &notification("new")).await;
        assert_eq!(status, 507);
        assert!(body.contains("capacity"));
        task.abort();
    }
}
