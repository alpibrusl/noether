//! noether-grid-worker — advertises local LLM capacity, runs graphs on
//! request from a grid broker.
//!
//! Research binary. Phase 1 scope: in-memory state, capability
//! discovery from env, best-effort budget tracking.

use axum::{extract::State, http::StatusCode, routing, Json, Router};
use chrono::Utc;
use clap::Parser;
use noether_engine::lagrange::CompositionGraph;
use noether_grid_protocol::{
    AuthVia, ExecuteRequest, Heartbeat, JobId, JobResult, JobStatus, LlmCapability,
    WorkerAdvertisement, WorkerId,
};
use noether_store::{JsonFileStore, StageStore};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};

#[derive(Parser, Debug)]
#[command(name = "noether-grid-worker", about = "Noether grid worker")]
struct Cli {
    /// Broker URL (e.g. http://broker.corp:8088).
    #[arg(long, env = "NOETHER_GRID_BROKER")]
    broker: String,
    /// Bind address for this worker's HTTP server.
    #[arg(long, env = "NOETHER_GRID_WORKER_BIND", default_value = "0.0.0.0:8089")]
    bind: String,
    /// URL the broker should POST `/execute` to. Must be reachable from
    /// the broker. Defaults to `http://<hostname>:<port>`.
    #[arg(long, env = "NOETHER_GRID_WORKER_URL")]
    url: Option<String>,
    /// Shared grid secret. Empty = no auth (dev only).
    #[arg(long, env = "NOETHER_GRID_SECRET", default_value = "")]
    secret: String,
    /// Heartbeat interval in seconds.
    #[arg(long, default_value = "10")]
    heartbeat_secs: u64,
    /// Path to the local noether store (for stage resolution).
    /// Defaults to `$HOME/.noether/store.json` — must match what the
    /// broker loads, or dispatched stages will come back "not found".
    #[arg(long, env = "NOETHER_STORE_PATH")]
    store_path: Option<String>,
}

struct WorkerState {
    store: JsonFileStore,
    in_flight: AtomicU32,
}

