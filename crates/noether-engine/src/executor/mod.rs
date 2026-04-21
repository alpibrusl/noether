// Native-only executor backends: require Nix, OS processes, LLM, or SQLite.
#[cfg(feature = "native")]
pub mod composite;
#[cfg(feature = "native")]
pub mod isolation;
#[cfg(feature = "native")]
pub mod nix;
#[cfg(feature = "native")]
pub mod runtime;

// Always-available executor implementations.
pub mod budget;
pub mod inline;
pub mod mock;
pub mod pure_cache;
pub mod runner;
pub mod stages;
pub mod validating;

pub use inline::InlineRegistry;

use noether_core::stage::StageId;

#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("stage {0:?} not found")]
    StageNotFound(StageId),
    #[error("stage {stage_id:?} failed: {message}")]
    StageFailed { stage_id: StageId, message: String },
    #[error("stage {stage_id:?} timed out after {timeout_secs}s")]
    TimedOut {
        stage_id: StageId,
        timeout_secs: u64,
    },
    #[error("cost budget exceeded: spent {spent_cents}¢ of {budget_cents}¢ limit")]
    BudgetExceeded { spent_cents: u64, budget_cents: u64 },
    #[error("retry exhausted after {attempts} attempts for stage {stage_id:?}")]
    RetryExhausted { stage_id: StageId, attempts: u32 },
    #[error("remote call to {url} failed: {reason}")]
    RemoteCallFailed { url: String, reason: String },
}

/// Pluggable execution interface for individual stages.
pub trait StageExecutor {
    fn execute(
        &self,
        stage_id: &StageId,
        input: &serde_json::Value,
    ) -> Result<serde_json::Value, ExecutionError>;
}
