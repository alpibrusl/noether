/// End-to-end tests for the NixExecutor path.
/// These tests are fast (Nix caches runtimes) but require `nix` to be installed.
/// Run with: cargo test --test nix_e2e -p noether-engine -- --nocapture

// ── Test 1: single stage via NixExecutor ─────────────────────────────────────

#[test]
fn nix_e2e_word_count() {
    use noether_core::stage::{StageBuilder, StageLifecycle};
    use noether_core::types::NType;
    use noether_engine::executor::composite::CompositeExecutor;
    use noether_engine::executor::StageExecutor;
    use noether_store::{MemoryStore, StageStore};
    use serde_json::json;

    let python_code = r#"
def execute(input_value):
    text = input_value.get("text", "")
    words = text.split()
    return {"count": len(words), "words": words}
"#;

    let impl_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(python_code.as_bytes()))
    };

    let stage = StageBuilder::new("word_count")
        .input(NType::Record(
            [("text".into(), NType::Text)]
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>(),
        ))
        .output(NType::Any)
        .description("Count words in a text string")
        .example(
            json!({"text": "hello world"}),
            json!({"count": 2, "words": ["hello", "world"]}),
        )
        .example(
            json!({"text": "one two three"}),
            json!({"count": 3, "words": ["one", "two", "three"]}),
        )
        .example(
            json!({"text": "a b c d e"}),
            json!({"count": 5, "words": ["a", "b", "c", "d", "e"]}),
        )
        .implementation_code(python_code, "python")
        .build_unsigned(impl_hash)
        .unwrap();

    let stage_id = stage.id.clone();

    let mut store = MemoryStore::new();
    let _ = store.put(stage);
    let _ = store.update_lifecycle(&stage_id, StageLifecycle::Active);

    let executor = CompositeExecutor::from_store(&store);

    if !executor.nix_available() {
        eprintln!("nix not available, skipping NixExecutor path");
        return;
    }

    let input = json!({"text": "the quick brown fox jumps over the lazy dog"});
    let result = executor
        .execute(&stage_id, &input)
        .expect("execution failed");

    let count = result["count"].as_u64().expect("count should be a number");
    assert_eq!(count, 9, "expected 9 words");
    let words = result["words"]
        .as_array()
        .expect("words should be an array");
    assert_eq!(words.len(), 9);
    assert_eq!(words[0], json!("the"));

    println!("word_count result: {result}");
}

// ── Test 2: implementation_code persists across store serialize/deserialize ───
//
// Simulates: `noether compose` synthesizes a stage (persisted to store.json),
// then `noether run` in a NEW process reloads the store and executes via Nix.

#[test]
fn nix_e2e_synthesized_stage_survives_store_roundtrip() {
    use noether_core::stage::{StageBuilder, StageLifecycle};
    use noether_core::types::NType;
    use noether_engine::executor::composite::CompositeExecutor;
    use noether_engine::executor::runner::run_composition;
    use noether_engine::lagrange::{CompositionGraph, CompositionNode, Pinning};
    use noether_store::{JsonFileStore, StageStore};
    use serde_json::json;

    if noether_engine::executor::nix::NixExecutor::find_nix().is_none() {
        eprintln!("nix not available, skipping");
        return;
    }

    let store_path = std::env::temp_dir().join("noether-nix-roundtrip-test.json");
    let _ = std::fs::remove_file(&store_path);

    // ── Phase 1: "compose session" — register a synthesized stage ────────────
    let python_code = "def execute(x):\n    return {\"doubled\": x[\"n\"] * 2}";
    let impl_hash = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(python_code.as_bytes()))
    };

    let stage = StageBuilder::new("double_number")
        .input(NType::Record(
            [("n".into(), NType::Number)]
                .into_iter()
                .collect::<std::collections::BTreeMap<_, _>>(),
        ))
        .output(NType::Any)
        .description("Double the input number")
        .example(json!({"n": 1}), json!({"doubled": 2}))
        .example(json!({"n": 3}), json!({"doubled": 6}))
        .example(json!({"n": 10}), json!({"doubled": 20}))
        .implementation_code(python_code, "python")
        .build_unsigned(impl_hash)
        .unwrap();

    let stage_id = stage.id.clone();

    {
        let mut store = JsonFileStore::open(&store_path).expect("open store");
        store.put(stage).expect("put stage");
        store
            .update_lifecycle(&stage_id, StageLifecycle::Active)
            .expect("activate");
        // JsonFileStore flushes to disk on drop
    }

    // ── Phase 2: "run session" — fresh load, execute via Nix ─────────────────
    let store = JsonFileStore::open(&store_path).expect("reopen store");
    let executor = CompositeExecutor::from_store(&store);

    assert!(
        executor.nix_available(),
        "CompositeExecutor should detect Nix and build NixExecutor"
    );

    let graph = CompositionGraph::new(
        "double a number",
        CompositionNode::Stage {
            id: stage_id.clone(),
            pinning: Pinning::Signature,
            config: None,
        },
    );

    let result = run_composition(&graph.root, &json!({"n": 21}), &executor, "test-roundtrip")
        .expect("run_composition failed");

    let doubled = result.output["doubled"]
        .as_f64()
        .expect("doubled should be a number");
    assert_eq!(doubled, 42.0, "21 * 2 should be 42");

    println!("Roundtrip result: {}", result.output);
    let _ = std::fs::remove_file(&store_path);
}
