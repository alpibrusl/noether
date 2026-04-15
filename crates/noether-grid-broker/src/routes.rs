//! Axum HTTP handlers.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use noether_grid_protocol::{
    Heartbeat, JobId, JobResult, JobSpec, JobStatus, WorkerAdvertisement, WorkerId,
};
use serde_json::json;
use std::sync::Arc;

use crate::{registry, router, splitter, state::AppState};

pub async fn health(State(state): State<Arc<AppState>>) -> Response {
    let workers = state.snapshot_workers().await;
    let healthy = workers.iter().filter(|w| w.healthy).count();
    Json(json!({
        "ok": true,
        "workers_registered": workers.len(),
        "workers_healthy": healthy,
    }))
    .into_response()
}

pub async fn list_workers(State(state): State<Arc<AppState>>) -> Response {
    Json(state.snapshot_workers().await).into_response()
}

/// `POST /workers` — enrol. Requires `X-Grid-Secret` when the broker
/// has a non-empty secret configured.
pub async fn enrol_worker(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(advertisement): Json<WorkerAdvertisement>,
) -> Response {
    if let Err(r) = check_grid_secret(&state, &headers) {
        return r;
    }
    let id = registry::enrol(&state, advertisement).await;
    (StatusCode::CREATED, Json(json!({ "worker_id": id.0 }))).into_response()
}

pub async fn heartbeat(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(mut hb): Json<Heartbeat>,
) -> Response {
    if let Err(r) = check_grid_secret(&state, &headers) {
        return r;
    }
    // Path-vs-body consistency: the body carries worker_id but we trust
    // the path. Overwrite so a worker can't heartbeat for someone else.
    hb.worker_id = WorkerId(id);
    if registry::heartbeat(&state, hb).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown worker"})),
        )
            .into_response()
    }
}

pub async fn drain_worker(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    if let Err(r) = check_grid_secret(&state, &headers) {
        return r;
    }
    if registry::drain(&state, &WorkerId(id)).await {
        StatusCode::NO_CONTENT.into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "unknown worker"})),
        )
            .into_response()
    }
}

pub async fn submit_job(State(state): State<Arc<AppState>>, Json(spec): Json<JobSpec>) -> Response {
    // Parse the graph. Invalid JSON → 400 immediately; the graph wouldn't
    // run anyway and we want a clear error before any worker bookkeeping.
    let graph = match noether_engine::lagrange::parse_graph(&spec.graph.to_string()) {
        Ok(g) => g,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("invalid graph JSON: {e}")})),
            )
                .into_response();
        }
    };

    // Look up the graph's required LLM models from the broker's stage
    // catalogue. If the catalogue doesn't know a stage, the splitter
    // leaves it alone — no model requirement is contributed.
    let stages_snapshot = {
        let lock = state.stages.lock().await;
        clone_store(&lock)
    };
    let models = splitter::required_llm_models(&graph.root, &stages_snapshot);

    // Pre-flight pool capacity. With graph splitting we don't pick a
    // single worker — the splitter picks per-node — but we still need
    // at least one healthy worker that covers each model.
    if let Err(refusal) = router::select_worker(&state, &models).await {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({"error": refusal.to_string()})),
        )
            .into_response();
    }

    let job_id = JobId(uuid::Uuid::new_v4().to_string());
    let job_entry = crate::state::JobEntry {
        status: JobStatus::Queued,
        result: None,
        created_at: Utc::now(),
        dispatched_to: None, // assigned by the splitter
    };
    state.jobs.lock().await.insert(job_id.clone(), job_entry);

    // Fire-and-forget dispatch.
    let state_clone = state.clone();
    let job_id_clone = job_id.clone();
    tokio::spawn(async move {
        dispatch(state_clone, job_id_clone, graph, spec).await;
    });

    (
        StatusCode::ACCEPTED,
        Json(json!({
            "job_id": job_id.0,
            "status": "queued",
        })),
    )
        .into_response()
}

fn clone_store(src: &noether_store::MemoryStore) -> noether_store::MemoryStore {
    use noether_store::StageStore;
    let mut out = noether_store::MemoryStore::new();
    for stage in src.list(None) {
        let _ = out.upsert(stage.clone());
    }
    out
}

pub async fn get_job(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let jobs = state.jobs.lock().await;
    match jobs.get(&JobId(id.clone())) {
        Some(entry) => {
            let body = json!({
                "job_id": id,
                "status": entry.status,
                "dispatched_to": entry.dispatched_to,
                "result": entry.result,
            });
            Json(body).into_response()
        }
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "job not found"})),
        )
            .into_response(),
    }
}

// ── Internal helpers ────────────────────────────────────────────────────────

#[allow(clippy::result_large_err)] // Response body size is fine; only called in HTTP handlers.
fn check_grid_secret(state: &AppState, headers: &HeaderMap) -> Result<(), Response> {
    if state.secret.is_empty() {
        return Ok(());
    }
    let provided = headers
        .get("x-grid-secret")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if provided == state.secret {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid or missing X-Grid-Secret"})),
        )
            .into_response())
    }
}

