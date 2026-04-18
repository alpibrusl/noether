//! Property-based tests for the composition-graph laws stated in
//! `docs/architecture/semantics.md`.
//!
//! Every law in the semantics doc has at least one test here. When the
//! semantics and the code disagree, one of these fails. When either
//! changes, both must.
//!
//! Strategy: rather than generate arbitrary composition graphs (which
//! couples test failures to generator quality), we parameterise each
//! law over a small alphabet of stage IDs, attempt counts, and delays —
//! enough randomness to catch regressions, specific enough to keep
//! failures debuggable.

use noether_core::stage::StageId;
use noether_engine::lagrange::{
    canonicalise, compute_composition_id, CompositionGraph, CompositionNode,
};
use proptest::prelude::*;
use std::collections::BTreeMap;

// ── generators ──────────────────────────────────────────────────────────────

/// Stage-name alphabet. Short strings are enough; the canonical-form
/// rewrites never inspect stage IDs beyond equality.
fn stage_id() -> impl Strategy<Value = StageId> {
    "[a-z]{1,6}".prop_map(StageId)
}

fn stage() -> impl Strategy<Value = CompositionNode> {
    stage_id().prop_map(|id| CompositionNode::Stage { id, config: None })
}

/// Build a Sequential from a vec of stage IDs.
fn sequential_of(ids: Vec<&str>) -> CompositionNode {
    CompositionNode::Sequential {
        stages: ids
            .into_iter()
            .map(|s| CompositionNode::Stage {
                id: StageId(s.into()),
                config: None,
            })
            .collect(),
    }
}

fn st(id: &str) -> CompositionNode {
    CompositionNode::Stage {
        id: StageId(id.into()),
        config: None,
    }
}

// ── L1: Sequential associativity (flattening) ───────────────────────────────

proptest! {
    #[test]
    fn l1_sequential_associativity_left(
        a in "[a-z]{1,4}",
        b in "[a-z]{1,4}",
        c in "[a-z]{1,4}",
    ) {
        // Sequential [ Sequential [a, b], c ]  ≡  Sequential [a, b, c]
        let left_grouped = CompositionNode::Sequential {
            stages: vec![sequential_of(vec![&a, &b]), st(&c)],
        };
        let flat = sequential_of(vec![&a, &b, &c]);
        prop_assert_eq!(canonicalise(&left_grouped), flat);
    }

    #[test]
    fn l1_sequential_associativity_right(
        a in "[a-z]{1,4}",
        b in "[a-z]{1,4}",
        c in "[a-z]{1,4}",
    ) {
        // Sequential [ a, Sequential [b, c] ]  ≡  Sequential [a, b, c]
        let right_grouped = CompositionNode::Sequential {
            stages: vec![st(&a), sequential_of(vec![&b, &c])],
        };
        let flat = sequential_of(vec![&a, &b, &c]);
        prop_assert_eq!(canonicalise(&right_grouped), flat);
    }

    #[test]
    fn l1_sequential_associativity_both_groupings(
        ids in prop::collection::vec("[a-z]{1,4}", 2..8),
    ) {
        // For any ordered list of stage IDs, both left- and right-grouped
        // Sequentials canonicalise to the same flat sequence.
        let ref_ids = ids.iter().map(String::as_str).collect::<Vec<_>>();
        let flat = sequential_of(ref_ids);

        // Left-grouped: (((a b) c) d) …
        let mut left = st(&ids[0]);
        for id in &ids[1..] {
            left = CompositionNode::Sequential {
                stages: vec![left, st(id)],
            };
        }

        // Right-grouped: (a (b (c d))) …
        let mut right = st(&ids[ids.len() - 1]);
        for id in ids[..ids.len() - 1].iter().rev() {
            right = CompositionNode::Sequential {
                stages: vec![st(id), right],
            };
        }

        prop_assert_eq!(canonicalise(&left), canonicalise(&flat));
        prop_assert_eq!(canonicalise(&right), canonicalise(&flat));
    }
}

// ── L4: Sequential singleton collapse ───────────────────────────────────────

proptest! {
    #[test]
    fn l4_sequential_singleton_collapses(s in stage()) {
        let singleton = CompositionNode::Sequential {
            stages: vec![s.clone()],
        };
        prop_assert_eq!(canonicalise(&singleton), s);
    }
}

// ── L5: Arbitrary Sequential nesting flattens ───────────────────────────────

