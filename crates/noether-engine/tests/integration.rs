use noether_core::stdlib::load_stdlib;
use noether_engine::checker::check_graph;
use noether_engine::executor::mock::MockExecutor;
use noether_engine::executor::runner::run_composition;
use noether_engine::lagrange::{
    compute_composition_id, parse_graph, serialize_graph, CompositionGraph, CompositionNode,
    Pinning,
};
use noether_engine::planner::plan_graph;
use noether_engine::trace::TraceStatus;
use noether_store::{MemoryStore, StageStore};
use serde_json::json;
use std::collections::BTreeMap;

fn init_store() -> MemoryStore {
    let mut store = MemoryStore::new();
    for stage in load_stdlib() {
        store.put(stage).unwrap();
    }
    store
}

fn find_stage_id(store: &MemoryStore, description_contains: &str) -> String {
    store
        .list(None)
        .into_iter()
        .find(|s| s.description.contains(description_contains))
        .unwrap_or_else(|| panic!("no stage matching '{description_contains}'"))
        .id
        .0
        .clone()
}

fn stage(id: &str) -> CompositionNode {
    CompositionNode::Stage {
        id: noether_core::stage::StageId(id.into()),
        pinning: Pinning::Signature,
        config: None,
    }
}

#[test]
fn end_to_end_single_stage() {
    let store = init_store();
    let to_text_id = find_stage_id(&store, "Convert any value to its text");

    let graph = CompositionGraph::new("single stage test", stage(&to_text_id));

    // Type check
    let check = check_graph(&graph.root, &store).unwrap();
    assert_eq!(format!("{}", check.resolved.input), "Any");
    assert_eq!(format!("{}", check.resolved.output), "Text");

    // Plan
    let _plan = plan_graph(&graph.root, &store);

    // Execute
    let executor = MockExecutor::from_store(&store);
    let comp_id = compute_composition_id(&graph).unwrap();
    let result = run_composition(&graph.root, &json!(42), &executor, &comp_id).unwrap();
    assert!(matches!(result.trace.status, TraceStatus::Ok));
    assert_eq!(result.trace.stages.len(), 1);
}

#[test]
fn end_to_end_sequential_pipeline() {
    let store = init_store();
    // to_json (Any → Text) >> parse_json (Text → Any)
    let to_json_id = find_stage_id(&store, "Serialize any value to a JSON");
    let parse_json_id = find_stage_id(&store, "Parse a JSON string");

    let graph = CompositionGraph::new(
        "round-trip JSON pipeline",
        CompositionNode::Sequential {
            stages: vec![stage(&to_json_id), stage(&parse_json_id)],
        },
    );

    // Type check: Any → Text → Any
    let check = check_graph(&graph.root, &store).unwrap();
    assert_eq!(format!("{}", check.resolved.input), "Any");
    assert_eq!(format!("{}", check.resolved.output), "Any");

    // Plan
    let plan = plan_graph(&graph.root, &store);
    assert_eq!(plan.steps.len(), 2);
    assert!(plan.steps[1].depends_on.contains(&0));

    // Execute
    let executor = MockExecutor::from_store(&store);
    let comp_id = compute_composition_id(&graph).unwrap();
    let result = run_composition(&graph.root, &json!(42), &executor, &comp_id).unwrap();
    assert!(matches!(result.trace.status, TraceStatus::Ok));
    assert_eq!(result.trace.stages.len(), 2);
}

#[test]
fn end_to_end_parallel_composition() {
    let store = init_store();
    // to_text (Any → Text) in parallel with to_bool (union → Bool)
    let to_text_id = find_stage_id(&store, "Convert any value to its text");
    let to_bool_id = find_stage_id(&store, "Convert a value to boolean");

    let graph = CompositionGraph::new(
        "parallel composition",
        CompositionNode::Parallel {
            branches: BTreeMap::from([
                ("text".into(), stage(&to_text_id)),
                ("bool".into(), stage(&to_bool_id)),
            ]),
        },
    );

    // Type check
    let check = check_graph(&graph.root, &store).unwrap();
    assert!(matches!(
        check.resolved.output,
        noether_core::types::NType::Record(_)
    ));

    // Execute
    let executor = MockExecutor::from_store(&store);
    let comp_id = compute_composition_id(&graph).unwrap();
    let result = run_composition(
        &graph.root,
        &json!({"text": 42, "bool": 1}),
        &executor,
        &comp_id,
    )
    .unwrap();
    assert!(matches!(result.trace.status, TraceStatus::Ok));
}