async fn dispatch(
    state: Arc<AppState>,
    job_id: JobId,
    graph: noether_engine::lagrange::CompositionGraph,
    spec: JobSpec,
) {
    use noether_engine::executor::composite::CompositeExecutor;
    use noether_engine::executor::runner::run_composition;
    use noether_engine::lagrange::compute_composition_id;

    // Snapshot workers + stages for the splitter. Holding the locks
    // across the whole dispatch would block heartbeats.
    let workers_snapshot: Vec<crate::state::WorkerEntry> = {
        let workers = state.workers.lock().await;
        workers.values().cloned().collect()
    };
    let stages_snapshot = {
        let lock = state.stages.lock().await;
        clone_store(&lock)
    };

    // Rewrite Llm-effect Stage nodes into RemoteStage nodes pointing
    // at the chosen worker's /stage/{id} endpoint.
    let pick = splitter::pick_worker_for(&workers_snapshot);
    let split = match splitter::split_graph(&graph.root, &stages_snapshot, pick) {
        Ok(s) => s,
        Err(refusal) => {
            set_job_status(
                &state,
                &job_id,
                JobStatus::Failed,
                None,
                Some(format!("graph splitting failed: {refusal}")),
            )
            .await;
            return;
        }
    };

    // Bookkeeping: bump in-flight counters on every assigned worker.
    let assigned = split.assigned_workers.clone();
    {
        let mut workers = state.workers.lock().await;
        for id in &assigned {
            if let Some(w) = workers.get_mut(id) {
                w.in_flight_jobs = w.in_flight_jobs.saturating_add(1);
            }
        }
    }

    set_job_status(&state, &job_id, JobStatus::Running, None, None).await;
    {
        let mut jobs = state.jobs.lock().await;
        if let Some(entry) = jobs.get_mut(&job_id) {
            entry.dispatched_to = assigned.first().cloned();
        }
    }

    // Run the rewritten graph on a blocking thread — the noether
    // engine is synchronous + the RemoteStage executor uses
    // reqwest::blocking under the hood.
    let rewritten =
        noether_engine::lagrange::CompositionGraph::new(graph.description.clone(), split.rewritten);
    let composition_id = compute_composition_id(&rewritten).unwrap_or_else(|_| "unknown".into());
    let input = spec.input.clone();
    let exec_store = stages_snapshot;
    let comp_id = composition_id.clone();
    let run_outcome = tokio::task::spawn_blocking(move || {
        let executor = CompositeExecutor::from_store(&exec_store);
        run_composition(&rewritten.root, &input, &executor, &comp_id)
    })
    .await;

    // Decrement in-flight counters.
    {
        let mut workers = state.workers.lock().await;
        for id in &assigned {
            if let Some(w) = workers.get_mut(id) {
                w.in_flight_jobs = w.in_flight_jobs.saturating_sub(1);
            }
        }
    }

    let result = match run_outcome {
        Ok(Ok(comp)) => JobResult {
            job_id: job_id.clone(),
            status: JobStatus::Ok,
            output: comp.output,
            spent_cents: comp.spent_cents,
            composition_id: Some(composition_id.clone()),
            error: None,
            completed_at: Utc::now(),
        },
        Ok(Err(e)) => JobResult {
            job_id: job_id.clone(),
            status: JobStatus::Failed,
            output: serde_json::Value::Null,
            spent_cents: 0,
            composition_id: Some(composition_id.clone()),
            error: Some(format!("{e}")),
            completed_at: Utc::now(),
        },
        Err(e) => JobResult {
            job_id: job_id.clone(),
            status: JobStatus::Abandoned,
            output: serde_json::Value::Null,
            spent_cents: 0,
            composition_id: Some(composition_id.clone()),
            error: Some(format!("dispatch task panicked: {e}")),
            completed_at: Utc::now(),
        },
    };

    // Cost ledger: subtract spent_cents from each assigned worker's
    // budget. Distribute proportionally across the workers actually
    // used; phase 3 makes this fully per-stage.
    if result.spent_cents > 0 && !assigned.is_empty() {
        let per = result.spent_cents / assigned.len() as u64;
        let mut workers = state.workers.lock().await;
        for id in &assigned {
            if let Some(w) = workers.get_mut(id) {
                for cap in w.advertisement.capabilities.iter_mut() {
                    cap.budget_remaining_cents = cap.budget_remaining_cents.saturating_sub(per);
                }
            }
        }
    }

    set_job_status(&state, &job_id, result.status.clone(), Some(result), None).await;
}

async fn set_job_status(
    state: &AppState,
    job_id: &JobId,
    status: JobStatus,
    result: Option<JobResult>,
    error: Option<String>,
) {
    let mut jobs = state.jobs.lock().await;
    if let Some(entry) = jobs.get_mut(job_id) {
        entry.status = status;
        if let Some(r) = result {
            entry.result = Some(r);
        } else if let Some(msg) = error {
            entry.result = Some(JobResult {
                job_id: job_id.clone(),
                status: entry.status.clone(),
                output: serde_json::Value::Null,
                spent_cents: 0,
                composition_id: None,
                error: Some(msg),
                completed_at: Utc::now(),
            });
        }
    }
}
