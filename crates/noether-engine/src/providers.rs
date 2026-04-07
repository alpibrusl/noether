//! Provider factory — builds embedding and LLM providers from environment config.
//!
//! ## Provider priority (LLM and embeddings)
//!
//! 1. **Mistral native** (`MISTRAL_API_KEY` set) — calls `api.mistral.ai` directly.
//!    Preferred for the European deployment stack: no Google Cloud dependency,
//!    EU-hosted, GDPR-compliant. Set `MISTRAL_MODEL` to override the model.
//!
//! 2. **Vertex AI** (`VERTEX_AI_PROJECT` + credentials set) — Google Cloud.
//!    Supports Mistral, Gemini, and Claude via Vertex Model Garden.
//!
//! 3. **Mock** (fallback) — deterministic hash-based embeddings, echo LLM.
//!    Used in tests and when no cloud credentials are present.
//!
//! ## Environment variables
//!
//! | Variable | Description | Default |
//! |---|---|---|
//! | `MISTRAL_API_KEY` | Native Mistral API key (console.mistral.ai) | — |
//! | `MISTRAL_MODEL` | Mistral model name | `mistral-small-latest` |
//! | `MISTRAL_EMBEDDING_MODEL` | Mistral embedding model | `mistral-embed` |
//! | `VERTEX_AI_PROJECT` | GCP project ID | `a2p-common` |
//! | `VERTEX_AI_LOCATION` | GCP region | `europe-west4` |
//! | `VERTEX_AI_TOKEN` | Static GCP auth token | auto-detect |
//! | `VERTEX_AI_MODEL` | Vertex model name | `mistral-small-2503` |
//! | `NOETHER_LLM_PROVIDER` | Force: `mistral` \| `vertex` \| `mock` | auto |
//! | `NOETHER_EMBEDDING_PROVIDER` | Force: `mistral` \| `vertex` \| `mock` | auto |

use crate::index::embedding::{EmbeddingProvider, MockEmbeddingProvider};
use crate::llm::mistral::{MistralNativeEmbeddingProvider, MistralNativeProvider};
use crate::llm::vertex::{
    MistralLlmProvider, VertexAiConfig, VertexAiEmbeddingProvider, VertexAiLlmProvider,
};
use crate::llm::{LlmProvider, MockLlmProvider};

/// Returns true if the model name is a Mistral/Codestral model.
fn is_mistral_model(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower.contains("mistral") || lower.contains("codestral")
}

/// Build the best available LLM provider based on env config.
///
/// Priority: Mistral native → Vertex AI → Mock.
pub fn build_llm_provider() -> (Box<dyn LlmProvider>, &'static str) {
    let forced = std::env::var("NOETHER_LLM_PROVIDER").unwrap_or_default();

    match forced.as_str() {
        "mock" => return (Box::new(MockLlmProvider::new("{}")), "mock"),
        "mistral" => match build_mistral_native_llm() {
            Ok(p) => return (p, "mistral-native"),
            Err(e) => {
                eprintln!("Warning: Mistral native LLM unavailable: {e}. Falling back.");
            }
        },
        "vertex" => match build_vertex_or_mistral_llm() {
            Ok((p, name)) => return (p, name),
            Err(e) => {
                eprintln!("Warning: Vertex AI LLM unavailable: {e}. Falling back to mock.");
                return (Box::new(MockLlmProvider::new("{}")), "mock");
            }
        },
        _ => {} // auto-detect below
    }

    // Auto-detect: Mistral native first, then Vertex, then mock.
    if let Ok(p) = build_mistral_native_llm() {
        return (p, "mistral-native");
    }
    if let Ok((p, name)) = build_vertex_or_mistral_llm() {
        return (p, name);
    }
    eprintln!("Warning: No LLM provider configured. Using mock.");
    eprintln!("  Set MISTRAL_API_KEY for the native Mistral API (recommended),");
    eprintln!("  or set GOOGLE_APPLICATION_CREDENTIALS for Vertex AI.");
    (Box::new(MockLlmProvider::new("{}")), "mock")
}

/// Build the best available embedding provider based on env config.
///
/// Priority: Mistral native → Vertex AI → Mock.
pub fn build_embedding_provider() -> (Box<dyn EmbeddingProvider>, &'static str) {
    let forced = std::env::var("NOETHER_EMBEDDING_PROVIDER").unwrap_or_default();

    match forced.as_str() {
        "mock" => return (Box::new(MockEmbeddingProvider::new(128)), "mock"),
        "mistral" => match MistralNativeEmbeddingProvider::from_env() {
            Ok(p) => return (Box::new(p), "mistral-native"),
            Err(e) => {
                eprintln!("Warning: Mistral native embeddings unavailable: {e}. Falling back.");
            }
        },
        "vertex" => match build_vertex_embedding() {
            Ok(p) => return (p, "vertex"),
            Err(e) => {
                eprintln!("Warning: Vertex AI embeddings unavailable: {e}. Falling back to mock.");
                return (Box::new(MockEmbeddingProvider::new(128)), "mock");
            }
        },
        _ => {} // auto-detect below
    }

    // Auto-detect: Mistral native first (1024-dim), then Vertex, then mock.
    if let Ok(p) = MistralNativeEmbeddingProvider::from_env() {
        return (Box::new(p), "mistral-native");
    }
    if let Ok(p) = build_vertex_embedding() {
        return (p, "vertex");
    }
    (Box::new(MockEmbeddingProvider::new(128)), "mock")
}

// ── Internal builders ────────────────────────────────────────────────────────

fn build_mistral_native_llm() -> Result<Box<dyn LlmProvider>, String> {
    Ok(Box::new(MistralNativeProvider::from_env()?))
}

fn build_vertex_or_mistral_llm() -> Result<(Box<dyn LlmProvider>, &'static str), String> {
    let model = std::env::var("VERTEX_AI_MODEL")
        .unwrap_or_else(|_| crate::llm::LlmConfig::default().model.clone());
    let config = VertexAiConfig::from_env()?;

    if is_mistral_model(&model) {
        Ok((Box::new(MistralLlmProvider::new(config)), "mistral-vertex"))
    } else {
        Ok((Box::new(VertexAiLlmProvider::new(config)), "vertex"))
    }
}

fn build_vertex_embedding() -> Result<Box<dyn EmbeddingProvider>, String> {
    let config = VertexAiConfig::from_env()?;
    let model = std::env::var("VERTEX_AI_EMBEDDING_MODEL").ok();
    let dimensions = std::env::var("VERTEX_AI_EMBEDDING_DIMENSIONS")
        .ok()
        .and_then(|s| s.parse().ok());
    Ok(Box::new(VertexAiEmbeddingProvider::new(config, model, dimensions)))
}
