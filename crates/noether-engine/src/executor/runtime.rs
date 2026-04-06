//! Runtime executor: handles stages that need external dependencies —
//! an LLM provider, the stage store, or the semantic index.
//!
//! ## Stages handled
//!
//! | Stage description                                        | Needs       |
//! |----------------------------------------------------------|-------------|
//! | Generate text completion using a language model          | LLM         |
//! | Generate a vector embedding for text                     | LLM         |
//! | Classify text into one of the provided categories        | LLM         |
//! | Extract structured data from text according to a schema  | LLM         |
//! | Get detailed information about a stage by its ID         | store cache |
//! | Search the stage store by semantic query                 | store cache |
//! | Check if one type is a structural subtype of another     | pure        |

use super::{ExecutionError, StageExecutor};
use noether_core::stage::StageId;
use noether_core::types::NType;
use noether_store::StageStore;
use serde_json::{json, Value};
use std::collections::HashMap;

use crate::llm::{LlmConfig, LlmProvider, Message};

// ── Cached stage info (built once at construction) ────────────────────────────

#[derive(Clone)]
struct CachedStage {
    id: String,
    description: String,
    input_display: String,
    output_display: String,
    lifecycle: String,
}

// ── RuntimeExecutor ───────────────────────────────────────────────────────────

pub struct RuntimeExecutor {
    llm: Option<Box<dyn LlmProvider>>,
    llm_config: LlmConfig,
    /// stage_id → description (for dispatch)
    descriptions: HashMap<String, String>,
    /// Flattened stage list for search and describe
    stage_cache: Vec<CachedStage>,
}

impl RuntimeExecutor {
    /// Build from a store. LLM is not required; stages that need it will
    /// return `ExecutionError::StageFailed` with a clear message.
    pub fn from_store(store: &dyn StageStore) -> Self {
        let mut descriptions = HashMap::new();
        let mut stage_cache = Vec::new();

        for stage in store.list(None) {
            descriptions.insert(stage.id.0.clone(), stage.description.clone());
            stage_cache.push(CachedStage {
                id: stage.id.0.clone(),
                description: stage.description.clone(),
                input_display: format!("{}", stage.signature.input),
                output_display: format!("{}", stage.signature.output),
                lifecycle: format!("{:?}", stage.lifecycle).to_lowercase(),
            });
        }

        Self {
            llm: None,
            llm_config: LlmConfig::default(),
            descriptions,
            stage_cache,
        }
    }

    /// Attach an LLM provider, enabling llm_* stages.
    pub fn with_llm(mut self, llm: Box<dyn LlmProvider>, config: LlmConfig) -> Self {
        self.llm = Some(llm);
        self.llm_config = config;
        self
    }

    /// Set or replace the LLM provider in-place.
    pub fn set_llm(&mut self, llm: Box<dyn LlmProvider>, config: LlmConfig) {
        self.llm = Some(llm);
        self.llm_config = config;
    }

    /// True if this executor can handle the given stage.
    pub fn has_implementation(&self, stage_id: &StageId) -> bool {
        matches!(
            self.descriptions.get(&stage_id.0).map(|s| s.as_str()),
            Some(
                "Generate text completion using a language model"
                    | "Generate a vector embedding for text"
                    | "Classify text into one of the provided categories"
                    | "Extract structured data from text according to a schema"
                    | "Get detailed information about a stage by its ID"
                    | "Search the stage store by semantic query"
                    | "Check if one type is a structural subtype of another"
                    | "Verify that a composition graph type-checks correctly"
                    | "Register a new stage in the store"
                    | "Retrieve the execution trace of a past composition"
            )
        )
    }

    // ── Dispatch ──────────────────────────────────────────────────────────────

