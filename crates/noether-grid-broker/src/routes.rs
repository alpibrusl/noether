//! Axum HTTP handlers.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::Utc;
use noether_grid_protocol::{
    ExecuteRequest, Heartbeat, JobId, JobResult, JobSpec, JobStatus, WorkerAdvertisement, WorkerId,
};
use serde_json::json;
use std::sync::Arc;

use crate::{registry, router, state::AppState};

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
    // Infer LLM requirements from the graph so the router picks
    // appropriately. Parse through noether-engine; on any parse error
    // fail fast with 400 (the graph wouldn't run anyway).
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
    let models = infer_llm_models(&graph);

    // Dummy store for effect inference — we only need the graph's
    // declared effects, not actual stage lookups. If a stage isn't in
    // the store the inferred effect is Unknown, which our router
    // treats as "any worker".
    let empty_store = noether_store::MemoryStore::new();
    let _ = noether_engine::checker::infer_effects(&graph.root, &empty_store);

    let chosen = match router::select_worker(&state, &models).await {
        Ok(id) => id,
        Err(refusal) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": refusal.to_string()})),
            )
                .into_response();
        }
    };

    let job_id = JobId(uuid::Uuid::new_v4().to_string());
    let job_entry = crate::state::JobEntry {
        status: JobStatus::Queued,
        result: None,
        created_at: Utc::now(),
        dispatched_to: Some(chosen.clone()),
    };
    state.jobs.lock().await.insert(job_id.clone(), job_entry);

    // Fire-and-forget dispatch. The caller polls `GET /jobs/{id}` for
    // the result — full synchronous dispatch would block the HTTP
    // handler through the worker's entire run.
    let state_clone = state.clone();
    let job_id_clone = job_id.clone();
    tokio::spawn(async move {
        dispatch(state_clone, job_id_clone, chosen, spec).await;
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

/// Walk the graph and collect every `Effect::Llm{model}` string.
fn infer_llm_models(graph: &noether_engine::lagrange::CompositionGraph) -> Vec<String> {
    // Phase 1: the stages aren't in a store we control, so we can't
    // actually look up their declared effects. Look at the graph
    // structure itself — any `Stage` node that has `Effect::Llm` would
    // need to be in the store; until then the router accepts "any"
    // match. Keep the infrastructure here so phase 2 can plug in a
    // shared store reference.
    let _ = graph;
    Vec::new()
}

async fn dispatch(state: Arc<AppState>, job_id: JobId, worker_id: WorkerId, spec: JobSpec) {
    // Look up the worker's URL.
    let url = match state.workers.lock().await.get(&worker_id) {
        Some(w) => w.advertisement.url.clone(),
        None => {
            set_job_status(
                &state,
                &job_id,
                JobStatus::Abandoned,
                None,
                Some("worker disappeared before dispatch".into()),
            )
            .await;
            return;
        }
    };

    set_job_status(&state, &job_id, JobStatus::Running, None, None).await;

    // Increment in-flight counter on the worker.
    {
        let mut workers = state.workers.lock().await;
        if let Some(w) = workers.get_mut(&worker_id) {
            w.in_flight_jobs += 1;
        }
    }

    let request = ExecuteRequest {
        job_id: job_id.clone(),
        graph: spec.graph,
        input: spec.input,
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .expect("reqwest client");
    let response = client
        .post(format!("{url}/execute"))
        .json(&request)
        .send()
        .await;

    // Always decrement in-flight regardless of outcome.
    {
        let mut workers = state.workers.lock().await;
        if let Some(w) = workers.get_mut(&worker_id) {
            w.in_flight_jobs = w.in_flight_jobs.saturating_sub(1);
        }
    }

    let result = match response {
        Ok(r) if r.status().is_success() => match r.json::<JobResult>().await {
            Ok(res) => res,
            Err(e) => {
                set_job_status(
                    &state,
                    &job_id,
                    JobStatus::Failed,
                    None,
                    Some(format!("malformed worker response: {e}")),
                )
                .await;
                return;
            }
        },
        Ok(r) => {
            let code = r.status();
            let body = r.text().await.unwrap_or_default();
            set_job_status(
                &state,
                &job_id,
                JobStatus::Failed,
                None,
                Some(format!("worker returned {code}: {body}")),
            )
            .await;
            return;
        }
        Err(e) => {
            set_job_status(
                &state,
                &job_id,
                JobStatus::Abandoned,
                None,
                Some(format!("worker unreachable: {e}")),
            )
            .await;
            return;
        }
    };

    // Bookkeeping: subtract spent_cents across any capability whose
    // model the graph might have used. Phase 1 is crude — it simply
    // decrements *every* LLM capability on the worker by spent_cents /
    // N, so the ledger tracks rough burn. Phase 2 makes this precise.
    if result.spent_cents > 0 {
        let mut workers = state.workers.lock().await;
        if let Some(w) = workers.get_mut(&worker_id) {
            let n = w.advertisement.capabilities.len().max(1) as u64;
            let per = result.spent_cents / n;
            for cap in w.advertisement.capabilities.iter_mut() {
                cap.budget_remaining_cents = cap.budget_remaining_cents.saturating_sub(per);
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
