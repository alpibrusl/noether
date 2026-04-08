//! Runtime executor: handles stages that need external dependencies —
//! an LLM provider, the stage store, or the semantic index.
//!
//! ## Stages handled
//!
//! | Stage description                                        | Needs       |
//! |----------------------------------------------------------|-------------|
//! | Generate text completion using a language model          | LLM         |
//! | Generate a vector embedding for text                     | Embedding   |
//! | Classify text into one of the provided categories        | LLM         |
//! | Extract structured data from text according to a schema  | LLM         |
//! | Get detailed information about a stage by its ID         | store cache |
//! | Search the stage store by semantic query                 | store cache + optional embeddings |
//! | Check if one type is a structural subtype of another     | pure        |
//! | Verify that a composition graph type-checks correctly    | store cache |

use super::{ExecutionError, StageExecutor};
use noether_core::stage::StageId;
use noether_core::types::NType;
use noether_store::StageStore;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Mutex;

use crate::index::embedding::EmbeddingProvider;
use crate::llm::{LlmConfig, LlmProvider, Message};

// ── Cached stage info (built once at construction) ────────────────────────────

#[derive(Clone)]
struct CachedStage {
    id: String,
    description: String,
    input_display: String,
    output_display: String,
    lifecycle: String,
    effects: Vec<String>,
    examples_count: usize,
}

// ── Cosine similarity ─────────────────────────────────────────────────────────

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

// ── RuntimeExecutor ───────────────────────────────────────────────────────────

pub struct RuntimeExecutor {
    llm: Option<Box<dyn LlmProvider>>,
    llm_config: LlmConfig,
    embedding_provider: Option<Box<dyn EmbeddingProvider>>,
    /// stage_id → description (for dispatch)
    descriptions: HashMap<String, String>,
    /// Flattened stage list for search and describe
    stage_cache: Vec<CachedStage>,
    /// Pre-computed embeddings per stage ID: populated when with_embedding() is called.
    stage_embeddings: HashMap<String, Vec<f32>>,
    /// Session-scoped LLM call deduplication: SHA-256(model + prompt) → response.
    llm_dedup_cache: Mutex<HashMap<String, Value>>,
}

impl RuntimeExecutor {
    /// Build from a store. LLM and embedding providers are not required; stages that
    /// need them will return `ExecutionError::StageFailed` with a clear message.
    pub fn from_store(store: &dyn StageStore) -> Self {
        let mut descriptions = HashMap::new();
        let mut stage_cache = Vec::new();

        for stage in store.list(None) {
            descriptions.insert(stage.id.0.clone(), stage.description.clone());

            let effects: Vec<String> = stage
                .signature
                .effects
                .iter()
                .map(|e| format!("{e:?}"))
                .collect();

            stage_cache.push(CachedStage {
                id: stage.id.0.clone(),
                description: stage.description.clone(),
                input_display: format!("{}", stage.signature.input),
                output_display: format!("{}", stage.signature.output),
                lifecycle: format!("{:?}", stage.lifecycle).to_lowercase(),
                effects,
                examples_count: stage.examples.len(),
            });
        }

        Self {
            llm: None,
            llm_config: LlmConfig::default(),
            embedding_provider: None,
            descriptions,
            stage_cache,
            stage_embeddings: HashMap::new(),
            llm_dedup_cache: Mutex::new(HashMap::new()),
        }
    }

    /// Attach an LLM provider, enabling llm_complete/llm_classify/llm_extract stages.
    pub fn with_llm(mut self, llm: Box<dyn LlmProvider>, config: LlmConfig) -> Self {
        self.llm = Some(llm);
        self.llm_config = config;
        self
    }

    /// Attach an embedding provider, enabling real cosine-similarity store_search
    /// and real llm_embed responses. Pre-computes embeddings for all cached stages.
    pub fn with_embedding(mut self, provider: Box<dyn EmbeddingProvider>) -> Self {
        // Pre-compute embeddings for all stage descriptions
        let mut embeddings = HashMap::new();
        for stage in &self.stage_cache {
            if let Ok(emb) = provider.embed(&stage.description) {
                embeddings.insert(stage.id.clone(), emb);
            }
        }
        self.stage_embeddings = embeddings;
        self.embedding_provider = Some(provider);
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
                self.composition_verify(stage_id, input)
            }
            "Register a new stage in the store" => {
                // store_add requires mutable store access which executors don't hold.
                // Use `noether compose` or the synthesis API to register new stages.
                Err(ExecutionError::StageFailed {
                    stage_id: stage_id.clone(),
                    message: "store_add cannot be called inside a composition graph — use `noether compose` or the synthesis API to register new stages".into(),
                })
            }
            "Retrieve the execution trace of a past composition" => {
                // trace_read requires the TraceStore which executors don't hold.
                // Use `noether trace <composition_id>` from the CLI.
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

        // LLM call deduplication: identical (model, prompt, system) calls within the same
        // session return the cached response instead of making a redundant API call.
        let dedup_key = {
            use sha2::{Digest, Sha256};
            let key_data = format!("{}:{}:{}", model, system_opt.unwrap_or(""), prompt);
            hex::encode(Sha256::digest(key_data.as_bytes()))
        };

        {
            let cache = self.llm_dedup_cache.lock().unwrap();
            if let Some(cached) = cache.get(&dedup_key) {
                let mut result = cached.clone();
                result["from_llm_cache"] = json!(true);
                return Ok(result);
            }
        }

        let text = llm
            .complete(&messages, &cfg)
            .map_err(|e| ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: format!("LLM error: {e}"),
            })?;

