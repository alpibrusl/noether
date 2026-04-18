use noether_core::stage::StageId;
use noether_core::types::NType;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How a `Stage` reference resolves to a concrete stage in the store.
///
/// Per M2 (v0.6.0), every graph node that names a stage declares its
/// pinning. Default is [`Pinning::Signature`], which picks up
/// implementation bugfixes automatically. [`Pinning::Both`] is the
/// bit-reproducible option — the resolver refuses to substitute a
/// different implementation even if the stored one has been deprecated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Pinning {
    /// Interpret the node's `id` as a [`noether_core::stage::SignatureId`]
    /// and resolve to whichever stage is currently Active with that
    /// signature. Default — matches the v0.6.0 recommendation in
    /// `STABILITY.md`.
    #[default]
    Signature,
    /// Interpret the node's `id` as an implementation-inclusive
    /// [`StageId`] and require an exact match. The resolver refuses to
    /// fall back to any other implementation of the same signature.
    Both,
}

impl Pinning {
    /// Helper for `#[serde(skip_serializing_if = ...)]` — omit the field
    /// from JSON when the value is the default (`Signature`).
    pub fn is_signature(&self) -> bool {
        matches!(self, Pinning::Signature)
    }
}

/// A composition graph node. The core AST for Noether's composition language.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum CompositionNode {
    /// Leaf node: reference to a stage in the store.
    ///
    /// The `id` field is interpreted according to `pinning`:
    /// - [`Pinning::Signature`] (default): `id` is a signature-level
    ///   hash (`SignatureId`) and the resolver returns the currently
    ///   Active implementation with that signature.
    /// - [`Pinning::Both`]: `id` is an implementation-inclusive hash
    ///   (`ImplementationId` / `StageId`) and the resolver requires an
    ///   exact match. No fallback.
    ///
    /// The optional `config` provides static parameter values merged
    /// with the pipeline input before the stage executes.
    ///
    /// Use [`CompositionNode::stage`] to construct a node with default
    /// pinning; use the struct literal only when you need a non-default
    /// pinning or a config.
    Stage {
        id: StageId,
        #[serde(default, skip_serializing_if = "Pinning::is_signature")]
        pinning: Pinning,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<BTreeMap<String, serde_json::Value>>,
    },

    /// Call a remote Noether API endpoint over HTTP.
    ///
    /// The declared `input` and `output` types are verified by the type checker
    /// at build time — the remote server does not need to be running during
    /// `noether build`. In native builds, execution uses reqwest. In browser
    /// builds, the JS runtime makes a `fetch()` call.
    RemoteStage {
        /// URL of the remote Noether API (e.g. "http://localhost:8080")
        url: String,
        /// Declared input type — what this node accepts from the pipeline
        input: NType,
        /// Declared output type — what this node returns to the pipeline
        output: NType,
    },

    /// Emits a constant JSON value, ignoring its input entirely.
    /// Used to inject literal strings, numbers, or objects into a pipeline.
    Const { value: serde_json::Value },

    /// A >> B >> C: output of each stage feeds the next.
    Sequential { stages: Vec<CompositionNode> },

    /// Execute branches concurrently, merge outputs into a Record keyed by
    /// branch name. Each branch receives `input[branch_name]` if the input is
    /// a Record containing that key; otherwise it receives the full input.
    /// `Const` branches ignore their input entirely — use them for literals.
    Parallel {
        branches: BTreeMap<String, CompositionNode>,
    },

    /// Conditional routing based on a predicate stage.
    Branch {
        predicate: Box<CompositionNode>,
        if_true: Box<CompositionNode>,
        if_false: Box<CompositionNode>,
    },

    /// Source output sent to all targets concurrently.
    Fanout {
        source: Box<CompositionNode>,
        targets: Vec<CompositionNode>,
    },

    /// Multiple sources merge into a single target.
    Merge {
        sources: Vec<CompositionNode>,
        target: Box<CompositionNode>,
    },

    /// Retry a stage up to max_attempts times on failure.
    Retry {
        stage: Box<CompositionNode>,
        max_attempts: u32,
        delay_ms: Option<u64>,
    },

    /// Bind named intermediate computations and reference them in `body`.
    ///
    /// Each binding sub-node receives the **outer Let input** (the same value
    /// passed to the Let node). After all bindings have produced a value, the
    /// `body` runs against an augmented input record:
    ///
    ///   `{ ...outer-input fields, <binding-name>: <binding-output>, ... }`
    ///
    /// Bindings with the same name as an outer-input field shadow it. This
    /// makes it possible to carry original-input fields into stages later in a
    /// Sequential pipeline — the canonical example is scan → hash → diff,
    /// where `diff` needs `state_path` from the original input but `hash`
    /// would otherwise erase it.
    ///
    /// All bindings are scheduled concurrently — there are no inter-binding
    /// references. If you need a binding to depend on another, wrap it in a
    /// nested `Sequential`.
    Let {
        bindings: BTreeMap<String, CompositionNode>,
        body: Box<CompositionNode>,
    },
}

