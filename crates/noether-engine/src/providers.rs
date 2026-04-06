//! Provider factory — builds embedding and LLM providers from environment config.
//!
//! Configuration via env vars:
//!   NOETHER_EMBEDDING_PROVIDER: "vertex" | "mock" (default: auto-detect)
//!   NOETHER_LLM_PROVIDER: "vertex" | "mock" (default: auto-detect)
//!   VERTEX_AI_PROJECT: GCP project (default: a2p-common)
//!   VERTEX_AI_LOCATION: region (default: global)
//!   VERTEX_AI_TOKEN: auth token (required for vertex providers)
//!   VERTEX_AI_MODEL: LLM model (default: gemini-2.5-flash)
//!   VERTEX_AI_EMBEDDING_MODEL: embedding model (default: text-embedding-005)
//!   VERTEX_AI_EMBEDDING_DIMENSIONS: embedding dimensions (default: 256)

use crate::index::embedding::{EmbeddingProvider, MockEmbeddingProvider};
use crate::llm::vertex::{VertexAiConfig, VertexAiEmbeddingProvider, VertexAiLlmProvider};
use crate::llm::{LlmProvider, MockLlmProvider};

/// Build the best available embedding provider based on env config.
/// Falls back to MockEmbeddingProvider if no cloud provider is configured.
pub fn build_embedding_provider() -> (Box<dyn EmbeddingProvider>, &'static str) {
    let provider_name = std::env::var("NOETHER_EMBEDDING_PROVIDER").unwrap_or_default();

    match provider_name.as_str() {
        "mock" => (Box::new(MockEmbeddingProvider::new(128)), "mock"),
        "vertex" => match build_vertex_embedding() {
            Ok(p) => (p, "vertex"),
            Err(e) => {
                eprintln!("Warning: Vertex AI embedding unavailable: {e}. Using mock.");
                (Box::new(MockEmbeddingProvider::new(128)), "mock")
            }
        },
        _ => {
            // Auto-detect: try vertex, fall back to mock
            match build_vertex_embedding() {
                Ok(p) => (p, "vertex"),
                Err(_) => (Box::new(MockEmbeddingProvider::new(128)), "mock"),
            }
        }
    }
}

/// Build the best available LLM provider based on env config.
/// Falls back to MockLlmProvider if no cloud provider is configured.
pub fn build_llm_provider() -> (Box<dyn LlmProvider>, &'static str) {
    let provider_name = std::env::var("NOETHER_LLM_PROVIDER").unwrap_or_default();

    match provider_name.as_str() {
        "mock" => (Box::new(MockLlmProvider::new("{}")), "mock"),
        "vertex" => match build_vertex_llm() {
            Ok(p) => (p, "vertex"),
            Err(e) => {
                eprintln!("Warning: Vertex AI LLM unavailable: {e}. Using mock.");
                (Box::new(MockLlmProvider::new("{}")), "mock")
            }
        },
        _ => {
            // Auto-detect: try vertex, fall back to mock
            match build_vertex_llm() {
                Ok(p) => (p, "vertex"),
                Err(e) => {
                    eprintln!("Warning: No LLM provider configured ({e}). Using mock.");
                    eprintln!("Set VERTEX_AI_TOKEN or NOETHER_LLM_PROVIDER to configure.");
                    (Box::new(MockLlmProvider::new("{}")), "mock")
                }
            }
        }
    }
}

fn build_vertex_embedding() -> Result<Box<dyn EmbeddingProvider>, String> {
    let config = VertexAiConfig::from_env()?;
    let model = std::env::var("VERTEX_AI_EMBEDDING_MODEL").ok();
    let dimensions = std::env::var("VERTEX_AI_EMBEDDING_DIMENSIONS")
        .ok()
        .and_then(|s| s.parse().ok());
    Ok(Box::new(VertexAiEmbeddingProvider::new(
        config, model, dimensions,
    )))
}

fn build_vertex_llm() -> Result<Box<dyn LlmProvider>, String> {
    let config = VertexAiConfig::from_env()?;
    Ok(Box::new(VertexAiLlmProvider::new(config)))
}
