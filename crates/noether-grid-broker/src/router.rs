//! Job → worker routing. Phase 1: single-worker-per-graph, pick the
//! healthy worker whose capabilities cover the graph's inferred LLM
//! needs, with highest remaining budget.

use noether_grid_protocol::WorkerId;

use crate::state::{AppState, WorkerEntry};

/// Human-readable description of why no worker was selected.
#[derive(Debug, Clone)]
pub enum RoutingRefusal {
    /// No workers are registered at all.
    NoWorkersRegistered,
    /// Workers exist but none have the required LLM capabilities.
    NoCapabilityMatch { needed: Vec<String> },
    /// Workers match but all are either at capacity or draining.
    AllBusy,
}

impl std::fmt::Display for RoutingRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoWorkersRegistered => write!(f, "no workers registered"),
            Self::NoCapabilityMatch { needed } => {
                write!(f, "no worker has all required LLM capabilities: {needed:?}")
            }
            Self::AllBusy => write!(f, "every matching worker is busy or draining"),
        }
    }
}

/// Select a worker for a graph.
///
/// `required_llm_models` is the set of `Effect::Llm{model}` strings
/// inferred from the graph. Phase 2 will split the graph across
/// workers; phase 1 requires one worker to cover them all.
pub async fn select_worker(
    state: &AppState,
    required_llm_models: &[String],
) -> Result<WorkerId, RoutingRefusal> {
    let now = chrono::Utc::now();
    let workers = state.workers.lock().await;

    if workers.is_empty() {
        return Err(RoutingRefusal::NoWorkersRegistered);
    }

    // Filter healthy workers whose capabilities cover every required
    // model. Empty requirement list means "any worker will do" —
    // routing is still useful for graphs that do no LLM work but want
    // hermetic execution off the caller's machine.
    let candidates: Vec<&WorkerEntry> = workers
        .values()
        .filter(|w| w.is_healthy(now))
        .filter(|w| {
            required_llm_models
                .iter()
                .all(|model| worker_has_model(w, model))
        })
        .collect();

    if candidates.is_empty() {
        if required_llm_models.is_empty() {
            return Err(RoutingRefusal::AllBusy);
        }
        return Err(RoutingRefusal::NoCapabilityMatch {
            needed: required_llm_models.to_vec(),
        });
    }

    // Pick the candidate with the highest total remaining budget across
    // the required models. Tiebreak: fewest in-flight jobs.
    let best = candidates
        .into_iter()
        .max_by_key(|w| {
            let remaining: u64 = w
                .advertisement
                .capabilities
                .iter()
                .filter(|c| required_llm_models.iter().any(|m| m == &c.model))
                .map(|c| c.budget_remaining_cents)
                .sum();
            // Invert in_flight so fewer jobs wins after budget tie.
            (remaining, -(i64::from(w.in_flight_jobs)))
        })
        .unwrap();

    Ok(best.advertisement.worker_id.clone())
}

fn worker_has_model(worker: &WorkerEntry, model: &str) -> bool {
    worker
        .advertisement
        .capabilities
        .iter()
        .any(|c| c.model == model && c.budget_remaining_cents > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use noether_grid_protocol::{AuthVia, LlmCapability, WorkerAdvertisement, WorkerId};

    fn advertisement(id: &str, models: &[(&str, u64)]) -> WorkerAdvertisement {
        WorkerAdvertisement {
            worker_id: WorkerId(id.into()),
            url: format!("http://{id}.corp:8080"),
            capabilities: models
                .iter()
                .map(|(m, b)| LlmCapability {
                    provider: "anthropic".into(),
                    model: (*m).into(),
                    auth_via: AuthVia::Cli,
                    budget_monthly_cents: *b,
                    budget_remaining_cents: *b,
                    rate_limit_rpm: None,
                })
                .collect(),
            noether_version: "0.3.2".into(),
            heartbeat_interval_secs: 10,
        }
    }

    async fn make_state_with(workers: Vec<WorkerAdvertisement>) -> AppState {
        let state = AppState::new(String::new());
        let now = chrono::Utc::now();
        let mut lock = state.workers.lock().await;
        for adv in workers {
            lock.insert(
                adv.worker_id.clone(),
                WorkerEntry {
                    advertisement: adv,
                    last_seen: now,
                    in_flight_jobs: 0,
                    draining: false,
                },
            );
        }
        drop(lock);
        state
    }

    #[tokio::test]
    async fn empty_registry_refuses() {
        let state = AppState::new(String::new());
        let err = select_worker(&state, &["claude-opus".into()])
            .await
            .unwrap_err();
        assert!(matches!(err, RoutingRefusal::NoWorkersRegistered));
    }

    #[tokio::test]
    async fn picks_worker_with_matching_model() {
        let state = make_state_with(vec![
            advertisement("alice", &[("gpt-4", 1000)]),
            advertisement("bob", &[("claude-opus", 5000)]),
        ])
        .await;
        let chosen = select_worker(&state, &["claude-opus".into()])
            .await
            .unwrap();
        assert_eq!(chosen, WorkerId("bob".into()));
    }

    #[tokio::test]
    async fn picks_highest_budget_when_multiple_match() {
        let state = make_state_with(vec![
            advertisement("alice", &[("claude-opus", 1000)]),
            advertisement("bob", &[("claude-opus", 5000)]),
            advertisement("carol", &[("claude-opus", 2000)]),
        ])
        .await;
        let chosen = select_worker(&state, &["claude-opus".into()])
            .await
            .unwrap();
        assert_eq!(chosen, WorkerId("bob".into()));
    }

    #[tokio::test]
    async fn no_match_reports_needed_models() {
        let state = make_state_with(vec![advertisement("alice", &[("gpt-4", 1000)])]).await;
        let err = select_worker(&state, &["claude-opus".into()])
            .await
            .unwrap_err();
        match err {
            RoutingRefusal::NoCapabilityMatch { needed } => {
                assert_eq!(needed, vec!["claude-opus".to_string()]);
            }
            other => panic!("wrong refusal: {other:?}"),
        }
    }
}
