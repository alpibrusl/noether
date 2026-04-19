//! Deprecation-chain resolver.
//!
//! Walks a [`CompositionNode`], replacing any `Stage { id }` that
//! points at a `Deprecated { successor_id }` stage with the final
//! non-deprecated implementation, following `successor_id` links.
//!
//! Paired with [`resolve_pinning`](super::resolver::resolve_pinning):
//! pinning maps signature → implementation, this maps deprecated
//! implementation → active successor. Most call sites want both,
//! pinning first.
//!
//! Every entry point that runs this (CLI `run`, `compose`, `serve`,
//! `build`, `build_browser`, scheduler, grid-broker, grid-worker)
//! used to carry its own copy of the walker. This module is the
//! shared implementation.

use noether_core::stage::{StageId, StageLifecycle};
use noether_store::StageStore;

use super::ast::CompositionNode;

/// Upper bound on successor-chain walks. Chains longer than this
/// (unusual in practice; indicates either a legitimate very-long
/// deprecation history or an accidental cycle) are truncated and
/// surface as [`ChainEvent::MaxHopsExceeded`].
pub const MAX_DEPRECATION_HOPS: usize = 10;

/// Single rewrite performed by [`resolve_deprecated_stages`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeprecationRewrite {
    pub from: StageId,
    pub to: StageId,
}

/// Condition that ended a successor-chain walk without finding a
/// non-deprecated stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChainEvent {
    /// Chain contained a cycle. `stage` is the id we revisited.
    /// Rewrites up to the cycle point are still applied; execution
    /// continues with the last distinct id before the loop.
    CycleDetected { stage: StageId },
    /// Chain was longer than [`MAX_DEPRECATION_HOPS`]. Execution
    /// continues with whatever id the walker reached at the cap.
    MaxHopsExceeded { stage: StageId },
}

/// Result of walking the graph's stage references.
#[derive(Debug, Default, Clone)]
pub struct DeprecationReport {
    /// Successful rewrites (`from` found Deprecated, was replaced
    /// with its eventual non-deprecated successor).
    pub rewrites: Vec<DeprecationRewrite>,
    /// Anomalies encountered during the walk. Callers should
    /// surface these to operators — silent truncation hides broken
    /// deprecation chains in the store.
    pub events: Vec<ChainEvent>,
}

/// Walk the graph, following `successor_id` chains to resolve
/// references to deprecated stages. Returns a report; the caller is
/// expected to log the events and/or present them to the user.
///
/// Cycles are detected explicitly via a per-root visited set rather
/// than relying on the hop cap alone — a cycle is a data-integrity
/// problem in the store, not a legitimate 11-hop chain, and the two
/// failure modes deserve different operator responses.
pub fn resolve_deprecated_stages(
    node: &mut CompositionNode,
    store: &dyn StageStore,
) -> DeprecationReport {
    let mut report = DeprecationReport::default();
    walk(node, store, &mut report);
    report
}

fn walk(node: &mut CompositionNode, store: &dyn StageStore, report: &mut DeprecationReport) {
    match node {
        CompositionNode::Stage { id, .. } => follow_chain(id, store, report),
        CompositionNode::Sequential { stages } => {
            for s in stages {
                walk(s, store, report);
            }
        }
        CompositionNode::Parallel { branches } => {
            for (_, branch) in branches.iter_mut() {
                walk(branch, store, report);
            }
        }
        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => {
            walk(predicate, store, report);
            walk(if_true, store, report);
            walk(if_false, store, report);
        }
        CompositionNode::Retry { stage, .. } => walk(stage, store, report),
        CompositionNode::Fanout { source, targets } => {
            walk(source, store, report);
            for t in targets {
                walk(t, store, report);
            }
        }
        CompositionNode::Merge { sources, target } => {
            for s in sources {
                walk(s, store, report);
            }
            walk(target, store, report);
        }
        CompositionNode::Const { .. } | CompositionNode::RemoteStage { .. } => {}
        CompositionNode::Let { bindings, body } => {
            for b in bindings.values_mut() {
                walk(b, store, report);
            }
            walk(body, store, report);
        }
    }
}

