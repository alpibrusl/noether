use noether_engine::{
    executor::{inline::InlineExecutor, runner::run_composition},
    lagrange::{compute_composition_id, parse_graph},
    providers,
    registry_client::RemoteStageStore,
};
use noether_store::{JsonFileStore, StageStore};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tracing::{error, info, warn};

// ── Schedule config ─────────────────────────────────────────────────────────

/// A single scheduled composition job.
#[derive(Debug, Deserialize)]
pub struct ScheduledJob {
    /// Job name (for logging and trace identification).
    pub name: String,
    /// Cron expression, e.g. "0 * * * *" (every hour).
    pub cron: String,
    /// Path to the Lagrange graph JSON file.
    pub graph: String,
    /// Optional static JSON input to inject.
    pub input: Option<serde_json::Value>,
    /// Optional webhook URL to POST the ACLI result to.
    pub webhook: Option<String>,
}

/// Top-level scheduler config (parsed from JSON).
///
/// Execution selection (in priority order):
/// 1. `grid_broker` → submit each job to a `noether-grid-broker` and let
///    it route to a worker. Stage resolution lives on the broker; this
///    scheduler instance just submits + polls. Use this to opt into
///    intra-company LLM-pool dispatch.
/// 2. `registry_url` + optional `registry_api_key` → `RemoteStageStore`,
///    composition runs locally.
/// 3. `store_path` → local `JsonFileStore`, composition runs locally.
#[derive(Debug, Deserialize)]
pub struct SchedulerConfig {
    /// Path to a local JsonFileStore (used when `registry_url` is absent).
    #[serde(default)]
    pub store_path: Option<String>,
    /// URL of a remote noether-registry (e.g. "https://registry.example.com").
    /// Takes priority over `store_path` when set.
    pub registry_url: Option<String>,
    /// API key for the remote registry (`X-API-Key` header).
    pub registry_api_key: Option<String>,
    /// Optional `noether-grid-broker` URL. When set, every scheduled job
    /// is submitted to the broker via `POST /jobs` and polled instead of
    /// being executed locally. The broker decides routing and graph
    /// splitting; this scheduler doesn't need to know about workers.
    pub grid_broker: Option<String>,
    pub jobs: Vec<ScheduledJob>,
}

// ── Webhook result ──────────────────────────────────────────────────────────

#[derive(Serialize)]
struct WebhookPayload {
    ok: bool,
    job: String,
    composition_id: String,
    output: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// Submit a job to a `noether-grid-broker` and poll for completion.
/// Returns `Some((composition_id, webhook_payload))` when the broker
/// reaches a terminal status, or `None` if dispatch never started.
async fn dispatch_to_grid(
    broker: &str,
    job: &ScheduledJob,
    graph_raw: &str,
    _graph: &noether_engine::lagrange::CompositionGraph,
) -> Option<(String, WebhookPayload)> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "graph": serde_json::from_str::<serde_json::Value>(graph_raw).ok()?,
        "input": job.input.clone().unwrap_or(serde_json::Value::Null),
    });

    let submit = match client
        .post(format!("{broker}/jobs"))
        .json(&body)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            error!("Job {} — broker submit failed: {e}", job.name);
            return None;
        }
    };
    if !submit.status().is_success() {
        let code = submit.status();
        let txt = submit.text().await.unwrap_or_default();
        error!("Job {} — broker rejected submit ({code}): {txt}", job.name);
        return None;
    }
    let submit_body: serde_json::Value = match submit.json().await {
        Ok(v) => v,
        Err(e) => {
            error!("Job {} — broker submit response not JSON: {e}", job.name);
            return None;
        }
    };
    let job_id = match submit_body.get("job_id").and_then(|v| v.as_str()) {
        Some(s) => s.to_string(),
        None => {
            error!("Job {} — broker response missing job_id", job.name);
            return None;
        }
    };

    info!("Job {} — submitted to grid as {job_id}", job.name);

    // Poll for terminal status. Cap at 10 minutes; the broker has its
    // own timeouts so this is just a sanity bound.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        if std::time::Instant::now() > deadline {
            error!("Job {} — grid dispatch timed out after 10 min", job.name);
            return None;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        let resp = match client.get(format!("{broker}/jobs/{job_id}")).send().await {
            Ok(r) => r,
            Err(e) => {
                warn!("Job {} — grid poll error: {e}", job.name);
                continue;
            }
        };
        let v: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let status = v["status"].as_str().unwrap_or("").to_string();
        if status == "ok" || status == "failed" || status == "abandoned" {
            let result = &v["result"];
            let composition_id = result["composition_id"]
                .as_str()
                .unwrap_or("unknown")
                .to_string();
            let ok = status == "ok";
            let payload = WebhookPayload {
                ok,
                job: job.name.clone(),
                composition_id: composition_id.clone(),
                output: result["output"].clone(),
                error: result["error"].as_str().map(String::from),
            };
            return Some((composition_id, payload));
        }
    }
}

