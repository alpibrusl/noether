//! Canonicalisation of composition graphs.
//!
//! Rewrites a `CompositionNode` into its canonical form so that
//! semantically equivalent graphs produce the same byte representation
//! (and therefore the same composition ID). The rules implemented here
//! are justified in `docs/architecture/semantics.md`; the property tests
//! in `crates/noether-engine/tests/laws.rs` check every law.
//!
//! Rules (M1):
//!
//! 1. Flatten nested `Sequential` into a single list, left-to-right.
//! 2. Collapse singleton `Sequential[s]` to `s`.
//! 3. `Retry { s, 1, _ }` → `s`.
//! 4. `Retry { Retry { s, n, d }, m, d }` → `Retry { s, n·m, d }`
//!    when both delays match.
//! 5. `Let { {}, body }` → `body`.
//! 6. `BTreeMap` in `Parallel.branches` and `Let.bindings` already
//!    guarantees alphabetical ordering in the serialised form; no
//!    active rewrite needed.
//!
//! What is *not* done in M1 (see semantics.md):
//!
//! - Stage-level identity detection (needs stage metadata, M2).
//! - `Const` absorption (`f >> Const` → `Const`) — deferred to M2 so
//!   this module remains purely structural.
//! - Dead-branch elimination when `Branch.predicate` is `Const`
//!   (optimizer territory, M3).
//!
//! Canonicalisation is idempotent: `canonicalise(canonicalise(g)) ==
//! canonicalise(g)`. That's tested as a law.

use crate::lagrange::ast::CompositionNode;

/// Rewrite a node into its canonical form. Recurses through the tree
/// bottom-up so each parent sees already-canonical children.
pub fn canonicalise(node: &CompositionNode) -> CompositionNode {
    // Canonicalise children first (bottom-up), then apply node-local
    // rewrites. This ordering ensures that e.g. `Sequential
    // [ Sequential [a, b], c ]` becomes `Sequential [a, b, c]` in a
    // single pass: the inner Sequential is already flattened when we
    // look at the outer one.
    let with_canonical_children = canonicalise_children(node);
    canonicalise_node(with_canonical_children)
}

/// Recurse into the structural children of `node`, replacing each with
/// its canonical form. Leaves atomic nodes (`Stage`, `RemoteStage`,
/// `Const`) unchanged — they have no structural children.
fn canonicalise_children(node: &CompositionNode) -> CompositionNode {
    match node {
        CompositionNode::Stage { .. }
        | CompositionNode::RemoteStage { .. }
        | CompositionNode::Const { .. } => node.clone(),

        CompositionNode::Sequential { stages } => CompositionNode::Sequential {
            stages: stages.iter().map(canonicalise).collect(),
        },

        CompositionNode::Parallel { branches } => {
            // BTreeMap preserves ordering; we just recurse into values.
            let branches = branches
                .iter()
                .map(|(k, v)| (k.clone(), canonicalise(v)))
                .collect();
            CompositionNode::Parallel { branches }
        }

        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => CompositionNode::Branch {
            predicate: Box::new(canonicalise(predicate)),
            if_true: Box::new(canonicalise(if_true)),
            if_false: Box::new(canonicalise(if_false)),
        },

        CompositionNode::Fanout { source, targets } => CompositionNode::Fanout {
            source: Box::new(canonicalise(source)),
            targets: targets.iter().map(canonicalise).collect(),
        },

        CompositionNode::Merge { sources, target } => CompositionNode::Merge {
            sources: sources.iter().map(canonicalise).collect(),
            target: Box::new(canonicalise(target)),
        },

        CompositionNode::Retry {
            stage,
            max_attempts,
            delay_ms,
        } => CompositionNode::Retry {
            stage: Box::new(canonicalise(stage)),
            max_attempts: *max_attempts,
            delay_ms: *delay_ms,
        },

        CompositionNode::Let { bindings, body } => {
            let bindings = bindings
                .iter()
                .map(|(k, v)| (k.clone(), canonicalise(v)))
                .collect();
            CompositionNode::Let {
                bindings,
                body: Box::new(canonicalise(body)),
            }
        }
    }
}