    fn dispatch(&self, stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        let desc = self
            .descriptions
            .get(&stage_id.0)
            .map(|s| s.as_str())
            .unwrap_or("");

        match desc {
            "Generate text completion using a language model" => self.llm_complete(stage_id, input),
            "Generate a vector embedding for text" => self.llm_embed(stage_id, input),
            "Classify text into one of the provided categories" => {
                self.llm_classify(stage_id, input)
            }
            "Extract structured data from text according to a schema" => {
                self.llm_extract(stage_id, input)
            }
            "Get detailed information about a stage by its ID" => {
                self.stage_describe(stage_id, input)
            }
            "Search the stage store by semantic query" => self.store_search(stage_id, input),
            "Check if one type is a structural subtype of another" => type_check(stage_id, input),
            "Verify that a composition graph type-checks correctly" => {
                composition_verify(stage_id, input)
            }
            "Register a new stage in the store" => {
                // store_add requires mutable store access which executors don't hold.
                // Agents should use `noether compose` or synthesize_stage() directly.
                Err(ExecutionError::StageFailed {
                    stage_id: stage_id.clone(),
                    message: "store_add cannot be called inside a composition graph — use `noether compose` or the synthesis API to register new stages".into(),
                })
            }
            "Retrieve the execution trace of a past composition" => {
                // trace_read requires access to the TraceStore which is not held by executors.
                // Use `noether trace <composition_id>` from the CLI instead.
                Err(ExecutionError::StageFailed {
                    stage_id: stage_id.clone(),
                    message: "trace_read cannot be called inside a composition graph — use `noether trace <composition_id>` from the CLI".into(),
                })
            }
            _ => Err(ExecutionError::StageNotFound(stage_id.clone())),
        }
    }

    // ── LLM helpers ───────────────────────────────────────────────────────────

    fn require_llm(&self, stage_id: &StageId) -> Result<&dyn LlmProvider, ExecutionError> {
        self.llm.as_deref().ok_or_else(|| ExecutionError::StageFailed {
            stage_id: stage_id.clone(),
            message: "LLM provider not configured (set VERTEX_AI_PROJECT, VERTEX_AI_TOKEN, VERTEX_AI_LOCATION)".into(),
        })
    }

    fn llm_complete(&self, stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        let llm = self.require_llm(stage_id)?;

        let prompt = input["prompt"].as_str().unwrap_or("").to_string();
        let model = input["model"]
            .as_str()
            .unwrap_or(&self.llm_config.model)
            .to_string();
        let max_tokens = input["max_tokens"]
            .as_u64()
            .map(|v| v as u32)
            .unwrap_or(self.llm_config.max_tokens);
        let temperature = input["temperature"]
            .as_f64()
            .map(|v| v as f32)
            .unwrap_or(self.llm_config.temperature);
        let system_opt = input["system"].as_str();

        let mut messages = vec![];
        if let Some(sys) = system_opt {
            messages.push(Message::system(sys));
        }
        messages.push(Message::user(&prompt));

        let cfg = LlmConfig {
            model: model.clone(),
            max_tokens,
            temperature,
        };

        let text = llm
            .complete(&messages, &cfg)
            .map_err(|e| ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: format!("LLM error: {e}"),
            })?;

        let tokens_used = text.split_whitespace().count() as u64;

