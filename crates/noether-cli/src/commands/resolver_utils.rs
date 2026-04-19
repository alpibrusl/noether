//! Shared graph-resolution preamble for every CLI entry point that
//! ingests a Lagrange graph.
//!
//! The pass is split into two steps by necessity:
//!
//! * [`resolve_pinning`] — signature/canonical → concrete implementation
//!   rewrites, surfacing invariant-violation warnings from the store.
//! * [`resolve_deprecated_stages`] — follow `successor_id` chains so
//!   graphs that still reference retired implementations keep running.
//!
//! Both used to live inline in `run.rs`. PR #32 extended resolution to
//! `compose`, `serve`, `build`, `build_browser`, scheduler, broker, and
//! grid-worker. Duplicating the diagnostic boilerplate at every site
//! would drift — hence this helper.
//!
//! `CompositionId` must be computed *before* calling
//! [`resolve_and_emit_diagnostics`]: the M1 "canonical form is identity"
//! contract says the hash reflects the graph as authored, not the
//! post-resolution rewrite. Callers hashing after resolution will
//! observe unstable IDs whenever the store's Active implementation
//! changes.

use noether_core::stage::{StageId, StageLifecycle};
use noether_engine::lagrange::{resolve_pinning, CompositionGraph, CompositionNode};
use noether_store::StageStore;

/// Run the post-parse resolution passes on `graph` and print any
/// diagnostics to stderr. Returns `Err(message)` on fatal resolution
/// errors; the caller decides whether to exit or recover.
pub fn resolve_and_emit_diagnostics(
    graph: &mut CompositionGraph,
    store: &dyn StageStore,
) -> Result<(), String> {
    let report =
        resolve_pinning(&mut graph.root, store).map_err(|e| format!("pinning resolution: {e}"))?;

    for rw in &report.rewrites {
        eprintln!(
            "Info: {:?}-pinned stage {} resolved to {}",
            rw.pinning,
            short(&rw.before),
            short(&rw.after),
        );
    }
    for w in &report.warnings {
        eprintln!(
            "Warning: signature {} has {} Active implementations ({}) — \
             picked {} deterministically, but the store's ≤1-Active-per-\
             signature invariant is violated. Deprecate the duplicates \
             via `noether stage activate` / `noether store retro`.",
            short(&w.signature_id),
            w.active_implementation_ids.len(),
            w.active_implementation_ids
                .iter()
                .map(|id| short(id).to_string())
                .collect::<Vec<_>>()
                .join(", "),
            short(&w.chosen),
        );
    }

    let rewrites = resolve_deprecated_stages(&mut graph.root, store);
    for (old, new) in &rewrites {
        eprintln!(
            "Warning: stage {} is deprecated → resolved to successor {}",
            short(&old.0),
            short(&new.0),
        );
    }
    Ok(())
}

fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

