use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Capability {
    Network,
    FsRead,
    FsWrite,
    Gpu,
    Llm,
    /// Spawn, signal, or wait on OS-level processes.
    Process,
}
