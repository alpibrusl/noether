use noether_core::stage::{Stage, StageId, StageLifecycle};
use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("stage with id {0:?} already exists")]
    AlreadyExists(StageId),
    #[error("stage with id {0:?} not found")]
    NotFound(StageId),
    #[error("invalid lifecycle transition: {reason}")]
    InvalidTransition { reason: String },
    #[error("invalid successor: {reason}")]
    InvalidSuccessor { reason: String },
    #[error("validation failed: {0:?}")]
    ValidationFailed(Vec<String>),
    #[error("I/O error: {message}")]
    IoError { message: String },
}

/// Summary statistics for a store.
#[derive(Debug, Clone)]
pub struct StoreStats {
    pub total: usize,
    pub by_lifecycle: BTreeMap<String, usize>,
    pub by_effect: BTreeMap<String, usize>,
}

/// Abstraction over stage storage.
pub trait StageStore {
    fn put(&mut self, stage: Stage) -> Result<StageId, StoreError>;
    /// Insert a stage, replacing any existing stage with the same ID.
    /// Used to upgrade unsigned stdlib stages after signing is added.
    fn upsert(&mut self, stage: Stage) -> Result<StageId, StoreError>;
    /// Remove a stage entirely. Returns `Ok(())` whether or not the stage existed.
    fn remove(&mut self, id: &StageId) -> Result<(), StoreError>;
    fn get(&self, id: &StageId) -> Result<Option<&Stage>, StoreError>;
    fn contains(&self, id: &StageId) -> bool;
    fn list(&self, lifecycle: Option<&StageLifecycle>) -> Vec<&Stage>;
    fn update_lifecycle(
        &mut self,
        id: &StageId,
        lifecycle: StageLifecycle,
    ) -> Result<(), StoreError>;
    fn stats(&self) -> StoreStats;

    // ── Owned accessors (default impls — no need to override) ──────────────

    /// Return an owned clone of the stage. Useful for async contexts where
    /// holding a borrow across lock boundaries is not permitted.
    fn get_owned(&self, id: &StageId) -> Result<Option<Stage>, StoreError> {
        Ok(self.get(id)?.cloned())
    }

    /// Return owned clones of all matching stages.
    fn list_owned(&self, lifecycle: Option<&StageLifecycle>) -> Vec<Stage> {
        self.list(lifecycle).into_iter().cloned().collect()
    }
}
