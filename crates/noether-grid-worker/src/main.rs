//! noether-grid-worker — advertises local LLM capacity, runs graphs on
//! request from a grid broker.
//!
//! Research binary. Phase 1 scope: in-memory state, capability
//! discovery from env, best-effort budget tracking.

use axum::{extract::State, http::StatusCode, routing, Json, Router};
use chrono::Utc;
use clap::Parser;
use noether_grid_protocol::{
    AuthVia, ExecuteRequest, Heartbeat, JobResult, JobStatus, LlmCapability, WorkerAdvertisement,
    WorkerId,
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
    #[arg(
        long,
        env = "NOETHER_STORE_PATH",
        default_value = ".noether/store.json"
    )]
    store_path: String,
}

struct WorkerState {
    store: JsonFileStore,
    in_flight: AtomicU32,
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

    let store = JsonFileStore::open(&cli.store_path)?;
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

    if caps.is_empty() {
        tracing::warn!(
            "no LLM credentials detected in env — worker will advertise zero capabilities"
        );
    }
    caps
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

async fn run_graph(store: &JsonFileStore, req: ExecuteRequest) -> JobResult {
    use noether_engine::executor::composite::CompositeExecutor;
    use noether_engine::executor::runner::run_composition;
    use noether_engine::lagrange::{compute_composition_id, parse_graph};

    let graph = match parse_graph(&req.graph.to_string()) {
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

    let composition_id = compute_composition_id(&graph).unwrap_or_else(|_| "unknown".into());
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