async fn fire_webhook(url: &str, payload: &WebhookPayload) {
    let client = reqwest::Client::new();
    match client.post(url).json(payload).send().await {
        Ok(resp) => info!("Webhook {} responded {}", url, resp.status()),
        Err(e) => warn!("Webhook {} failed: {}", url, e),
    }
}

// ── Job runner ──────────────────────────────────────────────────────────────

async fn run_job(job: &ScheduledJob, config: &SchedulerConfig) {
    info!("Running job: {}", job.name);

    let graph_json = match tokio::fs::read_to_string(&job.graph).await {
        Ok(s) => s,
        Err(e) => {
            error!(
                "Job {} — failed to read graph file {}: {}",
                job.name, job.graph, e
            );
            return;
        }
    };

    let mut graph = match parse_graph(&graph_json) {
        Ok(g) => g,
        Err(e) => {
            error!("Job {} — invalid graph JSON: {}", job.name, e);
            return;
        }
    };

    // Grid-broker dispatch: hand the graph + input to a remote broker
    // and poll for completion. The broker handles stage resolution,
    // worker selection, and graph splitting; we only relay the
    // outcome to the webhook the same way local execution would.
    if let Some(broker) = &config.grid_broker {
        if let Some((cid, payload)) = dispatch_to_grid(broker, job, &graph_json, &graph).await {
            if let Some(url) = &job.webhook {
                fire_webhook(url, &payload).await;
            }
            info!("Job {} — composition_id={cid}", job.name);
        }
        return;
    }

    // Build the store and run the composition synchronously, then drop the
    // store before any `.await` points so the future stays `Send`.
    let (composition_id, payload) = {
        // Build the store: prefer remote registry over local file.
        let store: Box<dyn StageStore> = if let Some(url) = &config.registry_url {
            let api_key = config.registry_api_key.as_deref();
            match RemoteStageStore::connect(url, api_key) {
                Ok(s) => {
                    info!("Job {} — using remote registry at {url}", job.name);
                    Box::new(s)
                }
                Err(e) => {
                    error!(
                        "Job {} — failed to connect to registry {url}: {e}",
                        job.name
                    );
                    return;
                }
            }
        } else {
            let path = config
                .store_path
                .as_deref()
                .unwrap_or(".noether/store.json");
            match JsonFileStore::open(path) {
                Ok(s) => {
                    info!("Job {} — using local store at {path}", job.name);
                    Box::new(s)
                }
                Err(e) => {
                    error!("Job {} — failed to open store: {e}", job.name);
                    return;
                }
            }
        };

        // Build executor: InlineExecutor for all pure/HOF stages, RuntimeExecutor
        // layered on top when LLM env vars are present.
        let (llm_provider, llm_name) = providers::build_llm_provider();
        let (emb_provider, _) = providers::build_embedding_provider();

        use noether_engine::executor::runtime::RuntimeExecutor;
        use noether_engine::llm::LlmConfig;

        let inline = InlineExecutor::from_store(store.as_ref());
        // composition_id from the pre-resolution graph — stable across
        // store changes, per #28.
        let cid = compute_composition_id(&graph).unwrap_or_else(|_| "unknown".into());
        // Resolve pinning against the store snapshot. Signature-pinned
        // refs rewrite to concrete implementation IDs; without this the
        // run would fail inside the executor's store.get() call.
        if let Err(e) = noether_engine::lagrange::resolve_pinning(&mut graph.root, store.as_ref()) {
            error!("Job {} — pinning resolution failed: {e}", job.name);
            return;
        }
        let job_input = job.input.clone().unwrap_or(serde_json::Value::Null);

        let result = if llm_name != "mock" {
            let runtime = RuntimeExecutor::from_store(store.as_ref())
                .with_llm(llm_provider, LlmConfig::default())
                .with_embedding(emb_provider);
            let chain = ChainExecutor {
                primary: runtime,
                fallback: inline,
            };
            run_composition(&graph.root, &job_input, &chain, &cid)
        } else {
            run_composition(&graph.root, &job_input, &inline, &cid)
        };

        // `store` is dropped at end of this block — before any `.await`.
        let composition_id = cid;
        let payload = match result {
            Ok(cr) => {
                info!(
                    "Job {} completed: {} stages executed",
                    job.name,
                    cr.trace.stages.len()
                );
                WebhookPayload {
                    ok: true,
                    job: job.name.clone(),
                    composition_id: composition_id.clone(),
                    output: cr.output,
                    error: None,
                }
            }
            Err(e) => {
                error!("Job {} failed: {}", job.name, e);
                WebhookPayload {
                    ok: false,
                    job: job.name.clone(),
                    composition_id: composition_id.clone(),
                    output: serde_json::Value::Null,
                    error: Some(e.to_string()),
                }
            }
        };
        (composition_id, payload)
    };

    if let Some(url) = &job.webhook {
        fire_webhook(url, &payload).await;
    }
    let _ = composition_id; // carried out of inner block for logging
}

