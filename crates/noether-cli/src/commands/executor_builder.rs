//! Builds a `CompositeExecutor` with LLM and embedding providers wired from
//! environment variables. Shared between `run.rs` and `compose.rs`.
//!
//! Uses `noether_engine::providers::build_llm_provider()` and
//! `build_embedding_provider()` for provider selection logic.

use noether_engine::executor::composite::CompositeExecutor;
use noether_engine::providers;
use noether_store::StageStore;

/// Build a `CompositeExecutor` for `store`, injecting LLM / embedding providers
/// detected from environment variables.
pub fn build_executor(store: &dyn StageStore) -> CompositeExecutor {
    let (llm_provider, llm_name) = providers::build_llm_provider();
    let (emb_provider, emb_name) = providers::build_embedding_provider();

    // Only log when real providers are wired (suppress "mock" noise in tests).
    if llm_name != "mock" {
        eprintln!("LLM provider: {llm_name}");
    }
    if emb_name != "mock" {
        eprintln!("Embedding provider: {emb_name}");
    }

    CompositeExecutor::from_store(store)
        .with_llm(llm_provider, noether_engine::llm::LlmConfig::default())
        .with_embedding(emb_provider)
}
