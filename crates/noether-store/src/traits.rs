use noether_core::stage::{SignatureId, Stage, StageId, StageLifecycle};
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

    /// Find all stages whose metadata `name` field matches exactly.
    /// Used by graph loaders so composition files can reference stages
    /// by their human-authored name instead of their 8-char content-hash
    /// prefix. Returns every match across all lifecycles; callers
    /// typically filter for `Active`.
    fn find_by_name(&self, name: &str) -> Vec<&Stage> {
        self.list(None)
            .into_iter()
            .filter(|s| s.name.as_deref() == Some(name))
            .collect()
    }

    /// Look up the Active stage for a given [`SignatureId`]. This is
    /// the M2 "resolve signature to latest implementation" pathway: a
    /// graph that pins a stage by `signature_id` gets whichever
    /// implementation is Active today.
    ///
    /// **Determinism.** When multiple Active stages share a signature
    /// (which a well-behaved store prevents via the `stage add`
    /// deprecation path, but which can happen transiently), this
    /// returns the stage with the lexicographically-smallest
    /// implementation ID. A "first match" would be nondeterministic
    /// under HashMap-backed stores.
    ///
    /// Callers that need to distinguish the "zero matches" and "many
    /// matches" cases should use [`active_stages_with_signature`] and
    /// inspect the length.
    fn get_by_signature(&self, signature_id: &SignatureId) -> Option<&Stage> {
        self.list(Some(&StageLifecycle::Active))
            .into_iter()
            .filter(|s| s.signature_id.as_ref() == Some(signature_id))
            .min_by(|a, b| a.id.0.cmp(&b.id.0))
    }

    /// Return every Active stage whose `signature_id` matches.
    /// Ordered lexicographically by implementation ID so iteration is
    /// stable across HashMap-backed stores.
    ///
    /// This is the diagnostic surface: a well-behaved store should
    /// return at most one entry here. A call that returns more is a
    /// signal that the "≤1 Active per signature" invariant has been
    /// broken — typically by a direct `store.put` + lifecycle change
    /// that bypassed the `stage add` deprecation path. The resolver
    /// uses this helper to warn on multi-match rather than silently
    /// picking one.
    fn active_stages_with_signature(&self, signature_id: &SignatureId) -> Vec<&Stage> {
        let mut matches: Vec<&Stage> = self
            .list(Some(&StageLifecycle::Active))
            .into_iter()
            .filter(|s| s.signature_id.as_ref() == Some(signature_id))
            .collect();
        matches.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        matches
    }
}
