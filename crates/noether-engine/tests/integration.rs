use noether_core::stdlib::load_stdlib;
use noether_engine::checker::check_graph;
use noether_engine::executor::mock::MockExecutor;
use noether_engine::executor::runner::run_composition;
use noether_engine::lagrange::{
    compute_composition_id, parse_graph, serialize_graph, CompositionGraph, CompositionNode,
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