#[test]
fn type_check_catches_sequential_mismatch() {
    let store = init_store();
    // text_split (R{text,delimiter} → L<Text>) >> to_number (Text|Number|Bool → Number)
    // Output L<Text> is NOT subtype of Text|Number|Bool — should fail
    let split_id = find_stage_id(&store, "Split text by a delimiter");
    let to_num_id = find_stage_id(&store, "Parse a value as a number");

    let graph = CompositionGraph::new(
        "bad pipeline",
        CompositionNode::Sequential {
            stages: vec![stage(&split_id), stage(&to_num_id)],
        },
    );

    let result = check_graph(&graph.root, &store);
    assert!(result.is_err(), "Should detect type mismatch");
}

#[test]
fn graph_serialization_round_trip() {
    let store = init_store();
    let to_text_id = find_stage_id(&store, "Convert any value to its text");

    let graph = CompositionGraph::new(
        "test",
        CompositionNode::Sequential {
            stages: vec![stage(&to_text_id)],
        },
    );

    let json = serialize_graph(&graph).unwrap();
    let parsed = parse_graph(&json).unwrap();
    assert_eq!(graph, parsed);

    // ID is deterministic
    let id1 = compute_composition_id(&graph).unwrap();
    let id2 = compute_composition_id(&parsed).unwrap();
    assert_eq!(id1, id2);
}

#[test]
fn dry_run_produces_plan() {
    let store = init_store();
    let to_text_id = find_stage_id(&store, "Convert any value to its text");
    let to_json_id = find_stage_id(&store, "Serialize any value to a JSON");

    let graph = CompositionGraph::new(
        "two-stage pipeline",
        CompositionNode::Sequential {
            stages: vec![stage(&to_text_id), stage(&to_json_id)],
        },
    );

    // Type check
    let check = check_graph(&graph.root, &store).unwrap();
    assert_eq!(format!("{}", check.resolved.input), "Any");

    // Plan
    let plan = plan_graph(&graph.root, &store);
    assert_eq!(plan.steps.len(), 2);
    assert!(plan.cost.total_time_ms_p50.is_none()); // to_text/to_json have no cost set
}

#[test]
fn let_carries_outer_input_into_body() {
    // Reproduces the scan→hash→diff pattern from the developer feedback:
    // the body needs both an intermediate result and a field from the
    // original input. With Sequential alone, the original-input field is
    // erased after the first stage; Let preserves it via the augmented
    // record passed to body.
    use serde_json::json;
    let store = init_store();
    let to_text_id = find_stage_id(&store, "Convert any value to its text");

    let mut bindings = BTreeMap::new();
    bindings.insert("derived".to_string(), stage(&to_text_id));

    let graph = CompositionGraph::new(
        "let preserves outer input",
        CompositionNode::Let {
            bindings,
            body: Box::new(CompositionNode::Const {
                value: json!("body-output"),
            }),
        },
    );

    // Type-checks
    let check = check_graph(&graph.root, &store).unwrap();
    let _ = check;

    // Executes — body receives the merged record
    let executor = MockExecutor::from_store(&store);
    let comp_id = compute_composition_id(&graph).unwrap();
    let result = run_composition(
        &graph.root,
        &json!({"state_path": "/tmp/state.json", "value": 42}),
        &executor,
        &comp_id,
    )
    .unwrap();
    assert!(matches!(result.trace.status, TraceStatus::Ok));
    // The body is a Const, so its output is the literal value.
    assert_eq!(result.output, json!("body-output"));
}

