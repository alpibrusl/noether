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
}

impl AppState {
    pub fn new(secret: String) -> Self {
        Self {
            workers: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            secret,
            stages: Mutex::new(noether_store::MemoryStore::new()),
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