impl CompositionNode {
    /// Build a `Stage` node with default pinning (`Signature`) and no
    /// config. Use this in place of the struct literal when you don't
    /// need to set pinning or config explicitly.
    pub fn stage(id: impl Into<String>) -> Self {
        Self::Stage {
            id: StageId(id.into()),
            pinning: Pinning::Signature,
            config: None,
        }
    }

    /// Build a `Stage` node with an explicit `Both` pinning — the
    /// resolver will require the exact implementation named by `id`.
    pub fn stage_pinned(id: impl Into<String>) -> Self {
        Self::Stage {
            id: StageId(id.into()),
            pinning: Pinning::Both,
            config: None,
        }
    }
}

/// Resolve a `CompositionNode::Stage` reference to a concrete stage in
/// the store, respecting the node's pinning.
///
/// - [`Pinning::Signature`]: tries `store.get_by_signature` first; on
///   miss, falls back to `store.get` (in case `id` is actually an
///   implementation hash left over from a name-resolver pass).
/// - [`Pinning::Both`]: requires `store.get` to return an exact match.
///   No fallback to signature-level resolution.
pub fn resolve_stage_ref<'a, S>(
    id: &StageId,
    pinning: Pinning,
    store: &'a S,
) -> Option<&'a noether_core::stage::Stage>
where
    S: noether_store::StageStore + ?Sized,
{
    use noether_core::stage::SignatureId;
    match pinning {
        Pinning::Signature => {
            let sig = SignatureId(id.0.clone());
            if let Some(stage) = store.get_by_signature(&sig) {
                return Some(stage);
            }
            // Fallback: a name-based or prefix-based resolver may have
            // rewritten `id` to an implementation_id before we got here.
            store.get(id).ok().flatten()
        }
        Pinning::Both => store.get(id).ok().flatten(),
    }
}

/// A complete composition graph with metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompositionGraph {
    pub description: String,
    pub root: CompositionNode,
    pub version: String,
}

impl CompositionGraph {
    pub fn new(description: impl Into<String>, root: CompositionNode) -> Self {
        Self {
            description: description.into(),
            root,
            version: "0.1.0".into(),
        }
    }
}

/// Collect all StageIds referenced in a composition node.
pub fn collect_stage_ids(node: &CompositionNode) -> Vec<&StageId> {
    let mut ids = Vec::new();
    collect_ids_recursive(node, &mut ids);
    ids
}

