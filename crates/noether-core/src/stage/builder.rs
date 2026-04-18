use crate::capability::Capability;
use crate::effects::EffectSet;
use crate::stage::hash::{compute_signature_id, compute_stage_id};
use crate::stage::schema::{CostEstimate, Example, Stage, StageLifecycle, StageSignature};
use crate::stage::signing::sign_stage_id;
use crate::types::NType;
use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

#[derive(Debug, thiserror::Error)]
pub enum StageBuilderError {
    #[error("missing required field: {0}")]
    MissingField(String),
    #[error("too few examples: need at least {min}, got {got}")]
    TooFewExamples { min: usize, got: usize },
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
}

/// Fluent builder for constructing Stage structs.
pub struct StageBuilder {
    name: Option<String>,
    input: Option<NType>,
    output: Option<NType>,
    effects: Option<EffectSet>,
    capabilities: BTreeSet<Capability>,
    cost: CostEstimate,
    description: Option<String>,
    examples: Vec<Example>,
    implementation_code: Option<String>,
    implementation_language: Option<String>,
    ui_style: Option<String>,
    tags: Vec<String>,
    aliases: Vec<String>,
}

impl StageBuilder {
    pub fn new(name: &str) -> Self {
        Self {
            name: Some(name.into()),
            input: None,
            output: None,
            effects: None,
            capabilities: BTreeSet::new(),
            cost: CostEstimate {
                time_ms_p50: None,
                tokens_est: None,
                memory_mb: None,
            },
            description: None,
            examples: Vec::new(),
            implementation_code: None,
            implementation_language: None,
            ui_style: None,
            tags: Vec::new(),
            aliases: Vec::new(),
        }
    }

    /// Attach the source code + language (e.g. "python") for synthesized stages.
    /// The `implementation_hash` fed into the signature will be derived from this code.
    pub fn implementation_code(mut self, code: &str, language: &str) -> Self {
        self.implementation_code = Some(code.into());
        self.implementation_language = Some(language.into());
        self
    }

    /// Attach scoped CSS for this stage's UI component.
    /// The browser build automatically prefixes every selector with `.nr-<id8>`.
    pub fn ui_style(mut self, css: &str) -> Self {
        self.ui_style = Some(css.into());
        self
    }

    /// Append a single browsable tag (e.g. `"network"`, `"pure"`, `"text"`).
    pub fn tag(mut self, t: impl Into<String>) -> Self {
        self.tags.push(t.into());
        self
    }

    /// Append a single alias / alternative name to improve search recall.
    pub fn alias(mut self, a: impl Into<String>) -> Self {
        self.aliases.push(a.into());
        self
    }

    pub fn input(mut self, t: NType) -> Self {
        self.input = Some(t);
        self
    }

    pub fn output(mut self, t: NType) -> Self {
        self.output = Some(t);
        self
    }

    pub fn effects(mut self, e: EffectSet) -> Self {
        self.effects = Some(e);
        self
    }

    pub fn pure(mut self) -> Self {
        self.effects = Some(EffectSet::pure());
        self
    }

    pub fn capability(mut self, c: Capability) -> Self {
        self.capabilities.insert(c);
        self
    }

    pub fn description(mut self, d: &str) -> Self {
        self.description = Some(d.into());
        self
    }

    pub fn example(mut self, input: serde_json::Value, output: serde_json::Value) -> Self {
        self.examples.push(Example { input, output });
        self
    }

    pub fn cost(
        mut self,
        time_ms: Option<u64>,
        tokens: Option<u64>,
        memory_mb: Option<u64>,
    ) -> Self {
        self.cost = CostEstimate {
            time_ms_p50: time_ms,
            tokens_est: tokens,
            memory_mb,
        };
        self
    }

    /// Build a stdlib stage: deterministic implementation_hash, signed, Active lifecycle.
    pub fn build_stdlib(self, signing_key: &SigningKey) -> Result<Stage, StageBuilderError> {
        let name = self
            .name
            .as_ref()
            .ok_or_else(|| StageBuilderError::MissingField("name".into()))?;
        let input = self
            .input
            .ok_or_else(|| StageBuilderError::MissingField("input".into()))?;
        let output = self
            .output
            .ok_or_else(|| StageBuilderError::MissingField("output".into()))?;
        let description = self
            .description
            .ok_or_else(|| StageBuilderError::MissingField("description".into()))?;

        if self.examples.len() < 5 {
            return Err(StageBuilderError::TooFewExamples {
                min: 5,
                got: self.examples.len(),
            });
        }

        let impl_hash = {
            let data = format!("noether-stdlib-v0.1.0:{name}");
            hex::encode(Sha256::digest(data.as_bytes()))
        };

        let effects = self.effects.unwrap_or_default();
        let signature_id = compute_signature_id(name, &input, &output, &effects)?;

        let signature = StageSignature {
            input,
            output,
            effects,
            implementation_hash: impl_hash,
        };

        let id = compute_stage_id(&signature)?;
        let sig_hex = sign_stage_id(&id, signing_key);
        let pub_hex = hex::encode(signing_key.verifying_key().to_bytes());

        Ok(Stage {
            id,
            signature_id: Some(signature_id),
            signature,
            capabilities: self.capabilities,
            cost: self.cost,
            description,
            examples: self.examples,
            lifecycle: StageLifecycle::Active,
            ed25519_signature: Some(sig_hex),
            signer_public_key: Some(pub_hex),
            implementation_code: None,
            implementation_language: None,
            ui_style: None,
            tags: self.tags,
            aliases: self.aliases,
            name: self.name.clone(),
        })
    }

