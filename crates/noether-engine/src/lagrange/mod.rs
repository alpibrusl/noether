mod ast;
pub mod canonical;

pub use ast::{collect_stage_ids, CompositionGraph, CompositionNode};
pub use canonical::canonicalise;

use noether_core::stage::{Stage, StageId};
use noether_store::StageStore;
use sha2::{Digest, Sha256};

/// Parse a Lagrange JSON string into a CompositionGraph.
pub fn parse_graph(json: &str) -> Result<CompositionGraph, serde_json::Error> {
    serde_json::from_str(json)
}

/// Errors raised by `resolve_stage_prefixes` when an ID in the graph cannot
/// be uniquely resolved against the store.
#[derive(Debug, Clone)]
pub enum PrefixResolutionError {
    /// The prefix did not match any stage in the store.
    NotFound { prefix: String },
    /// The prefix matched multiple stages — author must use a longer prefix.
    Ambiguous {
        prefix: String,
        matches: Vec<String>,
    },
}

impl std::fmt::Display for PrefixResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound { prefix } => {
                write!(f, "no stage in store matches prefix '{prefix}'")
            }
            Self::Ambiguous { prefix, matches } => {
                write!(
                    f,
                    "stage prefix '{prefix}' is ambiguous; matches {} stages — \
                     use a longer prefix. First few: {}",
                    matches.len(),
                    matches
                        .iter()
                        .take(3)
                        .map(|s| &s[..16.min(s.len())])
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }
        }
    }
}

impl std::error::Error for PrefixResolutionError {}

/// Snapshot of the store's identity metadata, used to resolve composition
/// references without holding the store reference across nested walks.
struct ResolverIndex {
    /// Every stage ID currently in the store, for prefix matching.
    all_ids: Vec<String>,
    /// Name → (active_ids, non_active_ids). A stage-ref string is tried as
    /// a name lookup when it doesn't match any ID prefix. Active matches
    /// win unconditionally; non-active are only considered when no Active
    /// candidate exists.
    by_name: std::collections::HashMap<String, (Vec<String>, Vec<String>)>,
}

/// Walk a composition graph and replace any stage IDs that are unique
/// prefixes — or human-authored names — of a real stage in the store with
/// their full 64-character IDs.
///
/// Resolution order for `{"op": "Stage", "id": "<ref>"}`:
///
///   1. `<ref>` is an exact full-length ID → pass through.
///   2. `<ref>` is a unique hex prefix of one stored ID → use it.
///   3. `<ref>` matches exactly one stored stage's `name` field — with
///      Active preferred over Draft/Deprecated — → use that stage's ID.
///   4. Otherwise error with `NotFound` or `Ambiguous`.
///
/// Hand-authored graphs can therefore reference stages by the name from
/// their spec (`{"id": "volvo_map"}`) without juggling 8-char prefixes.
pub fn resolve_stage_prefixes(
    node: &mut CompositionNode,
    store: &(impl StageStore + ?Sized),
) -> Result<(), PrefixResolutionError> {
    let stages: Vec<&Stage> = store.list(None);
    let mut by_name: std::collections::HashMap<String, (Vec<String>, Vec<String>)> =
        std::collections::HashMap::new();
    for s in &stages {
        if let Some(name) = &s.name {
            let entry = by_name.entry(name.clone()).or_default();
            if matches!(s.lifecycle, noether_core::stage::StageLifecycle::Active) {
                entry.0.push(s.id.0.clone());
            } else {
                entry.1.push(s.id.0.clone());
            }
        }
    }
    let index = ResolverIndex {
        all_ids: stages.iter().map(|s| s.id.0.clone()).collect(),
        by_name,
    };
    resolve_in_node(node, &index)
}

