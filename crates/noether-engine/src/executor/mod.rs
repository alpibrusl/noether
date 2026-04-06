pub mod composite;
pub mod inline;
pub mod mock;
pub mod nix;
pub mod runner;
pub mod stages;

use noether_core::stage::StageId;

#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("stage {0:?} not found")]
    StageNotFound(StageId),
    #[error("stage {stage_id:?} failed: {message}")]
    StageFailed { stage_id: StageId, message: String },
    #[error("retry exhausted after {attempts} attempts for stage {stage_id:?}")]
    RetryExhausted { stage_id: StageId, attempts: u32 },
}

/// Pluggable execution interface for individual stages.
pub trait StageExecutor {
    fn execute(
        &self,
        stage_id: &StageId,
        input: &serde_json::Value,
    ) -> Result<serde_json::Value, ExecutionError>;
}