        Ok(json!({
            "text": text,
            "tokens_used": tokens_used,
            "model": model,
        }))
    }

    fn llm_embed(&self, stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        let llm = self.require_llm(stage_id)?;

        let text = input["text"].as_str().unwrap_or("").to_string();
        let model = input["model"]
            .as_str()
            .unwrap_or("text-embedding-004")
            .to_string();

        // Ask the LLM to generate a JSON array of floats as the embedding.
        // This is a fallback — real embedding models should be used via VertexAiEmbeddingProvider.
        let prompt = format!(
            "Generate a compact 8-dimensional embedding vector for this text as a JSON array of floats: \"{text}\". Respond ONLY with a JSON array like [0.1, -0.2, ...]."
        );
        let messages = vec![
            Message::system("You are an embedding model. Respond only with a JSON float array."),
            Message::user(&prompt),
        ];
        let cfg = LlmConfig {
            model: model.clone(),
            max_tokens: 128,
            temperature: 0.0,
        };

        let response = llm
            .complete(&messages, &cfg)
            .map_err(|e| ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: format!("LLM error: {e}"),
            })?;

        // Extract JSON array from the response
        let embedding: Value =
            extract_json_array(&response).ok_or_else(|| ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: format!("could not parse embedding from LLM response: {response:?}"),
            })?;

        let dims = embedding.as_array().map(|a| a.len()).unwrap_or(0) as u64;

        Ok(json!({
            "embedding": embedding,
            "dimensions": dims,
            "model": model,
        }))
    }

    fn llm_classify(&self, stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        let llm = self.require_llm(stage_id)?;

        let text = input["text"].as_str().unwrap_or("").to_string();
        let model = input["model"]
            .as_str()
            .unwrap_or(&self.llm_config.model)
            .to_string();
        let categories: Vec<String> = input["categories"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();

        if categories.is_empty() {
            return Err(ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: "categories list is empty".into(),
            });
        }

        let cats_str = categories.join(", ");
        let prompt = format!(
            "Classify the following text into EXACTLY ONE of these categories: {cats_str}\n\nText: \"{text}\"\n\nRespond with ONLY valid JSON: {{\"category\": \"<one of the categories>\", \"confidence\": <0.0-1.0>}}"
        );

        let messages = vec![
            Message::system(
                "You are a text classifier. Always respond with valid JSON only. No explanation.",
            ),
            Message::user(&prompt),
        ];
        let cfg = LlmConfig {
            model: model.clone(),
            max_tokens: 64,
            temperature: 0.0,
        };

        let response = llm
            .complete(&messages, &cfg)
            .map_err(|e| ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: format!("LLM error: {e}"),
            })?;

        let parsed: Value =
            extract_json_object(&response).ok_or_else(|| ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: format!("could not parse classification JSON from: {response:?}"),
            })?;

        // Validate the returned category is one of the provided ones
        let category = parsed["category"].as_str().unwrap_or("").trim().to_string();
        if !categories.contains(&category) {
            return Err(ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: format!(
                    "LLM returned unknown category {category:?}; expected one of: {cats_str}"
                ),
            });
        }

        let confidence = parsed["confidence"].as_f64().unwrap_or(1.0);

        Ok(json!({
            "category": category,
            "confidence": confidence,
            "model": model,
        }))
    }

    fn llm_extract(&self, stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        let llm = self.require_llm(stage_id)?;

        let text = input["text"].as_str().unwrap_or("").to_string();
        let model = input["model"]
            .as_str()
            .unwrap_or(&self.llm_config.model)
            .to_string();
        let schema = input.get("schema").cloned().unwrap_or(json!({}));
        let schema_str = serde_json::to_string_pretty(&schema).unwrap_or_else(|_| "{}".to_string());

        let prompt = format!(
            "Extract structured data from the following text.\nSchema: {schema_str}\nText: \"{text}\"\n\nRespond with ONLY a valid JSON object matching the schema. No explanation."
        );

        let messages = vec![
            Message::system(
                "You are a structured data extractor. Always respond with valid JSON only.",
            ),
            Message::user(&prompt),
        ];
        let cfg = LlmConfig {
            model: model.clone(),
            max_tokens: 512,
            temperature: 0.0,
        };

        let response = llm
            .complete(&messages, &cfg)
            .map_err(|e| ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: format!("LLM error: {e}"),
            })?;

        let extracted =
            extract_json_object(&response).ok_or_else(|| ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: format!("could not parse extraction JSON from: {response:?}"),
            })?;

        Ok(json!({
            "extracted": extracted,
            "model": model,
        }))
    }

    // ── Store-aware stages ────────────────────────────────────────────────────

    fn stage_describe(&self, stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        let id = input["id"].as_str().unwrap_or("").to_string();

        let cached = self
            .stage_cache
            .iter()
            .find(|s| s.id == id || s.id.starts_with(&id))
            .ok_or_else(|| ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: format!("stage {id:?} not found"),
            })?;

        Ok(json!({
            "id": cached.id,
            "description": cached.description,
            "input": cached.input_display,
            "output": cached.output_display,
            "lifecycle": cached.lifecycle,
        }))
    }

    fn store_search(&self, _stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        let query = input["query"].as_str().unwrap_or("").to_lowercase();
        let limit = input["limit"].as_u64().unwrap_or(10) as usize;

        let results: Vec<Value> = self
            .stage_cache
            .iter()
            .filter(|s| {
                s.description.to_lowercase().contains(&query)
                    || s.input_display.to_lowercase().contains(&query)
                    || s.output_display.to_lowercase().contains(&query)
            })
            .take(limit)
            .map(|s| {
                json!({
                    "id": s.id,
                    "description": s.description,
                    "input": s.input_display,
                    "output": s.output_display,
                    "score": 1.0,  // text match — no cosine score available here
                })
            })
            .collect();

        Ok(Value::Array(results))
    }
}

