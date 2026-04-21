//! Runtime enforcement of [`NType::Refined`] predicates at stage boundaries.
//!
//! [`ValidatingExecutor`] wraps any [`StageExecutor`] and validates a stage's
//! declared input refinements against the value it receives and its declared
//! output refinements against the value it returns. A violation short-circuits
//! with [`ExecutionError::StageFailed`] carrying the human-readable reason
//! from [`validate_refinement`].
//!
//! Refinements the executor does not know about (because the stage is not in
//! the store, or the stage's types carry no [`NType::Refined`] layers) are a
//! no-op — the call is forwarded unchanged.
//!
//! ## Wire once per execution
//!
//! Construction scans the store once and caches the refinement list per stage.
//! The wrapper owns no store reference, so it is safe to ship into threads or
//! hand to downstream code that runs after the store drops out of scope.
//!
//! ## Opt-out
//!
//! The CLI wraps the executor by default. Set `NOETHER_NO_REFINEMENT_CHECK=1`
//! in the environment to fall back to raw execution — useful when shipping a
//! stage whose refinements have drifted from its implementation and you need
//! a runtime to land the fix.
//!
//! [`NType::Refined`]: noether_core::types::NType::Refined

use super::{ExecutionError, StageExecutor};
use noether_core::stage::StageId;
use noether_core::types::{refinements_of, validate_refinement, Refinement};
use noether_store::StageStore;
use serde_json::Value;
use std::collections::HashMap;

/// Environment variable that disables runtime refinement enforcement when set
/// to any non-empty value. Checked once per call to [`ValidatingExecutor::is_disabled`].
pub const DISABLE_ENV_VAR: &str = "NOETHER_NO_REFINEMENT_CHECK";

/// Per-stage refinement bundle: predicates declared on the input side and on
/// the output side, in outermost-first order (the order [`validate_refinement`]
/// applies them).
#[derive(Default, Clone)]
struct RefinementBundle {
    input: Vec<Refinement>,
    output: Vec<Refinement>,
}

impl RefinementBundle {
    fn is_empty(&self) -> bool {
        self.input.is_empty() && self.output.is_empty()
    }
}

/// Wraps a [`StageExecutor`] and enforces refinement predicates at every
/// stage boundary.
pub struct ValidatingExecutor<E: StageExecutor> {
    inner: E,
    refinements: HashMap<StageId, RefinementBundle>,
}

impl<E: StageExecutor> ValidatingExecutor<E> {
    /// Build a validator by snapshotting refinements from every stage in
    /// `store`. Stages with no refined input or output never occupy a map
    /// entry — lookups are a single `HashMap::get` and cheap for the common
    /// no-refinement path.
    pub fn from_store(inner: E, store: &dyn StageStore) -> Self {
        let mut refinements = HashMap::new();
        for stage in store.list(None) {
            let bundle = RefinementBundle {
                input: refinements_of(&stage.signature.input)
                    .into_iter()
                    .cloned()
                    .collect(),
                output: refinements_of(&stage.signature.output)
                    .into_iter()
                    .cloned()
                    .collect(),
            };
            if !bundle.is_empty() {
                refinements.insert(stage.id.clone(), bundle);
            }
        }
        Self { inner, refinements }
    }

    /// Build a validator with no precomputed refinements — a drop-in pass-
    /// through used by tests that only want the trait surface.
    pub fn new(inner: E) -> Self {
        Self {
            inner,
            refinements: HashMap::new(),
        }
    }

    /// `true` when [`DISABLE_ENV_VAR`] is set to any non-empty value.
    pub fn is_disabled() -> bool {
        std::env::var(DISABLE_ENV_VAR)
            .map(|v| !v.is_empty())
            .unwrap_or(false)
    }

    /// Borrow the wrapped executor. The runner needs this to reach back into
    /// a [`BudgetedExecutor`] for `spent_cents()` after execution completes.
    pub fn inner(&self) -> &E {
        &self.inner
    }
}

