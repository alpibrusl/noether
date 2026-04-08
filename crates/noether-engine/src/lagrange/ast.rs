use noether_core::stage::StageId;
use noether_core::types::NType;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A composition graph node. The core AST for Noether's composition language.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum CompositionNode {
    /// Leaf node: reference to a stage by its content hash.
    Stage { id: StageId },

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
        CompositionNode::Stage { id } => ids.push(id),
        CompositionNode::RemoteStage { .. } => {} // no local stage ID; URL is resolved at runtime
        CompositionNode::Const { .. } => {} // no stage IDs in a constant
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stage(id: &str) -> CompositionNode {
        CompositionNode::Stage {
            id: StageId(id.into()),
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
