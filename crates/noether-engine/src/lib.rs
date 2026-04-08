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
pub mod planner;
pub mod trace;

pub use noether_core as core;
pub use noether_store as store;
