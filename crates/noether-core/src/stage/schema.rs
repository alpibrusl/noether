use crate::capability::Capability;
use crate::effects::EffectSet;
use crate::types::NType;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Content-addressed stage identity: hex-encoded SHA-256.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StageId(pub String);

/// Canonical identity: hex-encoded SHA-256 of (name + input + output + effects).
///
/// Two stages with the same canonical hash represent the same *concept* —
/// they have the same name, types, and effects, but may differ in implementation.
/// Only one active version per canonical hash should exist in the store.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CanonicalId(pub String);

/// The identity-determining fields of a stage.
///
/// Only these fields are included in the content hash that produces
/// the `StageId`. Two stages with identical signatures and implementations
/// are the same stage, regardless of metadata differences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageSignature {
    pub input: NType,
    pub output: NType,
    pub effects: EffectSet,
    pub implementation_hash: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CostEstimate {
    pub time_ms_p50: Option<u64>,
    pub tokens_est: Option<u64>,
    pub memory_mb: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Example {
    pub input: serde_json::Value,
    pub output: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum StageLifecycle {
    Draft,
    Active,
    Deprecated { successor_id: StageId },
    Tombstone,
}

/// The complete stage with all metadata.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stage {
    pub id: StageId,
    /// Canonical identity — same concept (name + types + effects), regardless of
    /// implementation. Used to detect re-registrations and auto-deprecate old versions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_id: Option<CanonicalId>,
    pub signature: StageSignature,
    pub capabilities: BTreeSet<Capability>,
    pub cost: CostEstimate,
    pub description: String,
    pub examples: Vec<Example>,
    pub lifecycle: StageLifecycle,
    pub ed25519_signature: Option<String>,
    pub signer_public_key: Option<String>,
    /// Source code of the implementation, if this is a synthesized or user-authored stage.
    /// Stdlib stages leave this None (their implementation is compiled into the binary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implementation_code: Option<String>,
    /// Language of the implementation: "python", "javascript", "bash", etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub implementation_language: Option<String>,
    /// Optional CSS scoped to this stage's UI component.
    /// The browser build automatically prefixes every selector with `.nr-<id8>`
    /// to avoid collisions with other stages' styles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_style: Option<String>,
    /// Browsable category labels (e.g. `["text", "pure", "string"]`).
    /// Not part of the content hash — changing tags never changes the StageId.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Alternative names / vocabulary for this stage that improve search recall
    /// (e.g. `["strlen", "count_chars"]` for `text_length`).
    /// Not part of the content hash.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// Human-authored name from the original stage spec (e.g. `volvo_map`).
    /// Used for name-based lookup in graph references — a composition can
    /// say `{"op": "Stage", "id": "volvo_map"}` and the loader resolves it
    /// to the latest Active stage with this name. Not part of the content
    /// hash (two stages with the same name but different types are distinct
    /// identities).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_signature() -> StageSignature {
        StageSignature {
            input: NType::Text,
            output: NType::Number,
            effects: EffectSet::pure(),
            implementation_hash: "abc123".into(),
        }
    }

    #[test]
    fn stage_serde_round_trip() {
        let stage = Stage {
            id: StageId("deadbeef".into()),
            canonical_id: Some(CanonicalId("canonical123".into())),
            signature: sample_signature(),
            capabilities: BTreeSet::from([Capability::Network]),
            cost: CostEstimate {
                time_ms_p50: Some(10),
                tokens_est: None,
                memory_mb: Some(1),
            },
            description: "converts text to number".into(),
            examples: vec![Example {
                input: serde_json::json!("42"),
                output: serde_json::json!(42),
            }],
            lifecycle: StageLifecycle::Active,
            ed25519_signature: None,
            signer_public_key: None,
            implementation_code: None,
            implementation_language: None,
            ui_style: None,
            tags: vec![],
            aliases: vec![],
            name: Some("text_to_number".into()),
        };
        let json = serde_json::to_string_pretty(&stage).unwrap();
        let deserialized: Stage = serde_json::from_str(&json).unwrap();
        assert_eq!(stage, deserialized);
    }

    #[test]
    fn lifecycle_deprecated_has_successor() {
        let lc = StageLifecycle::Deprecated {
            successor_id: StageId("newstage".into()),
        };
        let json = serde_json::to_string(&lc).unwrap();
        assert!(json.contains("newstage"));
    }
}
