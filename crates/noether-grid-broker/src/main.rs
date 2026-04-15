//! noether-grid-broker — pool worker LLM capacity, dispatch Lagrange jobs.
//!
//! Research binary. See `docs/research/grid.md` for the design; this is
//! Phase 1 scope: in-memory state, single-worker-per-graph routing, no
//! cost accounting beyond simple worker-declared caps.

mod registry;
mod router;
mod routes;
mod splitter;
mod state;

use axum::{routing, Router};
use clap::Parser;
use std::net::SocketAddr;
use std::sync::Arc;

use state::AppState;

#[derive(Parser, Debug)]
#[command(name = "noether-grid-broker", about = "Pool worker LLM capacity")]
struct Cli {
    /// Bind address.
    #[arg(long, env = "NOETHER_GRID_BIND", default_value = "0.0.0.0:8088")]
    bind: String,
    /// Shared secret workers present on enrolment. Empty = no auth
    /// (dev only).
    #[arg(long, env = "NOETHER_GRID_SECRET", default_value = "")]
    secret: String,
    /// Path to a JSON store the broker reads at startup to seed its
    /// stage catalogue. The catalogue is used by the graph splitter
    /// to look up each `Stage { id }`'s declared effects.
    #[arg(
        long,
        env = "NOETHER_STORE_PATH",
        default_value = ".noether/store.json"
    )]
    store_path: String,
    /// Optional path to a JSON file mapping API key → monthly quota
    /// (in US cents). Format: `{"key-abc": 50000, "key-def": 10000}`.
    /// When present, every `POST /jobs` checks the quota before
    /// accepting; over-quota submissions return 429.
    #[arg(long, env = "NOETHER_GRID_QUOTAS_FILE")]
    quotas_file: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "noether_grid_broker=info,tower_http=warn".into()),
        )
        .init();

    let cli = Cli::parse();
    let state = Arc::new(AppState::new(cli.secret.clone()));

    // Seed the broker's stage catalogue from a local JSON store. This
    // is what the graph splitter walks to identify Llm-effect stages
    // worth dispatching. Always include the stdlib so simple test
    // graphs (Const-only or stdlib-only) work without extra setup.
    {
        use noether_core::stdlib::load_stdlib;
        use noether_store::{JsonFileStore, StageStore};
        let mut seed = load_stdlib();
        if let Ok(store) = JsonFileStore::open(&cli.store_path) {
            for stage in store.list(None) {
                seed.push(stage.clone());
            }
        }
        let count = seed.len();
        state.seed_stages(seed).await;
        tracing::info!("loaded {count} stage(s) into broker catalogue");
    }

    // Optionally seed per-agent quotas from JSON. Format:
    // `{"<api-key>": <monthly-cents>}`. Missing file or parse error =
    // log + boot anyway with no quotas.
    if let Some(path) = &cli.quotas_file {
        match std::fs::read_to_string(path) {
            Ok(raw) => match serde_json::from_str::<std::collections::HashMap<String, u64>>(&raw) {
                Ok(map) => {
                    let n = map.len();
                    state.seed_quotas(map).await;
                    tracing::info!("loaded {n} per-agent quota(s) from {path}");
                }
                Err(e) => {
                    tracing::warn!(
                        "could not parse quotas file {path}: {e}; running without quotas"
                    );
                }
            },
            Err(e) => {
                tracing::warn!("could not read quotas file {path}: {e}; running without quotas");
            }
        }
    }

    // Spawn the worker-pruner so dead workers disappear from the
    // registry within 3× their declared heartbeat interval.
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                ticker.tick().await;
                state.prune_stale_workers().await;
            }
        });
    }

    // Background gauge refresher — workers_total / workers_healthy
    // gauges are only useful when up-to-date. Cheap snapshot every 5s.
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(std::time::Duration::from_secs(5));
            loop {
                ticker.tick().await;
                let snapshot = state.snapshot_workers().await;
                state.metrics.workers_total.set(snapshot.len() as i64);
                state
                    .metrics
                    .workers_healthy
                    .set(snapshot.iter().filter(|w| w.healthy).count() as i64);
            }
        });
    }

    let app = Router::new()
        .route("/health", routing::get(routes::health))
        .route("/workers", routing::get(routes::list_workers))
        .route("/workers", routing::post(routes::enrol_worker))
        .route("/workers/{id}/heartbeat", routing::post(routes::heartbeat))
        .route("/workers/{id}", routing::delete(routes::drain_worker))
        .route("/jobs", routing::post(routes::submit_job))
        .route("/jobs/{id}", routing::get(routes::get_job))
        .route("/metrics", routing::get(routes::metrics))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = cli.bind.parse()?;
    tracing::info!("noether-grid-broker listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