impl WorkerState {
    /// Snapshot the store into a fresh `MemoryStore`. We take a snapshot
    /// per single-stage call rather than borrowing across `.await` —
    /// `JsonFileStore` is sync and not `Sync`-safe to share across
    /// blocking thread boundaries.
    fn store_snapshot(&self) -> noether_store::MemoryStore {
        let mut snap = noether_store::MemoryStore::new();
        for stage in self.store.list(None) {
            let _ = snap.upsert(stage.clone());
        }
        snap
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "noether_grid_worker=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let worker_id = mint_worker_id();
    let url = cli.url.clone().unwrap_or_else(|| default_url(&cli.bind));

    tracing::info!(
        "worker {worker_id} starting; advertising {url} to {}",
        cli.broker
    );

    let resolved_store_path = cli.store_path.clone().unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.noether/store.json")
    });
    let store = JsonFileStore::open(&resolved_store_path)?;
    let stages_indexed = store.list(None).len();
    tracing::info!("store loaded from {resolved_store_path}: {stages_indexed} stage(s) indexed");
    if stages_indexed < 20 {
        tracing::warn!(
            "worker store at {resolved_store_path} looks small ({stages_indexed} stages) — \
             dispatched user stages may come back 'not found'. Match the broker's --store-path."
        );
    }
    let state = Arc::new(WorkerState {
        store,
        in_flight: AtomicU32::new(0),
    });

    // Enrol with the broker.
    let capabilities = discover_capabilities();
    let advertisement = WorkerAdvertisement {
        worker_id: worker_id.clone(),
        url: url.clone(),
        capabilities: capabilities.clone(),
        noether_version: env!("CARGO_PKG_VERSION").to_string(),
        heartbeat_interval_secs: cli.heartbeat_secs,
    };
    enrol_with_broker(&cli.broker, &cli.secret, &advertisement).await?;

    // Spawn the heartbeat loop.
    {
        let broker = cli.broker.clone();
        let secret = cli.secret.clone();
        let worker_id = worker_id.clone();
        let state = state.clone();
        let interval = cli.heartbeat_secs;
        let capabilities = capabilities.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval));
            loop {
                ticker.tick().await;
                let in_flight = state.in_flight.load(Ordering::Relaxed);
                let hb = Heartbeat {
                    worker_id: worker_id.clone(),
                    capabilities: capabilities.clone(),
                    in_flight_jobs: in_flight,
                };
                if let Err(e) = send_heartbeat(&broker, &secret, &worker_id, &hb).await {
                    tracing::warn!("heartbeat failed: {e}");
                }
            }
        });
    }

    let app = Router::new()
        .route("/health", routing::get(health))
        .route("/execute", routing::post(execute))
        // Single-stage RemoteStage-compatible endpoint. Used by the
        // broker's graph splitter — broker rewrites Stage{id} nodes
        // into RemoteStage pointing at this URL, so the existing
        // noether-engine RemoteStage executor handles the HTTP call
        // without any new code on its side.
        .route("/stage/{id}", routing::post(execute_single_stage))
        .with_state(state);

    let addr: SocketAddr = cli.bind.parse()?;
    tracing::info!("listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

fn mint_worker_id() -> WorkerId {
    let host = hostname::get()
        .ok()
        .and_then(|s| s.into_string().ok())
        .unwrap_or_else(|| "worker".into());
    let pid = std::process::id();
    WorkerId(format!("{host}-{pid}"))
}

fn default_url(bind: &str) -> String {
    // Best-effort. In production set NOETHER_GRID_WORKER_URL explicitly.
    let host = hostname::get()
        .ok()
        .and_then(|s| s.into_string().ok())
        .unwrap_or_else(|| "localhost".into());
    let port = bind.rsplit(':').next().unwrap_or("8089");
    format!("http://{host}:{port}")
}

/// Discover LLM capabilities by looking at what's configured in the
/// environment. Phase 1: trust the env. Phase 2 adds provider-side
/// remaining-quota probes.
fn discover_capabilities() -> Vec<LlmCapability> {
    let mut caps = Vec::new();

    if std::env::var("MISTRAL_API_KEY").is_ok() {
        caps.push(LlmCapability {
            provider: "mistral".into(),
            model: std::env::var("MISTRAL_MODEL").unwrap_or_else(|_| "mistral-small-latest".into()),
            auth_via: AuthVia::ApiKey,
            budget_monthly_cents: parse_budget("NOETHER_GRID_MISTRAL_BUDGET_CENTS"),
            budget_remaining_cents: parse_budget("NOETHER_GRID_MISTRAL_BUDGET_CENTS"),
            rate_limit_rpm: None,
        });
    }
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        caps.push(LlmCapability {
            provider: "anthropic".into(),
            model: std::env::var("ANTHROPIC_MODEL").unwrap_or_else(|_| "claude-opus-4-6".into()),
            auth_via: AuthVia::ApiKey,
            budget_monthly_cents: parse_budget("NOETHER_GRID_ANTHROPIC_BUDGET_CENTS"),
            budget_remaining_cents: parse_budget("NOETHER_GRID_ANTHROPIC_BUDGET_CENTS"),
            rate_limit_rpm: None,
        });
    }
    if std::env::var("OPENAI_API_KEY").is_ok() {
        caps.push(LlmCapability {
            provider: "openai".into(),
            model: std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4-turbo".into()),
            auth_via: AuthVia::ApiKey,
            budget_monthly_cents: parse_budget("NOETHER_GRID_OPENAI_BUDGET_CENTS"),
            budget_remaining_cents: parse_budget("NOETHER_GRID_OPENAI_BUDGET_CENTS"),
            rate_limit_rpm: None,
        });
    }
    if std::env::var("VERTEX_AI_PROJECT").is_ok() {
        caps.push(LlmCapability {
            provider: "vertex".into(),
            model: std::env::var("VERTEX_AI_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".into()),
            auth_via: AuthVia::Oauth,
            budget_monthly_cents: parse_budget("NOETHER_GRID_VERTEX_BUDGET_CENTS"),
            budget_remaining_cents: parse_budget("NOETHER_GRID_VERTEX_BUDGET_CENTS"),
            rate_limit_rpm: None,
        });
    }

    // CLI-based seats — probed, not env-gated. Each probe is a light
    // `<binary> --version` check; a missing tool isn't an error, it
    // just means no capability for that provider.
    caps.extend(probe_subscription_clis());

    if caps.is_empty() {
        tracing::warn!(
            "no LLM credentials detected in env or on PATH — worker will advertise zero capabilities"
        );
    }
    caps
}

