//! Shared wire-format types for noether-grid.
//!
//! Research status: types will churn. Nothing here is API-stable until
//! a first pilot lands.

use serde::{Deserialize, Serialize};

// ── Worker identity + advertisement ─────────────────────────────────────────

/// Opaque stable identifier for a worker. Chosen by the worker
/// (hostname + pid is fine) and reported on every heartbeat so the
/// broker's registry is keyed consistently.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkerId(pub String);

impl std::fmt::Display for WorkerId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// How a capability's credentials are authenticated on the worker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthVia {
    /// External CLI tool (e.g. `claude` with its own OAuth state).
    Cli,
    /// Direct API key in an env var.
    ApiKey,
    /// OAuth token the worker manages.
    Oauth,
}

/// One LLM-capacity advertisement. A single worker may report multiple
/// capabilities (one per provider × model) if it has more than one seat
/// available.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmCapability {
    /// Provider slug: `anthropic`, `openai`, `mistral`, `vertex`,
    /// `cursor`, `copilot`. Free-form string; the broker treats unknown
    /// providers as opaque tokens and matches by equality.
    pub provider: String,
    /// Model identifier as the worker would pass it to the provider
    /// (e.g. `claude-opus-4-6`, `gpt-4-turbo`).
    pub model: String,
    pub auth_via: AuthVia,
    /// Monthly budget cap declared by the worker owner (US cents).
    pub budget_monthly_cents: u64,
    /// Remaining spend the worker estimates for the current billing
    /// period (US cents). Reported on every heartbeat.
    pub budget_remaining_cents: u64,
    /// Optional per-minute rate limit the broker should respect.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit_rpm: Option<u32>,
}

/// Payload of `POST /workers` — registers a worker + its capabilities.
///
/// Workers re-POST this on every process restart. The broker treats a
/// re-registration with the same `worker_id` as an update.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerAdvertisement {
    pub worker_id: WorkerId,
    /// Fully-qualified URL the broker should POST `/execute` to. Must be
    /// reachable from the broker (i.e. no loopback for non-test setups).
    pub url: String,
    #[serde(default)]
    pub capabilities: Vec<LlmCapability>,
    /// Version of the noether binary running on the worker.
    pub noether_version: String,
    /// How often the worker intends to heartbeat (seconds). Broker
    /// prunes workers whose last heartbeat is older than 3× this.
    pub heartbeat_interval_secs: u64,
}

/// Payload of `POST /workers/{id}/heartbeat` — lightweight liveness ping
/// that can also refresh remaining budget and current load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub worker_id: WorkerId,
    /// Updated capacity snapshot. If empty, broker keeps the previous
    /// advertisement (useful for pure liveness pings).
    #[serde(default)]
    pub capabilities: Vec<LlmCapability>,
    /// Jobs currently executing on this worker.
    pub in_flight_jobs: u32,
}

// ── Job submission + execution ──────────────────────────────────────────────

/// Unique identifier for a submitted job. Assigned by the broker.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct JobId(pub String);

impl std::fmt::Display for JobId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// `POST /jobs` body. The graph is the canonical Lagrange JSON — same
/// format `noether run` accepts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    /// Serialised Lagrange graph. Opaque to the protocol crate (kept as
    /// `serde_json::Value` so this crate doesn't depend on
    /// `noether-engine`).
    pub graph: serde_json::Value,
    /// Root-input for the graph.
    pub input: serde_json::Value,
    /// Maximum acceptable wait (seconds) in the broker's queue before
    /// dispatch. Defaults to no-timeout when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_timeout_secs: Option<u64>,
    /// Hard cents budget the caller is willing to spend on this job.
    /// Broker refuses dispatch if the chosen worker's budget or this
    /// cap would be exceeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_cents: Option<u64>,
}

/// `POST /execute` body on the worker side.
///
/// Contains everything the worker needs to run the graph locally —
/// including the original `JobId` so traces round-trip cleanly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecuteRequest {
    pub job_id: JobId,
    pub graph: serde_json::Value,
    pub input: serde_json::Value,
}

/// Job-result envelope returned by the worker to the broker and relayed
/// to the caller. Follows the ACLI shape loosely.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobResult {
    pub job_id: JobId,
    pub status: JobStatus,
    /// Graph output (null on failure).
    pub output: serde_json::Value,
    /// Cents spent during execution (from
    /// `CompositionResult::spent_cents`).
    pub spent_cents: u64,
    /// Composition ID for trace lookup on the worker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub composition_id: Option<String>,
    /// Error message when `status == Failed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Completion timestamp (RFC3339). Assigned by the worker.
    pub completed_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Queued in the broker, not yet dispatched.
    Queued,
    /// Dispatched to a worker and executing.
    Running,
    /// Completed successfully.
    Ok,
    /// Worker reported an error.
    Failed,
    /// Worker timed out or went offline mid-execution.
    Abandoned,
}

// ── Broker observability ────────────────────────────────────────────────────

/// `GET /workers` returns this for each known worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSnapshot {
    pub worker_id: WorkerId,
    pub url: String,
    pub capabilities: Vec<LlmCapability>,
    pub in_flight_jobs: u32,
    pub last_seen: chrono::DateTime<chrono::Utc>,
    pub healthy: bool,
}

// ── Round-trip tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertisement_round_trip() {
        let adv = WorkerAdvertisement {
            worker_id: WorkerId("alice-mbp".into()),
            url: "http://alice-mbp.corp:8080".into(),
            capabilities: vec![LlmCapability {
                provider: "anthropic".into(),
                model: "claude-opus-4-6".into(),
                auth_via: AuthVia::Cli,
                budget_monthly_cents: 20000,
                budget_remaining_cents: 14200,
                rate_limit_rpm: Some(60),
            }],
            noether_version: "0.3.2".into(),
            heartbeat_interval_secs: 10,
        };
        let json = serde_json::to_string(&adv).unwrap();
        let parsed: WorkerAdvertisement = serde_json::from_str(&json).unwrap();
        assert_eq!(adv, parsed);
    }

    #[test]
    fn heartbeat_with_empty_capabilities_parses() {
        let hb =
            serde_json::from_str::<Heartbeat>(r#"{"worker_id":"bob","in_flight_jobs":2}"#).unwrap();
        assert_eq!(hb.worker_id, WorkerId("bob".into()));
        assert_eq!(hb.in_flight_jobs, 2);
        assert!(hb.capabilities.is_empty());
    }

    #[test]
    fn job_status_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&JobStatus::Abandoned).unwrap(),
            "\"abandoned\""
        );
    }
}
