//! Composite executor: routes synthesized stages to `NixExecutor` and
//! stdlib/inline stages to `InlineExecutor`.

use super::inline::InlineExecutor;
use super::nix::NixExecutor;
use super::{ExecutionError, StageExecutor};
use noether_core::stage::StageId;
use noether_store::StageStore;
use serde_json::Value;

/// Executor that combines `InlineExecutor` (stdlib) with an optional `NixExecutor`
/// (synthesized / user-authored stages).
///
/// Lookup order:
/// 1. If `NixExecutor` has a real implementation for the stage → use Nix.
/// 2. Otherwise fall through to `InlineExecutor` (stdlib + fallback outputs).
pub struct CompositeExecutor {
    inline: InlineExecutor,
    nix: Option<NixExecutor>,
}

impl CompositeExecutor {
    /// Build from a store.  `NixExecutor` is included only when `nix` is available.
    pub fn from_store(store: &dyn StageStore) -> Self {
        let inline = InlineExecutor::from_store(store);
        let nix = NixExecutor::from_store(store);

        if nix.is_some() {
            eprintln!("Nix executor: active (synthesized stages will run via nix)");
        }

        Self { inline, nix }
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
        if let Some(nix) = &self.nix {
            if nix.has_implementation(stage_id) {
                return nix.execute(stage_id, input);
            }
        }
        self.inline.execute(stage_id, input)
    }
}
