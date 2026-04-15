//! In-memory broker state. Phase 1 only — phase 3 swaps in postgres.

use chrono::{DateTime, Utc};
use noether_grid_protocol::{
    JobId, JobResult, JobStatus, WorkerAdvertisement, WorkerId, WorkerSnapshot,
};
use std::collections::HashMap;
use tokio::sync::Mutex;

/// A registered worker — the advertisement + liveness + current load.
#[derive(Clone, Debug)]
pub struct WorkerEntry {
    pub advertisement: WorkerAdvertisement,
    pub last_seen: DateTime<Utc>,
    pub in_flight_jobs: u32,
    pub draining: bool,
}

impl WorkerEntry {
    pub fn is_healthy(&self, now: DateTime<Utc>) -> bool {
        if self.draining {
            return false;
        }
        let ttl_secs = 3 * self.advertisement.heartbeat_interval_secs.max(1);
        (now - self.last_seen).num_seconds() < ttl_secs as i64
    }

    pub fn to_snapshot(&self, now: DateTime<Utc>) -> WorkerSnapshot {
        WorkerSnapshot {
            worker_id: self.advertisement.worker_id.clone(),
            url: self.advertisement.url.clone(),
            capabilities: self.advertisement.capabilities.clone(),
            in_flight_jobs: self.in_flight_jobs,
            last_seen: self.last_seen,
            healthy: self.is_healthy(now),
        }
    }
}

/// Job state tracked by the broker. Phase 1: completed jobs live in
/// memory for an hour after terminal status, then are pruned.
#[derive(Clone, Debug)]
pub struct JobEntry {
    pub status: JobStatus,
    pub result: Option<JobResult>,
    /// Unused in phase 1 (queue semantics are synchronous). Kept so
    /// phase 3 can prune completed jobs after an hour without a
    /// schema change.
    #[allow(dead_code)]
    pub created_at: DateTime<Utc>,
    pub dispatched_to: Option<WorkerId>,
    /// API key that submitted this job — captured at submit time so
    /// the dispatcher knows whose quota to debit when the job
    /// completes. Empty when no quotas are configured.
    pub api_key: String,
}

/// Per-API-key spending limit + running total. Looked up on every
/// `POST /jobs` so a single agent can't drain the pool.
#[derive(Debug, Clone)]
pub struct AgentQuota {
    pub monthly_cents: u64,
    pub spent_cents: u64,
}

/// Prometheus metrics surface. Counters + gauges, no histograms in
/// phase 3 — we'll add request-duration histograms when we have a
/// concrete dashboard requirement.
pub struct Metrics {
    pub workers_total: prometheus::IntGauge,
    pub workers_healthy: prometheus::IntGauge,
    pub jobs_submitted_total: prometheus::IntCounter,
    pub jobs_succeeded_total: prometheus::IntCounter,
    pub jobs_failed_total: prometheus::IntCounter,
    pub jobs_abandoned_total: prometheus::IntCounter,
    pub cents_spent_total: prometheus::IntCounter,
    pub registry: prometheus::Registry,
}

