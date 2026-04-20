// Native-only modules: require OS, network, or LLM provider.
#[cfg(feature = "native")]
pub mod agent;
#[cfg(feature = "native")]
pub mod composition_cache;
#[cfg(feature = "native")]
pub mod index;
#[cfg(feature = "native")]
pub mod llm;
#[cfg(feature = "native")]
pub mod providers;
#[cfg(feature = "native")]
pub mod registry_client;

// Always-available modules: compile for wasm32 and native.
pub mod checker;
pub mod error;
pub mod executor;
pub mod lagrange;
pub mod optimizer;
pub mod planner;
pub mod stage_test;
pub mod trace;

pub use noether_core as core;
pub use noether_store as store;

// Convenience re-export so downstream crates don't need to reach into executor submodules.
pub use executor::InlineRegistry;
