//! Worker-registry operations. Enrolment, heartbeat, drain.

use chrono::Utc;
use noether_grid_protocol::{Heartbeat, WorkerAdvertisement, WorkerId};

use crate::state::{AppState, WorkerEntry};

pub async fn enrol(state: &AppState, advertisement: WorkerAdvertisement) -> WorkerId {
    let id = advertisement.worker_id.clone();
    let entry = WorkerEntry {
        advertisement,
        last_seen: Utc::now(),
        in_flight_jobs: 0,
        draining: false,
    };
    state.workers.lock().await.insert(id.clone(), entry);
    tracing::info!("enrolled worker {id}");
    id
}

pub async fn heartbeat(state: &AppState, hb: Heartbeat) -> bool {
    let mut workers = state.workers.lock().await;
    match workers.get_mut(&hb.worker_id) {
        Some(entry) => {
            entry.last_seen = Utc::now();
            entry.in_flight_jobs = hb.in_flight_jobs;
            if !hb.capabilities.is_empty() {
                entry.advertisement.capabilities = hb.capabilities;
            }
            true
        }
        None => false,
    }
}

/// Mark a worker as draining — broker stops routing jobs to it but
/// doesn't drop it from the registry until it heartbeats again (or
/// times out). Graceful-shutdown path for worker restarts.
pub async fn drain(state: &AppState, id: &WorkerId) -> bool {
    let mut workers = state.workers.lock().await;
    match workers.get_mut(id) {
        Some(entry) => {
            entry.draining = true;
            tracing::info!("draining worker {id}");
            true
        }
        None => false,
    }
}