fn resolve_in_node(
    node: &mut CompositionNode,
    index: &ResolverIndex,
) -> Result<(), PrefixResolutionError> {
    match node {
        CompositionNode::Stage { id, .. } => {
            // 1. Exact full-length ID.
            if index.all_ids.iter().any(|i| i == &id.0) {
                return Ok(());
            }
            // 2. Hex prefix match. Guarded by "looks hex-ish" so a name
            //    that happens to start with hex chars doesn't block name
            //    lookup (e.g. the name `fade_in` would prefix-match
            //    "fade…" stage IDs).
            let looks_like_prefix = !id.0.is_empty() && id.0.chars().all(|c| c.is_ascii_hexdigit());
            if looks_like_prefix {
                let matches: Vec<&String> = index
                    .all_ids
                    .iter()
                    .filter(|i| i.starts_with(&id.0))
                    .collect();
                match matches.len() {
                    0 => {}
                    1 => {
                        *id = StageId(matches[0].clone());
                        return Ok(());
                    }
                    _ => {
                        return Err(PrefixResolutionError::Ambiguous {
                            prefix: id.0.clone(),
                            matches: matches.into_iter().cloned().collect(),
                        })
                    }
                }
            }
            // 3. Name lookup — Active preferred, then fall back.
            if let Some((active, other)) = index.by_name.get(&id.0) {
                let candidates = if !active.is_empty() { active } else { other };
                match candidates.len() {
                    0 => {}
                    1 => {
                        *id = StageId(candidates[0].clone());
                        return Ok(());
                    }
                    _ => {
                        return Err(PrefixResolutionError::Ambiguous {
                            prefix: id.0.clone(),
                            matches: candidates.clone(),
                        })
                    }
                }
            }
            Err(PrefixResolutionError::NotFound {
                prefix: id.0.clone(),
            })
        }
        CompositionNode::RemoteStage { .. } | CompositionNode::Const { .. } => Ok(()),
        CompositionNode::Sequential { stages } => {
            for s in stages {
                resolve_in_node(s, index)?;
            }
            Ok(())
        }
        CompositionNode::Parallel { branches } => {
            for b in branches.values_mut() {
                resolve_in_node(b, index)?;
            }
            Ok(())
        }
        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => {
            resolve_in_node(predicate, index)?;
            resolve_in_node(if_true, index)?;
            resolve_in_node(if_false, index)
        }
        CompositionNode::Fanout { source, targets } => {
            resolve_in_node(source, index)?;
            for t in targets {
                resolve_in_node(t, index)?;
            }
            Ok(())
        }
        CompositionNode::Merge { sources, target } => {
            for s in sources {
                resolve_in_node(s, index)?;
            }
            resolve_in_node(target, index)
        }
        CompositionNode::Retry { stage, .. } => resolve_in_node(stage, index),
        CompositionNode::Let { bindings, body } => {
            for b in bindings.values_mut() {
                resolve_in_node(b, index)?;
            }
            resolve_in_node(body, index)
        }
    }
}

/// Serialize a CompositionGraph to pretty-printed JSON.
pub fn serialize_graph(graph: &CompositionGraph) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(graph)
}

