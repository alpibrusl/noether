//! Graph optimizer — structural AST rewrites between type-check and
//! plan generation.
//!
//! # Where this runs in the pipeline
//!
//! ```text
//! parse → resolve → check_graph → [optimize] → plan → execute
//! ```
//!
//! By the time a pass sees the graph, resolver normalisation has
//! collapsed signature pins to implementation ids and type-check has
//! confirmed the wiring. Optimizer passes are structural: they reshape
//! the tree, never the leaf stage identities, so the `composition_id`
//! computed on the pre-resolution canonical form stays stable across
//! optimization.
//!
//! # Contract for passes
//!
//! Every pass must be a **semantics-preserving** rewrite: the
//! optimized graph must produce the same output as the original for
//! every input the original would accept. A pass that turns a
//! successful graph into a failing one — or changes the output value
//! — is a bug, not an optimization.
//!
//! # Running to fixpoint
//!
//! [`optimize`] iterates passes until no pass reports a change or the
//! iteration cap is hit. The cap exists as a safety against
//! oscillating passes (one flips A → B, another flips B → A) — in
//! correctly-written passes the fixpoint is reached within a handful
//! of iterations.
//!
//! # Current passes
//!
//! - [`canonical_structural::CanonicalStructural`] — lift the M1
//!   structural canonicalisation rules (flatten Sequential, collapse
//!   singleton Sequential, fuse nested Retry) onto the execution
//!   graph so the executor sees the canonical form, not just the
//!   hasher.
//! - [`dead_branch::DeadBranchElimination`] — fold
//!   `Branch { predicate: Const(bool), … }` into the selected arm.
//!
//! Future passes per M3 in `docs/roadmap.md`:
//! `fuse_pure_sequential`, `hoist_invariant`, `memoize_pure`.

pub mod canonical_structural;
pub mod dead_branch;

use crate::lagrange::CompositionNode;

/// Default fixpoint iteration cap. Chosen high enough to handle the
/// deepest realistic composition tree twice over; low enough that an
/// oscillating pass fails loudly rather than looping forever.
pub const DEFAULT_MAX_ITERATIONS: usize = 16;

/// A single structural rewrite of the composition AST.
///
/// Implementers should:
/// - Return `(node, false)` when no change was made.
/// - Recurse into child nodes — the fixpoint runner doesn't walk the
///   tree for you; a pass that only inspects the root will miss
///   rewrites deeper in the graph.
/// - Preserve the `composition_id` invariant: never rename or replace
///   a `Stage` node's `id` field. Structural rewrites (Branch folding,
///   empty-group collapse) are safe; identity rewrites are not.
pub trait OptimizerPass {
    /// Stable, short name for logs and reports. Matches the pass's
    /// canonical name in the roadmap so operators grepping traces
    /// can find what ran.
    fn name(&self) -> &'static str;

    /// Rewrite `node`, returning `(new_node, changed)`.
    ///
    /// `changed == true` means something was structurally different
    /// from the input; this drives fixpoint termination. If the
    /// output is byte-identical to the input, return `false`.
    fn rewrite(&self, node: CompositionNode) -> (CompositionNode, bool);
}

/// Report describing what the optimizer did to a graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptimizerReport {
    /// Names of passes that reported a change, in order of first
    /// firing. A pass that fired on multiple iterations only appears
    /// once — dedup keeps the log readable without losing the
    /// "which passes touched this graph" signal.
    pub passes_applied: Vec<&'static str>,
    /// How many full fixpoint iterations ran. `0` means no pass
    /// changed anything on the first loop.
    pub iterations: usize,
    /// `true` when the iteration cap was hit without convergence.
    /// This is a red flag — either the passes are oscillating or
    /// the graph is pathologically deep.
    pub hit_iteration_cap: bool,
}

impl OptimizerReport {
    fn empty() -> Self {
        Self {
            passes_applied: Vec::new(),
            iterations: 0,
            hit_iteration_cap: false,
        }
    }
}