    /// Build a signed stage for synthesized or user-authored stages.
    ///
    /// Identical to `build_unsigned` except the stage is signed with the provided key.
    /// The lifecycle is `Draft`; the store promotes it to `Active` after validation.
    pub fn build_signed(
        self,
        signing_key: &SigningKey,
        implementation_hash: String,
    ) -> Result<Stage, StageBuilderError> {
        let name = self.name.clone().unwrap_or_default();
        let input = self
            .input
            .ok_or_else(|| StageBuilderError::MissingField("input".into()))?;
        let output = self
            .output
            .ok_or_else(|| StageBuilderError::MissingField("output".into()))?;
        let description = self
            .description
            .ok_or_else(|| StageBuilderError::MissingField("description".into()))?;

        let effects = self.effects.unwrap_or_default();
        let signature_id = compute_signature_id(&name, &input, &output, &effects)?;

        let signature = StageSignature {
            input,
            output,
            effects,
            implementation_hash,
        };

        let id = compute_stage_id(&signature)?;
        let sig_hex = sign_stage_id(&id, signing_key);
        let pub_hex = hex::encode(signing_key.verifying_key().to_bytes());

        Ok(Stage {
            id,
            signature_id: Some(signature_id),
            signature,
            capabilities: self.capabilities,
            cost: self.cost,
            description,
            examples: self.examples,
            lifecycle: StageLifecycle::Draft,
            ed25519_signature: Some(sig_hex),
            signer_public_key: Some(pub_hex),
            implementation_code: self.implementation_code,
            implementation_language: self.implementation_language,
            ui_style: self.ui_style,
            tags: self.tags,
            aliases: self.aliases,
            name: self.name.clone(),
        })
    }

    /// Build an unsigned stage for user authoring. Requires an implementation_hash.
    pub fn build_unsigned(self, implementation_hash: String) -> Result<Stage, StageBuilderError> {
        let name = self.name.clone().unwrap_or_default();
        let input = self
            .input
            .ok_or_else(|| StageBuilderError::MissingField("input".into()))?;
        let output = self
            .output
            .ok_or_else(|| StageBuilderError::MissingField("output".into()))?;
        let description = self
            .description
            .ok_or_else(|| StageBuilderError::MissingField("description".into()))?;

        let effects = self.effects.unwrap_or_default();
        let signature_id = compute_signature_id(&name, &input, &output, &effects)?;

        let signature = StageSignature {
            input,
            output,
            effects,
            implementation_hash,
        };

        let id = compute_stage_id(&signature)?;

        Ok(Stage {
            id,
            signature_id: Some(signature_id),
            signature,
            capabilities: self.capabilities,
            cost: self.cost,
            description,
            examples: self.examples,
            lifecycle: StageLifecycle::Draft,
            ed25519_signature: None,
            signer_public_key: None,
            implementation_code: self.implementation_code,
            implementation_language: self.implementation_language,
            ui_style: self.ui_style,
            tags: self.tags,
            aliases: self.aliases,
            name: self.name.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::verify_stage_signature;
    use rand::rngs::OsRng;
    use serde_json::json;

    #[test]
    fn build_stdlib_stage() {
        let key = SigningKey::generate(&mut OsRng);
        let stage = StageBuilder::new("test_stage")
            .input(NType::Text)
            .output(NType::Number)
            .pure()
            .description("test stage")
            .example(json!("1"), json!(1))
            .example(json!("2"), json!(2))
            .example(json!("3"), json!(3))
            .example(json!("4"), json!(4))
            .example(json!("5"), json!(5))
            .build_stdlib(&key)
            .unwrap();

        assert_eq!(stage.lifecycle, StageLifecycle::Active);
        assert!(stage.ed25519_signature.is_some());
        assert!(verify_stage_signature(
            &stage.id,
            stage.ed25519_signature.as_ref().unwrap(),
            stage.signer_public_key.as_ref().unwrap(),
        )
        .unwrap());
    }

    #[test]
    fn too_few_examples_fails() {
        let key = SigningKey::generate(&mut OsRng);
        let result = StageBuilder::new("test")
            .input(NType::Text)
            .output(NType::Text)
            .description("test")
            .example(json!("a"), json!("b"))
            .build_stdlib(&key);
        assert!(result.is_err());
    }

    #[test]
    fn missing_field_fails() {
        let key = SigningKey::generate(&mut OsRng);
        let result = StageBuilder::new("test")
            .input(NType::Text)
            // missing output
            .description("test")
            .build_stdlib(&key);
        assert!(result.is_err());
    }

    #[test]
    fn build_unsigned_is_draft() {
        let stage = StageBuilder::new("user_stage")
            .input(NType::Text)
            .output(NType::Text)
            .description("user stage")
            .build_unsigned("somehash".into())
            .unwrap();
        assert_eq!(stage.lifecycle, StageLifecycle::Draft);
        assert!(stage.ed25519_signature.is_none());
    }
}
