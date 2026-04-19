//! Shared CLI-side graph-resolution preamble.
//!
//! Thin wrapper around the two engine-level passes —
//! [`resolve_pinning`] and [`resolve_deprecated_stages`] — that adds
//! stderr diagnostic emission in the shape CLI users expect. Broker
//! and worker crates call the engine passes directly and route
//! diagnostics through `tracing` instead; this module serves only
//! the CLI binary's stderr audience.
//!
//! `CompositionId` must be computed *before* calling
//! [`resolve_and_emit_diagnostics`]: the M1 "canonical form is identity"
//! contract says the hash reflects the graph as authored, not the
//! post-resolution rewrite. Callers hashing after resolution will
//! observe unstable IDs whenever the store's Active implementation
//! changes.

use noether_engine::lagrange::{
    resolve_deprecated_stages, resolve_pinning, ChainEvent, CompositionGraph,
};
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

    let dep_report = resolve_deprecated_stages(&mut graph.root, store);
    for rw in &dep_report.rewrites {
        eprintln!(
            "Warning: stage {} is deprecated → resolved to successor {}",
            short(&rw.from.0),
            short(&rw.to.0),
        );
    }
    for event in &dep_report.events {
        match event {
            ChainEvent::CycleDetected { stage } => {
                eprintln!(
                    "Warning: deprecation cycle detected at stage {} — \
                     the graph keeps the last distinct id before the \
                     cycle; the store has corrupt deprecation data \
                     and should be repaired.",
                    short(&stage.0)
                );
            }
            ChainEvent::MaxHopsExceeded { stage } => {
                eprintln!(
                    "Warning: deprecation chain at stage {} exceeded \
                     the {}-hop cap — execution continues with the \
                     chain truncated, but the chain should be flattened \
                     in the store.",
                    short(&stage.0),
                    noether_engine::lagrange::MAX_DEPRECATION_HOPS,
                );
            }
        }
    }
    Ok(())
}

/// Return the first 8 bytes of `id`, guarding against UTF-8
/// boundaries even though stage ids are hex in practice. `str::get`
/// returns `None` when the byte index falls mid-codepoint.
fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use noether_core::effects::EffectSet;
    use noether_core::stage::{
        compute_signature_id, compute_stage_id, CostEstimate, SignatureId, Stage, StageId,
        StageLifecycle, StageSignature,
    };
    use noether_core::types::NType;
    use noether_engine::lagrange::{CompositionNode, Pinning};
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
    fn composition_id_is_unstable_across_resolution() {
        // Regression guard for the compose.rs timing fix: if a
        // caller computes `composition_id` AFTER
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
    fn short_does_not_panic_on_non_ascii() {
        // Nit from the round-1 review: the old implementation used a
        // byte-index slice which could panic mid-codepoint. Stage ids
        // are hex in practice, but making the helper UTF-8 safe
        // removes a class of potential bugs.
        assert_eq!(super::short("abcdefghij"), "abcdefgh");
        assert_eq!(super::short("abc"), "abc");
        assert_eq!(super::short(""), "");
        // Mixed-codepoint input: the 8-byte boundary falls inside
        // the é (U+00E9, two UTF-8 bytes). `str::get` returns None
        // there so we fall back to the full string rather than
        // panicking.
        assert_eq!(super::short("abcdefgé"), "abcdefgé");
    }
}