/// Compute a deterministic composition ID.
///
/// The hash is taken over the **canonical form of the graph's root node**,
/// serialised via JCS (RFC 8785). Metadata fields (`description`,
/// `version`) do not contribute to the ID: cosmetic edits should not
/// shift a composition's identity, and equivalent graphs with different
/// surface syntax (nested Sequentials, permuted Parallel branches,
/// collapsed Retry layers, etc.) must produce identical IDs.
///
/// The canonicalisation rules are documented in
/// `docs/architecture/semantics.md` and implemented in
/// `crate::lagrange::canonical`.
///
/// **Compatibility note.** This changes composition IDs from the
/// pre-0.5 byte-of-the-whole-graph hash. Migration guidance lives in
/// the 0.5.0 release notes.
pub fn compute_composition_id(graph: &CompositionGraph) -> Result<String, serde_json::Error> {
    let canonical = canonicalise(&graph.root);
    let bytes = serde_jcs::to_vec(&canonical)?;
    let hash = Sha256::digest(&bytes);
    Ok(hex::encode(hash))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lagrange::ast::CompositionNode;
    use noether_core::stage::StageId;

    #[test]
    fn parse_and_serialize_round_trip() {
        let graph = CompositionGraph::new(
            "test",
            CompositionNode::Stage {
                id: StageId("abc".into()),
                config: None,
            },
        );
        let json = serialize_graph(&graph).unwrap();
        let parsed = parse_graph(&json).unwrap();
        assert_eq!(graph, parsed);
    }

    #[test]
    fn resolver_resolves_by_name_when_no_prefix_match() {
        use noether_core::capability::Capability;
        use noether_core::effects::EffectSet;
        use noether_core::stage::{CostEstimate, Stage, StageLifecycle, StageSignature};
        use noether_core::types::NType;
        use noether_store::MemoryStore;
        use noether_store::StageStore as _;
        use std::collections::BTreeSet;

        let sig = StageSignature {
            input: NType::Text,
            output: NType::Number,
            effects: EffectSet::pure(),
            implementation_hash: "hash".into(),
        };
        let stage = Stage {
            id: StageId("ffaa1122deadbeef0000000000000000000000000000000000000000000000ff".into()),
            signature_id: None,
            signature: sig,
            capabilities: BTreeSet::<Capability>::new(),
            cost: CostEstimate {
                time_ms_p50: None,
                tokens_est: None,
                memory_mb: None,
            },
            description: "stub".into(),
            examples: vec![],
            lifecycle: StageLifecycle::Active,
            ed25519_signature: None,
            signer_public_key: None,
            implementation_code: None,
            implementation_language: None,
            ui_style: None,
            tags: vec![],
            aliases: vec![],
            name: Some("volvo_map".into()),
        };
        let mut store = MemoryStore::new();
        store.put(stage.clone()).unwrap();

        let mut node = CompositionNode::Stage {
            id: StageId("volvo_map".into()),
            config: None,
        };
        resolve_stage_prefixes(&mut node, &store).unwrap();
        match node {
            CompositionNode::Stage { id, .. } => assert_eq!(id.0, stage.id.0),
            _ => panic!("expected Stage node"),
        }
    }

    #[test]
    fn resolver_prefers_active_when_duplicate_names() {
        use noether_core::capability::Capability;
        use noether_core::effects::EffectSet;
        use noether_core::stage::{CostEstimate, Stage, StageLifecycle, StageSignature};
        use noether_core::types::NType;
        use noether_store::MemoryStore;
        use noether_store::StageStore as _;
        use std::collections::BTreeSet;

        fn mk(id_hex: &str, lifecycle: StageLifecycle, hash: &str) -> Stage {
            Stage {
                id: StageId(id_hex.into()),
                signature_id: None,
                signature: StageSignature {
                    input: NType::Text,
                    output: NType::Number,
                    effects: EffectSet::pure(),
                    implementation_hash: hash.into(),
                },
                capabilities: BTreeSet::<Capability>::new(),
                cost: CostEstimate {
                    time_ms_p50: None,
                    tokens_est: None,
                    memory_mb: None,
                },
                description: "stub".into(),
                examples: vec![],
                lifecycle,
                ed25519_signature: None,
                signer_public_key: None,
                implementation_code: None,
                implementation_language: None,
                ui_style: None,
                tags: vec![],
                aliases: vec![],
                name: Some("shared".into()),
            }
        }

        let draft = mk(
            "1111111111111111111111111111111111111111111111111111111111111111",
            StageLifecycle::Draft,
            "h1",
        );
        let active = mk(
            "2222222222222222222222222222222222222222222222222222222222222222",
            StageLifecycle::Active,
            "h2",
        );
        let mut store = MemoryStore::new();
        store.put(draft).unwrap();
        store.put(active.clone()).unwrap();

        let mut node = CompositionNode::Stage {
            id: StageId("shared".into()),
            config: None,
        };
        resolve_stage_prefixes(&mut node, &store).unwrap();
        match node {
            CompositionNode::Stage { id, .. } => assert_eq!(id.0, active.id.0),
            _ => panic!("expected Stage node"),
        }
    }

    #[test]
    fn composition_id_is_deterministic() {
        let graph = CompositionGraph::new(
            "test",
            CompositionNode::Stage {
                id: StageId("abc".into()),
                config: None,
            },
        );
        let id1 = compute_composition_id(&graph).unwrap();
        let id2 = compute_composition_id(&graph).unwrap();
        assert_eq!(id1, id2);
        assert_eq!(id1.len(), 64);
    }
}
