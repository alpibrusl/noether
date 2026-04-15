//! noether-grid-broker — pool worker LLM capacity, dispatch Lagrange jobs.
//!
//! Research binary. See `docs/research/grid.md` for the design; this is
//! Phase 1 scope: in-memory state, single-worker-per-graph routing, no
//! cost accounting beyond simple worker-declared caps.

mod registry;
mod router;
mod routes;
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

    let app = Router::new()
        .route("/health", routing::get(routes::health))
        .route("/workers", routing::get(routes::list_workers))
        .route("/workers", routing::post(routes::enrol_worker))
        .route("/workers/{id}/heartbeat", routing::post(routes::heartbeat))
        .route("/workers/{id}", routing::delete(routes::drain_worker))
        .route("/jobs", routing::post(routes::submit_job))
        .route("/jobs/{id}", routing::get(routes::get_job))
        .layer(tower_http::trace::TraceLayer::new_for_http())
        .with_state(state);

    let addr: SocketAddr = cli.bind.parse()?;
    tracing::info!("noether-grid-broker listening on {addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}
