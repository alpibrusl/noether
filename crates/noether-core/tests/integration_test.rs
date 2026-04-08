use ed25519_dalek::SigningKey;
use noether_core::capability::Capability;
use noether_core::effects::{Effect, EffectSet};
use noether_core::stage::{
    compute_stage_id, sign_stage_id, verify_stage_signature, CostEstimate, Example, Stage,
    StageLifecycle, StageSignature,
};
use noether_core::types::{is_subtype_of, NType};
use rand::rngs::OsRng;
use std::collections::BTreeSet;

/// Full round-trip test: create signature → compute ID → sign → verify →
/// serialize full Stage to JSON → deserialize → verify everything matches.
#[test]
fn full_stage_round_trip() {
    // 1. Define a realistic stage signature
    let sig = StageSignature {
        input: NType::record([
            ("text", NType::Text),
            ("max_tokens", NType::optional(NType::Number)),
        ]),
        output: NType::record([("completion", NType::Text), ("tokens_used", NType::Number)]),
        effects: EffectSet::new([
            Effect::Llm {
                model: "claude-sonnet-4".into(),
            },
            Effect::NonDeterministic,
            Effect::Fallible,
            Effect::Cost { cents: 5 },
        ]),
        implementation_hash: "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4".into(),
    };

    // 2. Compute content-addressed ID
    let stage_id = compute_stage_id(&sig).unwrap();
    assert_eq!(stage_id.0.len(), 64);

    // 3. Sign it
    let signing_key = SigningKey::generate(&mut OsRng);
    let signature = sign_stage_id(&stage_id, &signing_key);
    let public_key = hex::encode(signing_key.verifying_key().to_bytes());

    // 4. Verify signature
    assert!(verify_stage_signature(&stage_id, &signature, &public_key).unwrap());

    // 5. Build the full Stage
    let stage = Stage {
        id: stage_id.clone(),
        signature: sig,
        capabilities: BTreeSet::from([Capability::Network, Capability::Llm]),
        cost: CostEstimate {
            time_ms_p50: Some(200),
            tokens_est: Some(500),
            memory_mb: Some(10),
        },
        description: "Complete text using an LLM".into(),
        examples: vec![Example {
            input: serde_json::json!({"text": "Hello", "max_tokens": 100}),
            output: serde_json::json!({"completion": "Hello world!", "tokens_used": 12}),
        }],
        lifecycle: StageLifecycle::Active,
        ed25519_signature: Some(signature),
        signer_public_key: Some(public_key.clone()),
        implementation_code: None,
        implementation_language: None,
            ui_style: None,
    };

    // 6. Serialize to JSON and back
    let json = serde_json::to_string_pretty(&stage).unwrap();
    let deserialized: Stage = serde_json::from_str(&json).unwrap();
    assert_eq!(stage, deserialized);

    // 7. Verify the deserialized stage's signature still works
    assert!(verify_stage_signature(
        &deserialized.id,
        deserialized.ed25519_signature.as_ref().unwrap(),
        deserialized.signer_public_key.as_ref().unwrap(),
    )
    .unwrap());

    // 8. Verify the hash is still deterministic after round-trip
    let recomputed_id = compute_stage_id(&deserialized.signature).unwrap();
    assert_eq!(stage_id, recomputed_id);
}

/// Test that composition type checking works for a realistic pipeline:
/// parse_json >> extract_field >> llm_complete
#[test]
fn composition_type_checking_pipeline() {
    // Stage 1: parse_json — Text → Record { data: Any }
    let parse_output = NType::record([("data", NType::Any)]);

    // Stage 2: extract_field — expects Record { data: Text }
    let extract_input = NType::record([("data", NType::Text)]);
    let extract_output = NType::Text;

    // Stage 3: llm_complete — expects Text
    let llm_input = NType::Text;

    // parse_json >> extract_field: output(parse_json) <: input(extract_field)?
    // Record { data: Any } <: Record { data: Text } — yes, because Any <: Text
    assert!(is_subtype_of(&parse_output, &extract_input).is_compatible());

    // extract_field >> llm_complete: output(extract_field) <: input(llm_complete)?
    // Text <: Text — yes
    assert!(is_subtype_of(&extract_output, &llm_input).is_compatible());
}

/// Test that incompatible compositions produce useful error messages.
#[test]
fn composition_type_error_is_actionable() {
    let stage_a_output = NType::List(Box::new(NType::record([("row", NType::Text)])));
    let stage_b_input = NType::record([("rows", NType::List(Box::new(NType::Number)))]);

    let result = is_subtype_of(&stage_a_output, &stage_b_input);
    assert!(!result.is_compatible());

    // The error should mention what went wrong
    if let noether_core::types::TypeCompatibility::Incompatible(reason) = result {
        let msg = format!("{reason}");
        assert!(
            msg.contains("expected") || msg.contains("missing") || msg.contains("List"),
            "Error message should be actionable, got: {msg}"
        );
    }
}

/// Test Display formatting for realistic types.
#[test]
fn display_formatting_realistic() {
    let t = NType::record([
        ("name", NType::Text),
        ("scores", NType::List(Box::new(NType::Number))),
        ("active", NType::optional(NType::Bool)),
    ]);
    let display = format!("{t}");
    assert!(display.contains("name: Text"));
    assert!(display.contains("scores: List<Number>"));
    assert!(display.contains("Bool | Null"));
}
