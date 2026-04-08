use crate::lagrange::CompositionNode;
use noether_core::effects::Effect;
use noether_core::stage::StageId;
use noether_store::StageStore;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ExecutionMode {
    Inline,
    Process,
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionStep {
    pub step_index: usize,
    pub stage_id: StageId,
    pub mode: ExecutionMode,
    pub depends_on: Vec<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostSummary {
    pub total_time_ms_p50: Option<u64>,
    pub total_tokens_est: Option<u64>,
    pub total_memory_mb_peak: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub steps: Vec<ExecutionStep>,
    pub cost: CostSummary,
    pub parallel_groups: Vec<Vec<usize>>,
}

/// Flatten a composition AST into a linear execution plan.
pub fn plan_graph(node: &CompositionNode, store: &(impl StageStore + ?Sized)) -> ExecutionPlan {
    let mut steps = Vec::new();
    let mut parallel_groups = Vec::new();
    flatten_node(node, &mut steps, &mut parallel_groups, store, &[]);

    let cost = estimate_cost(&steps, store);

    ExecutionPlan {
        steps,
        cost,
        parallel_groups,
    }
}

/// Returns the indices of the steps added for this node.
fn flatten_node(
    node: &CompositionNode,
    steps: &mut Vec<ExecutionStep>,
    parallel_groups: &mut Vec<Vec<usize>>,
    store: &(impl StageStore + ?Sized),
    depends_on: &[usize],
) -> Vec<usize> {
    match node {
        CompositionNode::Stage { id } => {
            let idx = steps.len();
            steps.push(ExecutionStep {
                step_index: idx,
                stage_id: id.clone(),
                mode: ExecutionMode::Inline,
                depends_on: depends_on.to_vec(),
            });
            vec![idx]
        }
        CompositionNode::Const { .. } => {
            // Const nodes produce no execution step — they are resolved inline
            // in the runner without touching the store.
            depends_on.to_vec()
        }
        CompositionNode::RemoteStage { .. } => {
            // RemoteStage nodes produce no local execution step.
            // Native runner handles these via reqwest; browser runtime via fetch().
            depends_on.to_vec()
        }
        CompositionNode::Sequential { stages } => {
            let mut prev_indices = depends_on.to_vec();

            let start_step = steps.len();
            for stage in stages {
                prev_indices = flatten_node(stage, steps, parallel_groups, store, &prev_indices);
            }
            let end_step = steps.len();

            // After flattening, check whether ALL direct children are Stage nodes
            // and all are Pure. If so, add them as a parallel group hint.
            let all_direct_pure_stages = stages.iter().all(|s| {
                if let CompositionNode::Stage { id } = s {
                    store
                        .get(id)
                        .ok()
                        .flatten()
                        .map(|st| st.signature.effects.contains(&Effect::Pure))
                        .unwrap_or(false)
                } else {
                    false
                }
            });

            if all_direct_pure_stages && stages.len() > 1 {
                let group: Vec<usize> = (start_step..end_step).collect();
                if group.len() > 1 {
                    parallel_groups.push(group);
                }
            }

            prev_indices
        }
        CompositionNode::Parallel { branches } => {
            let mut group = Vec::new();
            let mut all_outputs = Vec::new();
            for node in branches.values() {
                let outputs = flatten_node(node, steps, parallel_groups, store, depends_on);
                // The first step of each branch is in the parallel group
                if let Some(&first) = outputs.first() {
                    group.push(first);
                }
                all_outputs.extend(outputs);
            }
            if group.len() > 1 {
                parallel_groups.push(group);
            }
            all_outputs
        }
        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => {
            let pred_out = flatten_node(predicate, steps, parallel_groups, store, depends_on);
            let true_out = flatten_node(if_true, steps, parallel_groups, store, &pred_out);
            let false_out = flatten_node(if_false, steps, parallel_groups, store, &pred_out);
            let mut combined = true_out;
            combined.extend(false_out);
            combined
        }
        CompositionNode::Fanout { source, targets } => {
            let source_out = flatten_node(source, steps, parallel_groups, store, depends_on);
            let mut group = Vec::new();
            let mut all_outputs = Vec::new();
            for target in targets {
                let outputs = flatten_node(target, steps, parallel_groups, store, &source_out);
                if let Some(&first) = outputs.first() {
                    group.push(first);
                }
                all_outputs.extend(outputs);
            }
            if group.len() > 1 {
                parallel_groups.push(group);
            }
            all_outputs
        }
        CompositionNode::Merge { sources, target } => {
            let mut all_source_outputs = Vec::new();
            let mut group = Vec::new();
            for src in sources {
                let outputs = flatten_node(src, steps, parallel_groups, store, depends_on);
                if let Some(&first) = outputs.first() {
                    group.push(first);
                }
                all_source_outputs.extend(outputs);
            }
            if group.len() > 1 {
                parallel_groups.push(group);
            }
            flatten_node(target, steps, parallel_groups, store, &all_source_outputs)
        }
        CompositionNode::Retry { stage, .. } => {
            flatten_node(stage, steps, parallel_groups, store, depends_on)
        }
    }
}

fn estimate_cost(steps: &[ExecutionStep], store: &(impl StageStore + ?Sized)) -> CostSummary {
    let mut total_time: u64 = 0;
    let mut total_tokens: u64 = 0;
    let mut max_memory: u64 = 0;

    for step in steps {
        if let Ok(Some(stage)) = store.get(&step.stage_id) {
            if let Some(t) = stage.cost.time_ms_p50 {
                total_time += t;
            }
            if let Some(t) = stage.cost.tokens_est {
                total_tokens += t;
            }
            if let Some(m) = stage.cost.memory_mb {
                max_memory = max_memory.max(m);
            }
        }
    }

    CostSummary {
        total_time_ms_p50: if total_time > 0 {
            Some(total_time)
        } else {
            None
        },
        total_tokens_est: if total_tokens > 0 {
            Some(total_tokens)
        } else {
            None
        },
        total_memory_mb_peak: if max_memory > 0 {
            Some(max_memory)
        } else {
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noether_store::MemoryStore;
    use std::collections::BTreeMap;

    fn stage(id: &str) -> CompositionNode {
        CompositionNode::Stage {
            id: StageId(id.into()),
        }
    }

    #[test]
    fn plan_single_stage() {
        let store = MemoryStore::new();
        let plan = plan_graph(&stage("a"), &store);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].stage_id, StageId("a".into()));
        assert!(plan.steps[0].depends_on.is_empty());
    }

    #[test]
    fn plan_sequential_has_dependencies() {
        let store = MemoryStore::new();
        let node = CompositionNode::Sequential {
            stages: vec![stage("a"), stage("b"), stage("c")],
        };
        let plan = plan_graph(&node, &store);
        assert_eq!(plan.steps.len(), 3);
        assert!(plan.steps[0].depends_on.is_empty());
        assert_eq!(plan.steps[1].depends_on, vec![0]);
        assert_eq!(plan.steps[2].depends_on, vec![1]);
    }

    #[test]
    fn plan_parallel_creates_group() {
        let store = MemoryStore::new();
        let node = CompositionNode::Parallel {
            branches: BTreeMap::from([("a".into(), stage("s1")), ("b".into(), stage("s2"))]),
        };
        let plan = plan_graph(&node, &store);
        assert_eq!(plan.steps.len(), 2);
        assert_eq!(plan.parallel_groups.len(), 1);
        assert_eq!(plan.parallel_groups[0].len(), 2);
    }

    #[test]
    fn plan_sequential_with_parallel() {
        let store = MemoryStore::new();
        let node = CompositionNode::Sequential {
            stages: vec![
                stage("input"),
                CompositionNode::Parallel {
                    branches: BTreeMap::from([
                        ("a".into(), stage("s1")),
                        ("b".into(), stage("s2")),
                    ]),
                },
                stage("output"),
            ],
        };
        let plan = plan_graph(&node, &store);
        assert_eq!(plan.steps.len(), 4); // input, s1, s2, output
                                         // s1 and s2 depend on input (step 0)
        assert!(plan.steps[1].depends_on.contains(&0));
        assert!(plan.steps[2].depends_on.contains(&0));
        // output depends on both s1 and s2
        assert!(plan.steps[3].depends_on.contains(&1));
        assert!(plan.steps[3].depends_on.contains(&2));
    }
}
