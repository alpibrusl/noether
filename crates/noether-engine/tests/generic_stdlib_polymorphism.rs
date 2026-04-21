//! End-to-end tests for the generic stdlib stages landed in M3 slice 3.
//!
//! Each test builds a pipeline that pipes a concrete upstream into a
//! polymorphic stdlib stage and asserts that `check_graph` resolves
//! to the concrete type — proving slice 2b's substitution threading
//! actually reaches the stdlib.
//!
//! Before slice 2b, the resolved type would have been `<T>` rather
//! than `Number` / `Text` / `Bool`; the `is_subtype_of` permissive
//! Var short-circuit still passed the check, but the resolved output
//! carried no useful type information. These tests pin the post-2b
//! behaviour so a regression in substitution threading would make
//! them fail loudly.

use noether_core::stage::{CostEstimate, Stage, StageId, StageLifecycle, StageSignature};
use noether_core::stdlib::load_stdlib;
use noether_core::types::NType;
use noether_engine::checker::check_graph;
use noether_engine::lagrange::{CompositionNode, Pinning};
use noether_store::{MemoryStore, StageStore};
use std::collections::BTreeSet;

/// Build a store seeded with the full stdlib plus a handful of
/// hand-rolled stages that give us concrete types to feed into the
/// polymorphic ones.
fn init_store() -> MemoryStore {
    let mut store = MemoryStore::new();
    for stage in load_stdlib() {
        store.put(stage).unwrap();
    }
    // Probe stages: provide concrete upstream / downstream shapes.
    store
        .put(make_stage("text_to_num", NType::Text, NType::Number))
        .unwrap();
    store
        .put(make_stage(
            "num_to_list",
            NType::Number,
            NType::List(Box::new(NType::Number)),
        ))
        .unwrap();
    store
        .put(make_stage(
            "text_to_bool_list",
            NType::Text,
            NType::List(Box::new(NType::Bool)),
        ))
        .unwrap();
    store
}

fn make_stage(id: &str, input: NType, output: NType) -> Stage {
    Stage {
        id: StageId(id.into()),
        signature_id: None,
        signature: StageSignature {
            input,
            output,
            effects: noether_core::effects::EffectSet::pure(),
            implementation_hash: format!("impl_{id}"),
        },
        capabilities: BTreeSet::new(),
        cost: CostEstimate {
            time_ms_p50: Some(1),
            tokens_est: None,
            memory_mb: None,
        },
        description: format!("probe stage {id}"),
        examples: vec![],
        lifecycle: StageLifecycle::Active,
        ed25519_signature: None,
        signer_public_key: None,
        implementation_code: None,
        implementation_language: None,
        ui_style: None,
        tags: vec![],
        aliases: vec![],
        name: None,
        properties: Vec::new(),
    }
}

/// Find the first active stdlib stage whose `name` matches. Panics
/// if not found; the stage set is loaded directly from `load_stdlib`
/// so a mismatch means the generic stages didn't register.
fn find_stdlib(store: &MemoryStore, name: &str) -> StageId {
    store
        .list(None)
        .into_iter()
        .find(|s| s.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("stdlib stage '{name}' not found"))
        .id
        .clone()
}

fn stage_ref(id: StageId) -> CompositionNode {
    CompositionNode::Stage {
        id,
        pinning: Pinning::Signature,
        config: None,
    }
}

fn probe(name: &str) -> CompositionNode {
    CompositionNode::Stage {
        id: StageId(name.into()),
        pinning: Pinning::Both,
        config: None,
    }
}

#[test]
fn identity_resolves_to_concrete_output() {
    // text_to_num (Text -> Number) >> identity (<T> -> <T>)
    // Resolved output must be Number — slice 2b's substitution threading
    // binds <T> to Number at the edge.
    let store = init_store();
    let id = find_stdlib(&store, "identity");
    let graph = CompositionNode::Sequential {
        stages: vec![probe("text_to_num"), stage_ref(id)],
    };
    let check = check_graph(&graph, &store).expect("identity composition must type-check");
    assert_eq!(check.resolved.input, NType::Text);
    assert_eq!(
        check.resolved.output,
        NType::Number,
        "identity's <T> must be bound to Number after unification — \
         if the resolved output is a Var, slice 2b regressed"
    );
}

