use crate::lagrange::CompositionNode;
use noether_core::stage::StageId;
use noether_core::types::{is_subtype_of, IncompatibilityReason, NType, TypeCompatibility};
use noether_store::StageStore;
use std::collections::BTreeMap;
use std::fmt;

/// The resolved input/output types of a composition node.
#[derive(Debug, Clone)]
pub struct ResolvedType {
    pub input: NType,
    pub output: NType,
}

/// Errors detected during graph type checking.
#[derive(Debug, Clone)]
pub enum GraphTypeError {
    StageNotFound {
        id: StageId,
    },
    SequentialTypeMismatch {
        position: usize,
        from_output: NType,
        to_input: NType,
        reason: IncompatibilityReason,
    },
    BranchPredicateNotBool {
        actual: NType,
    },
    BranchOutputMismatch {
        true_output: NType,
        false_output: NType,
        reason: IncompatibilityReason,
    },
    FanoutInputMismatch {
        target_index: usize,
        source_output: NType,
        target_input: NType,
        reason: IncompatibilityReason,
    },
    MergeOutputMismatch {
        merged_type: NType,
        target_input: NType,
        reason: IncompatibilityReason,
    },
    EmptyNode {
        operator: String,
    },
}

impl fmt::Display for GraphTypeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphTypeError::StageNotFound { id } => {
                write!(f, "stage {} not found in store", id.0)
            }
            GraphTypeError::SequentialTypeMismatch {
                position,
                from_output,
                to_input,
                reason,
            } => write!(
                f,
                "type mismatch at position {position}: output {from_output} is not subtype of input {to_input}: {reason}"
            ),
            GraphTypeError::BranchPredicateNotBool { actual } => {
                write!(f, "branch predicate must produce Bool, got {actual}")
            }
            GraphTypeError::BranchOutputMismatch {
                true_output,
                false_output,
                reason,
            } => write!(
                f,
                "branch outputs must be compatible: if_true produces {true_output}, if_false produces {false_output}: {reason}"
            ),
            GraphTypeError::FanoutInputMismatch {
                target_index,
                source_output,
                target_input,
                reason,
            } => write!(
                f,
                "fanout target {target_index}: source output {source_output} is not subtype of target input {target_input}: {reason}"
            ),
            GraphTypeError::MergeOutputMismatch {
                merged_type,
                target_input,
                reason,
            } => write!(
                f,
                "merge: merged type {merged_type} is not subtype of target input {target_input}: {reason}"
            ),
            GraphTypeError::EmptyNode { operator } => {
                write!(f, "empty {operator} node")
            }
        }
    }
}

/// Type-check a composition graph against the stage store.
///
/// Returns the resolved input/output types of the entire graph,
/// or a list of errors if type checking fails.
pub fn check_graph(
    node: &CompositionNode,
    store: &(impl StageStore + ?Sized),
) -> Result<ResolvedType, Vec<GraphTypeError>> {
    let mut errors = Vec::new();
    let result = check_node(node, store, &mut errors);
    if errors.is_empty() {
        Ok(result.unwrap())
    } else {
        Err(errors)
    }
}

fn check_node(
    node: &CompositionNode,
    store: &(impl StageStore + ?Sized),
    errors: &mut Vec<GraphTypeError>,
) -> Option<ResolvedType> {
    match node {
        CompositionNode::Stage { id } => check_stage(id, store, errors),
        CompositionNode::Sequential { stages } => check_sequential(stages, store, errors),
        CompositionNode::Parallel { branches } => check_parallel(branches, store, errors),
        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => check_branch(predicate, if_true, if_false, store, errors),
        CompositionNode::Fanout { source, targets } => check_fanout(source, targets, store, errors),
        CompositionNode::Merge { sources, target } => check_merge(sources, target, store, errors),
        CompositionNode::Retry { stage, .. } => check_node(stage, store, errors),
    }
}

fn check_stage(
    id: &StageId,
    store: &(impl StageStore + ?Sized),
    errors: &mut Vec<GraphTypeError>,
) -> Option<ResolvedType> {
    match store.get(id) {
        Ok(Some(stage)) => Some(ResolvedType {
            input: stage.signature.input.clone(),
            output: stage.signature.output.clone(),
        }),
        _ => {
            errors.push(GraphTypeError::StageNotFound { id: id.clone() });
            None
        }
    }
}

fn check_sequential(
    stages: &[CompositionNode],
    store: &(impl StageStore + ?Sized),
    errors: &mut Vec<GraphTypeError>,
) -> Option<ResolvedType> {
    if stages.is_empty() {
        errors.push(GraphTypeError::EmptyNode {
            operator: "Sequential".into(),
        });
        return None;
    }

    let resolved: Vec<Option<ResolvedType>> = stages
        .iter()
        .map(|s| check_node(s, store, errors))
        .collect();

    // Check consecutive pairs
    for i in 0..resolved.len() - 1 {
        if let (Some(from), Some(to)) = (&resolved[i], &resolved[i + 1]) {
            if let TypeCompatibility::Incompatible(reason) = is_subtype_of(&from.output, &to.input)
            {
                errors.push(GraphTypeError::SequentialTypeMismatch {
                    position: i,
                    from_output: from.output.clone(),
                    to_input: to.input.clone(),
                    reason,
                });
            }
        }
    }

    let first_input = resolved
        .first()
        .and_then(|r| r.as_ref())
        .map(|r| r.input.clone());
    let last_output = resolved
        .last()
        .and_then(|r| r.as_ref())
        .map(|r| r.output.clone());

    match (first_input, last_output) {
        (Some(input), Some(output)) => Some(ResolvedType { input, output }),
        _ => None,
    }
}

