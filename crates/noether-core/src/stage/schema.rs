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

    /// Declarative properties the stage claims to satisfy for every
    /// `(input, output)` pair. Checked against examples at registration
    /// time (`noether stage verify --with-properties`) and optionally
    /// at runtime. Types say *what shape* the output has; properties
    /// say *which values* it may actually take. See
    /// [`crate::stage::property`] for the DSL.
    ///
    /// Not part of the content hash — a stage's properties can be
    /// strengthened in a follow-up release without forcing a new
    /// `StageId`. They **can** only grow within 1.x per
    /// `STABILITY.md`: existing entries cannot be removed or
    /// weakened.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<crate::stage::property::Property>,
}

/// Failure mode for [`Stage::check_properties`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CheckPropertiesError {
    /// The stage declares properties but has no examples to check
    /// them against. Previously this returned Ok vacuously — now it's
    /// an error so bogus properties can't hide behind a missing
    /// example set.
    #[error(
        "stage declares {count} properties but has no examples to \
         verify them against — add at least one example"
    )]
    NoExamples { count: usize },

    /// One or more (example, property) pairs failed. Each entry is
    /// `(example_index, violation)`.
    #[error("{} property violation(s) across examples", .0.len())]
    Violations(Vec<(usize, crate::stage::property::PropertyViolation)>),
}

impl Stage {
    /// Check every declared property against every declared example.
    /// Returns `Ok(())` if all `(example, property)` pairs pass, or a
    /// [`CheckPropertiesError`] describing the failure.
    ///
    /// A stage with no properties vacuously passes. A stage with
    /// properties but no examples is an error ([`CheckPropertiesError::NoExamples`]) —
    /// previously a vacuous pass, which let property bugs slip
    /// through on under-specified stages.
    ///
    /// This is the CI-time check the M2 roadmap promises. `noether
    /// stage verify --with-properties` wraps this with CLI reporting.
    pub fn check_properties(&self) -> Result<(), CheckPropertiesError> {
        if self.properties.is_empty() {
            return Ok(());
        }
        if self.examples.is_empty() {
            return Err(CheckPropertiesError::NoExamples {
                count: self.properties.len(),
            });
        }
        let mut violations = Vec::new();
        for (example_idx, example) in self.examples.iter().enumerate() {
            for property in &self.properties {
                if let Err(violation) = property.check(&example.input, &example.output) {
                    violations.push((example_idx, violation));
                }
            }
        }
        if violations.is_empty() {
            Ok(())
        } else {
            Err(CheckPropertiesError::Violations(violations))
        }
    }
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
            properties: vec![],
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
    fn check_properties_empty_passes_vacuously() {
        let mut stage = sample_stage();
        stage.properties = vec![];
        assert!(stage.check_properties().is_ok());
    }

    #[test]
    fn check_properties_with_no_examples_errors() {
        // The fix for the "vacuous pass" review finding: a stage that
        // declares properties but has no examples MUST error, not pass.
        // Otherwise under-specified stages would hide property bugs.
        use crate::stage::property::Property;
        let mut stage = sample_stage();
        stage.examples = vec![]; // deliberately empty
        stage.properties = vec![Property::Range {
            field: "output.x".into(),
            min: Some(0.0),
            max: None,
        }];
        match stage.check_properties().unwrap_err() {
            CheckPropertiesError::NoExamples { count } => assert_eq!(count, 1),
            other => panic!("expected NoExamples, got {other:?}"),
        }
    }

    /// Properties are documented as "not part of the content hash".
    /// This test pins that contract: two stages identical except for
    /// `properties` produce identical `id` and `signature_id`, and
    /// the hashes computed from their signatures match.
    #[test]
    fn properties_do_not_affect_stage_identity() {
        use crate::stage::property::Property;
        use crate::stage::{compute_signature_id, compute_stage_id};

        let name = "sample";
        let sig = sample_signature();

        // Compute both IDs from the signature directly — this is the
        // invariant we actually want to pin.
        let expected_stage_id = compute_stage_id(name, &sig).unwrap();
        let expected_sig_id =
            compute_signature_id(name, &sig.input, &sig.output, &sig.effects).unwrap();

        // Build two Stage values, one with properties and one without,
        // that are otherwise identical.
        let base = Stage {
            id: expected_stage_id.clone(),
            signature_id: Some(expected_sig_id.clone()),
            signature: sig,
            capabilities: BTreeSet::new(),
            cost: CostEstimate {
                time_ms_p50: None,
                tokens_est: None,
                memory_mb: None,
            },
            description: "sample".into(),
            examples: vec![],
            lifecycle: StageLifecycle::Active,
            ed25519_signature: None,
            signer_public_key: None,
            implementation_code: None,
            implementation_language: None,
            ui_style: None,
            tags: vec![],
            aliases: vec![],
            name: Some(name.into()),
            properties: vec![],
        };
        let mut with_properties = base.clone();
        with_properties.properties = vec![Property::Range {
            field: "output.x".into(),
            min: Some(0.0),
            max: Some(100.0),
        }];

        // StageId is unaffected by adding properties.
        assert_eq!(base.id, with_properties.id);
        // SignatureId is unaffected.
        assert_eq!(base.signature_id, with_properties.signature_id);

        // Both hashes computed from signature alone match the stored
        // values (the "not part of the hash" guarantee).
        assert_eq!(base.id, expected_stage_id);
        assert_eq!(base.signature_id.as_ref().unwrap(), &expected_sig_id);
    }

    #[test]
    fn check_properties_flags_all_violations_across_examples() {
        use crate::stage::property::{Property, PropertyViolation};
        let mut stage = sample_stage();
        // Two examples: one passes, one fails the range property.
        stage.examples = vec![
            Example {
                input: serde_json::json!(null),
                output: serde_json::json!({"soc": 50.0}),
            },
            Example {
                input: serde_json::json!(null),
                output: serde_json::json!({"soc": 150.0}),
            },
        ];
        stage.properties = vec![Property::Range {
            field: "output.soc".into(),
            min: Some(0.0),
            max: Some(100.0),
        }];

        let result = stage.check_properties();
        match result.unwrap_err() {
            CheckPropertiesError::Violations(violations) => {
                assert_eq!(violations.len(), 1);
                assert_eq!(violations[0].0, 1);
                assert!(matches!(
                    violations[0].1,
                    PropertyViolation::AboveMax { .. }
                ));
            }
            other => panic!("expected Violations, got {other:?}"),
        }
    }

    // Helper used only by the check_properties tests.
    fn sample_stage() -> Stage {
        Stage {
            id: StageId("deadbeef".into()),
            signature_id: Some(SignatureId("sig".into())),
            signature: sample_signature(),
            capabilities: BTreeSet::new(),
            cost: CostEstimate {
                time_ms_p50: None,
                tokens_est: None,
                memory_mb: None,
            },
            description: "sample".into(),
            examples: vec![],
            lifecycle: StageLifecycle::Active,
            ed25519_signature: None,
            signer_public_key: None,
            implementation_code: None,
            implementation_language: None,
            ui_style: None,
            tags: vec![],
            aliases: vec![],
            name: Some("sample".into()),
            properties: vec![],
        }
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
