//! Composite executor: routes stages to the right executor by capability.
//!
//! Lookup order:
//! 1. `NixExecutor`    — synthesized stages with `implementation_code`
//! 2. `RuntimeExecutor`— LLM + store-aware stdlib stages
//! 3. `InlineExecutor` — pure stdlib stages (function pointers)

use super::inline::{InlineExecutor, InlineRegistry};
use super::nix::NixExecutor;
use super::runtime::RuntimeExecutor;
use super::{ExecutionError, StageExecutor};
use noether_core::stage::StageId;
use noether_store::StageStore;
use serde_json::Value;

/// Executor that combines all three executor layers.
pub struct CompositeExecutor {
    inline: InlineExecutor,
    nix: Option<NixExecutor>,
    runtime: RuntimeExecutor,
}

impl CompositeExecutor {
    /// Build from a store using only the built-in stdlib implementations.
    /// `NixExecutor` is included only when `nix` is available in `PATH`.
    pub fn from_store(store: &dyn StageStore) -> Self {
        Self::from_store_with_registry(store, InlineRegistry::new())
    }

    /// Build from a store, augmenting the stdlib with additional inline
    /// stage implementations from `registry`.
    ///
    /// Use this when your project needs Pure Rust stage implementations
    /// without modifying `noether-core`.  See [`InlineRegistry`] for usage.
    pub fn from_store_with_registry(store: &dyn StageStore, registry: InlineRegistry) -> Self {
        let inline = InlineExecutor::from_store_with_registry(store, registry);
        let nix = NixExecutor::from_store(store);
        let runtime = RuntimeExecutor::from_store(store);

        if nix.is_some() {
            eprintln!("Nix executor: active (synthesized stages will run via nix)");
        }

        Self {
            inline,
            nix,
            runtime,
        }
    }

    /// Replace the isolation backend on the embedded NixExecutor.
    /// No-op when Nix isn't installed (synthesized stages can't run
    /// anyway, so the sandbox question doesn't arise).
    pub fn with_isolation(mut self, backend: super::isolation::IsolationBackend) -> Self {
        if let Some(nix) = self.nix.take() {
            use super::nix::{NixConfig, NixExecutor};
            // Rebuild NixExecutor with the new isolation setting.
            // NixExecutor doesn't expose a public with_isolation setter
            // today because the config field was just added; use the
            // builder pattern via `NixConfig::with_isolation` at
            // construction time. Reconstruct by re-reading the existing
            // config and swapping the backend.
            let old_config = nix.config_snapshot();
            let new_config = NixConfig {
                isolation: backend,
                ..old_config
            };
            self.nix = NixExecutor::rebuild_with_config(nix, new_config);
        }
        self
    }

    /// Attach an LLM provider so `llm_complete` / `llm_classify` / `llm_extract`
    /// stages are actually executed instead of returning a config error.
    pub fn with_llm(
        mut self,
        llm: Box<dyn crate::llm::LlmProvider>,
        config: crate::llm::LlmConfig,
    ) -> Self {
        self.runtime.set_llm(llm, config);
        self
    }

    /// Attach an embedding provider so `llm_embed` uses real embeddings and
    /// `store_search` uses cosine similarity instead of substring matching.
    pub fn with_embedding(
        mut self,
        provider: Box<dyn crate::index::embedding::EmbeddingProvider>,
    ) -> Self {
        self.runtime = self.runtime.with_embedding(provider);
        self
    }

    /// Register a freshly synthesized stage so it can be executed
    /// immediately without reloading the store. The caller **must**
    /// supply the stage's declared effects — the isolation policy is
    /// derived from them, and silently defaulting to
    /// [`EffectSet::pure`](noether_core::effects::EffectSet::pure)
    /// would put a Network-effect stage into a no-network sandbox and
    /// surface as an opaque DNS failure at runtime.
    pub fn register_synthesized(
        &mut self,
        stage_id: &StageId,
        code: &str,
        language: &str,
        effects: noether_core::effects::EffectSet,
    ) {
        if let Some(nix) = &mut self.nix {
            nix.register_with_effects(stage_id, code, language, effects);
        }
    }

    /// True when `nix` is available and will handle synthesized stages.
    pub fn nix_available(&self) -> bool {
        self.nix.is_some()
    }
}

impl StageExecutor for CompositeExecutor {
    fn execute(&self, stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        // 1. Synthesized stages (have implementation_code stored) → Nix
        if let Some(nix) = &self.nix {
            if nix.has_implementation(stage_id) {
                return nix.execute(stage_id, input);
            }
        }
        // 2. LLM + store-aware stages → RuntimeExecutor
        if self.runtime.has_implementation(stage_id) {
            return self.runtime.execute(stage_id, input);
        }
        // 3. Pure stdlib + registered extra stages → InlineExecutor
        self.inline.execute(stage_id, input)
    }
}