fn check_parallel(
    branches: &BTreeMap<String, CompositionNode>,
    store: &(impl StageStore + ?Sized),
    errors: &mut Vec<GraphTypeError>,
) -> Option<ResolvedType> {
    if branches.is_empty() {
        errors.push(GraphTypeError::EmptyNode {
            operator: "Parallel".into(),
        });
        return None;
    }

    let mut input_fields = BTreeMap::new();
    let mut output_fields = BTreeMap::new();

    for (name, node) in branches {
        if let Some(resolved) = check_node(node, store, errors) {
            input_fields.insert(name.clone(), resolved.input);
            output_fields.insert(name.clone(), resolved.output);
        }
    }

    if input_fields.len() == branches.len() {
        Some(ResolvedType {
            input: NType::Record(input_fields),
            output: NType::Record(output_fields),
        })
    } else {
        None
    }
}

fn check_branch(
    predicate: &CompositionNode,
    if_true: &CompositionNode,
    if_false: &CompositionNode,
    store: &(impl StageStore + ?Sized),
    errors: &mut Vec<GraphTypeError>,
) -> Option<ResolvedType> {
    let pred = check_node(predicate, store, errors);
    let true_branch = check_node(if_true, store, errors);
    let false_branch = check_node(if_false, store, errors);

    // Check predicate output is Bool
    if let Some(ref p) = pred {
        if let TypeCompatibility::Incompatible(_) = is_subtype_of(&p.output, &NType::Bool) {
            errors.push(GraphTypeError::BranchPredicateNotBool {
                actual: p.output.clone(),
            });
        }
    }

    // Branch outputs are unioned — both paths are valid return types.
    // No compatibility check required between branches; the consumer
    // of the branch output must handle the union type.
    match (pred, true_branch, false_branch) {
        (Some(p), Some(t), Some(f)) => Some(ResolvedType {
            input: p.input,
            output: NType::union(vec![t.output, f.output]),
        }),
        _ => None,
    }
}

fn check_fanout(
    source: &CompositionNode,
    targets: &[CompositionNode],
    store: &(impl StageStore + ?Sized),
    errors: &mut Vec<GraphTypeError>,
) -> Option<ResolvedType> {
    if targets.is_empty() {
        errors.push(GraphTypeError::EmptyNode {
            operator: "Fanout".into(),
        });
        return None;
    }

    let src = check_node(source, store, errors);
    let tgts: Vec<Option<ResolvedType>> = targets
        .iter()
        .map(|t| check_node(t, store, errors))
        .collect();

    // Check source output is subtype of each target input
    if let Some(ref s) = src {
        for (i, t) in tgts.iter().enumerate() {
            if let Some(ref t) = t {
                if let TypeCompatibility::Incompatible(reason) = is_subtype_of(&s.output, &t.input)
                {
                    errors.push(GraphTypeError::FanoutInputMismatch {
                        target_index: i,
                        source_output: s.output.clone(),
                        target_input: t.input.clone(),
                        reason,
                    });
                }
            }
        }
    }

    let output_types: Vec<NType> = tgts
        .iter()
        .filter_map(|t| t.as_ref().map(|r| r.output.clone()))
        .collect();

    match src {
        Some(s) if output_types.len() == targets.len() => Some(ResolvedType {
            input: s.input,
            output: NType::List(Box::new(if output_types.len() == 1 {
                output_types.into_iter().next().unwrap()
            } else {
                NType::union(output_types)
            })),
        }),
        _ => None,
    }
}