// ── Chain executor (RuntimeExecutor → InlineExecutor fallback) ───────────────

struct ChainExecutor<
    A: noether_engine::executor::StageExecutor,
    B: noether_engine::executor::StageExecutor,
> {
    primary: A,
    fallback: B,
}

impl<A: noether_engine::executor::StageExecutor, B: noether_engine::executor::StageExecutor>
    noether_engine::executor::StageExecutor for ChainExecutor<A, B>
{
    fn execute(
        &self,
        stage_id: &noether_core::stage::StageId,
        input: &serde_json::Value,
    ) -> Result<serde_json::Value, noether_engine::executor::ExecutionError> {
        use noether_engine::executor::ExecutionError;
        match self.primary.execute(stage_id, input) {
            Err(ExecutionError::StageNotFound(_)) => self.fallback.execute(stage_id, input),
            other => other,
        }
    }
}

// ── Scheduler loop ──────────────────────────────────────────────────────────

/// `noether-scheduler` — run Lagrange graphs on a cron schedule and fire
/// webhooks with the result.
///
/// All three forms are accepted:
///
///   noether-scheduler scheduler.json            (positional, legacy)
///   noether-scheduler --config scheduler.json   (flag)
///   noether-scheduler                            (defaults to ./scheduler.json)
#[derive(clap::Parser)]
#[command(name = "noether-scheduler", about = "Cron-based composition scheduler")]
struct Cli {
    /// Path to the scheduler config JSON. Takes precedence over the
    /// positional argument. Defaults to `./scheduler.json`.
    #[arg(long, value_name = "PATH")]
    config: Option<String>,

    /// Legacy positional config path — accepted so older invocations keep
    /// working. If both this and `--config` are provided, `--config` wins.
    config_positional: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    use clap::Parser;
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "noether_scheduler=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let config_path = cli
        .config
        .or(cli.config_positional)
        .unwrap_or_else(|| "scheduler.json".into());

    let config_raw = std::fs::read_to_string(&config_path)
        .unwrap_or_else(|_| panic!("Failed to read config from {config_path}"));
    let config: SchedulerConfig =
        serde_json::from_str(&config_raw).expect("Invalid scheduler config JSON");

    info!("Loaded {} job(s) from {}", config.jobs.len(), config_path);

    if let Some(url) = &config.registry_url {
        info!("Store: remote registry at {url}");
    } else {
        let path = config
            .store_path
            .as_deref()
            .unwrap_or(".noether/store.json");
        info!("Store: local file at {path}");
    }

    // Wrap config in Arc so it can be shared across spawned tasks.
    let config = std::sync::Arc::new(config);
    let mut handles = Vec::new();

    for job in config
        .jobs
        .iter()
        .map(|j| ScheduledJob {
            name: j.name.clone(),
            cron: j.cron.clone(),
            graph: j.graph.clone(),
            input: j.input.clone(),
            webhook: j.webhook.clone(),
        })
        .collect::<Vec<_>>()
    {
        let cfg = std::sync::Arc::clone(&config);

        let schedule = cron::Schedule::from_str(&job.cron).unwrap_or_else(|_| {
            panic!("Invalid cron expression for job {}: {}", job.name, job.cron)
        });

        handles.push(tokio::spawn(async move {
            loop {
                let now = chrono::Utc::now();
                if let Some(next) = schedule.upcoming(chrono::Utc).next() {
                    let wait = (next - now).to_std().unwrap_or_default();
                    info!("Job {} next run at {} (in {:?})", job.name, next, wait);
                    tokio::time::sleep(wait).await;
                    run_job(&job, &cfg).await;
                } else {
                    break;
                }
            }
        }));
    }

    for handle in handles {
        let _ = handle.await;
    }

    Ok(())
}
