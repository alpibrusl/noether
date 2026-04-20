use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "effect")]
pub enum Effect {
    Cost {
        cents: u64,
    },
    Fallible,
    /// Stage reads a specific host path. Use an absolute path; the
    /// sandbox binds it at the same location inside the sandbox (via
    /// a read-only bind mount). Multiple read paths are declared as
    /// separate `FsRead` entries — one per path.
    ///
    /// `from_effects` on the isolation policy turns each `FsRead(p)`
    /// into a `RoBind { host: p, sandbox: p }`.
    FsRead {
        path: PathBuf,
    },
    /// Stage writes to a specific host path. Use an absolute path;
    /// the sandbox binds it RW at the same location inside. This is
    /// a deliberate trust widening — the sandbox cannot validate
    /// whether binding (say) `/home/user` RW is sensible. Callers
    /// that need this are declaring the trust decision explicitly
    /// via this effect.
    ///
    /// `from_effects` on the isolation policy turns each `FsWrite(p)`
    /// into an `RwBind { host: p, sandbox: p }`.
    FsWrite {
        path: PathBuf,
    },
    Llm {
        model: String,
    },
    Network,
    NonDeterministic,
    /// Stage spawns, signals, or waits on OS-level processes.
    Process,
    Pure,
    Unknown,
}

/// The variant name of an [`Effect`], without associated data.
///
/// Used by [`EffectPolicy`] to allow/deny whole classes of effects regardless
/// of their parameters (e.g. deny all `Llm` calls irrespective of which model).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    Cost,
    Fallible,
    FsRead,
    FsWrite,
    Llm,
    Network,
    NonDeterministic,
    Process,
    Pure,
    Unknown,
}

impl Effect {
    /// Return the kind (variant discriminant) of this effect, dropping any
    /// associated data. Used for policy comparisons.
    pub fn kind(&self) -> EffectKind {
        match self {
            Effect::Cost { .. } => EffectKind::Cost,
            Effect::Fallible => EffectKind::Fallible,
            Effect::FsRead { .. } => EffectKind::FsRead,
            Effect::FsWrite { .. } => EffectKind::FsWrite,
            Effect::Llm { .. } => EffectKind::Llm,
            Effect::Network => EffectKind::Network,
            Effect::NonDeterministic => EffectKind::NonDeterministic,
            Effect::Process => EffectKind::Process,
            Effect::Pure => EffectKind::Pure,
            Effect::Unknown => EffectKind::Unknown,
        }
    }
}

impl std::fmt::Display for EffectKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            EffectKind::Cost => "cost",
            EffectKind::Fallible => "fallible",
            EffectKind::FsRead => "fs-read",
            EffectKind::FsWrite => "fs-write",
            EffectKind::Llm => "llm",
            EffectKind::Network => "network",
            EffectKind::NonDeterministic => "non-deterministic",
            EffectKind::Process => "process",
            EffectKind::Pure => "pure",
            EffectKind::Unknown => "unknown",
        };
        write!(f, "{s}")
    }
}

/// An ordered set of effects declared on a stage.
///
/// Uses `BTreeSet` for deterministic serialization order, which is
/// critical for canonical JSON hashing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectSet {
    effects: BTreeSet<Effect>,
}

impl EffectSet {
    pub fn unknown() -> Self {
        Self {
            effects: BTreeSet::from([Effect::Unknown]),
        }
    }

    pub fn pure() -> Self {
        Self {
            effects: BTreeSet::from([Effect::Pure]),
        }
    }

    pub fn new(effects: impl IntoIterator<Item = Effect>) -> Self {
        Self {
            effects: effects.into_iter().collect(),
        }
    }

    pub fn contains(&self, effect: &Effect) -> bool {
        self.effects.contains(effect)
    }

    pub fn is_unknown(&self) -> bool {
        self.effects.contains(&Effect::Unknown)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Effect> {
        self.effects.iter()
    }
}

impl Default for EffectSet {
    fn default() -> Self {
        Self::unknown()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_unknown() {
        let es = EffectSet::default();
        assert!(es.is_unknown());
        assert!(es.contains(&Effect::Unknown));
    }

    #[test]
    fn pure_does_not_contain_unknown() {
        let es = EffectSet::pure();
        assert!(!es.is_unknown());
        assert!(es.contains(&Effect::Pure));
    }

    #[test]
    fn serde_round_trip() {
        let es = EffectSet::new([
            Effect::Network,
            Effect::Fallible,
            Effect::Llm {
                model: "claude-sonnet-4".into(),
            },
        ]);
        let json = serde_json::to_string(&es).unwrap();
        let deserialized: EffectSet = serde_json::from_str(&json).unwrap();
        assert_eq!(es, deserialized);
    }

    #[test]
    fn fs_effects_round_trip_through_json() {
        // M3.x: path-bearing FsRead / FsWrite variants must round-trip
        // cleanly through the `#[serde(tag = "effect")]` shape, and
        // coexist with the existing effect variants (so a stage can
        // declare `{Pure, FsRead(/etc), FsWrite(/tmp/out)}` and hit
        // every branch of `from_effects`).
        let es = EffectSet::new([
            Effect::Pure,
            Effect::FsRead {
                path: PathBuf::from("/etc/ssl/certs"),
            },
            Effect::FsWrite {
                path: PathBuf::from("/tmp/agent-output"),
            },
        ]);
        let json = serde_json::to_string(&es).unwrap();
        // Contract: the wire shape uses the same tag key as every
        // other Effect variant. Downstream deserialisers (e.g. the
        // Python bindings the agentspec PR will grow) see a uniform
        // `{"effect": "FsRead", "path": "..."}` shape.
        assert!(
            json.contains(r#""effect":"FsRead""#),
            "expected FsRead tag in wire: {json}"
        );
        assert!(
            json.contains(r#""effect":"FsWrite""#),
            "expected FsWrite tag in wire: {json}"
        );
        let deserialized: EffectSet = serde_json::from_str(&json).unwrap();
        assert_eq!(es, deserialized);
    }

    #[test]
    fn fs_effect_kinds_map_one_to_one() {
        let read = Effect::FsRead {
            path: PathBuf::from("/a"),
        };
        let write = Effect::FsWrite {
            path: PathBuf::from("/b"),
        };
        assert_eq!(read.kind(), EffectKind::FsRead);
        assert_eq!(write.kind(), EffectKind::FsWrite);
        // Display for CLI surface (`--allow-effects fs-read,fs-write`).
        assert_eq!(EffectKind::FsRead.to_string(), "fs-read");
        assert_eq!(EffectKind::FsWrite.to_string(), "fs-write");
    }

    #[test]
    fn distinct_fs_read_paths_are_distinct_elements() {
        // EffectSet is a BTreeSet. Two FsRead effects with different
        // paths must be stored as two elements — otherwise declaring
        // "read /etc AND read /home" would collapse to one.
        let es = EffectSet::new([
            Effect::FsRead {
                path: PathBuf::from("/etc"),
            },
            Effect::FsRead {
                path: PathBuf::from("/home"),
            },
        ]);
        assert_eq!(es.iter().count(), 2);
    }
}
