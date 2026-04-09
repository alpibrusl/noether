pub mod prompt;

use crate::checker::check_graph;
use crate::index::SemanticIndex;
use crate::lagrange::{parse_graph, CompositionGraph};
use crate::llm::{LlmConfig, LlmProvider, Message};
use ed25519_dalek::SigningKey;
use noether_core::stage::validation::infer_type;
use noether_core::stage::{StageBuilder, StageId, StageLifecycle};
use noether_core::types::{is_subtype_of, TypeCompatibility};
use noether_store::{StageStore, StoreError};
use prompt::{
    build_effect_inference_prompt, build_synthesis_prompt, build_system_prompt,
    extract_effect_response, extract_json, extract_synthesis_response, extract_synthesis_spec,
    SynthesisSpec,
};

// ── Error ──────────────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("search failed: {0}")]
    Search(String),
    #[error("LLM call failed: {0}")]
    Llm(#[from] crate::llm::LlmError),
    #[error("no JSON found in LLM response")]
    NoJsonInResponse,
    #[error("invalid graph JSON: {0}")]
    InvalidGraph(String),
    #[error("type check failed after {attempts} attempts: {errors}")]
    TypeCheckFailed { attempts: u32, errors: String },
    #[error("stage synthesis failed: {0}")]
    SynthesisFailed(String),
}

// ── Result types ───────────────────────────────────────────────────────────

/// A stage that was synthesized during a compose() call.
#[derive(Debug)]
pub struct SynthesisResult {
    /// ID of the newly registered stage.
    pub stage_id: StageId,
    /// The generated implementation code.
    pub implementation: String,
    /// Language of the generated code (e.g. "python").
    pub language: String,
    /// Number of LLM attempts needed to produce a valid implementation.
    pub attempts: u32,
    /// False when a stage with an identical signature was already in the store.
    pub is_new: bool,
}

/// Result from the Composition Agent.
#[derive(Debug)]
pub struct ComposeResult {
    pub graph: CompositionGraph,
    /// Total LLM attempts used in the final composition round.
    pub attempts: u32,
    /// Stages synthesized during this compose call (0 or 1).
    pub synthesized: Vec<SynthesisResult>,
}

// ── Agent ──────────────────────────────────────────────────────────────────

/// The Composition Agent translates problem descriptions into valid composition graphs.
/// When no existing stage satisfies the required signature, it can synthesize a new one.
pub struct CompositionAgent<'a> {
    index: &'a mut SemanticIndex,
    llm: &'a dyn LlmProvider,
    llm_config: LlmConfig,
    max_retries: u32,
    /// Ephemeral Ed25519 key generated at construction; used to sign all stages
    /// synthesized during this agent session.
    ephemeral_signing_key: SigningKey,
}

impl<'a> CompositionAgent<'a> {
    pub fn new(
        index: &'a mut SemanticIndex,
        llm: &'a dyn LlmProvider,
        llm_config: LlmConfig,
        max_retries: u32,
    ) -> Self {
        Self {
            index,
            llm,
            llm_config,
            max_retries,
            ephemeral_signing_key: SigningKey::generate(&mut rand::rngs::OsRng),
        }
    }

