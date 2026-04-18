use crate::capability::Capability;
use crate::effects::EffectSet;
use crate::types::NType;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Implementation-level stage identity: hex-encoded SHA-256 of the
/// [`StageSignature`] (input, output, effects, and `implementation_hash`).
///
/// Two stages with the same `StageId` have the same *implementation* —
/// bit-exact if you pin to this ID. This is the store's primary key.
///
/// From M2 (v0.6.0) onwards this field is also exposed as
/// [`ImplementationId`] to make the role explicit at call sites. The
/// type alias is preserved so existing code keeps compiling.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct StageId(pub String);

/// Alias for [`StageId`]. New code should prefer this name — it makes
/// the intent explicit at call sites and distinguishes it from
/// [`SignatureId`], which is stable across implementation bugfixes.
pub type ImplementationId = StageId;

/// Signature-level stage identity: hex-encoded SHA-256 of
/// (name + input + output + effects). Excludes `implementation_hash`.
///
/// Two stages with the same `SignatureId` represent the same *concept* —
/// same name, types, and effects, but possibly different implementations.
/// This is the identity that is **stable across 1.x** per `STABILITY.md`:
/// a bugfix that changes `implementation_hash` changes the `StageId` but
/// not the `SignatureId`, so graphs pinned by signature keep working.
///
/// Only one active stage per `SignatureId` should exist in the store.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SignatureId(pub String);

/// Deprecated alias for [`SignatureId`]. Kept for back-compat with code
/// written against v0.4.x and v0.5.x. Callers should migrate to
/// [`SignatureId`] — this alias will be removed in v0.7.0.
#[deprecated(since = "0.6.0", note = "renamed to SignatureId")]
pub type CanonicalId = SignatureId;

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
    /// Signature identity — same concept (name + types + effects),
    /// regardless of implementation. Per M2 this is required, but the
    /// deserialiser accepts both the new `signature_id` field and the
    /// legacy `canonical_id` field so v0.5.x stage JSONs keep loading.
    ///
    /// Stages loaded from storage where neither field is present will
    /// have `signature_id == None`; such stages fail `stage verify`.
    /// Builders always populate this — only hand-crafted JSONs from
    /// before M2 can produce `None` here.
    #[serde(
        default,
        alias = "canonical_id",
        skip_serializing_if = "Option::is_none"
    )]
    pub signature_id: Option<SignatureId>,
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
            signature_id: Some(SignatureId("canonical123".into())),
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
    fn legacy_canonical_id_field_deserialises_into_signature_id() {
        // v0.5.x stage JSONs used `"canonical_id"`. After the M2 rename
        // the field is `"signature_id"`, but the deserialiser accepts
        // the old name via serde alias.
        let legacy_json = serde_json::json!({
            "id": "deadbeef",
            "canonical_id": "legacy_sig_hash",
            "signature": {
                "input": {"kind": "Text"},
                "output": {"kind": "Number"},
                "effects": {"effects": []},
                "implementation_hash": "abc123",
            },
            "capabilities": [],
            "cost": {},
            "description": "legacy",
            "examples": [],
            "lifecycle": "Active",
            "name": "legacy_stage",
        });
        let stage: Stage = serde_json::from_value(legacy_json).unwrap();
        assert_eq!(
            stage.signature_id,
            Some(SignatureId("legacy_sig_hash".into()))
        );
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
