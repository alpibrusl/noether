//! Pinning resolution pass.
//!
//! Rewrites a `CompositionNode` tree so every `Stage` node's `id` field
//! holds a concrete [`noether_core::stage::StageId`]
//! (implementation-level hash). After this pass runs, downstream
//! passes — effect inference, `--allow-effects` enforcement, Ed25519
//! verification, planner cost/parallel grouping, budget collection,
//! grid-broker splitter — can look up stages via `store.get(id)`
//! without regard for the original pinning.
//!
//! ## Why a separate pass?
//!
//! M2 introduced [`crate::lagrange::Pinning`]: a `Stage` node's `id`
//! is either a `SignatureId` (resolve to the current Active impl) or
//! an `ImplementationId` (bit-exact lookup). Teaching every downstream
//! pass about pinning would have been a dozen file changes, each of
//! them easy to get wrong.
//!
//! The pass approach is:
//!
//! 1. Call `resolve_pinning(&mut graph, &store)` once, after graph
//!    construction and after any prefix/name resolution.
//! 2. Every subsequent pass works on the mutated graph, where `id`
//!    is guaranteed to be an `ImplementationId` that exists in the
//!    store.
//!
//! This commits to "resolve once per execution". If the store changes
//! between resolution and execution, the resolved graph keeps
//! referring to the old implementation — a feature, not a bug: we
//! want a single execution to see a consistent snapshot.
//!
//! ## What the pass does NOT do
//!
//! - It does not change the `pinning` field. A node that was declared
//!   `Pinning::Signature` keeps that label even after its `id` has
//!   been rewritten to an implementation hash. Consumers that
//!   re-serialise the graph preserve the user's original intent (the
//!   wire format's `pinning: "signature"` still means "signature" on
//!   a future execution, not "both").
//! - It does not walk `RemoteStage` — that's resolved at call-time
//!   over HTTP, not via the local store.

use crate::lagrange::ast::{CompositionNode, Pinning};
use noether_core::stage::{SignatureId, StageId, StageLifecycle};
use noether_store::StageStore;

/// Error raised when a `Stage` node's reference cannot be resolved
/// against the store.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ResolutionError {
    #[error(
        "stage node with pinning=signature has id `{signature_id}` — \
         no Active stage in the store matches that signature"
    )]
    SignatureNotFound { signature_id: String },

    #[error(
        "stage node with pinning=both has id `{implementation_id}` — \
         no stage in the store has that implementation ID"
    )]
    ImplementationNotFound { implementation_id: String },

    #[error(
        "stage node with pinning=both has id `{implementation_id}` — \
         the stage exists but its lifecycle is {lifecycle:?}; only \
         Active stages may be referenced"
    )]
    ImplementationNotActive {
        implementation_id: String,
        lifecycle: StageLifecycle,
    },
}

/// Walk a composition tree and rewrite every `Stage` node's `id`
/// field to a concrete, in-store [`StageId`]. See the module doc for
/// rationale and invariants.
///
/// Returns the list of rewrites and diagnostics performed on success,
/// or the first [`ResolutionError`] on failure. The graph is left
/// partially-rewritten on error; callers that want atomic behaviour
/// should clone before calling.
pub fn resolve_pinning<S>(
    node: &mut CompositionNode,
    store: &S,
) -> Result<ResolutionReport, ResolutionError>
where
    S: StageStore + ?Sized,
{
    let mut report = ResolutionReport::default();
    resolve_recursive(node, store, &mut report)?;
    Ok(report)
}

/// Output of a successful [`resolve_pinning`] pass.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResolutionReport {
    /// One entry per Stage node whose `id` was changed by the pass.
    pub rewrites: Vec<Rewrite>,
    /// One entry per signature-pinned node where more than one Active
    /// implementation matched — a "≤1 Active per signature" invariant
    /// violation the CLI surfaces to the user. The pass still picks a
    /// deterministic winner via [`noether_store::StageStore::get_by_signature`];
    /// the warning exists so the user notices and fixes the store.
    pub warnings: Vec<MultiActiveWarning>,
}

/// Record of one rewrite the pass performed. Useful for tracing:
/// `noether run --trace-resolution` can print the before/after pairs.
#[derive(Debug, Clone, PartialEq)]
pub struct Rewrite {
    pub before: String,
    pub after: String,
    pub pinning: Pinning,
}