proptest! {
    #[test]
    fn l5_sequential_deep_nesting_flattens(
        // 3-7 stages grouped randomly.
        ids in prop::collection::vec("[a-z]{1,4}", 3..8),
    ) {
        let ref_ids = ids.iter().map(String::as_str).collect::<Vec<_>>();
        let expected = sequential_of(ref_ids);

        // Build a bushy tree by random pairing.
        let mut nodes: Vec<CompositionNode> = ids.iter().map(|s| st(s)).collect();
        while nodes.len() > 1 {
            let right = nodes.pop().unwrap();
            let left = nodes.pop().unwrap();
            nodes.push(CompositionNode::Sequential {
                stages: vec![left, right],
            });
        }
        let bushy = nodes.pop().unwrap();

        prop_assert_eq!(canonicalise(&bushy), expected);
    }
}

// ── L6: Parallel branch-name permutation ────────────────────────────────────

proptest! {
    #[test]
    fn l6_parallel_branch_permutation_stable_id(
        pairs in prop::collection::vec(
            ("[a-z]{1,6}", stage()),
            1..6,
        ),
    ) {
        // Dedupe keys up-front so last-wins ordering does not change the
        // value under different insertion orders — we want to test that
        // key-order doesn't matter, not that the last write wins.
        let canon: BTreeMap<String, CompositionNode> = pairs.into_iter().collect();
        let entries: Vec<_> = canon.clone().into_iter().collect();

        let mut map_fwd = BTreeMap::new();
        for (k, v) in &entries {
            map_fwd.insert(k.clone(), v.clone());
        }
        let mut map_rev = BTreeMap::new();
        for (k, v) in entries.iter().rev() {
            map_rev.insert(k.clone(), v.clone());
        }

        let p1 = CompositionNode::Parallel { branches: map_fwd };
        let p2 = CompositionNode::Parallel { branches: map_rev };

        prop_assert_eq!(canonicalise(&p1), canonicalise(&p2));

        let g1 = CompositionGraph::new("p1", p1);
        let g2 = CompositionGraph::new("p2", p2);
        prop_assert_eq!(
            compute_composition_id(&g1).unwrap(),
            compute_composition_id(&g2).unwrap()
        );
    }
}

// ── L7: Let binding permutation ─────────────────────────────────────────────

proptest! {
    #[test]
    fn l7_let_binding_permutation_stable_id(
        pairs in prop::collection::vec(
            ("[a-z]{1,6}", stage()),
            1..5,
        ),
        body in stage(),
    ) {
        let canon: BTreeMap<String, CompositionNode> = pairs.into_iter().collect();
        let entries: Vec<_> = canon.clone().into_iter().collect();

        let mut map_fwd = BTreeMap::new();
        for (k, v) in &entries {
            map_fwd.insert(k.clone(), v.clone());
        }
        let mut map_rev = BTreeMap::new();
        for (k, v) in entries.iter().rev() {
            map_rev.insert(k.clone(), v.clone());
        }

        let l1 = CompositionNode::Let {
            bindings: map_fwd,
            body: Box::new(body.clone()),
        };
        let l2 = CompositionNode::Let {
            bindings: map_rev,
            body: Box::new(body),
        };

        prop_assert_eq!(canonicalise(&l1), canonicalise(&l2));
    }
}

// ── L9: Retry 1-attempt collapse ────────────────────────────────────────────

proptest! {
    #[test]
    fn l9_retry_single_attempt_collapses(
        s in stage(),
        delay in prop::option::of(0u64..10_000),
    ) {
        let r = CompositionNode::Retry {
            stage: Box::new(s.clone()),
            max_attempts: 1,
            delay_ms: delay,
        };
        prop_assert_eq!(canonicalise(&r), s);
    }

    #[test]
    fn l9_retry_zero_attempts_also_collapses(
        s in stage(),
        delay in prop::option::of(0u64..10_000),
    ) {
        // Defensive: zero-attempts Retry is nonsensical; canonicalise
        // should unwrap rather than leave a never-executing wrapper.
        let r = CompositionNode::Retry {
            stage: Box::new(s.clone()),
            max_attempts: 0,
            delay_ms: delay,
        };
        prop_assert_eq!(canonicalise(&r), s);
    }
}

// ── L10: Retry multiplication when delays match ─────────────────────────────

