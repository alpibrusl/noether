use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "effect")]
pub enum Effect {
    Cost { cents: u64 },
    Fallible,
    Llm { model: String },
    Network,
    NonDeterministic,
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
    Llm,
    Network,
    NonDeterministic,
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
            Effect::Llm { .. } => EffectKind::Llm,
            Effect::Network => EffectKind::Network,
            Effect::NonDeterministic => EffectKind::NonDeterministic,
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
            EffectKind::Llm => "llm",
            EffectKind::Network => "network",
            EffectKind::NonDeterministic => "non-deterministic",
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
}