/// Diagnostic raised when a signature-pinned ref matches more than
/// one Active implementation. See [`ResolutionReport::warnings`].
#[derive(Debug, Clone, PartialEq)]
pub struct MultiActiveWarning {
    pub signature_id: String,
    pub active_implementation_ids: Vec<String>,
    pub chosen: String,
}

fn resolve_recursive<S>(
    node: &mut CompositionNode,
    store: &S,
    report: &mut ResolutionReport,
) -> Result<(), ResolutionError>
where
    S: StageStore + ?Sized,
{
    match node {
        CompositionNode::Stage { id, pinning, .. } => {
            let before = id.0.clone();
            // Diagnostic check for signature pinning: emit a warning if
            // more than one Active impl matches.
            if matches!(*pinning, Pinning::Signature) {
                let sig = SignatureId(id.0.clone());
                let matches = store.active_stages_with_signature(&sig);
                if matches.len() > 1 {
                    report.warnings.push(MultiActiveWarning {
                        signature_id: id.0.clone(),
                        active_implementation_ids: matches.iter().map(|s| s.id.0.clone()).collect(),
                        chosen: matches[0].id.0.clone(),
                    });
                }
            }
            let resolved = resolve_single(id, *pinning, store)?;
            if resolved.0 != before {
                report.rewrites.push(Rewrite {
                    before,
                    after: resolved.0.clone(),
                    pinning: *pinning,
                });
                *id = resolved;
            }
            Ok(())
        }
        // RemoteStage is resolved at call time over HTTP. Const has no
        // stage ID.
        CompositionNode::RemoteStage { .. } | CompositionNode::Const { .. } => Ok(()),
        CompositionNode::Sequential { stages } => {
            for s in stages {
                resolve_recursive(s, store, report)?;
            }
            Ok(())
        }
        CompositionNode::Parallel { branches } => {
            for b in branches.values_mut() {
                resolve_recursive(b, store, report)?;
            }
            Ok(())
        }
        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => {
            resolve_recursive(predicate, store, report)?;
            resolve_recursive(if_true, store, report)?;
            resolve_recursive(if_false, store, report)?;
            Ok(())
        }
        CompositionNode::Fanout { source, targets } => {
            resolve_recursive(source, store, report)?;
            for t in targets {
                resolve_recursive(t, store, report)?;
            }
            Ok(())
        }
        CompositionNode::Merge { sources, target } => {
            for s in sources {
                resolve_recursive(s, store, report)?;
            }
            resolve_recursive(target, store, report)?;
            Ok(())
        }
        CompositionNode::Retry { stage, .. } => resolve_recursive(stage, store, report),
        CompositionNode::Let { bindings, body } => {
            for b in bindings.values_mut() {
                resolve_recursive(b, store, report)?;
            }
            resolve_recursive(body, store, report)
        }
    }
}