/// Detect every subscription-CLI the noether engine knows about
/// (Claude, Gemini, Cursor Agent, OpenCode) and advertise each as a
/// pooled capability.
///
/// Checks:
///
/// 1. **Binary on PATH.** `<binary> --version` exits 0. Fast, cheap;
///    if no binary, we skip that provider.
/// 2. **`NOETHER_LLM_SKIP_CLI` not set.** When the operator has
///    explicitly suppressed CLI providers (the Nix-sandbox workaround
///    caloron-noether uses), we don't advertise any of them.
///
/// Auth-state verification is deliberately not done here — the real
/// check happens on first dispatch, where a logged-out CLI surfaces
/// as a dispatch error and the broker's retry path picks a different
/// worker. Trying to validate login state ahead of time means
/// reverse-engineering four different auth-file layouts, and they
/// drift under us whenever a vendor ships a new version.
fn probe_subscription_clis() -> Vec<LlmCapability> {
    use noether_engine::llm::cli_provider::{cli_providers_suppressed, specs, CliProvider};

    if cli_providers_suppressed() {
        tracing::info!(
            "NOETHER_LLM_SKIP_CLI is set — not advertising any subscription-CLI capabilities"
        );
        return Vec::new();
    }

    tracing::info!(
        "probing subscription CLIs; PATH={}",
        std::env::var("PATH").unwrap_or_else(|_| "<unset>".into())
    );

    let mut caps = Vec::new();
    for spec in specs::ALL {
        let provider = CliProvider::new(*spec);
        let resolved = which_on_path(spec.binary);
        let available = provider.available();
        tracing::info!(
            "probe {}: binary={} resolved={} available={}",
            spec.provider_slug,
            spec.binary,
            resolved.as_deref().unwrap_or("<not on PATH>"),
            available,
        );
        if !available {
            continue;
        }
        let budget = subscription_budget(spec.provider_slug);
        tracing::info!(
            "advertising {} capability (binary {}, model {}, budget ${})",
            spec.provider_slug,
            spec.binary,
            spec.default_model,
            budget / 100,
        );
        caps.push(LlmCapability {
            provider: spec.provider_slug.into(),
            model: spec.default_model.into(),
            auth_via: AuthVia::Cli,
            budget_monthly_cents: budget,
            budget_remaining_cents: budget,
            rate_limit_rpm: None,
        });
    }
    caps
}

