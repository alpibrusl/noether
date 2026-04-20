//! Pass: fold `Branch { predicate: Const(bool), … }` into the selected arm.
//!
//! # What this rewrites
//!
//! ```text
//! Branch {
//!   predicate: Const(true),
//!   if_true:   A,
//!   if_false:  B,
//! }
//! ```
//!
//! Becomes just `A`. Similarly, a `Const(false)` predicate folds to
//! the `if_false` arm. A predicate that isn't a [`CompositionNode::Const`]
//! with a JSON boolean value is left alone — we can't prove the dead
//! arm at compile time, so we don't prune it.
//!
//! # What this doesn't rewrite
//!
//! - Predicates whose value depends on input (the common case). Those
//!   are the whole point of `Branch`; leaving them is the right move.
//! - `Const` values that are non-boolean (numbers, strings, objects).
//!   Noether's `Branch` type-check already rejects these, but the
//!   pass is defensive: if a non-bool slips past, we leave the Branch
//!   untouched rather than guessing a truthiness rule.
//!
//! # Why it matters
//!
//! Agent-generated graphs (from `noether compose`) occasionally emit
//! `Branch { predicate: Const(true), if_true: <real>, if_false: <fallback> }`
//! as a defensive shape even when the fallback can never run. Folding
//! lets the planner and executor skip wiring the dead arm entirely.

use super::OptimizerPass;
use crate::lagrange::CompositionNode;

pub struct DeadBranchElimination;

impl OptimizerPass for DeadBranchElimination {
    fn name(&self) -> &'static str {
        "dead_branch"
    }

    fn rewrite(&self, node: CompositionNode) -> (CompositionNode, bool) {
        match node {
            CompositionNode::Branch {
                predicate,
                if_true,
                if_false,
            } => fold_branch(*predicate, *if_true, *if_false, self),

            // Structural recursion into children for every other
            // composite node. We don't try to "optimize" leaves
            // (Stage / RemoteStage / Const); nothing to fold there.
            CompositionNode::Sequential { stages } => {
                let mut changed = false;
                let new_stages: Vec<_> = stages
                    .into_iter()
                    .map(|s| {
                        let (n, c) = self.rewrite(s);
                        changed |= c;
                        n
                    })
                    .collect();
                (CompositionNode::Sequential { stages: new_stages }, changed)
            }

            CompositionNode::Parallel { branches } => {
                let mut changed = false;
                let new_branches = branches
                    .into_iter()
                    .map(|(k, v)| {
                        let (n, c) = self.rewrite(v);
                        changed |= c;
                        (k, n)
                    })
                    .collect();
                (
                    CompositionNode::Parallel {
                        branches: new_branches,
                    },
                    changed,
                )
            }

            CompositionNode::Fanout { source, targets } => {
                let (new_source, source_changed) = self.rewrite(*source);
                let mut changed = source_changed;
                let new_targets: Vec<_> = targets
                    .into_iter()
                    .map(|t| {
                        let (n, c) = self.rewrite(t);
                        changed |= c;
                        n
                    })
                    .collect();
                (
                    CompositionNode::Fanout {
                        source: Box::new(new_source),
                        targets: new_targets,
                    },
                    changed,
                )
            }

            CompositionNode::Merge { sources, target } => {
                let mut changed = false;
                let new_sources: Vec<_> = sources
                    .into_iter()
                    .map(|s| {
                        let (n, c) = self.rewrite(s);
                        changed |= c;
                        n
                    })
                    .collect();
                let (new_target, target_changed) = self.rewrite(*target);
                changed |= target_changed;
                (
                    CompositionNode::Merge {
                        sources: new_sources,
                        target: Box::new(new_target),
                    },
                    changed,
                )
            }

            CompositionNode::Retry {
                stage,
                max_attempts,
                delay_ms,
            } => {
                let (new_stage, changed) = self.rewrite(*stage);
                (
                    CompositionNode::Retry {
                        stage: Box::new(new_stage),
                        max_attempts,
                        delay_ms,
                    },
                    changed,
                )
            }

            CompositionNode::Let { bindings, body } => {
                let mut changed = false;
                let new_bindings = bindings
                    .into_iter()
                    .map(|(k, v)| {
                        let (n, c) = self.rewrite(v);
                        changed |= c;
                        (k, n)
                    })
                    .collect();
                let (new_body, body_changed) = self.rewrite(*body);
                changed |= body_changed;
                (
                    CompositionNode::Let {
                        bindings: new_bindings,
                        body: Box::new(new_body),
                    },
                    changed,
                )
            }

            // Leaves — nothing to recurse into.
            leaf @ (CompositionNode::Stage { .. }
            | CompositionNode::Const { .. }
            | CompositionNode::RemoteStage { .. }) => (leaf, false),
        }
    }
}

