//! End-to-end: runtime refinement enforcement at stage boundaries.
//!
//! Every test here drives a real stdlib stage through a real inline
//! executor, wrapped in [`ValidatingExecutor`]. These are the
//! scenarios that prove the wiring lands where it needs to:
//!
//! 1. In-range inputs pass.
//! 2. Out-of-range inputs abort before the implementation fires,
//!    with a clear "input refinement violation" message.
//! 3. Out-of-range outputs abort after the implementation returns,
//!    with an "output refinement violation" message. This catches
//!    implementation drift — the refined type says the output is
//!    bounded, the impl broke that promise, the executor notices.
//!
//! The disable-env-var path is tested separately because tests share
//! a process and `std::env::set_var` leaks; it's covered by the
//! `ValidatingExecutor::is_disabled` unit test.

use noether_core::effects::EffectSet;
use noether_core::stage::{CostEstimate, Stage, StageId, StageLifecycle, StageSignature};
use noether_core::stdlib::load_stdlib;
use noether_core::types::{NType, Refinement};
use noether_engine::executor::inline::InlineExecutor;
use noether_engine::executor::validating::ValidatingExecutor;
use noether_engine::executor::{ExecutionError, StageExecutor};
use noether_store::{MemoryStore, StageStore};
use serde_json::json;
use std::collections::BTreeSet;

fn init_store() -> MemoryStore {
    let mut store = MemoryStore::new();
    for stage in load_stdlib() {
        store.put(stage).unwrap();
    }
    store
}

fn find_by_name(store: &MemoryStore, name: &str) -> StageId {
    store
        .list(None)
        .into_iter()
        .find(|s| s.name.as_deref() == Some(name))
        .unwrap_or_else(|| panic!("stdlib stage '{name}' not found"))
        .id
        .clone()
}

#[test]
fn clamp_percent_accepts_in_range_input() {
    let store = init_store();
    let id = find_by_name(&store, "clamp_percent");
    let inner = InlineExecutor::from_store(&store);
    let exec = ValidatingExecutor::from_store(inner, &store);
    let out = exec
        .execute(&id, &json!(42))
        .expect("50 must pass the [0,100] refinement");
    assert_eq!(out, json!(42));
}

#[test]
fn clamp_percent_rejects_negative_input() {
    let store = init_store();
    let id = find_by_name(&store, "clamp_percent");
    let inner = InlineExecutor::from_store(&store);
    let exec = ValidatingExecutor::from_store(inner, &store);
    let err = exec.execute(&id, &json!(-1)).unwrap_err();
    let ExecutionError::StageFailed { stage_id, message } = err else {
        panic!("expected StageFailed, got {err:?}");
    };
    assert_eq!(stage_id, id);
    assert!(
        message.contains("input refinement violation"),
        "unexpected message: {message}"
    );
    assert!(
        message.contains("below minimum"),
        "expected below-minimum detail, got: {message}"
    );
}

#[test]
fn clamp_percent_rejects_input_above_hundred() {
    let store = init_store();
    let id = find_by_name(&store, "clamp_percent");
    let inner = InlineExecutor::from_store(&store);
    let exec = ValidatingExecutor::from_store(inner, &store);
    let err = exec.execute(&id, &json!(500)).unwrap_err();
    let ExecutionError::StageFailed { message, .. } = err else {
        panic!("expected StageFailed");
    };
    assert!(message.contains("input refinement violation"));
    assert!(message.contains("above maximum"));
}

/// Implementation drift: a stage that declares a refined output but
/// whose impl returns an out-of-range value. The executor catches
/// the violation post-call and surfaces it as a stage failure
/// rather than silently passing an invalid value downstream.
///
/// We construct a custom stage (inline-executed via the stdlib path)
/// that returns an out-of-range number despite declaring the same
/// refinement as clamp_percent. The stdlib dispatch uses description
/// strings, so reusing `clamp_percent`'s description re-routes to its
/// (honest) implementation — to fake drift we write a bespoke
/// `InlineExecutor` trick by registering a fresh stage with an impl
/// that lies. But the inline registry is driven by description
/// strings baked into the match table, so an easier route is to
/// stick a non-inline stage into the store with refined output and
/// wrap a MockExecutor that returns out-of-range.
#[test]
fn output_refinement_violation_is_caught_post_execute() {
    use noether_engine::executor::mock::MockExecutor;

    // Custom stage: claims output is in [0,100] but the mock returns 999.
    let refined_ty = NType::refined(
        NType::Number,
        Refinement::Range {
            min: Some(0.0),
            max: Some(100.0),
        },
    );
    let stage = Stage {
        id: StageId("drift".into()),
        signature_id: None,
        signature: StageSignature {
            input: NType::Number,
            output: refined_ty,
            effects: EffectSet::pure(),
            implementation_hash: "impl_drift".into(),
        },
        capabilities: BTreeSet::new(),
        cost: CostEstimate {
            time_ms_p50: None,
            tokens_est: None,
            memory_mb: None,
        },
        description: "drifted impl".into(),
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
    };
    let mut store = MemoryStore::new();
    store.put(stage).unwrap();

    let id = StageId("drift".into());
    let inner = MockExecutor::new().with_output(&id, json!(999));
    let exec = ValidatingExecutor::from_store(inner, &store);

    let err = exec.execute(&id, &json!(50)).unwrap_err();
    let ExecutionError::StageFailed { stage_id, message } = err else {
        panic!("expected StageFailed, got {err:?}");
    };
    assert_eq!(stage_id, id);
    assert!(
        message.contains("output refinement violation"),
        "unexpected: {message}"
    );
    assert!(
        message.contains("above maximum"),
        "expected bound detail, got: {message}"
    );
}