fn follow_chain(id: &mut StageId, store: &dyn StageStore, report: &mut DeprecationReport) {
    let mut visited: std::collections::HashSet<StageId> = std::collections::HashSet::new();
    visited.insert(id.clone());
    let mut current = id.clone();
    let mut hops = 0usize;

    while let Ok(Some(stage)) = store.get(&current) {
        let successor = match &stage.lifecycle {
            StageLifecycle::Deprecated { successor_id } => successor_id.clone(),
            _ => break,
        };

        if !visited.insert(successor.clone()) {
            // Cycle — stop before looping. `current` is the last
            // distinct id before the re-entry; that's what we
            // leave in the graph.
            report.events.push(ChainEvent::CycleDetected {
                stage: successor.clone(),
            });
            break;
        }

        hops += 1;
        if hops > MAX_DEPRECATION_HOPS {
            report.events.push(ChainEvent::MaxHopsExceeded {
                stage: successor.clone(),
            });
            break;
        }

        report.rewrites.push(DeprecationRewrite {
            from: current.clone(),
            to: successor.clone(),
        });
        current = successor;
    }

    if current != *id {
        *id = current;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lagrange::ast::Pinning;
    use noether_core::effects::EffectSet;
    use noether_core::stage::{
        compute_stage_id, CostEstimate, Stage, StageLifecycle, StageSignature,
    };
    use noether_core::types::NType;
    use noether_store::MemoryStore;
    use std::collections::BTreeSet;

    fn stage(name: &str, impl_hash: &str, lifecycle: StageLifecycle) -> Stage {
        let signature = StageSignature {
            input: NType::Text,
            output: NType::Text,
            effects: EffectSet::pure(),
            implementation_hash: impl_hash.into(),
        };
        let id = compute_stage_id(name, &signature).unwrap();
        Stage {
            id,
            signature_id: None,
            signature,
            capabilities: BTreeSet::new(),
            cost: CostEstimate {
                time_ms_p50: None,
                tokens_est: None,
                memory_mb: None,
            },
            description: "t".into(),
            examples: vec![],
            lifecycle,
            ed25519_signature: None,
            signer_public_key: None,
            implementation_code: None,
            implementation_language: None,
            ui_style: None,
            tags: vec![],
            aliases: vec![],
            name: Some(name.into()),
            properties: Vec::new(),
        }
    }

    fn leaf(id: &StageId) -> CompositionNode {
        CompositionNode::Stage {
            id: id.clone(),
            pinning: Pinning::Both,
            config: None,
        }
    }

    #[test]
    fn noop_on_active_stage() {
        let mut store = MemoryStore::new();
        let active = stage("a", "ha", StageLifecycle::Active);
        let id = active.id.clone();
        store.put(active).unwrap();

        let mut root = leaf(&id);
        let report = resolve_deprecated_stages(&mut root, &store);
        assert!(report.rewrites.is_empty());
        assert!(report.events.is_empty());
    }

    #[test]
    fn single_hop_rewrites() {
        let mut store = MemoryStore::new();
        let new_stage = stage("new", "hn", StageLifecycle::Active);
        let new_id = new_stage.id.clone();
        store.put(new_stage).unwrap();

        // Add `old` as Active then transition to Deprecated — only
        // way through the store's lifecycle-validation path.
        let old_active = stage("old", "ho", StageLifecycle::Active);
        let old_id = old_active.id.clone();
        store.put(old_active).unwrap();
        store
            .update_lifecycle(
                &old_id,
                StageLifecycle::Deprecated {
                    successor_id: new_id.clone(),
                },
            )
            .unwrap();

        let mut root = leaf(&old_id);
        let report = resolve_deprecated_stages(&mut root, &store);
        assert_eq!(
            report.rewrites,
            vec![DeprecationRewrite {
                from: old_id,
                to: new_id.clone(),
            }]
        );
        match root {
            CompositionNode::Stage { id, .. } => assert_eq!(id, new_id),
            _ => unreachable!(),
        }
        assert!(report.events.is_empty());
    }

    #[test]
    fn cycle_detected_and_surfaced() {
        // Two stages, each claiming the other as its successor.
        // A real store can't get into this state through the public
        // API (update_lifecycle requires the successor to exist at
        // deprecation time), so we construct a bespoke fake store
        // that serves the cycle directly.
        use noether_store::{StageStore, StoreError, StoreStats};
        use std::collections::HashMap;

        struct CyclicStore {
            stages: HashMap<String, Stage>,
        }

        impl StageStore for CyclicStore {
            fn put(&mut self, _s: Stage) -> Result<StageId, StoreError> {
                unimplemented!()
            }
            fn upsert(&mut self, _s: Stage) -> Result<StageId, StoreError> {
                unimplemented!()
            }
            fn remove(&mut self, _id: &StageId) -> Result<(), StoreError> {
                unimplemented!()
            }
            fn get(&self, id: &StageId) -> Result<Option<&Stage>, StoreError> {
                Ok(self.stages.get(&id.0))
            }
            fn contains(&self, id: &StageId) -> bool {
                self.stages.contains_key(&id.0)
            }
            fn list(&self, _lc: Option<&StageLifecycle>) -> Vec<&Stage> {
                self.stages.values().collect()
            }
            fn update_lifecycle(
                &mut self,
                _id: &StageId,
                _lc: StageLifecycle,
            ) -> Result<(), StoreError> {
                unimplemented!()
            }
            fn stats(&self) -> StoreStats {
                StoreStats {
                    total: self.stages.len(),
                    by_lifecycle: Default::default(),
                    by_effect: Default::default(),
                }
            }
        }

        let a = stage("a", "ha", StageLifecycle::Active);
        let b = stage("b", "hb", StageLifecycle::Active);
        let a_id = a.id.clone();
        let b_id = b.id.clone();

        let a_dep = Stage {
            lifecycle: StageLifecycle::Deprecated {
                successor_id: b_id.clone(),
            },
            ..a
        };
        let b_dep = Stage {
            lifecycle: StageLifecycle::Deprecated {
                successor_id: a_id.clone(),
            },
            ..b
        };
        let mut stages = HashMap::new();
        stages.insert(a_id.0.clone(), a_dep);
        stages.insert(b_id.0.clone(), b_dep);
        let store = CyclicStore { stages };

        let mut root = leaf(&a_id);
        let report = resolve_deprecated_stages(&mut root, &store);

        // Walker must terminate and surface the cycle.
        assert!(
            report
                .events
                .iter()
                .any(|e| matches!(e, ChainEvent::CycleDetected { .. })),
            "expected CycleDetected event, got {:?}",
            report.events
        );
    }
}