impl StageExecutor for RuntimeExecutor {
    fn execute(&self, stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        self.dispatch(stage_id, input)
    }
}

// ── Pure helpers (no LLM / store state) ──────────────────────────────────────

/// `type_check`: `{sub: NType JSON, sup: NType JSON}` → `{compatible: bool, reason: Text|Null}`
fn type_check(stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
    use noether_core::types::{is_subtype_of, TypeCompatibility};

    let sub = parse_ntype_input(&input["sub"]).ok_or_else(|| ExecutionError::StageFailed {
        stage_id: stage_id.clone(),
        message: format!("could not parse sub type from: {}", input["sub"]),
    })?;

    let sup = parse_ntype_input(&input["sup"]).ok_or_else(|| ExecutionError::StageFailed {
        stage_id: stage_id.clone(),
        message: format!("could not parse sup type from: {}", input["sup"]),
    })?;

    match is_subtype_of(&sub, &sup) {
        TypeCompatibility::Compatible => Ok(json!({"compatible": true, "reason": null})),
        TypeCompatibility::Incompatible(reason) => {
            Ok(json!({"compatible": false, "reason": format!("{reason}")}))
        }
    }
}

/// `composition_verify`: `{stages: [id], operators: [op]}` → `{valid, errors, warnings}`
///
/// In this stateless implementation we can only syntactically validate the
/// operator list; real type-checking requires the store and is performed by
/// the checker module. The stage returns a best-effort result.
fn composition_verify(_stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
    let stages = input["stages"].as_array().map(|a| a.len()).unwrap_or(0);
    let operators = input["operators"].as_array().map(|a| a.len()).unwrap_or(0);
    let _ = operators;

    let mut errors: Vec<String> = vec![];
    let mut warnings: Vec<String> = vec![];

    if stages == 0 {
        warnings.push("empty composition".into());
    }

    let valid_ops = [
        "sequential",
        "parallel",
        "branch",
        "fanout",
        "merge",
        "retry",
    ];
    if let Some(ops) = input["operators"].as_array() {
        for op in ops {
            if let Some(s) = op.as_str() {
                if !valid_ops.contains(&s.to_lowercase().as_str()) {
                    errors.push(format!("unknown operator: {s}"));
                }
            }
        }
    }

    let valid = errors.is_empty();
    Ok(json!({
        "valid": valid,
        "errors": errors,
        "warnings": warnings,
    }))
}

// ── Parsing helpers ───────────────────────────────────────────────────────────

/// Parse an NType from either:
/// - A JSON string like `"Text"`, `"Number"`, `"Bool"`, `"Any"`, `"Null"`, `"Bytes"`
/// - A JSON object (the NType serde representation) like `{"kind": "Text"}`
fn parse_ntype_input(v: &Value) -> Option<NType> {
    if let Some(s) = v.as_str() {
        // Simple string names
        match s {
            "Text" => return Some(NType::Text),
            "Number" => return Some(NType::Number),
            "Bool" => return Some(NType::Bool),
            "Any" => return Some(NType::Any),
            "Null" => return Some(NType::Null),
            "Bytes" => return Some(NType::Bytes),
            _ => {}
        }
    }
    // Try JSON NType deserialization
    serde_json::from_value(v.clone()).ok()
}