/// Run the given passes to fixpoint on `node`, capped at
/// `max_iterations` full loops through every pass.
pub fn optimize(
    mut node: CompositionNode,
    passes: &[&dyn OptimizerPass],
    max_iterations: usize,
) -> (CompositionNode, OptimizerReport) {
    let mut report = OptimizerReport::empty();
    if passes.is_empty() {
        return (node, report);
    }
    for iter in 0..max_iterations {
        report.iterations = iter + 1;
        let mut iteration_changed = false;
        for pass in passes {
            let (new_node, changed) = pass.rewrite(node);
            node = new_node;
            if changed {
                iteration_changed = true;
                let name = pass.name();
                if !report.passes_applied.contains(&name) {
                    report.passes_applied.push(name);
                }
            }
        }
        if !iteration_changed {
            return (node, report);
        }
    }
    report.hit_iteration_cap = true;
    (node, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use noether_core::stage::StageId;

    /// Trivial pass: renames a specific stage id once, then stops.
    /// Not a real optimization — used here to verify the fixpoint
    /// runner.
    struct RenameStagePass {
        from: &'static str,
        to: &'static str,
    }

    impl OptimizerPass for RenameStagePass {
        fn name(&self) -> &'static str {
            "test_rename"
        }
        fn rewrite(&self, node: CompositionNode) -> (CompositionNode, bool) {
            if let CompositionNode::Stage {
                id,
                pinning,
                config,
                ..
            } = &node
            {
                if id.0 == self.from {
                    return (
                        CompositionNode::Stage {
                            id: StageId(self.to.into()),
                            pinning: *pinning,
                            config: config.clone(),
                        },
                        true,
                    );
                }
            }
            (node, false)
        }
    }

    /// A pass that never changes anything.
    struct NoopPass;
    impl OptimizerPass for NoopPass {
        fn name(&self) -> &'static str {
            "noop"
        }
        fn rewrite(&self, node: CompositionNode) -> (CompositionNode, bool) {
            (node, false)
        }
    }

    /// A pass that alternates — turns "a" into "b" then "b" into "a"
    /// on successive calls. Exercises the iteration cap.
    struct OscillatingPass;
    impl OptimizerPass for OscillatingPass {
        fn name(&self) -> &'static str {
            "oscillate"
        }
        fn rewrite(&self, node: CompositionNode) -> (CompositionNode, bool) {
            if let CompositionNode::Stage {
                id,
                pinning,
                config,
                ..
            } = &node
            {
                let flipped = if id.0 == "a" { "b" } else { "a" };
                return (
                    CompositionNode::Stage {
                        id: StageId(flipped.into()),
                        pinning: *pinning,
                        config: config.clone(),
                    },
                    true,
                );
            }
            (node, false)
        }
    }

    fn stage(id: &str) -> CompositionNode {
        CompositionNode::stage(id)
    }

    #[test]
    fn empty_pass_list_is_a_noop() {
        let node = stage("a");
        let (out, report) = optimize(node.clone(), &[], DEFAULT_MAX_ITERATIONS);
        assert_eq!(out, node);
        assert!(report.passes_applied.is_empty());
        assert_eq!(report.iterations, 0);
        assert!(!report.hit_iteration_cap);
    }

    #[test]
    fn converges_after_one_successful_pass_and_one_quiet_iteration() {
        // First iter: rename fires (a → b). Second iter: rename
        // matches nothing, reports no change → runner exits.
        let pass = RenameStagePass { from: "a", to: "b" };
        let (out, report) = optimize(stage("a"), &[&pass], DEFAULT_MAX_ITERATIONS);
        assert_eq!(out, stage("b"));
        assert_eq!(report.passes_applied, vec!["test_rename"]);
        assert_eq!(
            report.iterations, 2,
            "runner needs one more quiet iteration to confirm fixpoint"
        );
        assert!(!report.hit_iteration_cap);
    }

    #[test]
    fn noop_pass_produces_empty_report() {
        let pass = NoopPass;
        let (out, report) = optimize(stage("a"), &[&pass], DEFAULT_MAX_ITERATIONS);
        assert_eq!(out, stage("a"));
        assert!(report.passes_applied.is_empty());
        assert_eq!(report.iterations, 1);
        assert!(!report.hit_iteration_cap);
    }

    #[test]
    fn oscillating_pass_hits_the_iteration_cap() {
        // Proves the cap exists and the report flags it. A
        // real-world oscillating pass should fail code review, but
        // the cap keeps the CLI from hanging while it does.
        let pass = OscillatingPass;
        let (_out, report) = optimize(stage("a"), &[&pass], 4);
        assert_eq!(report.iterations, 4);
        assert!(
            report.hit_iteration_cap,
            "oscillating pass must trigger the cap"
        );
    }

    #[test]
    fn pass_applied_names_deduped() {
        // A pass that fires on multiple iterations appears once in
        // the report — log readability.
        struct AlwaysChanging;
        impl OptimizerPass for AlwaysChanging {
            fn name(&self) -> &'static str {
                "always"
            }
            fn rewrite(&self, node: CompositionNode) -> (CompositionNode, bool) {
                (node, true)
            }
        }
        let (_out, report) = optimize(stage("a"), &[&AlwaysChanging], 3);
        assert_eq!(report.passes_applied, vec!["always"]);
        assert!(report.hit_iteration_cap);
    }
}
