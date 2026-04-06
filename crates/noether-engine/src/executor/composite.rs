//! Composite executor: routes stages to the right executor by capability.
//!
//! Lookup order:
//! 1. `NixExecutor`    — synthesized stages with `implementation_code`
//! 2. `RuntimeExecutor`— LLM + store-aware stdlib stages
//! 3. `InlineExecutor` — pure stdlib stages (function pointers)

use super::inline::InlineExecutor;
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
    /// Build from a store. `NixExecutor` is included only when `nix` is available.
    pub fn from_store(store: &dyn StageStore) -> Self {
        let inline = InlineExecutor::from_store(store);
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

    /// Register a freshly synthesized stage so it can be executed immediately
    /// without reloading the store.
    pub fn register_synthesized(&mut self, stage_id: &StageId, code: &str, language: &str) {
        if let Some(nix) = &mut self.nix {
            nix.register(stage_id, code, language);
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
        // 3. Pure stdlib stages → InlineExecutor
        self.inline.execute(stage_id, input)
    }
}