impl<E: StageExecutor> StageExecutor for ValidatingExecutor<E> {
    fn execute(&self, stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        let bundle = self.refinements.get(stage_id);

        if let Some(bundle) = bundle {
            for refinement in &bundle.input {
                if let Err(reason) = validate_refinement(input, refinement) {
                    return Err(ExecutionError::StageFailed {
                        stage_id: stage_id.clone(),
                        message: format!("input refinement violation ({refinement}): {reason}"),
                    });
                }
            }
        }

        let output = self.inner.execute(stage_id, input)?;

        if let Some(bundle) = bundle {
            for refinement in &bundle.output {
                if let Err(reason) = validate_refinement(&output, refinement) {
                    return Err(ExecutionError::StageFailed {
                        stage_id: stage_id.clone(),
                        message: format!("output refinement violation ({refinement}): {reason}"),
                    });
                }
            }
        }

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::mock::MockExecutor;
    use noether_core::effects::EffectSet;
    use noether_core::stage::{CostEstimate, Stage, StageLifecycle, StageSignature};
    use noether_core::types::{NType, Refinement};
    use noether_store::MemoryStore;
    use serde_json::json;
    use std::collections::BTreeSet;

    fn refined_stage(id: &str, input: NType, output: NType) -> Stage {
        Stage {
            id: StageId(id.into()),
            signature_id: None,
            signature: StageSignature {
                input,
                output,
                effects: EffectSet::pure(),
                implementation_hash: format!("impl_{id}"),
            },
            capabilities: BTreeSet::new(),
            cost: CostEstimate {
                time_ms_p50: None,
                tokens_est: None,
                memory_mb: None,
            },
            description: format!("refined stage {id}"),
            examples: vec![],
            lifecycle: StageLifecycle::Active,
            ed25519_signature: None,
            signer_public_key: None,
            implementation_code: None,
            implementation_language: None,
            ui_style: None,
            tags: vec![],
            aliases: vec![],
            name: None,
            properties: Vec::new(),
        }
    }

    fn percent_range() -> Refinement {
        Refinement::Range {
            min: Some(0.0),
            max: Some(100.0),
        }
    }

    #[test]
    fn passes_through_when_no_refinements_declared() {
        let id = StageId("plain".into());
        let inner = MockExecutor::new().with_output(&id, json!(42));
        let mut store = MemoryStore::new();
        store
            .put(refined_stage("plain", NType::Number, NType::Number))
            .unwrap();

        let exec = ValidatingExecutor::from_store(inner, &store);
        let out = exec.execute(&id, &json!(1)).unwrap();
        assert_eq!(out, json!(42));
    }

    #[test]
    fn accepts_input_inside_refinement() {
        let id = StageId("percent".into());
        let inner = MockExecutor::new().with_output(&id, json!(50));
        let ty = NType::refined(NType::Number, percent_range());
        let mut store = MemoryStore::new();
        store.put(refined_stage("percent", ty.clone(), ty)).unwrap();

        let exec = ValidatingExecutor::from_store(inner, &store);
        let out = exec.execute(&id, &json!(50)).unwrap();
        assert_eq!(out, json!(50));
    }

    #[test]
    fn rejects_input_outside_refinement() {
        let id = StageId("percent".into());
        let inner = MockExecutor::new().with_output(&id, json!(50));
        let ty = NType::refined(NType::Number, percent_range());
        let mut store = MemoryStore::new();
        store.put(refined_stage("percent", ty.clone(), ty)).unwrap();

        let exec = ValidatingExecutor::from_store(inner, &store);
        let err = exec.execute(&id, &json!(150)).unwrap_err();
        match err {
            ExecutionError::StageFailed { stage_id, message } => {
                assert_eq!(stage_id, id);
                assert!(
                    message.contains("input refinement violation"),
                    "unexpected message: {message}"
                );
                assert!(message.contains("above maximum"), "unexpected: {message}");
            }
            other => panic!("expected StageFailed, got {other:?}"),
        }
    }

    #[test]
    fn rejects_output_outside_refinement() {
        let id = StageId("bad_impl".into());
        // Inner returns 999 — well outside 0..=100.
        let inner = MockExecutor::new().with_output(&id, json!(999));
        let ty = NType::refined(NType::Number, percent_range());
        let mut store = MemoryStore::new();
        store
            .put(refined_stage("bad_impl", ty.clone(), ty))
            .unwrap();

        let exec = ValidatingExecutor::from_store(inner, &store);
        let err = exec.execute(&id, &json!(50)).unwrap_err();
        let ExecutionError::StageFailed { message, .. } = err else {
            panic!("expected StageFailed");
        };
        assert!(
            message.contains("output refinement violation"),
            "got: {message}"
        );
    }

    #[test]
    fn stage_not_in_store_is_passed_through() {
        // No entries in the refinement map at all — every call must forward.
        let id = StageId("anything".into());
        let inner = MockExecutor::new().with_output(&id, json!("ok"));
        let exec = ValidatingExecutor::new(inner);
        assert_eq!(exec.execute(&id, &json!(null)).unwrap(), json!("ok"));
    }
}