fn fold_branch(
    predicate: CompositionNode,
    if_true: CompositionNode,
    if_false: CompositionNode,
    pass: &DeadBranchElimination,
) -> (CompositionNode, bool) {
    // Is the predicate a constant boolean? If so, fold to the
    // selected arm — and recurse into it so chained dead branches
    // collapse in a single pass iteration.
    if let CompositionNode::Const { value } = &predicate {
        if let Some(b) = value.as_bool() {
            let selected = if b { if_true } else { if_false };
            let (folded, _) = pass.rewrite(selected);
            return (folded, true);
        }
    }

    // Non-constant predicate (or non-bool constant — which is a
    // type-check bug, not our problem). Recurse into every child
    // in case a nested Branch is foldable.
    let (new_pred, pred_changed) = pass.rewrite(predicate);
    let (new_true, true_changed) = pass.rewrite(if_true);
    let (new_false, false_changed) = pass.rewrite(if_false);
    (
        CompositionNode::Branch {
            predicate: Box::new(new_pred),
            if_true: Box::new(new_true),
            if_false: Box::new(new_false),
        },
        pred_changed || true_changed || false_changed,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optimizer::{optimize, DEFAULT_MAX_ITERATIONS};
    use serde_json::json;

    fn stage(id: &str) -> CompositionNode {
        CompositionNode::stage(id)
    }

    fn konst(v: serde_json::Value) -> CompositionNode {
        CompositionNode::Const { value: v }
    }

    #[test]
    fn true_predicate_folds_to_if_true_arm() {
        let node = CompositionNode::Branch {
            predicate: Box::new(konst(json!(true))),
            if_true: Box::new(stage("a")),
            if_false: Box::new(stage("b")),
        };
        let (out, changed) = DeadBranchElimination.rewrite(node);
        assert_eq!(out, stage("a"));
        assert!(changed);
    }

    #[test]
    fn false_predicate_folds_to_if_false_arm() {
        let node = CompositionNode::Branch {
            predicate: Box::new(konst(json!(false))),
            if_true: Box::new(stage("a")),
            if_false: Box::new(stage("b")),
        };
        let (out, changed) = DeadBranchElimination.rewrite(node);
        assert_eq!(out, stage("b"));
        assert!(changed);
    }

    #[test]
    fn non_const_predicate_is_left_alone() {
        // The common case — predicate depends on input. The pass
        // must not guess; leaving the Branch untouched is correct.
        let node = CompositionNode::Branch {
            predicate: Box::new(stage("has_network")),
            if_true: Box::new(stage("a")),
            if_false: Box::new(stage("b")),
        };
        let node_clone = node.clone();
        let (out, changed) = DeadBranchElimination.rewrite(node);
        assert_eq!(out, node_clone);
        assert!(!changed);
    }

    #[test]
    fn non_bool_const_predicate_is_left_alone() {
        // A `Const(42)` as a Branch predicate is a type-check bug —
        // the pass is defensive: no guessing a truthiness rule,
        // leave the Branch and let the type checker be the gate.
        let node = CompositionNode::Branch {
            predicate: Box::new(konst(json!(42))),
            if_true: Box::new(stage("a")),
            if_false: Box::new(stage("b")),
        };
        let node_clone = node.clone();
        let (out, changed) = DeadBranchElimination.rewrite(node);
        assert_eq!(out, node_clone);
        assert!(!changed);
    }

    #[test]
    fn nested_dead_branches_collapse_in_one_pass() {
        // Outer: Const(true) → take if_true
        //   Inner if_true: Const(false) → take if_false
        //     → final: stage "d"
        let node = CompositionNode::Branch {
            predicate: Box::new(konst(json!(true))),
            if_true: Box::new(CompositionNode::Branch {
                predicate: Box::new(konst(json!(false))),
                if_true: Box::new(stage("c")),
                if_false: Box::new(stage("d")),
            }),
            if_false: Box::new(stage("e")),
        };
        let (out, changed) = DeadBranchElimination.rewrite(node);
        assert_eq!(out, stage("d"));
        assert!(changed);
    }

    #[test]
    fn dead_branch_inside_sequential_is_folded() {
        // Sequential { [Branch(true → X, false → Y), Z] }
        //   → Sequential { [X, Z] }
        let node = CompositionNode::Sequential {
            stages: vec![
                CompositionNode::Branch {
                    predicate: Box::new(konst(json!(true))),
                    if_true: Box::new(stage("x")),
                    if_false: Box::new(stage("y")),
                },
                stage("z"),
            ],
        };
        let (out, changed) = DeadBranchElimination.rewrite(node);
        assert_eq!(
            out,
            CompositionNode::Sequential {
                stages: vec![stage("x"), stage("z")]
            }
        );
        assert!(changed);
    }

    #[test]
    fn fixpoint_runner_reports_dead_branch() {
        // Pass-through the public API: `optimize` picks up
        // DeadBranchElimination, records it in the report, converges
        // without hitting the cap.
        let node = CompositionNode::Branch {
            predicate: Box::new(konst(json!(true))),
            if_true: Box::new(stage("kept")),
            if_false: Box::new(stage("pruned")),
        };
        let (out, report) = optimize(node, &[&DeadBranchElimination], DEFAULT_MAX_ITERATIONS);
        assert_eq!(out, stage("kept"));
        assert_eq!(report.passes_applied, vec!["dead_branch"]);
        assert!(!report.hit_iteration_cap);
    }

    #[test]
    fn pass_is_a_no_op_on_pure_leaf_nodes() {
        // Stage / Const / RemoteStage — no Branches anywhere — no
        // change. Guards against a regression where the pass
        // spuriously rewrites leaves.
        for leaf in [
            stage("only"),
            konst(json!("hello")),
            konst(json!(42)),
            konst(json!(null)),
        ] {
            let clone = leaf.clone();
            let (out, changed) = DeadBranchElimination.rewrite(leaf);
            assert_eq!(out, clone);
            assert!(!changed);
        }
    }
}