    /// Translate a problem description into a valid composition graph.
    ///
    /// If the LLM determines that a new stage is needed it triggers synthesis
    /// (at most once per call): the stage is registered in `store`, indexed,
    /// then composition is retried with the new stage available.
    pub fn compose(
        &mut self,
        problem: &str,
        store: &mut dyn StageStore,
    ) -> Result<ComposeResult, AgentError> {
        let verbose = std::env::var("NOETHER_VERBOSE").is_ok();
        let mut synthesized: Vec<SynthesisResult> = Vec::new();
        let mut synthesis_done = false;

        // Outer loop: at most two passes — one normal, one post-synthesis.
        loop {
            // Build prompt inside a block so the store borrow is released
            // before we might need to mutate the store during synthesis.
            let (system_prompt, user_msg) = {
                let search_results = self
                    .index
                    .search(problem, 20)
                    .map_err(|e| AgentError::Search(e.to_string()))?;

                if verbose {
                    eprintln!("\n[compose] Semantic search: \"{}\"", problem);
                    eprintln!("[compose] Found {} candidates:", search_results.len());
                    for (i, r) in search_results.iter().enumerate().take(10) {
                        if let Ok(Some(s)) = store.get(&r.stage_id) {
                            eprintln!(
                                "  {:>2}. {:.3}  {}  {}",
                                i + 1,
                                r.score,
                                &s.id.0[..8],
                                &s.description[..s.description.len().min(60)]
                            );
                        }
                    }
                    if search_results.len() > 10 {
                        eprintln!("  ... and {} more", search_results.len() - 10);
                    }
                }

                let candidates: Vec<_> = search_results
                    .iter()
                    .filter_map(|r| {
                        store
                            .get(&r.stage_id)
                            .ok()
                            .flatten()
                            .map(|stage| (r, stage))
                    })
                    .collect();

                let sp = build_system_prompt(&candidates);

                if verbose {
                    eprintln!(
                        "\n[compose] System prompt: {} chars, {} candidate stages",
                        sp.len(),
                        candidates.len()
                    );
                }

                let um = match synthesized.last() {
                    Some(syn) => format!(
                        "{problem}\n\nIMPORTANT: Stage `{id}` has been synthesized and added to \
                         the Available Stages list above. Now output a COMPOSITION GRAPH (not \
                         another synthesis request) that uses this stage. Output ONLY a JSON \
                         code block containing the CompositionGraph.",
                        id = syn.stage_id.0
                    ),
                    None => problem.to_string(),
                };
                (sp, um)
                // search_results and candidates (which borrow store) are dropped here
            };

            let mut messages = vec![Message::system(&system_prompt), Message::user(&user_msg)];
            let mut last_errors = String::new();
            let mut last_error_type = LastErrorType::None;
            let mut did_synthesize_this_round = false;

            for attempt in 1..=self.max_retries {
                if verbose {
                    eprintln!(
                        "\n[compose] LLM call (attempt {}/{}, model: {})",
                        attempt, self.max_retries, self.llm_config.model
                    );
                }
                let response = self.llm.complete(&messages, &self.llm_config)?;

                if verbose {
                    // Show a condensed version of the response
                    let trimmed = response.trim();
                    if trimmed.len() <= 300 {
                        eprintln!("[compose] LLM response:\n{trimmed}");
                    } else {
                        eprintln!(
                            "[compose] LLM response ({} chars):\n{}...",
                            trimmed.len(),
                            &trimmed[..300]
                        );
                    }
                }

                // Optional raw-response debug output.
                if std::env::var("NOETHER_DEBUG").is_ok() {
                    eprintln!(
                        "[agent debug] attempt {attempt} raw response:\n---\n{response}\n---"
                    );
                }

                // Check for synthesis request (only once per compose call).
                if !synthesis_done {
                    if let Some(spec) = extract_synthesis_spec(&response) {
                        let syn = self.synthesize_stage(&spec, store)?;
                        // Only index the stage when it is genuinely new.
                        if syn.is_new {
                            let new_stage = store
                                .get(&syn.stage_id)
                                .map_err(|e| AgentError::SynthesisFailed(e.to_string()))?
                                .ok_or_else(|| {
                                    AgentError::SynthesisFailed(
                                        "synthesized stage missing from store".into(),
                                    )
                                })?;
                            self.index
                                .add_stage(new_stage)
                                .map_err(|e| AgentError::SynthesisFailed(e.to_string()))?;
                        }
                        synthesized.push(syn);
                        synthesis_done = true;
                        did_synthesize_this_round = true;
                        break; // break inner loop → outer loop retries
                    }
                } else if extract_synthesis_spec(&response).is_some() {
                    // Synthesis already done but LLM returned another synthesis request.
                    // Redirect: ask it to produce a composition graph using the new stage.
                    let stage_id = synthesized
                        .last()
                        .map(|s| s.stage_id.0.as_str())
                        .unwrap_or("the newly synthesized stage");
                    last_error_type = LastErrorType::InvalidGraph;
                    last_errors = "synthesis already performed".into();
                    if attempt < self.max_retries {
                        messages.push(Message::assistant(&response));
                        messages.push(Message::user(format!(
                            "The new stage has already been synthesized (id: `{stage_id}`). \
                             Now produce a COMPOSITION GRAPH (not another synthesis request) \
                             that uses this stage. Output ONLY a JSON code block."
                        )));
                    }
                    continue;
                }

                // Normal composition path.
                let json_str = match extract_json(&response) {
                    Some(j) => j.to_string(),
                    None => {
                        last_error_type = LastErrorType::NoJson;
                        if attempt < self.max_retries {
                            messages.push(Message::assistant(&response));
                            messages.push(Message::user(
                                "Your response contained no JSON code block. \
                                 Respond with ONLY a JSON code block containing the \
                                 CompositionGraph.",
                            ));
                        }
                        continue;
                    }
                };

                let graph = match parse_graph(&json_str) {
                    Ok(g) => g,
                    Err(e) => {
                        last_errors = e.to_string();
                        last_error_type = LastErrorType::InvalidGraph;
                        if attempt < self.max_retries {
                            messages.push(Message::assistant(&response));
                            let hint = if last_errors.contains("missing field `op`") {
                                " REMINDER: every node in the graph MUST have an \"op\" field \
                                 (\"Stage\", \"Sequential\", \"Parallel\", \"Branch\", etc.). \
                                 A synthesis request (\"action\": \"synthesize\") is NOT a valid \
                                 graph node — it must be a standalone top-level response."
                            } else {
                                ""
                            };
                            messages.push(Message::user(format!(
                                "The JSON was not a valid CompositionGraph: {e}.{hint} \
                                 Please fix and try again."
                            )));
                        }
                        continue;
                    }
                };

                match check_graph(&graph.root, store) {
                    Ok(_) => {
                        if verbose {
                            eprintln!("[compose] ✓ Type check passed on attempt {attempt}");
                        }
                        return Ok(ComposeResult {
                            graph,
                            attempts: attempt,
                            synthesized,
                        });
                    }
                    Err(errors) => {
                        last_errors = errors
                            .iter()
                            .map(|e| format!("{e}"))
                            .collect::<Vec<_>>()
                            .join("; ");
                        last_error_type = LastErrorType::TypeCheck;
                        if verbose {
                            eprintln!(
                                "[compose] ✗ Type error on attempt {attempt}: {}",
                                &last_errors[..last_errors.len().min(150)]
                            );
                        }
                        if attempt < self.max_retries {
                            messages.push(Message::assistant(&response));
                            messages.push(Message::user(format!(
                                "The composition graph has type errors:\n{last_errors}\n\n\
                                 If the error is about a bare value (List, Text, Number) not matching \
                                 a Record input, DO NOT try to fix it with Parallel+Const wiring. \
                                 Instead, SYNTHESIZE a single stage that performs the entire operation. \
                                 Otherwise, fix the graph and try again."
                            )));
                        }
                    }
                }
            }

            // If synthesis happened this round, loop again with the new stage available.
            if did_synthesize_this_round {
                continue;
            }

            // Inner loop exhausted all attempts without a valid graph.
            return Err(match last_error_type {
                LastErrorType::NoJson => AgentError::NoJsonInResponse,
                LastErrorType::InvalidGraph => AgentError::InvalidGraph(last_errors),
                LastErrorType::TypeCheck | LastErrorType::None => AgentError::TypeCheckFailed {
                    attempts: self.max_retries,
                    errors: last_errors,
                },
            });
        }
    }

