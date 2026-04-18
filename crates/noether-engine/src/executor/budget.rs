//! Runtime cost-budget enforcement for composition execution.
//!
//! [`BudgetedExecutor`] wraps any [`StageExecutor`] and tracks actual cost
//! consumed by each stage using its declared [`Effect::Cost`] effects.
//! The accounting uses an `AtomicU64` so parallel branches are handled
//! correctly without a mutex.
//!
//! ## Semantics
//!
//! Cost is **deducted before** a stage runs.  If adding a stage's declared
//! cost would push `spent_cents` past `budget_cents`, the call returns
//! [`ExecutionError::BudgetExceeded`] immediately — the stage is never
//! invoked.  This is conservative: a stage that fails does not refund its
//! cost.
//!
//! Parallel branches that collectively exceed the budget will each see the
//! up-to-date atomic counter.  The first branch to cross the limit aborts;
//! others may proceed transiently if they add their cost in the same
//! microsecond, but the overall spent total accurately reflects reality.
//!
//! ## Usage
//!
//! ```no_run
//! use noether_engine::executor::budget::{BudgetedExecutor, build_cost_map};
//! use noether_engine::executor::mock::MockExecutor;
//! use noether_engine::lagrange::CompositionNode;
//! use noether_store::MemoryStore;
//!
//! let store = MemoryStore::new();
//! let cost_map = build_cost_map(&CompositionNode::Const { value: serde_json::Value::Null }, &store);
//! let inner = MockExecutor::new();
//! let budgeted = BudgetedExecutor::new(inner, cost_map, 100 /* cents */);
//! ```

use super::{ExecutionError, StageExecutor};
use noether_core::effects::Effect;
use noether_core::stage::StageId;
use noether_store::StageStore;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

// ── Cost map ─────────────────────────────────────────────────────────────────

/// Walk a composition graph and build a map of `StageId → declared_cents`.
///
/// Only stages that declare at least one `Effect::Cost { cents }` appear in
/// the map.  Stages not in the store (e.g. RemoteStage) are ignored.
pub fn build_cost_map(
    node: &crate::lagrange::CompositionNode,
    store: &(impl StageStore + ?Sized),
) -> HashMap<StageId, u64> {
    let mut map = HashMap::new();
    collect_costs(node, store, &mut map);
    map
}

fn collect_costs(
    node: &crate::lagrange::CompositionNode,
    store: &(impl StageStore + ?Sized),
    map: &mut HashMap<StageId, u64>,
) {
    use crate::lagrange::CompositionNode::*;
    match node {
        Stage { id, .. } => {
            if let Ok(Some(stage)) = store.get(id) {
                let total: u64 = stage
                    .signature
                    .effects
                    .iter()
                    .filter_map(|e| {
                        if let Effect::Cost { cents } = e {
                            Some(*cents)
                        } else {
                            None
                        }
                    })
                    .sum();
                if total > 0 {
                    map.insert(id.clone(), total);
                }
            }
        }
        RemoteStage { .. } | Const { .. } => {}
        Sequential { stages } => {
            for s in stages {
                collect_costs(s, store, map);
            }
        }
        Parallel { branches } => {
            for b in branches.values() {
                collect_costs(b, store, map);
            }
        }
        Branch {
            predicate,
            if_true,
            if_false,
        } => {
            collect_costs(predicate, store, map);
            collect_costs(if_true, store, map);
            collect_costs(if_false, store, map);
        }
        Fanout { source, targets } => {
            collect_costs(source, store, map);
            for t in targets {
                collect_costs(t, store, map);
            }
        }
        Merge { sources, target } => {
            for s in sources {
                collect_costs(s, store, map);
            }
            collect_costs(target, store, map);
        }
        Retry { stage, .. } => collect_costs(stage, store, map),
        Let { bindings, body } => {
            for b in bindings.values() {
                collect_costs(b, store, map);
            }
            collect_costs(body, store, map);
        }
    }
}

// ── BudgetedExecutor ──────────────────────────────────────────────────────────

/// An executor wrapper that enforces a runtime cost budget.
///
/// Maintains an `Arc<AtomicU64>` counter of cents spent so that concurrent
/// parallel branches all see the same running total.
pub struct BudgetedExecutor<E: StageExecutor> {
    inner: E,
    /// Declared cost in cents per stage id.
    cost_map: HashMap<StageId, u64>,
    /// Running total shared with all clones / concurrent uses.
    spent_cents: Arc<AtomicU64>,
    /// Hard limit in cents.
    budget_cents: u64,
}

impl<E: StageExecutor> BudgetedExecutor<E> {
    /// Create a new budgeted executor wrapping `inner`.
    ///
    /// `cost_map` maps stage ids to their declared cost in cents
    /// (build it with [`build_cost_map`]).
    /// `budget_cents` is the hard limit; execution aborts when it would
    /// be exceeded.
    pub fn new(inner: E, cost_map: HashMap<StageId, u64>, budget_cents: u64) -> Self {
        Self {
            inner,
            cost_map,
            spent_cents: Arc::new(AtomicU64::new(0)),
            budget_cents,
        }
    }

    /// Return a snapshot of cents spent so far.
    pub fn spent_cents(&self) -> u64 {
        self.spent_cents.load(Ordering::Relaxed)
    }
}