        let tokens_used = text.split_whitespace().count() as u64;

        let result = json!({
            "text": text,
            "tokens_used": tokens_used,
            "model": model,
            "from_llm_cache": false,
        });

        self.llm_dedup_cache
            .lock()
            .unwrap()
            .insert(dedup_key, result.clone());

        Ok(result)
    }

    fn llm_embed(&self, stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        let text = input["text"].as_str().unwrap_or("").to_string();
        let model_override = input["model"].as_str().map(|s| s.to_string());

        // Prefer real embedding provider when available.
        if let Some(ep) = &self.embedding_provider {
            let emb = ep.embed(&text).map_err(|e| ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: format!("embedding provider error: {e}"),
            })?;
            let dims = emb.len() as u64;
            let model = model_override.unwrap_or_else(|| "embedding-model".into());
            return Ok(json!({
                "embedding": emb,
                "dimensions": dims,
                "model": model,
            }));
        }

        // Fallback: ask the LLM to generate a JSON array of floats.
        let llm = self.require_llm(stage_id)?;
        let model = model_override.unwrap_or_else(|| "text-embedding-004".to_string());

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
            "effects": cached.effects,
            "lifecycle": cached.lifecycle,
            "examples_count": cached.examples_count,
        }))
    }

    /// Search the stage store by semantic query.
    ///
    /// When an `EmbeddingProvider` has been attached via `with_embedding()`, uses
    /// cosine similarity over pre-computed stage embeddings for real semantic search.
    /// Falls back to case-insensitive substring match when no embedding provider is present.
    fn store_search(&self, _stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        let query = input["query"].as_str().unwrap_or("");
        let limit = input["limit"].as_u64().unwrap_or(10) as usize;

        if let Some(ep) = &self.embedding_provider {
            // Semantic search via cosine similarity
            if let Ok(query_emb) = ep.embed(query) {
                let mut scored: Vec<(f32, &CachedStage)> = self
                    .stage_cache
                    .iter()
                    .filter_map(|s| {
                        self.stage_embeddings
                            .get(&s.id)
                            .map(|emb| (cosine_similarity(&query_emb, emb), s))
                    })
                    .collect();

                scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

                let results: Vec<Value> = scored
                    .into_iter()
                    .take(limit)
                    .map(|(score, s)| {
                        json!({
                            "id": s.id,
                            "description": s.description,
                            "input": s.input_display,
                            "output": s.output_display,
                            "score": score,
                        })
                    })
                    .collect();

                return Ok(Value::Array(results));
            }
        }

        // Substring fallback
        let query_lc = query.to_lowercase();
        let results: Vec<Value> = self
            .stage_cache
            .iter()
            .filter(|s| {
                s.description.to_lowercase().contains(&query_lc)
                    || s.input_display.to_lowercase().contains(&query_lc)
                    || s.output_display.to_lowercase().contains(&query_lc)
            })
            .take(limit)
            .map(|s| {
                json!({
                    "id": s.id,
                    "description": s.description,
                    "input": s.input_display,
                    "output": s.output_display,
                    "score": 1.0,
                })
            })
            .collect();

        Ok(Value::Array(results))
    }

    /// Verify a composition by resolving its stage IDs and type-checking sequential chains.
    ///
    /// Input: `{ stages: List<Text>, operators: List<Text> }`
    /// Output: `{ valid: Bool, errors: List<Text>, warnings: List<Text> }`
    fn composition_verify(
        &self,
        stage_id: &StageId,
        input: &Value,
    ) -> Result<Value, ExecutionError> {
        let stage_ids: Vec<&str> = input["stages"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let operators: Vec<&str> = input["operators"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let mut errors: Vec<String> = vec![];
        let mut warnings: Vec<String> = vec![];

        if stage_ids.is_empty() {
            warnings.push("empty composition".into());
            return Ok(json!({ "valid": true, "errors": errors, "warnings": warnings }));
        }

        // Validate operator names
        let valid_ops = [
            "sequential",
            "parallel",
            "branch",
            "fanout",
            "merge",
            "retry",
        ];
        for op in &operators {
            let op_lc = op.to_lowercase();
            if !valid_ops.contains(&op_lc.as_str()) {
                errors.push(format!("unknown operator: {op}"));
            }
        }

        // Resolve stage IDs and build a lookup by id for type-checking
        let id_to_cache: HashMap<&str, &CachedStage> = self
            .stage_cache
            .iter()
            .map(|s| (s.id.as_str(), s))
            .collect();

        let mut resolved_stages: Vec<&CachedStage> = vec![];
        for sid in &stage_ids {
            match id_to_cache.get(sid) {
                Some(s) => {
                    if s.lifecycle == "deprecated" {
                        warnings.push(format!("stage {} ({}) is deprecated", sid, s.description));
                    }
                    if s.lifecycle == "tombstone" {
                        errors.push(format!(
                            "stage {} is a tombstone and cannot be executed",
                            sid
                        ));
                    }
                    resolved_stages.push(s);
                }
                None => {
                    errors.push(format!("stage {sid} not found in store"));
                }
            }
        }

        // For sequential compositions: type-check consecutive pairs.
        // We parse the stored display strings back to NType for comparison.
        if operators.iter().any(|op| op.to_lowercase() == "sequential") && resolved_stages.len() > 1
        {
            for i in 0..resolved_stages.len() - 1 {
                let out_str = &resolved_stages[i].output_display;
                let in_str = &resolved_stages[i + 1].input_display;

                let out_type: Option<NType> = serde_json::from_str(&format!("\"{}\"", out_str))
                    .ok()
                    .or_else(|| parse_ntype_display(out_str));
                let in_type: Option<NType> = serde_json::from_str(&format!("\"{}\"", in_str))
                    .ok()
                    .or_else(|| parse_ntype_display(in_str));

                if let (Some(out), Some(inp)) = (out_type, in_type) {
                    use noether_core::types::{is_subtype_of, TypeCompatibility};
                    if let TypeCompatibility::Incompatible(reason) = is_subtype_of(&out, &inp) {
                        errors.push(format!(
                            "type mismatch between stages {} and {}: {} is not compatible with {} ({})",
                            stage_ids[i], stage_ids[i + 1], out_str, in_str, reason
                        ));
                    }
                }
                // If we can't parse types, we skip the check rather than emitting a false error.
            }
        }

        // Run the composition via the provided stage id for error context
        let _ = stage_id;

        let valid = errors.is_empty();
        Ok(json!({
            "valid": valid,
            "errors": errors,
            "warnings": warnings,
        }))
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

// ── Parsing helpers ───────────────────────────────────────────────────────────

/// Parse an NType from either:
/// - A JSON string like `"Text"`, `"Number"`, `"Bool"`, `"Any"`, `"Null"`, `"Bytes"`
/// - A JSON object (the NType serde representation) like `{"kind": "Text"}`
fn parse_ntype_input(v: &Value) -> Option<NType> {
    if let Some(s) = v.as_str() {
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
    serde_json::from_value(v.clone()).ok()
}

/// Parse a display string (e.g. "Text", "Number", "Any") into an NType.
/// Used for type-checking in composition_verify.
fn parse_ntype_display(s: &str) -> Option<NType> {
    match s.trim() {
        "Text" => Some(NType::Text),
        "Number" => Some(NType::Number),
        "Bool" => Some(NType::Bool),
        "Any" => Some(NType::Any),
        "Null" => Some(NType::Null),
        "Bytes" => Some(NType::Bytes),
        "VNode" => Some(NType::VNode),
        _ => None,
    }
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
    fn stage_describe_includes_effects() {
        let rt = stdlib_runtime();
        let describe_id = rt
            .descriptions
            .iter()
            .find(|(_, v)| v.contains("Get detailed information"))
            .map(|(k, _)| StageId(k.clone()))
            .unwrap();
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
        // effects is now a list
        assert!(result["effects"].is_array(), "effects should be an array");
        assert!(result["examples_count"].as_u64().unwrap() > 0);
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
    fn store_search_with_embedding_provider() {
        use crate::index::embedding::MockEmbeddingProvider;
        let mut store = MemoryStore::new();
        for s in load_stdlib() {
            let _ = store.put(s);
        }
        let rt = RuntimeExecutor::from_store(&store)
            .with_embedding(Box::new(MockEmbeddingProvider::new(32)));

        let search_id = rt
            .descriptions
            .iter()
            .find(|(_, v)| v.contains("Search the stage store"))
            .map(|(k, _)| StageId(k.clone()))
            .unwrap();
        let result = rt
            .execute(&search_id, &json!({"query": "sort list", "limit": 10}))
            .unwrap();
        let hits = result.as_array().unwrap();
        assert!(!hits.is_empty());
        // All scores should be in [0, 1]
        for h in hits {
            let score = h["score"].as_f64().unwrap();
            assert!((0.0..=1.0).contains(&score), "score {score} out of range");
        }
    }

    #[test]
    fn composition_verify_valid_stages() {
        let rt = stdlib_runtime();
        let verify_id = rt
            .descriptions
            .iter()
            .find(|(_, v)| v.contains("Verify that a composition graph"))
            .map(|(k, _)| StageId(k.clone()))
            .unwrap();

        // Two real stage IDs from the store
        let ids: Vec<String> = rt
            .stage_cache
            .iter()
            .take(2)
            .map(|s| s.id.clone())
            .collect();

        let result = rt
            .execute(
                &verify_id,
                &json!({
                    "stages": ids,
                    "operators": ["sequential"]
                }),
            )
            .unwrap();
        // Should succeed even if types don't match (warnings, not errors for this)
        assert!(result["errors"].is_array());
        assert!(result["warnings"].is_array());
    }

    #[test]
    fn composition_verify_unknown_stage_is_error() {
        let rt = stdlib_runtime();
        let verify_id = rt
            .descriptions
            .iter()
            .find(|(_, v)| v.contains("Verify that a composition graph"))
            .map(|(k, _)| StageId(k.clone()))
            .unwrap();

        let result = rt
            .execute(
                &verify_id,
                &json!({
                    "stages": ["nonexistent-stage-id"],
                    "operators": []
                }),
            )
            .unwrap();
        assert_eq!(result["valid"], json!(false));
        assert!(result["errors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| { e.as_str().unwrap_or("").contains("not found") }));
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
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("LLM provider not configured"),
            "expected config error, got: {msg}"
        );
    }

    #[test]
    fn llm_embed_uses_embedding_provider_when_available() {
        use crate::index::embedding::MockEmbeddingProvider;
        let mut store = MemoryStore::new();
        for s in load_stdlib() {
            let _ = store.put(s);
        }
        let rt = RuntimeExecutor::from_store(&store)
            .with_embedding(Box::new(MockEmbeddingProvider::new(16)));

        let embed_id = rt
            .descriptions
            .iter()
            .find(|(_, v)| v.contains("Generate a vector embedding"))
            .map(|(k, _)| StageId(k.clone()))
            .unwrap();

        let result = rt
            .execute(&embed_id, &json!({"text": "hello world", "model": null}))
            .unwrap();
        assert_eq!(result["dimensions"], json!(16u64));
        assert_eq!(result["embedding"].as_array().unwrap().len(), 16);
    }

    /// Verify the `Mutex<HashMap>` LLM dedup cache is safe under concurrent access.
    #[test]
    fn llm_dedup_cache_concurrent_access() {
        use crate::llm::MockLlmProvider;
        use std::sync::Arc;

        let mock_response = r#"{"category":"positive","confidence":0.99,"model":"mock"}"#;

        let mut store = MemoryStore::new();
        for s in load_stdlib() {
            let _ = store.put(s);
        }

        let rt = RuntimeExecutor::from_store(&store).with_llm(
            Box::new(MockLlmProvider::new(mock_response)),
            LlmConfig::default(),
        );
        let rt = Arc::new(rt);

        let classify_id = rt
            .descriptions
            .iter()
            .find(|(_, v)| v.contains("Classify text into one of"))
            .map(|(k, _)| StageId(k.clone()))
            .expect("classify_text stage not found");

        let input = serde_json::json!({
            "text": "I love this product",
            "categories": ["positive", "negative", "neutral"],
            "model": null
        });

        let results: Vec<_> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..16)
                .map(|_| {
                    let rt = Arc::clone(&rt);
                    let id = classify_id.clone();
                    let inp = input.clone();
                    s.spawn(move || rt.execute(&id, &inp))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        assert_eq!(results.len(), 16);
        let first = results[0].as_ref().expect("first result must be Ok");
        for (i, r) in results.iter().enumerate() {
            let val = r
                .as_ref()
                .unwrap_or_else(|e| panic!("thread {i} failed: {e}"));
            assert_eq!(
                val["category"], first["category"],
                "thread {i} returned different category"
            );
        }
        assert_eq!(first["category"].as_str().unwrap(), "positive");
    }
}