fn collect_ids_recursive<'a>(node: &'a CompositionNode, ids: &mut Vec<&'a StageId>) {
    match node {
        CompositionNode::Stage { id, .. } => ids.push(id),
        CompositionNode::RemoteStage { .. } => {} // no local stage ID; URL is resolved at runtime
        CompositionNode::Const { .. } => {}       // no stage IDs in a constant
        CompositionNode::Sequential { stages } => {
            for s in stages {
                collect_ids_recursive(s, ids);
            }
        }
        CompositionNode::Parallel { branches } => {
            for b in branches.values() {
                collect_ids_recursive(b, ids);
            }
        }
        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => {
            collect_ids_recursive(predicate, ids);
            collect_ids_recursive(if_true, ids);
            collect_ids_recursive(if_false, ids);
        }
        CompositionNode::Fanout { source, targets } => {
            collect_ids_recursive(source, ids);
            for t in targets {
                collect_ids_recursive(t, ids);
            }
        }
        CompositionNode::Merge { sources, target } => {
            for s in sources {
                collect_ids_recursive(s, ids);
            }
            collect_ids_recursive(target, ids);
        }
        CompositionNode::Retry { stage, .. } => {
            collect_ids_recursive(stage, ids);
        }
        CompositionNode::Let { bindings, body } => {
            for b in bindings.values() {
                collect_ids_recursive(b, ids);
            }
            collect_ids_recursive(body, ids);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stage(id: &str) -> CompositionNode {
        CompositionNode::Stage {
            id: StageId(id.into()),
            pinning: Pinning::Signature,
            config: None,
        }
    }

    #[test]
    fn serde_stage_round_trip() {
        let node = stage("abc123");
        let json = serde_json::to_string(&node).unwrap();
        let parsed: CompositionNode = serde_json::from_str(&json).unwrap();
        assert_eq!(node, parsed);
    }

    #[test]
    fn serde_sequential() {
        let node = CompositionNode::Sequential {
            stages: vec![stage("a"), stage("b"), stage("c")],
        };
        let json = serde_json::to_string_pretty(&node).unwrap();
        let parsed: CompositionNode = serde_json::from_str(&json).unwrap();
        assert_eq!(node, parsed);
    }

    #[test]
    fn serde_parallel() {
        let mut branches = BTreeMap::new();
        branches.insert("left".into(), stage("a"));
        branches.insert("right".into(), stage("b"));
        let node = CompositionNode::Parallel { branches };
        let json = serde_json::to_string(&node).unwrap();
        let parsed: CompositionNode = serde_json::from_str(&json).unwrap();
        assert_eq!(node, parsed);
    }

    #[test]
    fn serde_branch() {
        let node = CompositionNode::Branch {
            predicate: Box::new(stage("pred")),
            if_true: Box::new(stage("yes")),
            if_false: Box::new(stage("no")),
        };
        let json = serde_json::to_string(&node).unwrap();
        let parsed: CompositionNode = serde_json::from_str(&json).unwrap();
        assert_eq!(node, parsed);
    }

    #[test]
    fn serde_retry() {
        let node = CompositionNode::Retry {
            stage: Box::new(stage("fallible")),
            max_attempts: 3,
            delay_ms: Some(500),
        };
        let json = serde_json::to_string(&node).unwrap();
        let parsed: CompositionNode = serde_json::from_str(&json).unwrap();
        assert_eq!(node, parsed);
    }

    #[test]
    fn serde_full_graph() {
        let graph = CompositionGraph::new(
            "test pipeline",
            CompositionNode::Sequential {
                stages: vec![stage("parse"), stage("transform"), stage("output")],
            },
        );
        let json = serde_json::to_string_pretty(&graph).unwrap();
        let parsed: CompositionGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(graph, parsed);
    }

    #[test]
    fn serde_nested_composition() {
        let node = CompositionNode::Sequential {
            stages: vec![
                stage("input"),
                CompositionNode::Retry {
                    stage: Box::new(CompositionNode::Sequential {
                        stages: vec![stage("a"), stage("b")],
                    }),
                    max_attempts: 2,
                    delay_ms: None,
                },
                stage("output"),
            ],
        };
        let json = serde_json::to_string(&node).unwrap();
        let parsed: CompositionNode = serde_json::from_str(&json).unwrap();
        assert_eq!(node, parsed);
    }

    #[test]
    fn collect_stage_ids_finds_all() {
        let node = CompositionNode::Sequential {
            stages: vec![
                stage("a"),
                CompositionNode::Parallel {
                    branches: BTreeMap::from([("x".into(), stage("b")), ("y".into(), stage("c"))]),
                },
                stage("d"),
            ],
        };
        let ids = collect_stage_ids(&node);
        assert_eq!(ids.len(), 4);
    }

    #[test]
    fn json_format_is_tagged() {
        let node = stage("abc123");
        let v: serde_json::Value = serde_json::to_value(&node).unwrap();
        assert_eq!(v["op"], json!("Stage"));
        assert_eq!(v["id"], json!("abc123"));
    }

    #[test]
    fn default_pinning_omitted_from_json() {
        // A Stage node with the default Signature pinning should not
        // emit `"pinning"` in the JSON — keeps the wire format small
        // and backwards-compatible with readers that only expect `id`.
        let node = stage("abc123");
        let v: serde_json::Value = serde_json::to_value(&node).unwrap();
        assert!(
            v.get("pinning").is_none(),
            "default Signature pinning should be omitted from JSON, got: {v}"
        );
    }

    #[test]
    fn both_pinning_serialises_explicitly() {
        let node = CompositionNode::stage_pinned("impl_abc");
        let v: serde_json::Value = serde_json::to_value(&node).unwrap();
        assert_eq!(v["pinning"], json!("both"));
    }

    #[test]
    fn legacy_graph_without_pinning_deserialises() {
        // v0.5.x graphs had only `{"op": "Stage", "id": "..."}`. The
        // new `pinning` field defaults to Signature when the legacy
        // JSON is loaded.
        let legacy = json!({
            "op": "Stage",
            "id": "legacy_hash",
        });
        let parsed: CompositionNode = serde_json::from_value(legacy).unwrap();
        match parsed {
            CompositionNode::Stage { id, pinning, .. } => {
                assert_eq!(id.0, "legacy_hash");
                assert_eq!(pinning, Pinning::Signature);
            }
            _ => panic!("expected Stage variant"),
        }
    }

    #[test]
    fn explicit_both_pinning_deserialises() {
        let pinned = json!({
            "op": "Stage",
            "id": "impl_xyz",
            "pinning": "both",
        });
        let parsed: CompositionNode = serde_json::from_value(pinned).unwrap();
        match parsed {
            CompositionNode::Stage { pinning, .. } => {
                assert_eq!(pinning, Pinning::Both);
            }
            _ => panic!("expected Stage variant"),
        }
    }

    #[test]
    fn serde_remote_stage_round_trip() {
        let node = CompositionNode::RemoteStage {
            url: "http://localhost:8080".into(),
            input: NType::record([("count", NType::Number)]),
            output: NType::VNode,
        };
        let json = serde_json::to_string(&node).unwrap();
        let parsed: CompositionNode = serde_json::from_str(&json).unwrap();
        assert_eq!(node, parsed);
    }

    #[test]
    fn remote_stage_json_shape() {
        let node = CompositionNode::RemoteStage {
            url: "http://api.example.com".into(),
            input: NType::Text,
            output: NType::Number,
        };
        let v: serde_json::Value = serde_json::to_value(&node).unwrap();
        assert_eq!(v["op"], json!("RemoteStage"));
        assert_eq!(v["url"], json!("http://api.example.com"));
        assert!(v["input"].is_object());
        assert!(v["output"].is_object());
    }

    #[test]
    fn collect_stage_ids_skips_remote_stage() {
        let node = CompositionNode::Sequential {
            stages: vec![
                stage("local-a"),
                CompositionNode::RemoteStage {
                    url: "http://remote".into(),
                    input: NType::Text,
                    output: NType::Text,
                },
                stage("local-b"),
            ],
        };
        let ids = collect_stage_ids(&node);
        // Only local stages contribute IDs
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0].0, "local-a");
        assert_eq!(ids[1].0, "local-b");
    }

    #[test]
    fn remote_stage_in_full_graph_serde() {
        let graph = CompositionGraph::new(
            "full-stack pipeline",
            CompositionNode::Sequential {
                stages: vec![
                    CompositionNode::RemoteStage {
                        url: "http://api:8080".into(),
                        input: NType::record([("query", NType::Text)]),
                        output: NType::List(Box::new(NType::Text)),
                    },
                    stage("render"),
                ],
            },
        );
        let json = serde_json::to_string_pretty(&graph).unwrap();
        let parsed: CompositionGraph = serde_json::from_str(&json).unwrap();
        assert_eq!(graph, parsed);
    }
}