#[test]
fn chained_identity_propagates_through_multiple_hops() {
    // text_to_num >> identity >> identity
    // Two hops; substitution must survive across both.
    let store = init_store();
    let id = find_stdlib(&store, "identity");
    let graph = CompositionNode::Sequential {
        stages: vec![probe("text_to_num"), stage_ref(id.clone()), stage_ref(id)],
    };
    let check = check_graph(&graph, &store).expect("chained identity must type-check");
    assert_eq!(check.resolved.output, NType::Number);
}

#[test]
fn head_of_concrete_list_resolves_to_element_type() {
    // num_to_list (Number -> List<Number>) >> head (List<<T>> -> <T>)
    // head's output is Number, not <T>.
    let store = init_store();
    let head_id = find_stdlib(&store, "head");
    let graph = CompositionNode::Sequential {
        stages: vec![probe("num_to_list"), stage_ref(head_id)],
    };
    let check = check_graph(&graph, &store).expect("head composition must type-check");
    assert_eq!(
        check.resolved.output,
        NType::Number,
        "head's <T> must be bound to Number"
    );
}

#[test]
fn tail_preserves_list_element_type() {
    // num_to_list >> tail
    // tail's output is List<Number>, not List<<T>>.
    let store = init_store();
    let tail_id = find_stdlib(&store, "tail");
    let graph = CompositionNode::Sequential {
        stages: vec![probe("num_to_list"), stage_ref(tail_id)],
    };
    let check = check_graph(&graph, &store).expect("tail composition must type-check");
    assert_eq!(
        check.resolved.output,
        NType::List(Box::new(NType::Number)),
        "tail's List<<T>> must be bound to List<Number>"
    );
}

#[test]
fn mark_done_preserves_upstream_fields_via_row_polymorphism() {
    // M3 row-poly: mark_done has signature
    //   input:  RecordWith { fields: {}, rest: R }
    //   output: RecordWith { fields: { done: Bool }, rest: R }
    //
    // Piping a concrete Record { name: Text, age: Number } into it
    // should resolve the output to
    //   Record { name: Text, age: Number, done: Bool }
    // — proving the row variable actually captured and carried through
    // the upstream's extra fields, not silently dropped them.
    use std::collections::BTreeMap;
    let mut store = init_store();
    // Upstream: produces a concrete record with two fields.
    store
        .put(make_stage(
            "make_person",
            NType::Text,
            NType::record([("name", NType::Text), ("age", NType::Number)]),
        ))
        .unwrap();
    let mark_done_id = find_stdlib(&store, "mark_done");
    let graph = CompositionNode::Sequential {
        stages: vec![probe("make_person"), stage_ref(mark_done_id)],
    };
    let check = check_graph(&graph, &store).expect("mark_done composition must type-check");

    let expected: BTreeMap<String, NType> = [
        ("age".to_string(), NType::Number),
        ("done".to_string(), NType::Bool),
        ("name".to_string(), NType::Text),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        check.resolved.output,
        NType::Record(expected),
        "row variable must have bound the upstream's name+age fields so the \
         output is a closed Record with name, age, done"
    );
}

#[test]
fn head_then_identity_binds_both_vars_to_same_concrete() {
    // num_to_list >> head >> identity
    // head binds its <T> to Number. identity's independent <T> must
    // then bind to Number too (different variable name, same concrete).
    let store = init_store();
    let head_id = find_stdlib(&store, "head");
    let identity_id = find_stdlib(&store, "identity");
    let graph = CompositionNode::Sequential {
        stages: vec![
            probe("num_to_list"),
            stage_ref(head_id),
            stage_ref(identity_id),
        ],
    };
    let check = check_graph(&graph, &store).expect("chain must type-check");
    assert_eq!(
        check.resolved.output,
        NType::Number,
        "identity after head must carry Number through, not a fresh Var"
    );
}
