//! Pass: apply the M1 canonical-form structural rewrites to the
//! execution graph.
//!
//! # What this rewrites
//!
//! This pass delegates to [`crate::lagrange::canonical::canonicalise`],
//! which already implements the M1 canonical-form rules:
//!
//! 1. Flatten nested `Sequential` into a single list, left-to-right.
//! 2. Collapse singleton `Sequential[s]` to `s`.
//! 3. Normalise Parallel branch ordering (BTreeMap already keeps keys
//!    sorted; this is a no-op but kept explicit for completeness).
//! 4. `Retry { Retry { s, n, d }, m, d }` → `Retry { s, n·m, d }` —
//!    fuse adjacent retries.
//!
//! These rewrites are **semantics-preserving by construction** —
//! they're tested as laws in `lagrange::laws` (see M1 property
//! tests). Lifting them into an optimizer pass means the **executor**
//! sees the canonical form, not just the hasher.
//!
//! # Why this matters
//!
//! Today [`canonicalise`] runs only as part of
//! [`compute_composition_id`](crate::lagrange::compute_composition_id) —
//! it shapes the form we hash, not the form we run. A graph with
//! deeply nested `Sequential` wrappers (common output of
//! `noether compose`) reaches the executor with every wrapper intact.
//! The planner walks each wrapper; the trace carries one entry per
//! wrapper. Pointless overhead and noise.
//!
//! This pass runs post-resolve + post-type-check, so by the time it
//! rewrites, the graph's identity and types are settled. The
//! `composition_id` was computed much earlier on the pre-resolution
//! canonical form and stays stable.
//!
//! # Interaction with other passes
//!
//! This pass is conservative: it never removes or renames a `Stage`
//! node, only reshapes the tree structure around them. Safe to run
//! before or after other passes. In `cmd_run`, it runs **first** so
//! subsequent passes see the flattened form — notably
//! [`crate::optimizer::dead_branch::DeadBranchElimination`] can then
//! fold a `Branch` that was hidden inside a collapsible singleton
//! Sequential wrapper.

use super::OptimizerPass;
use crate::lagrange::canonical::canonicalise;
use crate::lagrange::CompositionNode;

pub struct CanonicalStructural;

impl OptimizerPass for CanonicalStructural {
    fn name(&self) -> &'static str {
        "canonical_structural"
    }

    fn rewrite(&self, node: CompositionNode) -> (CompositionNode, bool) {
        let canonical = canonicalise(&node);
        let changed = canonical != node;
        (canonical, changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::optimizer::{optimize, DEFAULT_MAX_ITERATIONS};

    fn stage(id: &str) -> CompositionNode {
        CompositionNode::stage(id)
    }

    #[test]
    fn nested_sequential_is_flattened() {
        // Sequential[ Sequential[a, b], c ] → Sequential[a, b, c]
        let node = CompositionNode::Sequential {
            stages: vec![
                CompositionNode::Sequential {
                    stages: vec![stage("a"), stage("b")],
                },
                stage("c"),
            ],
        };
        let (out, changed) = CanonicalStructural.rewrite(node);
        assert_eq!(
            out,
            CompositionNode::Sequential {
                stages: vec![stage("a"), stage("b"), stage("c")]
            }
        );
        assert!(changed);
    }

    #[test]
    fn singleton_sequential_collapses_to_inner() {
        // Sequential[a] → a
        let node = CompositionNode::Sequential {
            stages: vec![stage("a")],
        };
        let (out, changed) = CanonicalStructural.rewrite(node);
        assert_eq!(out, stage("a"));
        assert!(changed);
    }

    #[test]
    fn nested_retry_is_fused() {
        // Retry{ Retry{ s, 3, _ }, 2, _ } → Retry{ s, 6, _ }
        let node = CompositionNode::Retry {
            stage: Box::new(CompositionNode::Retry {
                stage: Box::new(stage("s")),
                max_attempts: 3,
                delay_ms: None,
            }),
            max_attempts: 2,
            delay_ms: None,
        };
        let (out, changed) = CanonicalStructural.rewrite(node);
        assert_eq!(
            out,
            CompositionNode::Retry {
                stage: Box::new(stage("s")),
                max_attempts: 6,
                delay_ms: None,
            }
        );
        assert!(changed);
    }

    #[test]
    fn already_canonical_graph_reports_no_change() {
        // Singleton Sequential is already collapsed — the canonical
        // form of a plain `stage("a")` is itself. The pass must not
        // lie about changing something that didn't change, otherwise
        // the fixpoint runner loops forever.
        let node = stage("a");
        let (out, changed) = CanonicalStructural.rewrite(node.clone());
        assert_eq!(out, node);
        assert!(!changed);
    }

    #[test]
    fn deeply_nested_sequential_flattens_in_one_pass() {
        // Sequential[ Sequential[ Sequential[a] ], b ]
        //   → Sequential[a, b] (after singleton-collapse + flatten)
        let node = CompositionNode::Sequential {
            stages: vec![
                CompositionNode::Sequential {
                    stages: vec![CompositionNode::Sequential {
                        stages: vec![stage("a")],
                    }],
                },
                stage("b"),
            ],
        };
        let (out, changed) = CanonicalStructural.rewrite(node);
        assert_eq!(
            out,
            CompositionNode::Sequential {
                stages: vec![stage("a"), stage("b")]
            }
        );
        assert!(changed);
    }

    #[test]
    fn fixpoint_runner_reports_canonical_structural() {
        // Public-API integration: the pass plugs into the fixpoint
        // runner and the report records the rewrite.
        let node = CompositionNode::Sequential {
            stages: vec![CompositionNode::Sequential {
                stages: vec![stage("a"), stage("b")],
            }],
        };
        let (out, report) = optimize(node, &[&CanonicalStructural], DEFAULT_MAX_ITERATIONS);
        // Singleton-collapse unwraps the outer Sequential too.
        assert_eq!(
            out,
            CompositionNode::Sequential {
                stages: vec![stage("a"), stage("b")]
            }
        );
        assert_eq!(report.passes_applied, vec!["canonical_structural"]);
        assert!(!report.hit_iteration_cap);
    }

    #[test]
    fn idempotent_after_fixpoint() {
        // Running the pass twice against its own output must be a
        // no-op on the second invocation — that's the fixpoint
        // property the fixpoint runner relies on.
        let node = CompositionNode::Sequential {
            stages: vec![
                CompositionNode::Sequential {
                    stages: vec![stage("a"), stage("b")],
                },
                stage("c"),
            ],
        };
        let (once, _) = CanonicalStructural.rewrite(node);
        let (twice, changed) = CanonicalStructural.rewrite(once.clone());
        assert_eq!(twice, once);
        assert!(!changed);
    }
}