/// Walk the composition graph and replace any deprecated stage IDs with
/// their successor, following the chain (up to 10 hops to prevent cycles).
pub fn resolve_deprecated_stages(
    node: &mut CompositionNode,
    store: &dyn StageStore,
) -> Vec<(StageId, StageId)> {
    let mut rewrites = Vec::new();

    match node {
        CompositionNode::Stage { id, .. } => {
            let mut current = id.clone();
            for _ in 0..10 {
                match store.get(&current) {
                    Ok(Some(stage)) => {
                        if let StageLifecycle::Deprecated { successor_id } = &stage.lifecycle {
                            let old = current.clone();
                            current = successor_id.clone();
                            rewrites.push((old, current.clone()));
                        } else {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            if current != *id {
                *id = current;
            }
        }
        CompositionNode::Sequential { stages } => {
            for s in stages {
                rewrites.extend(resolve_deprecated_stages(s, store));
            }
        }
        CompositionNode::Parallel { branches } => {
            for (_, branch) in branches.iter_mut() {
                rewrites.extend(resolve_deprecated_stages(branch, store));
            }
        }
        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => {
            rewrites.extend(resolve_deprecated_stages(predicate, store));
            rewrites.extend(resolve_deprecated_stages(if_true, store));
            rewrites.extend(resolve_deprecated_stages(if_false, store));
        }
        CompositionNode::Retry { stage, .. } => {
            rewrites.extend(resolve_deprecated_stages(stage, store));
        }
        CompositionNode::Fanout { source, targets } => {
            rewrites.extend(resolve_deprecated_stages(source, store));
            for t in targets {
                rewrites.extend(resolve_deprecated_stages(t, store));
            }
        }
        CompositionNode::Merge { sources, target } => {
            for s in sources {
                rewrites.extend(resolve_deprecated_stages(s, store));
            }
            rewrites.extend(resolve_deprecated_stages(target, store));
        }
        CompositionNode::Const { .. } | CompositionNode::RemoteStage { .. } => {}
        CompositionNode::Let { bindings, body } => {
            for b in bindings.values_mut() {
                rewrites.extend(resolve_deprecated_stages(b, store));
            }
            rewrites.extend(resolve_deprecated_stages(body, store));
        }
    }

    rewrites
}

#[cfg(test)]
mod tests {
    use super::*;
    use noether_core::effects::EffectSet;
    use noether_core::stage::{
        compute_signature_id, compute_stage_id, CostEstimate, SignatureId, Stage, StageSignature,
    };
    use noether_core::types::NType;
    use noether_engine::lagrange::{CompositionGraph, CompositionNode, Pinning};
    use noether_store::MemoryStore;
    use std::collections::BTreeSet;

    fn make_stage(name: &str, impl_hash: &str, lifecycle: StageLifecycle) -> Stage {
        let signature = StageSignature {
            input: NType::Text,
            output: NType::Text,
            effects: EffectSet::pure(),
            implementation_hash: impl_hash.into(),
        };
        let id = compute_stage_id(name, &signature).unwrap();
        let signature_id = compute_signature_id(
            name,
            &signature.input,
            &signature.output,
            &signature.effects,
        )
        .unwrap();
        Stage {
            id,
            signature_id: Some(signature_id),
            signature,
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
            name: Some(name.into()),
            properties: Vec::new(),
        }
    }

    #[test]
    fn resolve_and_emit_handles_signature_pinning() {
        // Signature-pinned node must be rewritten to carry the
        // implementation id so downstream passes can `store.get(id)`.
        let mut store = MemoryStore::new();
        let stage = make_stage("sigpin_stage", "impl_hash_a", StageLifecycle::Active);
        let sig_id: SignatureId = stage.signature_id.clone().unwrap();
        let expected_impl = stage.id.clone();
        store.put(stage).unwrap();

        let mut graph = CompositionGraph::new(
            "t",
            CompositionNode::Stage {
                id: StageId(sig_id.0.clone()),
                pinning: Pinning::Signature,
                config: None,
            },
        );

        resolve_and_emit_diagnostics(&mut graph, &store).unwrap();

        match &graph.root {
            CompositionNode::Stage { id, .. } => {
                assert_eq!(*id, expected_impl, "signature pin must rewrite to impl id");
            }
            other => panic!("expected Stage, got {other:?}"),
        }
    }

    #[test]
    fn resolve_deprecated_follows_successor() {
        // old stage lives in store as Deprecated pointing at new's id.
        // Graph references old; walker must rewrite to new.
        let mut store = MemoryStore::new();
        let new_stage = make_stage("newer", "impl_new", StageLifecycle::Active);
        let new_id = new_stage.id.clone();
        store.put(new_stage).unwrap();
        let old_stage = Stage {
            // Manually stamp Deprecated lifecycle — we can't go through
            // `update_lifecycle(Draft→Deprecated)` without a real Draft.
            lifecycle: StageLifecycle::Deprecated {
                successor_id: new_id.clone(),
            },
            ..make_stage("older", "impl_old", StageLifecycle::Active)
        };
        let old_id = old_stage.id.clone();
        // Store lifecycle is Draft → Active → Deprecated. Walk it.
        let mut draft = old_stage.clone();
        draft.lifecycle = StageLifecycle::Draft;
        store.put(draft).unwrap();
        store
            .update_lifecycle(&old_id, StageLifecycle::Active)
            .unwrap();
        store
            .update_lifecycle(
                &old_id,
                StageLifecycle::Deprecated {
                    successor_id: new_id.clone(),
                },
            )
            .unwrap();

        let mut root = CompositionNode::Stage {
            id: old_id.clone(),
            pinning: Pinning::Both,
            config: None,
        };
        let rewrites = resolve_deprecated_stages(&mut root, &store);
        assert_eq!(rewrites, vec![(old_id, new_id.clone())]);
        match root {
            CompositionNode::Stage { id, .. } => assert_eq!(id, new_id),
            _ => unreachable!(),
        }
    }

    #[test]
    fn composition_id_is_unstable_across_resolution() {
        // This test is the regression guard for the compose.rs timing
        // fix: if a caller computes `composition_id` AFTER
        // `resolve_and_emit_diagnostics`, the same source graph will
        // produce different ids as the store's Active impl rotates.
        // The M1/#28 contract says the id must reflect the graph as
        // authored (pre-resolution), so we assert here that the two
        // ids differ — any call site that computes after resolution
        // is silently violating the contract.
        use noether_engine::lagrange::compute_composition_id;

        let mut store = MemoryStore::new();
        let stage = make_stage("sig_stable", "impl_x", StageLifecycle::Active);
        let sig_id = stage.signature_id.clone().unwrap();
        store.put(stage).unwrap();

        let build_graph = || {
            CompositionGraph::new(
                "t",
                CompositionNode::Stage {
                    id: StageId(sig_id.0.clone()),
                    pinning: Pinning::Signature,
                    config: None,
                },
            )
        };

        let pre = build_graph();
        let pre_id = compute_composition_id(&pre).unwrap();

        let mut post = build_graph();
        resolve_and_emit_diagnostics(&mut post, &store).unwrap();
        let post_id = compute_composition_id(&post).unwrap();

        assert_ne!(
            pre_id, post_id,
            "compose.rs must compute composition_id on the pre-resolution \
             graph — if these ids match, the resolver is a no-op here and \
             this test needs a more aggressive rewrite, not a silent fix"
        );
    }

    #[test]
    fn resolve_deprecated_is_noop_on_active_stage() {
        // Regression guard: a graph referencing an Active stage must
        // not be mutated by the deprecation walker.
        let mut store = MemoryStore::new();
        let stage = make_stage("active", "impl_a", StageLifecycle::Active);
        let id = stage.id.clone();
        store.put(stage).unwrap();

        let mut root = CompositionNode::Stage {
            id: id.clone(),
            pinning: Pinning::Both,
            config: None,
        };
        let rewrites = resolve_deprecated_stages(&mut root, &store);
        assert!(rewrites.is_empty());
        match root {
            CompositionNode::Stage { id: out, .. } => assert_eq!(out, id),
            _ => unreachable!(),
        }
    }
}