fn check_merge(
    sources: &[CompositionNode],
    target: &CompositionNode,
    store: &(impl StageStore + ?Sized),
    errors: &mut Vec<GraphTypeError>,
) -> Option<ResolvedType> {
    if sources.is_empty() {
        errors.push(GraphTypeError::EmptyNode {
            operator: "Merge".into(),
        });
        return None;
    }

    let srcs: Vec<Option<ResolvedType>> = sources
        .iter()
        .map(|s| check_node(s, store, errors))
        .collect();
    let tgt = check_node(target, store, errors);

    // Build merged output record from sources
    let mut merged_fields = BTreeMap::new();
    for (i, s) in srcs.iter().enumerate() {
        if let Some(ref r) = s {
            merged_fields.insert(format!("source_{i}"), r.output.clone());
        }
    }
    let merged_type = NType::Record(merged_fields);

    // Check merged type is subtype of target input
    if let Some(ref t) = tgt {
        if let TypeCompatibility::Incompatible(reason) = is_subtype_of(&merged_type, &t.input) {
            errors.push(GraphTypeError::MergeOutputMismatch {
                merged_type: merged_type.clone(),
                target_input: t.input.clone(),
                reason,
            });
        }
    }

    // Overall: input is record of source inputs, output is target output
    let mut input_fields = BTreeMap::new();
    for (i, s) in srcs.iter().enumerate() {
        if let Some(ref r) = s {
            input_fields.insert(format!("source_{i}"), r.input.clone());
        }
    }

    match tgt {
        Some(t) => Some(ResolvedType {
            input: NType::Record(input_fields),
            output: t.output,
        }),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noether_core::effects::EffectSet;
    use noether_core::stage::{CostEstimate, Stage, StageSignature};
    use noether_store::MemoryStore;
    use std::collections::BTreeSet;

    fn make_stage(id: &str, input: NType, output: NType) -> Stage {
        Stage {
            id: StageId(id.into()),
            signature: StageSignature {
                input,
                output,
                effects: EffectSet::pure(),
                implementation_hash: format!("impl_{id}"),
            },
            capabilities: BTreeSet::new(),
            cost: CostEstimate {
                time_ms_p50: Some(10),
                tokens_est: None,
                memory_mb: None,
            },
            description: format!("test stage {id}"),
            examples: vec![],
            lifecycle: noether_core::stage::StageLifecycle::Active,
            ed25519_signature: None,
            signer_public_key: None,
            implementation_code: None,
            implementation_language: None,
        }
    }

    fn test_store() -> MemoryStore {
        let mut store = MemoryStore::new();
        store
            .put(make_stage("text_to_num", NType::Text, NType::Number))
            .unwrap();
        store
            .put(make_stage("num_to_bool", NType::Number, NType::Bool))
            .unwrap();
        store
            .put(make_stage("text_to_text", NType::Text, NType::Text))
            .unwrap();
        store
            .put(make_stage("bool_pred", NType::Text, NType::Bool))
            .unwrap();
        store
            .put(make_stage("any_to_text", NType::Any, NType::Text))
            .unwrap();
        store
    }

    fn stage(id: &str) -> CompositionNode {
        CompositionNode::Stage {
            id: StageId(id.into()),
        }
    }

    #[test]
    fn check_single_stage() {
        let store = test_store();
        let result = check_graph(&stage("text_to_num"), &store);
        let resolved = result.unwrap();
        assert_eq!(resolved.input, NType::Text);
        assert_eq!(resolved.output, NType::Number);
    }

    #[test]
    fn check_missing_stage() {
        let store = test_store();
        let result = check_graph(&stage("nonexistent"), &store);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(errors[0], GraphTypeError::StageNotFound { .. }));
    }

    #[test]
    fn check_valid_sequential() {
        let store = test_store();
        let node = CompositionNode::Sequential {
            stages: vec![stage("text_to_num"), stage("num_to_bool")],
        };
        let result = check_graph(&node, &store);
        let resolved = result.unwrap();
        assert_eq!(resolved.input, NType::Text);
        assert_eq!(resolved.output, NType::Bool);
    }

    #[test]
    fn check_invalid_sequential() {
        let store = test_store();
        // Bool output cannot feed Text input
        let node = CompositionNode::Sequential {
            stages: vec![stage("num_to_bool"), stage("text_to_num")],
        };
        let result = check_graph(&node, &store);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(matches!(
            errors[0],
            GraphTypeError::SequentialTypeMismatch { .. }
        ));
    }

    #[test]
    fn check_parallel() {
        let store = test_store();
        let node = CompositionNode::Parallel {
            branches: BTreeMap::from([
                ("nums".into(), stage("text_to_num")),
                ("bools".into(), stage("bool_pred")),
            ]),
        };
        let result = check_graph(&node, &store);
        let resolved = result.unwrap();
        // Input is Record { bools: Text, nums: Text }
        // Output is Record { bools: Bool, nums: Number }
        assert!(matches!(resolved.input, NType::Record(_)));
        assert!(matches!(resolved.output, NType::Record(_)));
    }

    #[test]
    fn check_branch_valid() {
        let store = test_store();
        let node = CompositionNode::Branch {
            predicate: Box::new(stage("bool_pred")),
            if_true: Box::new(stage("text_to_num")),
            if_false: Box::new(stage("text_to_text")),
        };
        // Predicate: Text -> Bool ✓
        // Both branches take Text, so input matches
        // Outputs are Number and Text, which union into Number | Text
        let result = check_graph(&node, &store);
        let resolved = result.unwrap();
        assert_eq!(resolved.input, NType::Text);
    }

    #[test]
    fn check_retry_transparent() {
        let store = test_store();
        let node = CompositionNode::Retry {
            stage: Box::new(stage("text_to_num")),
            max_attempts: 3,
            delay_ms: Some(100),
        };
        let result = check_graph(&node, &store);
        let resolved = result.unwrap();
        assert_eq!(resolved.input, NType::Text);
        assert_eq!(resolved.output, NType::Number);
    }
}