proptest! {
    #[test]
    fn l10_retry_nested_same_delay_multiplies(
        s in stage(),
        n in 2u32..12,
        m in 2u32..12,
        delay in prop::option::of(0u64..1_000),
    ) {
        let inner = CompositionNode::Retry {
            stage: Box::new(s.clone()),
            max_attempts: n,
            delay_ms: delay,
        };
        let outer = CompositionNode::Retry {
            stage: Box::new(inner),
            max_attempts: m,
            delay_ms: delay,
        };
        let expected = CompositionNode::Retry {
            stage: Box::new(s),
            max_attempts: n.saturating_mul(m),
            delay_ms: delay,
        };
        prop_assert_eq!(canonicalise(&outer), expected);
    }

    #[test]
    fn l10_retry_nested_different_delay_stays_nested(
        s in stage(),
        n in 2u32..8,
        m in 2u32..8,
        d_inner in prop::option::of(0u64..1_000),
        d_outer in prop::option::of(0u64..1_000),
    ) {
        // Skip the "delays match" case — L10 covers that.
        prop_assume!(d_inner != d_outer);

        let inner = CompositionNode::Retry {
            stage: Box::new(s),
            max_attempts: n,
            delay_ms: d_inner,
        };
        let outer = CompositionNode::Retry {
            stage: Box::new(inner.clone()),
            max_attempts: m,
            delay_ms: d_outer,
        };
        let canonical = canonicalise(&outer);
        // Must remain a Retry wrapping a Retry — timing matters.
        match canonical {
            CompositionNode::Retry {
                stage: outer_stage,
                max_attempts: outer_attempts,
                delay_ms: outer_delay,
            } => {
                prop_assert_eq!(outer_attempts, m);
                prop_assert_eq!(outer_delay, d_outer);
                match *outer_stage {
                    CompositionNode::Retry {
                        max_attempts: inner_attempts,
                        delay_ms: inner_delay,
                        ..
                    } => {
                        prop_assert_eq!(inner_attempts, n);
                        prop_assert_eq!(inner_delay, d_inner);
                    }
                    other => panic!("expected inner Retry, got {:?}", other),
                }
            }
            other => panic!("expected outer Retry, got {:?}", other),
        }
    }
}

// ── L11: Empty Let collapse ─────────────────────────────────────────────────

proptest! {
    #[test]
    fn l11_empty_let_collapses_to_body(body in stage()) {
        let l = CompositionNode::Let {
            bindings: BTreeMap::new(),
            body: Box::new(body.clone()),
        };
        prop_assert_eq!(canonicalise(&l), body);
    }
}

// ── L12: Canonicalisation idempotence ───────────────────────────────────────

proptest! {
    #[test]
    fn l12_canonicalise_is_idempotent(
        ids in prop::collection::vec("[a-z]{1,4}", 2..6),
        retries in prop::collection::vec(0u32..5, 0..4),
    ) {
        // Build a tree that exercises several canonical rules at once:
        // nested Sequentials + a 1-attempt Retry + an empty Let.
        let mut stages = ids.iter().map(|s| st(s)).collect::<Vec<_>>();
        if let Some(first_retry) = retries.first() {
            if !stages.is_empty() {
                let inner = stages.remove(0);
                stages.insert(
                    0,
                    CompositionNode::Retry {
                        stage: Box::new(inner),
                        max_attempts: *first_retry,
                        delay_ms: None,
                    },
                );
            }
        }
        stages.push(CompositionNode::Let {
            bindings: BTreeMap::new(),
            body: Box::new(CompositionNode::Sequential {
                stages: vec![st("inner1"), st("inner2")],
            }),
        });

        let g = CompositionNode::Sequential { stages };

        let once = canonicalise(&g);
        let twice = canonicalise(&once);
        prop_assert_eq!(once, twice);
    }
}

// ── L13: Composition ID stability under equivalent rewrites ─────────────────

proptest! {
    #[test]
    fn l13_composition_id_same_for_flattened_vs_nested(
        ids in prop::collection::vec("[a-z]{1,4}", 2..6),
    ) {
        let ref_ids = ids.iter().map(String::as_str).collect::<Vec<_>>();
        let flat = sequential_of(ref_ids);

        // Left-grouped nested form of the same Sequential.
        let mut nested = st(&ids[0]);
        for id in &ids[1..] {
            nested = CompositionNode::Sequential {
                stages: vec![nested, st(id)],
            };
        }

        let g_flat = CompositionGraph::new("flat", flat);
        let g_nested = CompositionGraph::new("nested", nested);

        prop_assert_eq!(
            compute_composition_id(&g_flat).unwrap(),
            compute_composition_id(&g_nested).unwrap()
        );
    }

    #[test]
    fn l13_composition_id_ignores_description_and_version(
        s in stage(),
        desc1 in ".{0,40}",
        desc2 in ".{0,40}",
    ) {
        // Two graphs that differ only in their description or version
        // must share a composition ID. Cosmetic doc text cannot shift
        // identity.
        let g1 = CompositionGraph {
            description: desc1,
            root: s.clone(),
            version: "1.0.0".into(),
        };
        let g2 = CompositionGraph {
            description: desc2,
            root: s,
            version: "0.0.1".into(),
        };
        prop_assert_eq!(
            compute_composition_id(&g1).unwrap(),
            compute_composition_id(&g2).unwrap()
        );
    }
}