    /// Synthesize a new stage from a spec: call the LLM for implementation + examples,
    /// validate examples against the declared types, register in `store`.
    fn synthesize_stage(
        &self,
        spec: &SynthesisSpec,
        store: &mut dyn StageStore,
    ) -> Result<SynthesisResult, AgentError> {
        let synthesis_prompt = build_synthesis_prompt(spec);
        let messages = vec![
            Message::system(&synthesis_prompt),
            Message::user(format!("Implement the `{}` stage.", spec.name)),
        ];

        let mut last_error = String::new();

        for attempt in 1..=self.max_retries {
            let response = self.llm.complete(&messages, &self.llm_config)?;

            let syn_resp = match extract_synthesis_response(&response) {
                Some(r) => r,
                None => {
                    last_error = "no valid synthesis JSON in LLM response".into();
                    continue;
                }
            };

            if let Err(e) =
                validate_synthesis_examples(&syn_resp.examples, &spec.input, &spec.output)
            {
                last_error = e;
                continue;
            }

            let impl_hash = compute_impl_hash(&syn_resp.implementation);

            // Effect inference: ask the LLM what effects the generated code has.
            // On failure (or non-deterministic response) we fall back to Unknown gracefully.
            let inferred_effects = {
                let inference_prompt =
                    build_effect_inference_prompt(&syn_resp.implementation, &syn_resp.language);
                let inference_messages = vec![
                    Message::system(&inference_prompt),
                    Message::user("Analyze the code above and return the effects JSON array."),
                ];
                match self.llm.complete(&inference_messages, &self.llm_config) {
                    Ok(resp) => extract_effect_response(&resp),
                    Err(_) => noether_core::effects::EffectSet::unknown(),
                }
            };

            let mut builder = StageBuilder::new(&spec.name)
                .input(spec.input.clone())
                .output(spec.output.clone())
                .description(&spec.description)
                .implementation_code(&syn_resp.implementation, &syn_resp.language)
                .effects(inferred_effects);

            for ex in &syn_resp.examples {
                builder = builder.example(ex.input.clone(), ex.output.clone());
            }

            let stage: noether_core::stage::Stage =
                match builder.build_signed(&self.ephemeral_signing_key, impl_hash) {
                    Ok(s) => s,
                    Err(e) => {
                        last_error = e.to_string();
                        continue;
                    }
                };

            // Pre-insertion deduplication: if an existing stage is semantically
            // near-identical (>= 0.92 cosine on description), reuse it instead.
            // Exception: if the existing stage has no signature, replace it with the
            // newly signed version so that signature verification passes.
            if let Ok(Some((existing_id, similarity))) = self
                .index
                .check_duplicate_before_insert(&spec.description, 0.92)
            {
                let existing_is_signed = store
                    .get(&existing_id)
                    .ok()
                    .flatten()
                    .map(|s| s.ed25519_signature.is_some())
                    .unwrap_or(false);

                if existing_is_signed {
                    eprintln!(
                        "Synthesis dedup: description matches existing stage {} \
                         (similarity {similarity:.3}); reusing.",
                        existing_id.0
                    );
                    return Ok(SynthesisResult {
                        stage_id: existing_id,
                        implementation: syn_resp.implementation,
                        language: syn_resp.language,
                        attempts: attempt,
                        is_new: false,
                    });
                }
                // Existing stage is unsigned — fall through to upsert with signed version.
                eprintln!(
                    "Synthesis dedup: existing stage {} is unsigned; replacing with signed version.",
                    existing_id.0
                );
            }

            let (stage_id, is_new) = match store.put(stage.clone()) {
                Ok(id) => {
                    // Newly inserted as Draft — promote to Active.
                    store
                        .update_lifecycle(&id, StageLifecycle::Active)
                        .map_err(|e| AgentError::SynthesisFailed(e.to_string()))?;
                    (id, true)
                }
                // A stage with the same signature already exists.
                // If the existing stage lacks a signature, replace it with the signed version.
                Err(StoreError::AlreadyExists(id)) => {
                    let needs_signing = store
                        .get(&id)
                        .ok()
                        .flatten()
                        .map(|s| s.ed25519_signature.is_none())
                        .unwrap_or(false);
                    if needs_signing {
                        store
                            .upsert(stage)
                            .map_err(|e| AgentError::SynthesisFailed(e.to_string()))?;
                        eprintln!(
                            "Synthesis: replaced unsigned stage {} with signed version.",
                            id.0
                        );
                    }
                    (id, false)
                }
                Err(e) => return Err(AgentError::SynthesisFailed(e.to_string())),
            };

            return Ok(SynthesisResult {
                stage_id,
                implementation: syn_resp.implementation,
                language: syn_resp.language,
                attempts: attempt,
                is_new,
            });
        }

        Err(AgentError::SynthesisFailed(last_error))
    }
}

