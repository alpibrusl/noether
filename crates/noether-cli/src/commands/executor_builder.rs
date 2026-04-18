//! Builds a `CompositeExecutor` with LLM and embedding providers wired from
//! environment variables. Shared between `run.rs` and `compose.rs`.
//!
//! Uses `noether_engine::providers::build_llm_provider()` and
//! `build_embedding_provider()` for provider selection logic.

use noether_engine::executor::composite::CompositeExecutor;
use noether_engine::executor::isolation::IsolationBackend;
use noether_engine::providers;
use noether_store::StageStore;

/// Build a `CompositeExecutor` for `store`, injecting LLM provider only.
///
/// Does NOT attach the embedding provider — that pre-computes embeddings for
/// all stages, which is slow and unnecessary for `noether run`. Use
/// `build_executor_with_embeddings` for commands that need semantic search
/// (e.g., `noether compose`).
pub fn build_executor(store: &dyn StageStore) -> CompositeExecutor {
    build_executor_with_isolation(store, IsolationBackend::None)
}

/// Build a `CompositeExecutor` with a specific isolation backend for
/// the embedded NixExecutor. `noether run` uses this to respect the
/// `--isolate` flag; other entry points that don't accept the flag
/// call [`build_executor`] which is equivalent to
/// `build_executor_with_isolation(store, IsolationBackend::None)`.
pub fn build_executor_with_isolation(
    store: &dyn StageStore,
    isolation: IsolationBackend,
) -> CompositeExecutor {
    let (llm_provider, llm_name) = providers::build_llm_provider();

    if llm_name != "mock" {
        eprintln!("LLM provider: {llm_name}");
    }

    CompositeExecutor::from_store(store)
        .with_llm(llm_provider, noether_engine::llm::LlmConfig::default())
        .with_isolation(isolation)
}

/// Build a `CompositeExecutor` with LLM + embedding providers.
///
/// The embedding provider pre-computes embeddings for all stages in the store,
/// enabling semantic search in `store_search` and `llm_embed` stages.
/// This is needed for `noether compose` but NOT for `noether run`.
pub fn build_executor_with_embeddings(store: &dyn StageStore) -> CompositeExecutor {
    let (llm_provider, llm_name) = providers::build_llm_provider();
    let (emb_provider, emb_name) = providers::build_embedding_provider();

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