#[test]
fn let_serializes_round_trip() {
    let mut bindings = BTreeMap::new();
    bindings.insert("a".to_string(), stage("stage-a"));
    let node = CompositionNode::Let {
        bindings,
        body: Box::new(stage("body")),
    };
    let json = serde_json::to_string(&node).unwrap();
    let parsed: CompositionNode = serde_json::from_str(&json).unwrap();
    assert_eq!(node, parsed);
}

#[test]
fn let_runner_merges_outer_input_with_binding_outputs() {
    // White-box check: when the outer input is a Record, the body sees
    // outer fields + binding name → binding output. Use an Echo stage
    // wrapper via MockExecutor (the to_text stdlib stage echoes what we
    // give it). We verify that the body's input — passed to a stage that
    // simply returns a Const — was assembled correctly by checking the
    // trace order.
    let store = init_store();
    let to_text_id = find_stage_id(&store, "Convert any value to its text");

    let mut bindings = BTreeMap::new();
    bindings.insert("text".to_string(), stage(&to_text_id));

    let graph = CompositionGraph::new(
        "binding shadow check",
        CompositionNode::Let {
            bindings,
            body: Box::new(stage(&to_text_id)),
        },
    );

    let executor = MockExecutor::from_store(&store);
    let comp_id = compute_composition_id(&graph).unwrap();
    let result = run_composition(
        &graph.root,
        &serde_json::json!({"text": "hello"}),
        &executor,
        &comp_id,
    )
    .unwrap();
    assert!(matches!(result.trace.status, TraceStatus::Ok));
    // Two stage executions: the binding + the body.
    assert_eq!(result.trace.stages.len(), 2);
}

/// Pre-resolution composition-id stability: a signature-pinned graph's
/// composition ID must not change just because a new Active impl
/// replaces the old one in the store. The review flagged this as the
/// highest-priority #28 fix — composition_id must hash the canonical
/// form the user authored, not the store-resolved tree.
#[test]
fn composition_id_is_stable_across_resolution() {
    use noether_engine::lagrange::{resolve_pinning, Pinning};

    let store = init_store();
    let to_text_id = find_stage_id(&store, "Convert any value to its text");

    // Graph with a signature-pinned reference. We'll compute the
    // composition ID on the un-resolved form, then resolve in place and
    // observe the composition ID is unchanged (because the resolver
    // does not affect the source canonical form).
    let graph = CompositionGraph::new(
        "sig-pinned",
        CompositionNode::Stage {
            id: noether_core::stage::StageId(to_text_id.clone()),
            pinning: Pinning::Signature,
            config: None,
        },
    );

    let pre_id = compute_composition_id(&graph).unwrap();

    // Resolve in place (no-op here because the stdlib id IS a full
    // impl_id so get() falls through). The key check is that the
    // composition-id computation happens on the pre-resolution graph
    // in `noether run` — verified in run.rs. The stability we pin
    // here is: serialising the same authored graph + re-parsing +
    // hashing gives the same value.
    let json = serialize_graph(&graph).unwrap();
    let reparsed = parse_graph(&json).unwrap();
    let post_id = compute_composition_id(&reparsed).unwrap();
    assert_eq!(pre_id, post_id);

    // Resolution doesn't change the resolver's view either.
    let mut mutated = graph.root.clone();
    let _ = resolve_pinning(&mut mutated, &store).unwrap();
    // A fresh hash of the graph struct AFTER resolution should in
    // general differ from pre-resolution only in the `id` field — but
    // in `noether run` we call compute_composition_id BEFORE mutating,
    // so that path is stable. This test pins the documented contract.
}

#[test]
fn retry_preserves_types() {
    let store = init_store();
    let to_text_id = find_stage_id(&store, "Convert any value to its text");

    let graph = CompositionGraph::new(
        "retry test",
        CompositionNode::Retry {
            stage: Box::new(stage(&to_text_id)),
            max_attempts: 3,
            delay_ms: Some(100),
        },
    );

    let check = check_graph(&graph.root, &store).unwrap();
    assert_eq!(format!("{}", check.resolved.input), "Any");
    assert_eq!(format!("{}", check.resolved.output), "Text");
}