// ── Helpers ────────────────────────────────────────────────────────────────

#[derive(Debug)]
enum LastErrorType {
    None,
    NoJson,
    InvalidGraph,
    TypeCheck,
}

/// Validate that all examples structurally conform to the declared types.
/// Requires at least 3 examples.
fn validate_synthesis_examples(
    examples: &[prompt::SynthesisExample],
    input_type: &noether_core::types::NType,
    output_type: &noether_core::types::NType,
) -> Result<(), String> {
    if examples.len() < 3 {
        return Err(format!("need at least 3 examples, got {}", examples.len()));
    }

    for (i, ex) in examples.iter().enumerate() {
        let inferred = infer_type(&ex.input);
        if matches!(
            is_subtype_of(&inferred, input_type),
            TypeCompatibility::Incompatible(_)
        ) {
            return Err(format!(
                "example {i} input `{inferred}` is not subtype of `{input_type}`"
            ));
        }

        let inferred = infer_type(&ex.output);
        if matches!(
            is_subtype_of(&inferred, output_type),
            TypeCompatibility::Incompatible(_)
        ) {
            return Err(format!(
                "example {i} output `{inferred}` is not subtype of `{output_type}`"
            ));
        }
    }

    Ok(())
}

/// SHA-256 hex digest of an implementation string — used as implementation_hash.
fn compute_impl_hash(implementation: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(implementation.as_bytes()))
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::embedding::MockEmbeddingProvider;
    use crate::index::IndexConfig;
    use crate::llm::{MockLlmProvider, SequenceMockLlmProvider};
    use noether_core::stdlib::load_stdlib;
    use noether_core::types::NType;
    use noether_store::{MemoryStore, StageStore};

    fn test_setup() -> (MemoryStore, SemanticIndex) {
        let mut store = MemoryStore::new();
        for stage in load_stdlib() {
            store.put(stage).unwrap();
        }
        let index = SemanticIndex::build(
            &store,
            Box::new(MockEmbeddingProvider::new(128)),
            IndexConfig::default(),
        )
        .unwrap();
        (store, index)
    }

    fn find_stage_id(store: &MemoryStore, desc_contains: &str) -> String {
        store
            .list(None)
            .into_iter()
            .find(|s| s.description.contains(desc_contains))
            .unwrap()
            .id
            .0
            .clone()
    }

    // ── Composition tests (existing behaviour) ─────────────────────────────

    #[test]
    fn compose_with_valid_mock_response() {
        let (mut store, mut index) = test_setup();
        let to_text_id = find_stage_id(&store, "Convert any value to its text");

        let mock_response = format!(
            "```json\n{}\n```",
            serde_json::json!({
                "description": "convert to text",
                "version": "0.1.0",
                "root": { "op": "Stage", "id": to_text_id }
            })
        );

        let llm = MockLlmProvider::new(mock_response);
        let mut agent = CompositionAgent::new(&mut index, &llm, LlmConfig::default(), 3);
        let result = agent.compose("convert input to text", &mut store).unwrap();
        assert_eq!(result.attempts, 1);
        assert_eq!(result.graph.description, "convert to text");
        assert!(result.synthesized.is_empty());
    }

    #[test]
    fn compose_with_valid_sequential() {
        let (mut store, mut index) = test_setup();
        let to_json_id = find_stage_id(&store, "Serialize any value to a JSON");
        let parse_json_id = find_stage_id(&store, "Parse a JSON string");

        let mock_response = format!(
            "```json\n{}\n```",
            serde_json::json!({
                "description": "round-trip JSON",
                "version": "0.1.0",
                "root": {
                    "op": "Sequential",
                    "stages": [
                        {"op": "Stage", "id": to_json_id},
                        {"op": "Stage", "id": parse_json_id}
                    ]
                }
            })
        );

        let llm = MockLlmProvider::new(mock_response);
        let mut agent = CompositionAgent::new(&mut index, &llm, LlmConfig::default(), 3);
        let result = agent
            .compose("serialize and parse JSON", &mut store)
            .unwrap();
        assert_eq!(result.attempts, 1);
    }

    #[test]
    fn compose_fails_with_no_json() {
        let (mut store, mut index) = test_setup();
        let llm = MockLlmProvider::new("I don't know how to help with that.");
        let mut agent = CompositionAgent::new(&mut index, &llm, LlmConfig::default(), 1);
        assert!(agent.compose("do something", &mut store).is_err());
    }

    #[test]
    fn compose_fails_with_invalid_stage_id() {
        let (mut store, mut index) = test_setup();
        let mock_response = "```json\n{\"description\":\"test\",\"version\":\"0.1.0\",\"root\":{\"op\":\"Stage\",\"id\":\"nonexistent\"}}\n```";
        let llm = MockLlmProvider::new(mock_response);
        let mut agent = CompositionAgent::new(&mut index, &llm, LlmConfig::default(), 1);
        assert!(agent.compose("test", &mut store).is_err());
    }

    // ── Synthesis tests ────────────────────────────────────────────────────

    /// Validates examples against types — acceptance case.
    #[test]
    fn validate_examples_accepts_valid_set() {
        use serde_json::json;
        let examples = vec![
            prompt::SynthesisExample {
                input: json!("hello"),
                output: json!(5),
            },
            prompt::SynthesisExample {
                input: json!("hi"),
                output: json!(2),
            },
            prompt::SynthesisExample {
                input: json!("world"),
                output: json!(5),
            },
        ];
        assert!(validate_synthesis_examples(&examples, &NType::Text, &NType::Number).is_ok());
    }

    /// Validates examples — rejects when output type mismatches.
    #[test]
    fn validate_examples_rejects_wrong_output_type() {
        use serde_json::json;
        let examples = vec![
            prompt::SynthesisExample {
                input: json!("hello"),
                output: json!("five"), // should be Number
            },
            prompt::SynthesisExample {
                input: json!("hi"),
                output: json!("two"),
            },
            prompt::SynthesisExample {
                input: json!("world"),
                output: json!("five"),
            },
        ];
        assert!(validate_synthesis_examples(&examples, &NType::Text, &NType::Number).is_err());
    }

    /// Validates examples — rejects when fewer than 3 examples provided.
    #[test]
    fn validate_examples_rejects_too_few() {
        use serde_json::json;
        let examples = vec![
            prompt::SynthesisExample {
                input: json!("hello"),
                output: json!(5),
            },
            prompt::SynthesisExample {
                input: json!("hi"),
                output: json!(2),
            },
        ];
        assert!(validate_synthesis_examples(&examples, &NType::Text, &NType::Number).is_err());
    }

    /// Full synthesis flow: first LLM call returns a synthesis request, second
    /// returns the implementation, third returns the final composition graph.
    #[test]
    fn compose_triggers_synthesis_then_succeeds() {
        use serde_json::json;

        let (mut store, mut index) = test_setup();
        let to_text_id = find_stage_id(&store, "Convert any value to its text");

        // Round 1: LLM signals synthesis needed for a "count_words" stage.
        let synthesis_request = format!(
            "```json\n{}\n```",
            json!({
                "action": "synthesize",
                "spec": {
                    "name": "count_words",
                    "description": "Count the number of words in a text string",
                    "input": {"kind": "Text"},
                    "output": {"kind": "Number"},
                    "rationale": "No existing stage counts words in text"
                }
            })
        );

        // Round 2 (codegen): LLM returns implementation + valid examples.
        let synthesis_response = format!(
            "```json\n{}\n```",
            json!({
                "examples": [
                    {"input": "hello world", "output": 2.0},
                    {"input": "one two three", "output": 3.0},
                    {"input": "single", "output": 1.0}
                ],
                "implementation": "def execute(input_value):\n    return len(input_value.split())",
                "language": "python"
            })
        );

        // Round 2b (effect inference): LLM returns effect classification.
        let effect_inference_response = "```json\n[\"Pure\"]\n```".to_string();

        // Round 3: LLM composes using the newly synthesized stage ID.
        // We use to_text as a stand-in since we don't know count_words ID yet.
        // The actual test verifies the graph passes type-check.
        let composition = format!(
            "```json\n{}\n```",
            json!({
                "description": "convert input to text",
                "version": "0.1.0",
                "root": {"op": "Stage", "id": to_text_id}
            })
        );

        let llm = SequenceMockLlmProvider::new(
            vec![
                synthesis_request,
                synthesis_response,
                effect_inference_response,
                composition,
            ],
            "no more responses".to_string(),
        );

        let mut agent = CompositionAgent::new(&mut index, &llm, LlmConfig::default(), 3);
        let result = agent
            .compose("count the words in some text", &mut store)
            .unwrap();

        // One stage was synthesized.
        assert_eq!(result.synthesized.len(), 1);
        let syn = &result.synthesized[0];
        assert_eq!(syn.language, "python");
        assert!(syn.implementation.contains("execute"));

        // The synthesized stage is in the store and active.
        let new_stage = store.get(&syn.stage_id).unwrap().unwrap();
        assert_eq!(new_stage.lifecycle, StageLifecycle::Active);
        assert_eq!(new_stage.signature.input, NType::Text);
        assert_eq!(new_stage.signature.output, NType::Number);
        assert_eq!(new_stage.examples.len(), 3);
    }

    /// When synthesis codegen returns bad examples, the agent returns SynthesisFailed.
    #[test]
    fn compose_synthesis_fails_on_bad_examples() {
        use serde_json::json;

        let (mut store, mut index) = test_setup();

        let synthesis_request = format!(
            "```json\n{}\n```",
            json!({
                "action": "synthesize",
                "spec": {
                    "name": "bad_stage",
                    "description": "A stage with wrong example types",
                    "input": {"kind": "Text"},
                    "output": {"kind": "Number"},
                    "rationale": "testing"
                }
            })
        );

        // Wrong output type in all examples (Text instead of Number).
        let bad_codegen = format!(
            "```json\n{}\n```",
            json!({
                "examples": [
                    {"input": "a", "output": "wrong"},
                    {"input": "b", "output": "wrong"},
                    {"input": "c", "output": "wrong"}
                ],
                "implementation": "def execute(v): return 'wrong'",
                "language": "python"
            })
        );

        let llm = SequenceMockLlmProvider::new(
            vec![
                synthesis_request,
                bad_codegen.clone(),
                bad_codegen.clone(),
                bad_codegen,
            ],
            String::new(),
        );

        let mut agent = CompositionAgent::new(&mut index, &llm, LlmConfig::default(), 1);
        let result = agent.compose("do something", &mut store);
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), AgentError::SynthesisFailed(_)),
            "expected SynthesisFailed"
        );
    }

    /// After synthesis, if the LLM keeps returning synthesis requests, the agent
    /// redirects it to produce a composition graph.
    #[test]
    fn compose_redirects_after_duplicate_synthesis_request() {
        use serde_json::json;

        let (mut store, mut index) = test_setup();
        let to_text_id = find_stage_id(&store, "Convert any value to its text");

        let synthesis_request = format!(
            "```json\n{}\n```",
            json!({
                "action": "synthesize",
                "spec": {
                    "name": "count_chars",
                    "description": "Count characters in a string",
                    "input": {"kind": "Text"},
                    "output": {"kind": "Number"},
                    "rationale": "No existing stage counts characters"
                }
            })
        );
        let codegen = format!(
            "```json\n{}\n```",
            json!({
                "examples": [
                    {"input": "hi", "output": 2.0},
                    {"input": "hello", "output": 5.0},
                    {"input": "world", "output": 5.0}
                ],
                "implementation": "def execute(v): return len(v)",
                "language": "python"
            })
        );
        let effect_resp = "```json\n[\"Pure\"]\n```".to_string();
        // Second outer pass: LLM returns synthesis request again (bug scenario),
        // then a valid graph on retry.
        let graph = format!(
            "```json\n{}\n```",
            json!({
                "description": "count chars",
                "version": "0.1.0",
                "root": {"op": "Stage", "id": to_text_id}
            })
        );

        let llm = SequenceMockLlmProvider::new(
            vec![
                synthesis_request.clone(), // round 1: trigger synthesis
                codegen,                   // codegen for synthesis
                effect_resp,               // effect inference
                synthesis_request,         // round 2 attempt 1: LLM repeats synthesis → redirect
                graph,                     // round 2 attempt 2: proper graph
            ],
            String::new(),
        );

        let mut agent = CompositionAgent::new(&mut index, &llm, LlmConfig::default(), 3);
        let result = agent.compose("count characters in text", &mut store);
        assert!(result.is_ok(), "expected Ok, got: {result:?}");
        assert_eq!(result.unwrap().synthesized.len(), 1);
    }

    /// Synthesis is idempotent: registering the same implementation twice does not error.
    #[test]
    fn synthesize_stage_is_idempotent() {
        use serde_json::json;

        let (mut store, mut index) = test_setup();

        let synthesis_request = format!(
            "```json\n{}\n```",
            json!({
                "action": "synthesize",
                "spec": {
                    "name": "noop_stage",
                    "description": "Return input unchanged",
                    "input": {"kind": "Text"},
                    "output": {"kind": "Text"},
                    "rationale": "testing idempotency"
                }
            })
        );

        let codegen = format!(
            "```json\n{}\n```",
            json!({
                "examples": [
                    {"input": "a", "output": "a"},
                    {"input": "b", "output": "b"},
                    {"input": "c", "output": "c"}
                ],
                "implementation": "def execute(v): return v",
                "language": "python"
            })
        );

        let effect_inference_response = "```json\n[\"Pure\"]\n```".to_string();

        let to_text_id = find_stage_id(&store, "Convert any value to its text");
        let graph_json = format!(
            "```json\n{}\n```",
            json!({
                "description": "noop",
                "version": "0.1.0",
                "root": {"op": "Stage", "id": to_text_id}
            })
        );

        // First compose (triggers synthesis).
        {
            let llm = SequenceMockLlmProvider::new(
                vec![
                    synthesis_request.clone(),
                    codegen.clone(),
                    effect_inference_response.clone(),
                    graph_json.clone(),
                ],
                String::new(),
            );
            let mut agent = CompositionAgent::new(&mut index, &llm, LlmConfig::default(), 3);
            agent.compose("noop", &mut store).unwrap();
        }

        // Second compose with identical synthesis response — should not fail.
        {
            let llm = SequenceMockLlmProvider::new(
                vec![
                    synthesis_request,
                    codegen,
                    effect_inference_response,
                    graph_json,
                ],
                String::new(),
            );
            let mut agent = CompositionAgent::new(&mut index, &llm, LlmConfig::default(), 3);
            let result = agent.compose("noop", &mut store);
            assert!(result.is_ok());
        }
    }
}
