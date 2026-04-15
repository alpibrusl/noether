//! End-to-end dispatch test — spin up the broker on a random port, a
//! stub worker on another, enrol, submit a job, assert round-trip.
//!
//! No real LLM call — the stub worker just echoes input back as output.
//! This covers the protocol layer (enrol + heartbeat + dispatch + job
//! result relay) without needing a real `noether-engine` executor.

use axum::{
    routing::{get, post},
    Json, Router,
};
use chrono::Utc;
use noether_grid_protocol::{
    AuthVia, ExecuteRequest, JobResult, JobSpec, JobStatus, LlmCapability, WorkerAdvertisement,
    WorkerId,
};
use serde_json::json;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;

/// Pick a random free localhost port by binding ephemeral + dropping.
async fn pick_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

async fn stub_worker_echo(req: Json<ExecuteRequest>) -> Json<JobResult> {
    Json(JobResult {
        job_id: req.job_id.clone(),
        status: JobStatus::Ok,
        output: req.input.clone(),
        spent_cents: 12,
        composition_id: Some("test-composition".into()),
        error: None,
        completed_at: Utc::now(),
    })
}

async fn spawn_stub_worker(port: u16) -> tokio::task::JoinHandle<()> {
    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/execute", post(stub_worker_echo));
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let listener = TcpListener::bind(&addr).await.unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    })
}

#[tokio::test]
async fn broker_dispatches_to_healthy_worker() {
    // ── Spawn broker ─────────────────────────────────────────────────
    let broker_port = pick_port().await;
    let broker_bin = env!("CARGO_BIN_EXE_noether-grid-broker");
    let mut broker_proc = std::process::Command::new(broker_bin)
        .env("NOETHER_GRID_BIND", format!("127.0.0.1:{broker_port}"))
        .env("NOETHER_GRID_SECRET", "")
        .env("RUST_LOG", "error")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn broker");

    // Wait for broker to accept connections.
    let broker_url = format!("http://127.0.0.1:{broker_port}");
    let client = reqwest::Client::new();
    let mut ready = false;
    for _ in 0..50 {
        if client
            .get(format!("{broker_url}/health"))
            .send()
            .await
            .is_ok()
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ready, "broker never came up on {broker_url}");

    // ── Spawn stub worker ────────────────────────────────────────────
    let worker_port = pick_port().await;
    let _worker_task = spawn_stub_worker(worker_port).await;
    let worker_url = format!("http://127.0.0.1:{worker_port}");

    // Wait for stub to serve.
    for _ in 0..50 {
        if client
            .get(format!("{worker_url}/health"))
            .send()
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // ── Enrol worker with broker ────────────────────────────────────
    let adv = WorkerAdvertisement {
        worker_id: WorkerId("stub-1".into()),
        url: worker_url.clone(),
        capabilities: vec![LlmCapability {
            provider: "anthropic".into(),
            model: "claude-opus-4-6".into(),
            auth_via: AuthVia::ApiKey,
            budget_monthly_cents: 10000,
            budget_remaining_cents: 10000,
            rate_limit_rpm: None,
        }],
        noether_version: "test".into(),
        heartbeat_interval_secs: 10,
    };
    let resp = client
        .post(format!("{broker_url}/workers"))
        .json(&adv)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "enrol failed: {}",
        resp.status()
    );

    // Observability
    let list: Vec<serde_json::Value> = client
        .get(format!("{broker_url}/workers"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list.len(), 1);

    // ── Submit a job ────────────────────────────────────────────────
    // Minimal valid Lagrange: a single Const node so the broker's
    // graph-parse pass succeeds. Routing in phase 1 accepts any
    // worker regardless of declared models.
    let spec = JobSpec {
        graph: json!({
            "description": "test",
            "version": "0.1.0",
            "root": { "op": "Const", "value": null }
        }),
        input: json!({"hello": "world"}),
        queue_timeout_secs: None,
        budget_cents: None,
    };
    let submit: serde_json::Value = client
        .post(format!("{broker_url}/jobs"))
        .json(&spec)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let job_id = submit["job_id"].as_str().unwrap().to_string();

    // ── Poll for completion ─────────────────────────────────────────
    let mut final_status = None;
    for _ in 0..50 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        let v: serde_json::Value = client
            .get(format!("{broker_url}/jobs/{job_id}"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let status = v["status"].as_str().unwrap_or("").to_string();
        if status == "ok" || status == "failed" || status == "abandoned" {
            final_status = Some((status, v));
            break;
        }
    }
    let (status, envelope) = final_status.expect("job never completed");
    assert_eq!(status, "ok", "job did not succeed: {envelope}");
    assert_eq!(envelope["result"]["output"], json!({"hello": "world"}));
    assert_eq!(envelope["result"]["spent_cents"], json!(12));

    // ── Clean up ────────────────────────────────────────────────────
    let _ = broker_proc.kill();
    let _ = broker_proc.wait();
}