fn resolve_single<S>(id: &StageId, pinning: Pinning, store: &S) -> Result<StageId, ResolutionError>
where
    S: StageStore + ?Sized,
{
    match pinning {
        Pinning::Signature => {
            // First: treat id as a SignatureId and look up the Active
            // implementation.
            let sig = SignatureId(id.0.clone());
            if let Some(stage) = store.get_by_signature(&sig) {
                return Ok(stage.id.clone());
            }
            // Fallback: a name- or prefix-resolver pass may have
            // already rewritten id into an implementation hash. Accept
            // that lookup only if it points at an Active stage.
            if let Ok(Some(stage)) = store.get(id) {
                if matches!(stage.lifecycle, StageLifecycle::Active) {
                    return Ok(stage.id.clone());
                }
            }
            Err(ResolutionError::SignatureNotFound {
                signature_id: id.0.clone(),
            })
        }
        Pinning::Both => match store.get(id) {
            Ok(Some(stage)) => match &stage.lifecycle {
                StageLifecycle::Active => Ok(stage.id.clone()),
                other => Err(ResolutionError::ImplementationNotActive {
                    implementation_id: id.0.clone(),
                    lifecycle: other.clone(),
                }),
            },
            _ => Err(ResolutionError::ImplementationNotFound {
                implementation_id: id.0.clone(),
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noether_core::effects::EffectSet;
    use noether_core::stage::{CostEstimate, SignatureId, Stage, StageSignature};
    use noether_core::types::NType;
    use noether_store::MemoryStore;
    use std::collections::{BTreeMap, BTreeSet};

    fn make_stage(impl_id: &str, sig_id: Option<&str>, lifecycle: StageLifecycle) -> Stage {
        Stage {
            id: StageId(impl_id.into()),
            signature_id: sig_id.map(|s| SignatureId(s.into())),
            signature: StageSignature {
                input: NType::Text,
                output: NType::Number,
                effects: EffectSet::pure(),
                implementation_hash: format!("impl_{impl_id}"),
            },
            capabilities: BTreeSet::new(),
            cost: CostEstimate {
                time_ms_p50: None,
                tokens_est: None,
                memory_mb: None,
            },
            description: "test".into(),
            examples: vec![],
            lifecycle,
            ed25519_signature: None,
            signer_public_key: None,
            implementation_code: None,
            implementation_language: None,
            ui_style: None,
            tags: vec![],
            aliases: vec![],
            name: None,
            properties: vec![],
        }
    }

    fn store_with_impl(impl_id: &str, sig_id: &str) -> MemoryStore {
        let mut store = MemoryStore::new();
        store
            .put(make_stage(impl_id, Some(sig_id), StageLifecycle::Active))
            .unwrap();
        store
    }

    #[test]
    fn signature_pinning_rewrites_to_impl_id() {
        let store = store_with_impl("impl_abc", "sig_xyz");

        let mut node = CompositionNode::Stage {
            id: StageId("sig_xyz".into()),
            pinning: Pinning::Signature,
            config: None,
        };
        let report = resolve_pinning(&mut node, &store).unwrap();

        match &node {
            CompositionNode::Stage { id, pinning, .. } => {
                assert_eq!(id.0, "impl_abc", "id should be rewritten to impl hash");
                // Pinning label is preserved — re-serialisation keeps user intent.
                assert_eq!(*pinning, Pinning::Signature);
            }
            _ => panic!("expected Stage"),
        }
        assert_eq!(report.rewrites.len(), 1);
        assert_eq!(report.rewrites[0].before, "sig_xyz");
        assert_eq!(report.rewrites[0].after, "impl_abc");
    }

    #[test]
    fn both_pinning_accepts_matching_impl_id() {
        let store = store_with_impl("impl_abc", "sig_xyz");

        let mut node = CompositionNode::Stage {
            id: StageId("impl_abc".into()),
            pinning: Pinning::Both,
            config: None,
        };
        let report = resolve_pinning(&mut node, &store).unwrap();

        // No rewrite — it already held a valid impl_id.
        assert!(report.rewrites.is_empty());
    }

    #[test]
    fn both_pinning_rejects_missing_impl() {
        let store = store_with_impl("impl_abc", "sig_xyz");

        let mut node = CompositionNode::Stage {
            id: StageId("impl_does_not_exist".into()),
            pinning: Pinning::Both,
            config: None,
        };
        let err = resolve_pinning(&mut node, &store).unwrap_err();
        assert!(matches!(
            err,
            ResolutionError::ImplementationNotFound { .. }
        ));
    }

    #[test]
    fn both_pinning_rejects_deprecated_impl() {
        let mut store = MemoryStore::new();
        store
            .put(make_stage(
                "impl_old",
                Some("sig_xyz"),
                StageLifecycle::Active,
            ))
            .unwrap();
        // Putting a second Active stage with the same signature_id
        // auto-deprecates impl_old — see M2.3 invariant enforcement in
        // MemoryStore::put.
        store
            .put(make_stage(
                "impl_new",
                Some("sig_xyz"),
                StageLifecycle::Active,
            ))
            .unwrap();
        assert!(matches!(
            store
                .get(&StageId("impl_old".into()))
                .unwrap()
                .unwrap()
                .lifecycle,
            StageLifecycle::Deprecated { .. }
        ));

        let mut node = CompositionNode::Stage {
            id: StageId("impl_old".into()),
            pinning: Pinning::Both,
            config: None,
        };
        let err = resolve_pinning(&mut node, &store).unwrap_err();
        assert!(matches!(
            err,
            ResolutionError::ImplementationNotActive { .. }
        ));
    }

    #[test]
    fn signature_pinning_rejects_missing_signature() {
        let store = store_with_impl("impl_abc", "sig_xyz");

        let mut node = CompositionNode::Stage {
            id: StageId("sig_does_not_exist".into()),
            pinning: Pinning::Signature,
            config: None,
        };
        let err = resolve_pinning(&mut node, &store).unwrap_err();
        assert!(matches!(err, ResolutionError::SignatureNotFound { .. }));
    }

    #[test]
    fn signature_pinning_falls_back_to_impl_id_for_legacy_flows() {
        // A prefix-resolver pass may have rewritten the id into an
        // impl_id already. resolve_pinning accepts that, provided the
        // stage is Active.
        let store = store_with_impl("impl_abc", "sig_xyz");

        let mut node = CompositionNode::Stage {
            id: StageId("impl_abc".into()),
            pinning: Pinning::Signature,
            config: None,
        };
        let report = resolve_pinning(&mut node, &store).unwrap();
        // No rewrite needed — the id already pointed at the right stage.
        assert!(report.rewrites.is_empty());
    }

    #[test]
    fn walks_into_nested_sequential() {
        let store = store_with_impl("impl_abc", "sig_xyz");

        let mut node = CompositionNode::Sequential {
            stages: vec![
                CompositionNode::Stage {
                    id: StageId("sig_xyz".into()),
                    pinning: Pinning::Signature,
                    config: None,
                },
                CompositionNode::Stage {
                    id: StageId("sig_xyz".into()),
                    pinning: Pinning::Signature,
                    config: None,
                },
            ],
        };
        let report = resolve_pinning(&mut node, &store).unwrap();
        assert_eq!(report.rewrites.len(), 2);
    }

    #[test]
    fn walks_into_parallel_branches() {
        let store = store_with_impl("impl_abc", "sig_xyz");

        let mut branches = BTreeMap::new();
        branches.insert(
            "a".into(),
            CompositionNode::Stage {
                id: StageId("sig_xyz".into()),
                pinning: Pinning::Signature,
                config: None,
            },
        );
        branches.insert(
            "b".into(),
            CompositionNode::Stage {
                id: StageId("sig_xyz".into()),
                pinning: Pinning::Signature,
                config: None,
            },
        );
        let mut node = CompositionNode::Parallel { branches };
        let report = resolve_pinning(&mut node, &store).unwrap();
        assert_eq!(report.rewrites.len(), 2);
    }

    #[test]
    fn walks_into_branch_predicate_and_arms() {
        let store = store_with_impl("impl_abc", "sig_xyz");
        let sig = || CompositionNode::Stage {
            id: StageId("sig_xyz".into()),
            pinning: Pinning::Signature,
            config: None,
        };
        let mut node = CompositionNode::Branch {
            predicate: Box::new(sig()),
            if_true: Box::new(sig()),
            if_false: Box::new(sig()),
        };
        let report = resolve_pinning(&mut node, &store).unwrap();
        assert_eq!(report.rewrites.len(), 3);
    }

    #[test]
    fn walks_into_fanout_source_and_targets() {
        let store = store_with_impl("impl_abc", "sig_xyz");
        let sig = || CompositionNode::Stage {
            id: StageId("sig_xyz".into()),
            pinning: Pinning::Signature,
            config: None,
        };
        let mut node = CompositionNode::Fanout {
            source: Box::new(sig()),
            targets: vec![sig(), sig(), sig()],
        };
        let report = resolve_pinning(&mut node, &store).unwrap();
        assert_eq!(report.rewrites.len(), 4);
    }

    #[test]
    fn walks_into_merge_sources_and_target() {
        let store = store_with_impl("impl_abc", "sig_xyz");
        let sig = || CompositionNode::Stage {
            id: StageId("sig_xyz".into()),
            pinning: Pinning::Signature,
            config: None,
        };
        let mut node = CompositionNode::Merge {
            sources: vec![sig(), sig()],
            target: Box::new(sig()),
        };
        let report = resolve_pinning(&mut node, &store).unwrap();
        assert_eq!(report.rewrites.len(), 3);
    }

    #[test]
    fn walks_into_let_bindings_and_body() {
        let store = store_with_impl("impl_abc", "sig_xyz");
        let sig = || CompositionNode::Stage {
            id: StageId("sig_xyz".into()),
            pinning: Pinning::Signature,
            config: None,
        };
        let mut bindings = BTreeMap::new();
        bindings.insert("a".into(), sig());
        bindings.insert("b".into(), sig());
        let mut node = CompositionNode::Let {
            bindings,
            body: Box::new(sig()),
        };
        let report = resolve_pinning(&mut node, &store).unwrap();
        assert_eq!(report.rewrites.len(), 3);
    }

    #[test]
    fn walks_into_retry_inner_stage() {
        let store = store_with_impl("impl_abc", "sig_xyz");
        let mut node = CompositionNode::Retry {
            stage: Box::new(CompositionNode::Stage {
                id: StageId("sig_xyz".into()),
                pinning: Pinning::Signature,
                config: None,
            }),
            max_attempts: 3,
            delay_ms: None,
        };
        let report = resolve_pinning(&mut node, &store).unwrap();
        assert_eq!(report.rewrites.len(), 1);
    }

    #[test]
    fn stops_at_first_error_leaves_partial_rewrites() {
        // First Stage resolves; second does not. Graph is
        // partially-mutated when the pass returns Err.
        let store = store_with_impl("impl_abc", "sig_xyz");

        let mut node = CompositionNode::Sequential {
            stages: vec![
                CompositionNode::Stage {
                    id: StageId("sig_xyz".into()),
                    pinning: Pinning::Signature,
                    config: None,
                },
                CompositionNode::Stage {
                    id: StageId("sig_missing".into()),
                    pinning: Pinning::Signature,
                    config: None,
                },
            ],
        };
        let err = resolve_pinning(&mut node, &store).unwrap_err();
        assert!(matches!(err, ResolutionError::SignatureNotFound { .. }));
        // Verify the first stage was rewritten before the error.
        match &node {
            CompositionNode::Sequential { stages } => match &stages[0] {
                CompositionNode::Stage { id, .. } => assert_eq!(id.0, "impl_abc"),
                _ => panic!(),
            },
            _ => panic!(),
        }
    }

    #[test]
    fn idempotent_on_already_resolved_graph() {
        let store = store_with_impl("impl_abc", "sig_xyz");

        let mut node = CompositionNode::Stage {
            id: StageId("sig_xyz".into()),
            pinning: Pinning::Signature,
            config: None,
        };
        let first = resolve_pinning(&mut node, &store).unwrap();
        let second = resolve_pinning(&mut node, &store).unwrap();
        assert_eq!(first.rewrites.len(), 1);
        // Second pass is a no-op — the id is already an impl_id that
        // the store has, and the signature-lookup fallback finds the
        // same stage.
        assert!(second.rewrites.is_empty());
    }

    #[test]
    fn multi_active_signature_emits_warning() {
        // The store-level "≤1 Active per signature" invariant (M2.3)
        // auto-deprecates duplicate Actives at `put` time, so the
        // resolver's warning path isn't reachable through normal
        // `put` sequences anymore. To exercise it, bypass `put` and
        // mutate the internal HashMap directly (tests-only).
        let mut store = MemoryStore::new();
        store
            .put(make_stage(
                "impl_a",
                Some("shared_sig"),
                StageLifecycle::Active,
            ))
            .unwrap();
        // Inject a second Active duplicate without going through put/upsert,
        // emulating a violated invariant (e.g., a corrupted file store).
        let extra = make_stage("impl_b", Some("shared_sig"), StageLifecycle::Active);
        store.inject_raw_for_testing(extra);

        let mut node = CompositionNode::Stage {
            id: StageId("shared_sig".into()),
            pinning: Pinning::Signature,
            config: None,
        };
        let report = resolve_pinning(&mut node, &store).unwrap();
        assert_eq!(report.warnings.len(), 1);
        let w = &report.warnings[0];
        assert_eq!(w.signature_id, "shared_sig");
        assert_eq!(w.active_implementation_ids.len(), 2);
        // Deterministic pick: lexicographically smallest impl id.
        assert_eq!(w.chosen, "impl_a");
    }
}