/// Extract the first JSON array `[...]` found in a string.
fn extract_json_array(s: &str) -> Option<Value> {
    let start = s.find('[')?;
    let end = s.rfind(']').map(|i| i + 1)?;
    serde_json::from_str(&s[start..end]).ok()
}

/// Extract the first JSON object `{...}` found in a string.
fn extract_json_object(s: &str) -> Option<Value> {
    let start = s.find('{')?;
    let end = s.rfind('}').map(|i| i + 1)?;
    serde_json::from_str(&s[start..end]).ok()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use noether_core::stdlib::load_stdlib;
    use noether_store::MemoryStore;

    fn stdlib_runtime() -> RuntimeExecutor {
        let mut store = MemoryStore::new();
        for s in load_stdlib() {
            let _ = store.put(s);
        }
        RuntimeExecutor::from_store(&store)
    }

    #[test]
    fn type_check_compatible() {
        let rt = stdlib_runtime();
        let id = rt
            .descriptions
            .iter()
            .find(|(_, v)| v.contains("structural subtype"))
            .map(|(k, _)| StageId(k.clone()))
            .unwrap();
        let result = rt
            .execute(&id, &json!({"sub": "Text", "sup": "Text"}))
            .unwrap();
        assert_eq!(result["compatible"], json!(true));
        assert_eq!(result["reason"], json!(null));
    }

    #[test]
    fn type_check_incompatible() {
        let rt = stdlib_runtime();
        let id = rt
            .descriptions
            .iter()
            .find(|(_, v)| v.contains("structural subtype"))
            .map(|(k, _)| StageId(k.clone()))
            .unwrap();
        let result = rt
            .execute(&id, &json!({"sub": "Text", "sup": "Number"}))
            .unwrap();
        assert_eq!(result["compatible"], json!(false));
        assert!(result["reason"].is_string());
    }

    #[test]
    fn stage_describe_returns_metadata() {
        let rt = stdlib_runtime();
        // Find the stage_describe stage ID
        let describe_id = rt
            .descriptions
            .iter()
            .find(|(_, v)| v.contains("Get detailed information"))
            .map(|(k, _)| StageId(k.clone()))
            .unwrap();
        // Find to_text stage ID (a known stage to describe)
        let to_text_id = rt
            .descriptions
            .iter()
            .find(|(_, v)| v.contains("Convert any value to its text"))
            .map(|(k, _)| k.clone())
            .unwrap();

        let result = rt
            .execute(&describe_id, &json!({"id": to_text_id}))
            .unwrap();
        assert_eq!(result["id"], json!(to_text_id));
        assert!(result["description"].as_str().unwrap().contains("text"));
    }

    #[test]
    fn store_search_finds_stages() {
        let rt = stdlib_runtime();
        let search_id = rt
            .descriptions
            .iter()
            .find(|(_, v)| v.contains("Search the stage store"))
            .map(|(k, _)| StageId(k.clone()))
            .unwrap();
        let result = rt
            .execute(&search_id, &json!({"query": "sort", "limit": 5}))
            .unwrap();
        let hits = result.as_array().unwrap();
        assert!(!hits.is_empty());
        assert!(hits
            .iter()
            .any(|h| h["description"].as_str().unwrap_or("").contains("Sort")));
    }

    #[test]
    fn llm_complete_fails_gracefully_without_llm() {
        let rt = stdlib_runtime();
        let llm_id = rt
            .descriptions
            .iter()
            .find(|(_, v)| v.contains("Generate text completion"))
            .map(|(k, _)| StageId(k.clone()))
            .unwrap();
        let result = rt.execute(
            &llm_id,
            &json!({"prompt": "Hello", "model": null, "max_tokens": null, "temperature": null, "system": null}),
        );
        // Should fail with a clear message about unconfigured LLM
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("LLM provider not configured"),
            "expected config error, got: {msg}"
        );
    }
}
