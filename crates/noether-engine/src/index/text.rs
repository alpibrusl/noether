use noether_core::stage::Stage;

/// Generate text for the signature index: "input_type -> output_type".
pub fn signature_text(stage: &Stage) -> String {
    format!("{} -> {}", stage.signature.input, stage.signature.output)
}

/// Generate text for the semantic/description index.
pub fn description_text(stage: &Stage) -> String {
    stage.description.clone()
}

/// Generate text for the example index: all input/output pairs.
pub fn examples_text(stage: &Stage) -> String {
    stage
        .examples
        .iter()
        .map(|ex| format!("{} => {}", ex.input, ex.output))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use noether_core::effects::EffectSet;
    use noether_core::stage::{CostEstimate, Example, StageId, StageLifecycle, StageSignature};
    use noether_core::types::NType;
    use std::collections::BTreeSet;

    fn test_stage() -> Stage {
        Stage {
            id: StageId("test".into()),
            signature: StageSignature {
                input: NType::Text,
                output: NType::Number,
                effects: EffectSet::pure(),
                implementation_hash: "impl".into(),
            },
            capabilities: BTreeSet::new(),
            cost: CostEstimate {
                time_ms_p50: None,
                tokens_est: None,
                memory_mb: None,
            },
            description: "Convert text to number".into(),
            examples: vec![
                Example {
                    input: serde_json::json!("42"),
                    output: serde_json::json!(42),
                },
                Example {
                    input: serde_json::json!("3"),
                    output: serde_json::json!(3),
                },
            ],
            lifecycle: StageLifecycle::Active,
            ed25519_signature: None,
            signer_public_key: None,
            implementation_code: None,
            implementation_language: None,
        }
    }

    #[test]
    fn signature_text_formats_types() {
        let stage = test_stage();
        assert_eq!(signature_text(&stage), "Text -> Number");
    }

    #[test]
    fn description_text_returns_description() {
        let stage = test_stage();
        assert_eq!(description_text(&stage), "Convert text to number");
    }

    #[test]
    fn examples_text_formats_pairs() {
        let stage = test_stage();
        let text = examples_text(&stage);
        assert!(text.contains("\"42\" => 42"));
        assert!(text.contains("\"3\" => 3"));
    }
}
