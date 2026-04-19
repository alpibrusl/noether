//! Re-export of the [`noether_isolation`] crate.
//!
//! The isolation primitives used to live here. They were extracted
//! into their own crate in v0.7.1 so downstream consumers that want
//! the sandbox primitive without pulling in all of `noether-engine`
//! (agentspec's trust-enforcement path, the standalone
//! `noether-sandbox` binary, future language bindings) can depend on
//! `noether-isolation` directly.
//!
//! Existing callers that reach through `noether_engine::executor::isolation`
//! keep working via this re-export — nothing here is new API, only a
//! new location. See the [`noether_isolation`] crate docs for the
//! authoritative source.

pub use noether_isolation::{
    build_bwrap_command, find_bwrap, IsolationBackend, IsolationError, IsolationPolicy, NOBODY_GID,
    NOBODY_UID, TRUSTED_BWRAP_PATHS,
};
