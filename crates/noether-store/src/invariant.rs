//! Shared enforcement of the "≤1 Active per `signature_id`" invariant.
//!
//! Both [`MemoryStore`](crate::MemoryStore) and
//! [`JsonFileStore`](crate::JsonFileStore) call into these helpers from
//! their `put`, `upsert`, and `update_lifecycle` paths.

use noether_core::stage::{SignatureId, Stage, StageId, StageLifecycle};
use std::collections::HashMap;

/// Return the Active stage IDs in `stages` that share `signature_id`
/// with the stage identified by `exclude_id` (the stage currently being
/// inserted or promoted — it must not deprecate itself).
///
/// An empty `signature_id` short-circuits to an empty vector: a stage
/// without a signature has nothing to deduplicate against.
pub(crate) fn duplicate_active_ids_for(
    stages: &HashMap<String, Stage>,
    exclude_id: &StageId,
    signature_id: Option<&SignatureId>,
) -> Vec<StageId> {
    let sig = match signature_id {
        Some(s) => s,
        None => return Vec::new(),
    };
    stages
        .values()
        .filter(|s| {
            s.id != *exclude_id
                && matches!(s.lifecycle, StageLifecycle::Active)
                && s.signature_id.as_ref() == Some(sig)
        })
        .map(|s| s.id.clone())
        .collect()
}

/// Variant used on the `put`/`upsert` path, where the incoming stage's
/// lifecycle decides whether the invariant applies at all: a Draft
/// insert never deprecates existing Actives.
pub(crate) fn duplicate_active_ids_for_incoming(
    stages: &HashMap<String, Stage>,
    incoming: &Stage,
) -> Vec<StageId> {
    if !matches!(incoming.lifecycle, StageLifecycle::Active) {
        return Vec::new();
    }
    duplicate_active_ids_for(stages, &incoming.id, incoming.signature_id.as_ref())
}

/// Emit a structured `warn!` for each stage auto-deprecated by the
/// invariant. Operators rely on this event to notice registry churn —
/// a silent deprecation hides meaningful state changes.
pub(crate) fn log_auto_deprecation(
    deprecated_ids: &[StageId],
    successor: &StageId,
    signature_id: Option<&SignatureId>,
) {
    for old in deprecated_ids {
        tracing::warn!(
            target: "noether_store::invariant",
            deprecated_stage_id = %old.0,
            successor_stage_id = %successor.0,
            signature_id = signature_id.map(|s| s.0.as_str()).unwrap_or(""),
            "auto-deprecated Active stage: another Active stage shares its signature_id"
        );
    }
}