/// Per-provider monthly budget declaration. None of the subscription
/// plans expose a machine-readable remaining-quota endpoint for
/// individual users, so the operator declares a monthly cap via env.
/// Env var name: `NOETHER_GRID_<UPPER_SLUG>_BUDGET_CENTS`.
/// Default when unset: typical personal-plan monthly price
/// (Claude Pro $20, Gemini Advanced $20, Cursor Pro $20, OpenCode $0
/// since it's free / self-hosted).
fn subscription_budget(provider_slug: &str) -> u64 {
    let env_name = format!(
        "NOETHER_GRID_{}_BUDGET_CENTS",
        provider_slug.replace('-', "_").to_uppercase()
    );
    let declared = std::env::var(&env_name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if declared != 0 {
        return declared;
    }
    match provider_slug {
        "anthropic-cli" | "google-cli" | "cursor-cli" => 2000, // $20/mo
        "opencode" => 0,
        _ => 0,
    }
}

/// Resolve a bare binary name against `$PATH` the same way a shell
/// would, for diagnostic logging. Returns `None` if not found.
fn which_on_path(binary: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn parse_budget(var: &str) -> u64 {
    std::env::var(var)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

async fn enrol_with_broker(
    broker: &str,
    secret: &str,
    advertisement: &WorkerAdvertisement,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let mut req = client.post(format!("{broker}/workers")).json(advertisement);
    if !secret.is_empty() {
        req = req.header("X-Grid-Secret", secret);
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        return Err(format!("broker enrolment failed: {}", resp.status()).into());
    }
    tracing::info!("enroled with broker");
    Ok(())
}

async fn send_heartbeat(
    broker: &str,
    secret: &str,
    worker_id: &WorkerId,
    hb: &Heartbeat,
) -> Result<(), reqwest::Error> {
    let client = reqwest::Client::new();
    let mut req = client
        .post(format!("{broker}/workers/{}/heartbeat", worker_id.0))
        .json(hb);
    if !secret.is_empty() {
        req = req.header("X-Grid-Secret", secret);
    }
    let _ = req.send().await?;
    Ok(())
}

// ── Axum handlers ────────────────────────────────────────────────────────────

async fn health(State(state): State<Arc<WorkerState>>) -> Json<serde_json::Value> {
    Json(json!({
        "ok": true,
        "stages_indexed": state.store.list(None).len(),
        "in_flight": state.in_flight.load(Ordering::Relaxed),
    }))
}

async fn execute(
    State(state): State<Arc<WorkerState>>,
    Json(req): Json<ExecuteRequest>,
) -> (StatusCode, Json<JobResult>) {
    state.in_flight.fetch_add(1, Ordering::Relaxed);
    let result = run_graph(&state.store, req).await;
    state.in_flight.fetch_sub(1, Ordering::Relaxed);
    (StatusCode::OK, Json(result))
}

/// `POST /stage/{id}` — RemoteStage-compatible single-stage execution.
///
/// Body: `{"input": <value>}`. Response (success):
/// `{"ok": true, "data": {"output": <value>, "spent_cents": <u64>}}`.
/// On failure: `{"ok": false, "error": <msg>}` with HTTP 500.
///
/// This is the contract noether-engine's `RemoteStage` executor expects
/// (it reads `data.output`). Used by the broker's graph splitter — when
/// the broker rewrites a `Stage { id }` node as
/// `RemoteStage { url: ".../stage/<id>" }`, the RemoteStage executor
/// hits this endpoint without further code changes.
async fn execute_single_stage(
    State(state): State<Arc<WorkerState>>,
    axum::extract::Path(id): axum::extract::Path<String>,
    Json(body): Json<serde_json::Value>,
) -> (StatusCode, Json<serde_json::Value>) {
    use noether_core::stage::StageId;
    use noether_engine::executor::composite::CompositeExecutor;
    use noether_engine::executor::StageExecutor;

    let input = body
        .get("input")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let stage_id = StageId(id);

    state.in_flight.fetch_add(1, Ordering::Relaxed);
    let store_clone = state.store_snapshot();
    let result = tokio::task::spawn_blocking(move || {
        let executor = CompositeExecutor::from_store(&store_clone);
        executor.execute(&stage_id, &input)
    })
    .await;
    state.in_flight.fetch_sub(1, Ordering::Relaxed);

    match result {
        Ok(Ok(output)) => (
            StatusCode::OK,
            Json(json!({
                "ok": true,
                "data": { "output": output, "spent_cents": 0 }
            })),
        ),
        Ok(Err(e)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": format!("{e}")})),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"ok": false, "error": format!("task panicked: {e}")})),
        ),
    }
}

/// Hash the pre-resolution graph or return a ready-made failure
/// `JobResult` describing why we couldn't hash it.
///
/// Extracted from [`run_graph`] so the error path (contract:
/// `status == Failed` + `composition_id == None` + non-empty
/// `error`) can be exercised by a unit test without fabricating a
/// serde_jcs-hostile `serde_json::Value` — the test injects a
/// closure that returns `Err` directly.
///
/// The contract this helper encodes:
/// - `Ok(id)` on successful hash — never the literal `"unknown"`.
/// - `Err(JobResult)` on hash failure, with `composition_id: None`
///   (so broker correlation can distinguish "unhashable" from any
///   real composition id) and a structured error message.
//
// `result_large_err` fires because `JobResult` is chunky (Value +
// timestamps + JobId); boxing it here would obscure the helper's
// intent for essentially zero runtime payoff — the failure path
// only fires on serde_jcs anomalies, which are exceptional.
#[allow(clippy::result_large_err)]
fn hash_or_build_failure<H>(
    graph: &CompositionGraph,
    job_id: &JobId,
    hasher: H,
) -> Result<String, JobResult>
where
    H: FnOnce(&CompositionGraph) -> Result<String, serde_json::Error>,
{
    match hasher(graph) {
        Ok(id) => Ok(id),
        Err(e) => Err(JobResult {
            job_id: job_id.clone(),
            status: JobStatus::Failed,
            output: serde_json::Value::Null,
            spent_cents: 0,
            composition_id: None,
            error: Some(format!("failed to hash composition graph: {e}")),
            completed_at: Utc::now(),
        }),
    }
}

async fn run_graph(store: &JsonFileStore, req: ExecuteRequest) -> JobResult {
    use noether_engine::executor::composite::CompositeExecutor;
    use noether_engine::executor::runner::run_composition;
    use noether_engine::lagrange::{compute_composition_id, parse_graph, resolve_pinning};

    let mut graph = match parse_graph(&req.graph.to_string()) {
        Ok(g) => g,
        Err(e) => {
            return JobResult {
                job_id: req.job_id,
                status: JobStatus::Failed,
                output: serde_json::Value::Null,
                spent_cents: 0,
                composition_id: None,
                error: Some(format!("invalid graph JSON: {e}")),
                completed_at: Utc::now(),
            };
        }
    };

    // Composition identity comes from the pre-resolution graph (M1
    // canonical-form contract); grid-submitted graphs ride through
    // the resolver before dispatch so signature-pinned nodes reach
    // the executor with concrete impl ids.
    //
    // Invariant: `composition_id` is set from the pre-resolution
    // graph and therefore stays populated even on `Failed` results —
    // including failures surfaced by the resolver itself. A broker
    // correlating runs across workers sees the same id for the same
    // source graph regardless of which (possibly now-deprecated)
    // implementation it resolved to on any given run. The one
    // exception is `hash_or_build_failure` returning `Err` below,
    // where no id can be produced — we record `composition_id:
    // None` rather than shipping a stringly-typed "unknown" that
    // would collide with other unrelated hash failures.
    let composition_id = match hash_or_build_failure(&graph, &req.job_id, compute_composition_id) {
        Ok(id) => id,
        Err(result) => return result,
    };
    if let Err(e) = resolve_pinning(&mut graph.root, store) {
        return JobResult {
            job_id: req.job_id,
            status: JobStatus::Failed,
            output: serde_json::Value::Null,
            spent_cents: 0,
            composition_id: Some(composition_id),
            error: Some(format!("pinning resolution: {e}")),
            completed_at: Utc::now(),
        };
    }
    let executor = CompositeExecutor::from_store(store);

    // Run the graph on a blocking thread — the executor is synchronous.
    let graph_root = graph.root.clone();
    let input = req.input.clone();
    let comp_id = composition_id.clone();
    let run_result = tokio::task::spawn_blocking(move || {
        run_composition(&graph_root, &input, &executor, &comp_id)
    })
    .await;

    match run_result {
        Ok(Ok(result)) => JobResult {
            job_id: req.job_id,
            status: JobStatus::Ok,
            output: result.output,
            spent_cents: result.spent_cents,
            composition_id: Some(composition_id),
            error: None,
            completed_at: Utc::now(),
        },
        Ok(Err(e)) => JobResult {
            job_id: req.job_id,
            status: JobStatus::Failed,
            output: serde_json::Value::Null,
            spent_cents: 0,
            composition_id: Some(composition_id),
            error: Some(format!("{e}")),
            completed_at: Utc::now(),
        },
        Err(e) => JobResult {
            job_id: req.job_id,
            status: JobStatus::Failed,
            output: serde_json::Value::Null,
            spent_cents: 0,
            composition_id: Some(composition_id),
            error: Some(format!("task panicked: {e}")),
            completed_at: Utc::now(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noether_core::stage::StageId;
    use noether_engine::lagrange::{compute_composition_id, CompositionNode, Pinning};

    fn dummy_graph() -> CompositionGraph {
        CompositionGraph::new(
            "test",
            CompositionNode::Stage {
                id: StageId("abc".into()),
                pinning: Pinning::Signature,
                config: None,
            },
        )
    }

    // serde_jcs rejects very little that serde_json's own `Value`
    // can represent (non-finite floats are already rejected at
    // `Value` construction time, so they never reach the
    // canonicaliser). Injecting a hasher that returns `Err` is the
    // contract-level way to exercise the failure path — the
    // reviewer on PR #40 explicitly listed "mock
    // compute_composition_id to return Err" as acceptable.
    #[test]
    fn hash_failure_yields_failed_jobresult_with_no_composition_id() {
        let graph = dummy_graph();
        let job_id = JobId("job-1".into());
        let failing_hasher =
            |_: &CompositionGraph| Err(serde_json::from_str::<()>("not json").unwrap_err());

        let outcome = hash_or_build_failure(&graph, &job_id, failing_hasher);
        let failure = outcome.expect_err("hash should have failed");

        assert_eq!(failure.status, JobStatus::Failed);
        assert_eq!(failure.composition_id, None);
        assert_eq!(failure.output, serde_json::Value::Null);
        assert_eq!(failure.spent_cents, 0);
        assert_eq!(failure.job_id.0, "job-1");
        let err = failure.error.as_deref().expect("error must be populated");
        assert!(
            err.contains("failed to hash composition graph"),
            "error message should identify the failure: {err}"
        );
        // Guard against the pre-fix behaviour sneaking back in.
        assert!(
            !err.contains("unknown"),
            "error should not stringly-type to 'unknown': {err}"
        );
    }

    #[test]
    fn successful_hash_returns_composition_id() {
        let graph = dummy_graph();
        let job_id = JobId("job-2".into());
        let outcome = hash_or_build_failure(&graph, &job_id, compute_composition_id);
        let id = outcome.expect("real hasher should succeed on a trivial graph");
        assert!(!id.is_empty());
        assert_ne!(id, "unknown");
    }
}
