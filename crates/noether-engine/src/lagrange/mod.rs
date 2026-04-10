mod ast;

pub use ast::{collect_stage_ids, CompositionGraph, CompositionNode};

use sha2::{Digest, Sha256};

/// Parse a Lagrange JSON string into a CompositionGraph.
pub fn parse_graph(json: &str) -> Result<CompositionGraph, serde_json::Error> {
    serde_json::from_str(json)
}

/// Serialize a CompositionGraph to pretty-printed JSON.
pub fn serialize_graph(graph: &CompositionGraph) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(graph)
}

/// Compute a deterministic composition ID (SHA-256 of canonical JSON).
pub fn compute_composition_id(graph: &CompositionGraph) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(graph)?;
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