/// Apply node-local canonicalisation rules, assuming children are
/// already canonical.
fn canonicalise_node(node: CompositionNode) -> CompositionNode {
    match node {
        // Rule 1 + 2: flatten nested Sequentials and collapse singletons.
        CompositionNode::Sequential { stages } => {
            let flattened: Vec<CompositionNode> = stages
                .into_iter()
                .flat_map(|s| match s {
                    CompositionNode::Sequential { stages: inner } => inner,
                    other => vec![other],
                })
                .collect();

            if flattened.len() == 1 {
                flattened.into_iter().next().unwrap()
            } else {
                CompositionNode::Sequential { stages: flattened }
            }
        }

        // Rule 3 + 4: single-attempt retry is the inner stage; nested
        // retries with matching delay multiply attempts.
        CompositionNode::Retry {
            stage,
            max_attempts,
            delay_ms,
        } => {
            if max_attempts <= 1 {
                return *stage;
            }
            if let CompositionNode::Retry {
                stage: inner_stage,
                max_attempts: inner_attempts,
                delay_ms: inner_delay,
            } = *stage
            {
                if inner_delay == delay_ms {
                    let combined = max_attempts.saturating_mul(inner_attempts);
                    // Defensive: the combined Retry could itself be subject to
                    // further node-local collapse (e.g. combined == 1). Re-feed
                    // through canonicalise_node so one pass is enough
                    // regardless of input tree shape. Proptest L12 locks
                    // `canonicalise(canonicalise(g)) == canonicalise(g)`.
                    return canonicalise_node(CompositionNode::Retry {
                        stage: inner_stage,
                        max_attempts: combined,
                        delay_ms,
                    });
                }
                // Different delays: keep the nesting so timing behaviour is
                // preserved. The inner stage is already canonical because
                // children were processed bottom-up.
                return CompositionNode::Retry {
                    stage: Box::new(CompositionNode::Retry {
                        stage: inner_stage,
                        max_attempts: inner_attempts,
                        delay_ms: inner_delay,
                    }),
                    max_attempts,
                    delay_ms,
                };
            }
            CompositionNode::Retry {
                stage,
                max_attempts,
                delay_ms,
            }
        }

        // Rule 5: empty Let is the body.
        CompositionNode::Let { bindings, body } => {
            if bindings.is_empty() {
                return *body;
            }
            CompositionNode::Let { bindings, body }
        }

        // Other nodes have no local rewrites in M1.
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noether_core::stage::StageId;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn stage(id: &str) -> CompositionNode {
        CompositionNode::Stage {
            id: StageId(id.into()),
            config: None,
        }
    }

    #[test]
    fn atomic_nodes_unchanged() {
        assert_eq!(canonicalise(&stage("a")), stage("a"));

        let c = CompositionNode::Const { value: json!(42) };
        assert_eq!(canonicalise(&c), c);
    }

    #[test]
    fn sequential_singleton_collapses() {
        let g = CompositionNode::Sequential {
            stages: vec![stage("a")],
        };
        assert_eq!(canonicalise(&g), stage("a"));
    }

    #[test]
    fn sequential_nested_flattens_left() {
        // Sequential [ Sequential [a, b], c ]  ->  Sequential [a, b, c]
        let g = CompositionNode::Sequential {
            stages: vec![
                CompositionNode::Sequential {
                    stages: vec![stage("a"), stage("b")],
                },
                stage("c"),
            ],
        };
        let expected = CompositionNode::Sequential {
            stages: vec![stage("a"), stage("b"), stage("c")],
        };
        assert_eq!(canonicalise(&g), expected);
    }

    #[test]
    fn sequential_nested_flattens_right() {
        let g = CompositionNode::Sequential {
            stages: vec![
                stage("a"),
                CompositionNode::Sequential {
                    stages: vec![stage("b"), stage("c")],
                },
            ],
        };
        let expected = CompositionNode::Sequential {
            stages: vec![stage("a"), stage("b"), stage("c")],
        };
        assert_eq!(canonicalise(&g), expected);
    }

    #[test]
    fn sequential_deeply_nested_flattens() {
        // Sequential [ Sequential [a, Sequential [b, c]], Sequential [d] ]
        let g = CompositionNode::Sequential {
            stages: vec![
                CompositionNode::Sequential {
                    stages: vec![
                        stage("a"),
                        CompositionNode::Sequential {
                            stages: vec![stage("b"), stage("c")],
                        },
                    ],
                },
                CompositionNode::Sequential {
                    stages: vec![stage("d")],
                },
            ],
        };
        let expected = CompositionNode::Sequential {
            stages: vec![stage("a"), stage("b"), stage("c"), stage("d")],
        };
        assert_eq!(canonicalise(&g), expected);
    }

    #[test]
    fn retry_single_attempt_collapses() {
        let g = CompositionNode::Retry {
            stage: Box::new(stage("a")),
            max_attempts: 1,
            delay_ms: Some(500),
        };
        assert_eq!(canonicalise(&g), stage("a"));
    }

    #[test]
    fn retry_zero_attempts_also_collapses() {
        // Defensive: max_attempts=0 shouldn't wrap at all.
        let g = CompositionNode::Retry {
            stage: Box::new(stage("a")),
            max_attempts: 0,
            delay_ms: None,
        };
        assert_eq!(canonicalise(&g), stage("a"));
    }

    #[test]
    fn retry_nested_same_delay_multiplies() {
        // Retry { Retry { s, 3, 100 }, 4, 100 }  ->  Retry { s, 12, 100 }
        let g = CompositionNode::Retry {
            stage: Box::new(CompositionNode::Retry {
                stage: Box::new(stage("a")),
                max_attempts: 3,
                delay_ms: Some(100),
            }),
            max_attempts: 4,
            delay_ms: Some(100),
        };
        let expected = CompositionNode::Retry {
            stage: Box::new(stage("a")),
            max_attempts: 12,
            delay_ms: Some(100),
        };
        assert_eq!(canonicalise(&g), expected);
    }

    #[test]
    fn retry_nested_different_delay_preserved() {
        let g = CompositionNode::Retry {
            stage: Box::new(CompositionNode::Retry {
                stage: Box::new(stage("a")),
                max_attempts: 3,
                delay_ms: Some(100),
            }),
            max_attempts: 4,
            delay_ms: Some(200),
        };
        // Should stay nested — different delays produce observably
        // different timing behaviour.
        let canonical = canonicalise(&g);
        match canonical {
            CompositionNode::Retry {
                stage: outer_stage,
                max_attempts: 4,
                delay_ms: Some(200),
            } => match *outer_stage {
                CompositionNode::Retry {
                    max_attempts: 3,
                    delay_ms: Some(100),
                    ..
                } => {}
                other => panic!("expected inner Retry, got {:?}", other),
            },
            other => panic!("expected outer Retry, got {:?}", other),
        }
    }

    #[test]
    fn empty_let_collapses_to_body() {
        let g = CompositionNode::Let {
            bindings: BTreeMap::new(),
            body: Box::new(stage("body")),
        };
        assert_eq!(canonicalise(&g), stage("body"));
    }

    #[test]
    fn non_empty_let_preserved() {
        let mut bindings = BTreeMap::new();
        bindings.insert("x".into(), stage("compute_x"));
        let g = CompositionNode::Let {
            bindings: bindings.clone(),
            body: Box::new(stage("body")),
        };
        assert_eq!(canonicalise(&g), g);
    }

    #[test]
    fn canonicalise_is_idempotent() {
        // Property L12: canonicalise(canonicalise(g)) == canonicalise(g)
        let g = CompositionNode::Sequential {
            stages: vec![
                CompositionNode::Sequential {
                    stages: vec![stage("a"), stage("b")],
                },
                CompositionNode::Retry {
                    stage: Box::new(stage("c")),
                    max_attempts: 1,
                    delay_ms: None,
                },
                CompositionNode::Let {
                    bindings: BTreeMap::new(),
                    body: Box::new(stage("d")),
                },
            ],
        };
        let once = canonicalise(&g);
        let twice = canonicalise(&once);
        assert_eq!(once, twice);
    }

    #[test]
    fn parallel_branches_preserved_under_btreemap() {
        // BTreeMap serialises keys alphabetically, so two Parallels
        // built from the same {name → branch} set should compare equal
        // regardless of insertion order.
        let mut a = BTreeMap::new();
        a.insert("alpha".into(), stage("x"));
        a.insert("beta".into(), stage("y"));

        let mut b = BTreeMap::new();
        b.insert("beta".into(), stage("y"));
        b.insert("alpha".into(), stage("x"));

        let g1 = CompositionNode::Parallel { branches: a };
        let g2 = CompositionNode::Parallel { branches: b };

        assert_eq!(canonicalise(&g1), canonicalise(&g2));
    }

    #[test]
    fn fanout_target_order_preserved() {
        // Fanout is ordered; two Fanouts with permuted targets are NOT
        // semantically equivalent (output is [t1(s), t2(s)] vs
        // [t2(s), t1(s)]).
        let g1 = CompositionNode::Fanout {
            source: Box::new(stage("src")),
            targets: vec![stage("a"), stage("b")],
        };
        let g2 = CompositionNode::Fanout {
            source: Box::new(stage("src")),
            targets: vec![stage("b"), stage("a")],
        };
        assert_ne!(canonicalise(&g1), canonicalise(&g2));
    }

    #[test]
    fn inner_canonicalisation_bubbles_up() {
        // A Sequential whose child is a collapsible Retry should end up
        // with the unwrapped stage after one canonicalise call.
        let g = CompositionNode::Sequential {
            stages: vec![
                stage("a"),
                CompositionNode::Retry {
                    stage: Box::new(stage("b")),
                    max_attempts: 1,
                    delay_ms: Some(50),
                },
                stage("c"),
            ],
        };
        let expected = CompositionNode::Sequential {
            stages: vec![stage("a"), stage("b"), stage("c")],
        };
        assert_eq!(canonicalise(&g), expected);
    }
}