impl<E: StageExecutor + Sync> StageExecutor for BudgetedExecutor<E> {
    fn execute(&self, stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        let cost = self.cost_map.get(stage_id).copied().unwrap_or(0);

        if cost > 0 {
            // Atomically reserve the cost before executing.
            // fetch_add returns the *previous* value, so newly_spent = prev + cost.
            let prev = self.spent_cents.fetch_add(cost, Ordering::SeqCst);
            let newly_spent = prev + cost;

            if newly_spent > self.budget_cents {
                // Roll back: we're not going to run this stage.
                self.spent_cents.fetch_sub(cost, Ordering::SeqCst);
                return Err(ExecutionError::BudgetExceeded {
                    spent_cents: prev,
                    budget_cents: self.budget_cents,
                });
            }
        }

        self.inner.execute(stage_id, input)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::mock::MockExecutor;
    use crate::lagrange::CompositionNode;
    use noether_core::effects::{Effect, EffectSet};
    use noether_core::stage::{CostEstimate, Stage, StageId, StageLifecycle, StageSignature};
    use noether_core::types::NType;
    use noether_store::MemoryStore;
    use serde_json::json;
    use std::collections::BTreeSet;

    fn make_costly_stage(id: &str, cents: u64) -> Stage {
        Stage {
            id: StageId(id.into()),
            signature_id: None,
            signature: StageSignature {
                input: NType::Any,
                output: NType::Any,
                effects: EffectSet::new([
                    Effect::Cost { cents },
                    Effect::Llm {
                        model: "gpt".into(),
                    },
                ]),
                implementation_hash: format!("impl_{id}"),
            },
            capabilities: BTreeSet::new(),
            cost: CostEstimate {
                time_ms_p50: None,
                tokens_est: None,
                memory_mb: None,
            },
            description: format!("costly stage {id}"),
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
        }
    }

    #[test]
    fn no_cost_stages_pass_through() {
        let executor = MockExecutor::new().with_output(&StageId("a".into()), json!(1));
        let budgeted = BudgetedExecutor::new(executor, HashMap::new(), 0);
        let result = budgeted.execute(&StageId("a".into()), &json!(null));
        assert_eq!(result.unwrap(), json!(1));
        assert_eq!(budgeted.spent_cents(), 0);
    }

    #[test]
    fn within_budget_executes_and_tracks_cost() {
        let id = StageId("llm".into());
        let executor = MockExecutor::new().with_output(&id, json!("ok"));
        let cost_map = HashMap::from([(id.clone(), 10u64)]);
        let budgeted = BudgetedExecutor::new(executor, cost_map, 100);
        assert!(budgeted.execute(&id, &json!(null)).is_ok());
        assert_eq!(budgeted.spent_cents(), 10);
    }

    #[test]
    fn over_budget_returns_error_and_rolls_back() {
        let id = StageId("expensive".into());
        let executor = MockExecutor::new().with_output(&id, json!("ok"));
        let cost_map = HashMap::from([(id.clone(), 50u64)]);
        let budgeted = BudgetedExecutor::new(executor, cost_map, 49);

        let err = budgeted.execute(&id, &json!(null)).unwrap_err();
        assert!(
            matches!(
                err,
                ExecutionError::BudgetExceeded {
                    spent_cents: 0,
                    budget_cents: 49
                }
            ),
            "expected BudgetExceeded, got {err:?}"
        );
        // Counter rolled back — no cost was charged.
        assert_eq!(budgeted.spent_cents(), 0);
    }

    #[test]
    fn second_stage_pushes_over_budget() {
        let a = StageId("a".into());
        let b = StageId("b".into());
        let executor = MockExecutor::new()
            .with_output(&a, json!(1))
            .with_output(&b, json!(2));
        let cost_map = HashMap::from([(a.clone(), 60u64), (b.clone(), 50u64)]);
        let budgeted = BudgetedExecutor::new(executor, cost_map, 100);

        // First call: 60¢ → within 100¢ budget.
        assert!(budgeted.execute(&a, &json!(null)).is_ok());
        assert_eq!(budgeted.spent_cents(), 60);

        // Second call: 60 + 50 = 110¢ > 100¢ → abort.
        let err = budgeted.execute(&b, &json!(null)).unwrap_err();
        assert!(
            matches!(
                err,
                ExecutionError::BudgetExceeded {
                    spent_cents: 60,
                    budget_cents: 100
                }
            ),
            "got {err:?}"
        );
        // Rolled back.
        assert_eq!(budgeted.spent_cents(), 60);
    }

    #[test]
    fn build_cost_map_extracts_costs_from_store() {
        let mut store = MemoryStore::new();
        store.put(make_costly_stage("s1", 25)).unwrap();
        store.put(make_costly_stage("s2", 75)).unwrap();

        let node = CompositionNode::Sequential {
            stages: vec![
                CompositionNode::Stage {
                    id: StageId("s1".into()),
                    config: None,
                },
                CompositionNode::Stage {
                    id: StageId("s2".into()),
                    config: None,
                },
            ],
        };

        let map = build_cost_map(&node, &store);
        assert_eq!(map[&StageId("s1".into())], 25);
        assert_eq!(map[&StageId("s2".into())], 75);
    }

    #[test]
    fn build_cost_map_ignores_free_stages() {
        let mut store = MemoryStore::new();
        // Stage with no Cost effect.
        let free = Stage {
            id: StageId("free".into()),
            signature_id: None,
            signature: StageSignature {
                input: NType::Any,
                output: NType::Any,
                effects: EffectSet::pure(),
                implementation_hash: "impl".into(),
            },
            capabilities: BTreeSet::new(),
            cost: CostEstimate {
                time_ms_p50: None,
                tokens_est: None,
                memory_mb: None,
            },
            description: "free stage".into(),
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
        };
        store.put(free).unwrap();

        let node = CompositionNode::Stage {
            id: StageId("free".into()),
            config: None,
        };
        let map = build_cost_map(&node, &store);
        assert!(map.is_empty(), "free stage should not appear in cost map");
    }
}