impl Metrics {
    pub fn new() -> Self {
        let registry = prometheus::Registry::new();
        let workers_total =
            prometheus::IntGauge::new("noether_grid_workers_total", "registered workers").unwrap();
        let workers_healthy = prometheus::IntGauge::new(
            "noether_grid_workers_healthy",
            "workers within heartbeat TTL and not draining",
        )
        .unwrap();
        let jobs_submitted_total = prometheus::IntCounter::new(
            "noether_grid_jobs_submitted_total",
            "POST /jobs that reached dispatch",
        )
        .unwrap();
        let jobs_succeeded_total = prometheus::IntCounter::new(
            "noether_grid_jobs_succeeded_total",
            "jobs whose final status was Ok",
        )
        .unwrap();
        let jobs_failed_total = prometheus::IntCounter::new(
            "noether_grid_jobs_failed_total",
            "jobs whose final status was Failed",
        )
        .unwrap();
        let jobs_abandoned_total = prometheus::IntCounter::new(
            "noether_grid_jobs_abandoned_total",
            "jobs whose final status was Abandoned (worker death, panic)",
        )
        .unwrap();
        let cents_spent_total = prometheus::IntCounter::new(
            "noether_grid_cents_spent_total",
            "cumulative spent_cents observed across all workers",
        )
        .unwrap();
        for c in [workers_total.clone(), workers_healthy.clone()] {
            registry.register(Box::new(c)).unwrap();
        }
        for c in [
            jobs_submitted_total.clone(),
            jobs_succeeded_total.clone(),
            jobs_failed_total.clone(),
            jobs_abandoned_total.clone(),
            cents_spent_total.clone(),
        ] {
            registry.register(Box::new(c)).unwrap();
        }
        Self {
            workers_total,
            workers_healthy,
            jobs_submitted_total,
            jobs_succeeded_total,
            jobs_failed_total,
            jobs_abandoned_total,
            cents_spent_total,
            registry,
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared application state.
pub struct AppState {
    pub workers: Mutex<HashMap<WorkerId, WorkerEntry>>,
    pub jobs: Mutex<HashMap<JobId, JobEntry>>,
    /// Shared secret workers present when enrolling. Empty = no auth.
    pub secret: String,
    /// Stage catalogue used by the graph splitter to look up stage
    /// metadata (effects, signatures) for each `Stage { id }` it
    /// encounters in submitted graphs. Snapshot of whatever is in the
    /// store the broker was launched against — the broker doesn't keep
    /// it live-synced. Re-run the broker after `stage add` if
    /// freshness matters.
    pub stages: Mutex<noether_store::MemoryStore>,
    /// API-key → quota mapping. Loaded once at startup from the
    /// `--quotas-file` JSON. Empty map means "no per-agent limits".
    pub quotas: Mutex<HashMap<String, AgentQuota>>,
    pub metrics: Metrics,
    /// Durability backend. `Persistence::in_memory()` for the
    /// development / single-LAN demo path; `Persistence::postgres(url)`
    /// for production where broker restart should not lose the cost
    /// ledger or the agent-quota spend totals. Reads always come from
    /// the in-memory caches; postgres is write-through.
    pub persistence: crate::persistence::Persistence,
}

impl AppState {
    /// Convenience constructor for tests / single-tenant in-memory
    /// deployments. Production calls `with_persistence` directly.
    #[allow(dead_code)]
    pub fn new(secret: String) -> Self {
        Self::with_persistence(secret, crate::persistence::Persistence::in_memory())
    }

    pub fn with_persistence(secret: String, persistence: crate::persistence::Persistence) -> Self {
        Self {
            workers: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            secret,
            stages: Mutex::new(noether_store::MemoryStore::new()),
            quotas: Mutex::new(HashMap::new()),
            metrics: Metrics::new(),
            persistence,
        }
    }

    /// Hydrate in-memory caches from the persistence layer. Called
    /// once on broker boot. With the default in-memory persistence
    /// this is a no-op.
    pub async fn hydrate_from_persistence(&self) {
        let snap = self.persistence.hydrate().await;
        if !snap.workers.is_empty() {
            let mut workers = self.workers.lock().await;
            for entry in snap.workers {
                workers.insert(entry.advertisement.worker_id.clone(), entry);
            }
        }
        if !snap.quota_spend.is_empty() {
            let mut quotas = self.quotas.lock().await;
            for (key, spent) in snap.quota_spend {
                if let Some(q) = quotas.get_mut(&key) {
                    q.spent_cents = spent;
                }
            }
        }
    }

    /// Replace the quota map with the contents of `entries`. Called at
    /// boot from the optional --quotas-file JSON.
    pub async fn seed_quotas(&self, entries: HashMap<String, u64>) {
        let mut quotas = self.quotas.lock().await;
        *quotas = HashMap::new();
        for (key, monthly_cents) in entries {
            quotas.insert(
                key,
                AgentQuota {
                    monthly_cents,
                    spent_cents: 0,
                },
            );
        }
    }

    /// Replace the stage catalogue with `stages`. Used at boot from a
    /// JsonFileStore or remote registry seed.
    pub async fn seed_stages(&self, stages: Vec<noether_core::stage::Stage>) {
        use noether_store::StageStore;
        let mut store = self.stages.lock().await;
        *store = noether_store::MemoryStore::new();
        for s in stages {
            let _ = store.upsert(s);
        }
    }

    /// Remove workers whose heartbeat TTL has expired.
    pub async fn prune_stale_workers(&self) {
        let now = Utc::now();
        let mut workers = self.workers.lock().await;
        let before = workers.len();
        workers.retain(|_, entry| entry.is_healthy(now));
        let after = workers.len();
        if after < before {
            tracing::info!(
                "pruned {} stale worker(s); {} healthy remaining",
                before - after,
                after
            );
        }
    }

    pub async fn snapshot_workers(&self) -> Vec<WorkerSnapshot> {
        let now = Utc::now();
        self.workers
            .lock()
            .await
            .values()
            .map(|e| e.to_snapshot(now))
            .collect()
    }
}
