//! Declarative properties that a stage claims to satisfy for every
//! `(input, output)` pair it accepts. Properties are the M2 answer to
//! the question *"what does this stage actually guarantee, beyond its
//! type signature?"*. Types say `output: Number`; properties say
//! `output >= 0 and output <= 100`.
//!
//! ## Scope
//!
//! Per the M2 roadmap, the DSL is deliberately tiny:
//!
//! - [`Property::SetMember`] — a named field is one of a fixed set of
//!   JSON values.
//! - [`Property::Range`] — a named numeric field is within `[min, max]`
//!   (either bound optional).
//!
//! Higher-order predicates (implications over examples, quantifiers,
//! type-class predicates) are explicit non-goals for 1.0.
//!
//! ## Wire format
//!
//! Properties live on the Stage spec as a JSON array:
//!
//! ```json
//! "properties": [
//!   { "kind": "set_member",
//!     "field": "output.severity",
//!     "set": ["CRITICAL", "HIGH", "WARNING"] },
//!   { "kind": "range",
//!     "field": "output.soc_percent",
//!     "min": 0.0, "max": 100.0 }
//! ]
//! ```
//!
//! The field path is dot-separated. The first segment must be either
//! `input` or `output`; the rest navigate into whichever side's JSON.
//! A path that doesn't resolve produces a [`PropertyViolation::FieldMissing`].

use serde::{Deserialize, Serialize};

/// A declarative property claim on a stage.
///
/// The wire format is a tagged union on the `"kind"` field. Unknown
/// kinds deserialise into [`Property::Unknown`] so a 1.0 reader can
/// still load a graph written against 1.1 (forward compatibility).
/// Readers that can't evaluate an unknown property should skip it
/// with a warning; they must not treat "couldn't parse" as "property
/// holds".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Property {
    /// The value at `field` must equal one of the entries in `set`.
    /// Comparison is JSON-value equality (strings compare as strings,
    /// numbers as numbers, etc.).
    SetMember {
        /// Dot-separated path, e.g. `"output.severity"` or
        /// `"input.battery.soc"`.
        field: String,
        /// Allowed JSON values. Order does not matter.
        set: Vec<serde_json::Value>,
    },
    /// The numeric value at `field` must lie within `[min, max]`. Either
    /// bound may be omitted; an omitted bound means "unbounded on that
    /// side". A non-numeric value at the field path fails with
    /// [`PropertyViolation::NotNumber`].
    Range {
        /// Dot-separated path to a numeric field.
        field: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
    },
    /// A property kind this reader doesn't know how to evaluate.
    /// Produced by the deserialiser for forward compatibility when a
    /// future minor release adds a new `kind` variant; the original
    /// `kind` string is preserved so callers can report which kind
    /// was skipped.
    #[serde(untagged)]
    Unknown {
        /// The raw JSON object for the unknown property.
        #[serde(flatten)]
        raw: serde_json::Value,
    },
}

/// A specific way a property failed to hold. Each variant carries
/// enough context to make the failure actionable without a stack trace.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PropertyViolation {
    #[error(
        "field path `{path}` is malformed: must start with `input.` or `output.` (got: `{reason}`)"
    )]
    BadPath { path: String, reason: String },

    #[error("field `{path}` is missing or unreachable in the {side} JSON")]
    FieldMissing { path: String, side: &'static str },

    #[error("field `{path}` is `{actual}`; expected one of: {expected:?}")]
    NotInSet {
        path: String,
        actual: serde_json::Value,
        expected: Vec<serde_json::Value>,
    },

    #[error("field `{path}` is {actual}; expected a number for range check")]
    NotNumber {
        path: String,
        actual: serde_json::Value,
    },

    #[error("field `{path}` is {actual}; expected >= {min}")]
    BelowMin { path: String, actual: f64, min: f64 },

    #[error("field `{path}` is {actual}; expected <= {max}")]
    AboveMax { path: String, actual: f64, max: f64 },
}

/// Why a property failed static validation against a stage's declared
/// input/output types.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PropertyTypeError {
    #[error(
        "property field `{path}` is malformed: first segment must be \
         `input` or `output` (got: `{reason}`)"
    )]
    BadPath { path: String, reason: String },

    #[error(
        "property field `{path}` is not reachable in the stage's declared \
         {side} type `{declared_type}`"
    )]
    FieldNotInType {
        path: String,
        side: &'static str,
        declared_type: String,
    },

    #[error(
        "property `{path}` requires a numeric field but the declared \
         type at that path is `{declared_type}`"
    )]
    RangeOnNonNumber { path: String, declared_type: String },
}

impl Property {
    /// The field path this property constrains. Used by callers that
    /// want to group properties by target field for reporting.
    /// Returns an empty string for [`Property::Unknown`] since the
    /// reader doesn't know how to interpret it.
    pub fn field(&self) -> &str {
        match self {
            Property::SetMember { field, .. } | Property::Range { field, .. } => field,
            Property::Unknown { .. } => "",
        }
    }

    /// True if this is an `Unknown` variant — the reader doesn't know
    /// how to evaluate the property. Callers that aggregate results
    /// should treat these as skips, not passes or failures.
    pub fn is_unknown(&self) -> bool {
        matches!(self, Property::Unknown { .. })
    }

    /// Validate that this property's field path is reachable in the
    /// declared `input`/`output` types, and (for `Range`) that the
    /// target field is numeric.
    ///
    /// Called at stage-registration time so bogus property declarations
    /// (e.g. `Range { field: "output.color" }` on a Text-typed stage)
    /// fail early, not only on first violating example.
    ///
    /// [`Property::Unknown`] short-circuits to `Ok(())` — the reader
    /// can't validate a kind it doesn't know.
    pub fn validate_against_types(
        &self,
        input_type: &crate::types::NType,
        output_type: &crate::types::NType,
    ) -> Result<(), PropertyTypeError> {
        if self.is_unknown() {
            return Ok(());
        }
        use crate::types::NType;
        let path = self.field();
        let mut parts = path.split('.');
        let (root, side_label) = match parts.next() {
            Some("input") => (input_type, "input"),
            Some("output") => (output_type, "output"),
            Some(other) => {
                return Err(PropertyTypeError::BadPath {
                    path: path.to_string(),
                    reason: format!("first segment must be `input` or `output`, got `{other}`"),
                });
            }
            None => {
                return Err(PropertyTypeError::BadPath {
                    path: path.to_string(),
                    reason: "empty path".into(),
                });
            }
        };

        // Walk into the type. Any field that descends through `Any`
        // or a Union keeps the claim alive (we can't prove absence),
        // so we short-circuit. For Record / Map / List, descend; for
        // primitives, the remaining path must be empty.
        let mut cursor: &NType = root;
        for segment in parts {
            cursor = match cursor {
                NType::Any => {
                    // Can't prove the field is absent under Any; accept.
                    return self.validate_terminal(path, &NType::Any);
                }
                NType::Record(fields) => match fields.get(segment) {
                    Some(t) => t,
                    None => {
                        return Err(PropertyTypeError::FieldNotInType {
                            path: path.to_string(),
                            side: side_label,
                            declared_type: format!("{root:?}"),
                        });
                    }
                },
                _ => {
                    // Can't descend into a non-record type.
                    return Err(PropertyTypeError::FieldNotInType {
                        path: path.to_string(),
                        side: side_label,
                        declared_type: format!("{root:?}"),
                    });
                }
            };
        }

        self.validate_terminal(path, cursor)
    }

    fn validate_terminal(
        &self,
        path: &str,
        terminal: &crate::types::NType,
    ) -> Result<(), PropertyTypeError> {
        use crate::types::NType;
        match self {
            // Unknown: the reader can't validate a property kind it
            // doesn't recognise. Skip rather than error — the property
            // may be fine under a future reader.
            Property::Unknown { .. } => Ok(()),
            // SetMember accepts anything — JSON-value equality is type-blind.
            Property::SetMember { .. } => Ok(()),
            // Range needs a Number (or Any / Union containing Number —
            // we're permissive here).
            Property::Range { .. } => match terminal {
                NType::Number | NType::Any => Ok(()),
                NType::Union(variants) => {
                    if variants
                        .iter()
                        .any(|v| matches!(v, NType::Number | NType::Any))
                    {
                        Ok(())
                    } else {
                        Err(PropertyTypeError::RangeOnNonNumber {
                            path: path.to_string(),
                            declared_type: format!("{terminal:?}"),
                        })
                    }
                }
                other => Err(PropertyTypeError::RangeOnNonNumber {
                    path: path.to_string(),
                    declared_type: format!("{other:?}"),
                }),
            },
        }
    }

    /// Check whether the property holds for the given `input` /
    /// `output` pair. Returns `Ok(())` on success, a
    /// [`PropertyViolation`] describing exactly what broke on failure.
    ///
    /// [`Property::Unknown`] variants return `Ok(())` — a reader that
    /// doesn't know how to evaluate a property must not treat that
    /// as a failure. Callers that want to surface "X skipped because
    /// unknown kind" should check [`Property::is_unknown`] separately.
    pub fn check(
        &self,
        input: &serde_json::Value,
        output: &serde_json::Value,
    ) -> Result<(), PropertyViolation> {
        if let Property::Unknown { .. } = self {
            return Ok(());
        }
        let path = self.field();
        let value = resolve_path(path, input, output)?;
        match self {
            Property::SetMember { set, .. } => {
                if set.iter().any(|allowed| allowed == value) {
                    Ok(())
                } else {
                    Err(PropertyViolation::NotInSet {
                        path: path.to_string(),
                        actual: value.clone(),
                        expected: set.clone(),
                    })
                }
            }
            Property::Unknown { .. } => unreachable!("handled above"),
            Property::Range { min, max, .. } => {
                let n = value
                    .as_f64()
                    .or_else(|| value.as_i64().map(|i| i as f64))
                    .or_else(|| value.as_u64().map(|u| u as f64))
                    .ok_or_else(|| PropertyViolation::NotNumber {
                        path: path.to_string(),
                        actual: value.clone(),
                    })?;
                if let Some(lo) = min {
                    if n < *lo {
                        return Err(PropertyViolation::BelowMin {
                            path: path.to_string(),
                            actual: n,
                            min: *lo,
                        });
                    }
                }
                if let Some(hi) = max {
                    if n > *hi {
                        return Err(PropertyViolation::AboveMax {
                            path: path.to_string(),
                            actual: n,
                            max: *hi,
                        });
                    }
                }
                Ok(())
            }
        }
    }
}

/// Navigate a dot-separated path into either the input or the output
/// JSON value. Returns a reference into the chosen side, or a
/// [`PropertyViolation`] describing why the path didn't resolve.
fn resolve_path<'a>(
    path: &str,
    input: &'a serde_json::Value,
    output: &'a serde_json::Value,
) -> Result<&'a serde_json::Value, PropertyViolation> {
    let mut parts = path.split('.');
    let side = parts.next().ok_or_else(|| PropertyViolation::BadPath {
        path: path.to_string(),
        reason: "empty path".into(),
    })?;
    let (root, side_label): (&serde_json::Value, &'static str) = match side {
        "input" => (input, "input"),
        "output" => (output, "output"),
        other => {
            return Err(PropertyViolation::BadPath {
                path: path.to_string(),
                reason: format!("first segment must be `input` or `output`, got `{other}`"),
            });
        }
    };
    let mut cursor = root;
    for segment in parts {
        cursor = match cursor {
            serde_json::Value::Object(map) => {
                map.get(segment)
                    .ok_or_else(|| PropertyViolation::FieldMissing {
                        path: path.to_string(),
                        side: side_label,
                    })?
            }
            // Array indexing is deliberately out of scope for M2.
            _ => {
                return Err(PropertyViolation::FieldMissing {
                    path: path.to_string(),
                    side: side_label,
                });
            }
        };
    }
    Ok(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn severity_prop() -> Property {
        Property::SetMember {
            field: "output.severity".into(),
            set: vec![json!("CRITICAL"), json!("HIGH"), json!("WARNING")],
        }
    }

    fn percent_prop() -> Property {
        Property::Range {
            field: "output.soc_percent".into(),
            min: Some(0.0),
            max: Some(100.0),
        }
    }

    #[test]
    fn set_member_passes_on_allowed_value() {
        let p = severity_prop();
        assert!(p.check(&json!(null), &json!({"severity": "HIGH"})).is_ok());
    }

    #[test]
    fn set_member_fails_on_disallowed_value() {
        let p = severity_prop();
        let err = p
            .check(&json!(null), &json!({"severity": "INFO"}))
            .unwrap_err();
        assert!(matches!(err, PropertyViolation::NotInSet { .. }));
    }

    #[test]
    fn range_passes_in_bounds() {
        let p = percent_prop();
        assert!(p.check(&json!(null), &json!({"soc_percent": 42.0})).is_ok());
    }

    #[test]
    fn range_fails_below_min() {
        let p = percent_prop();
        let err = p
            .check(&json!(null), &json!({"soc_percent": -1.0}))
            .unwrap_err();
        assert!(matches!(err, PropertyViolation::BelowMin { .. }));
    }

    #[test]
    fn range_fails_above_max() {
        let p = percent_prop();
        let err = p
            .check(&json!(null), &json!({"soc_percent": 101.0}))
            .unwrap_err();
        assert!(matches!(err, PropertyViolation::AboveMax { .. }));
    }

    #[test]
    fn range_accepts_integer_representation() {
        let p = percent_prop();
        // JSON `42` (i64) must be treated as 42.0 for range checks.
        assert!(p.check(&json!(null), &json!({"soc_percent": 42})).is_ok());
    }

    #[test]
    fn range_unbounded_min_only() {
        let p = Property::Range {
            field: "output.x".into(),
            min: None,
            max: Some(10.0),
        };
        assert!(p.check(&json!(null), &json!({"x": -100})).is_ok());
        assert!(p.check(&json!(null), &json!({"x": 11})).is_err());
    }

    #[test]
    fn path_resolves_into_input() {
        let p = Property::SetMember {
            field: "input.mode".into(),
            set: vec![json!("fast"), json!("slow")],
        };
        assert!(p.check(&json!({"mode": "fast"}), &json!(null)).is_ok());
    }

    #[test]
    fn path_resolves_nested() {
        let p = Property::Range {
            field: "output.battery.soc".into(),
            min: Some(0.0),
            max: Some(100.0),
        };
        assert!(p
            .check(&json!(null), &json!({"battery": {"soc": 42}}))
            .is_ok());
    }

    #[test]
    fn missing_field_errors_descriptively() {
        let p = severity_prop();
        let err = p.check(&json!(null), &json!({})).unwrap_err();
        assert!(
            matches!(err, PropertyViolation::FieldMissing { side: "output", .. }),
            "expected FieldMissing(output), got {err:?}"
        );
    }

    #[test]
    fn non_numeric_range_check_errors() {
        let p = percent_prop();
        let err = p
            .check(&json!(null), &json!({"soc_percent": "oops"}))
            .unwrap_err();
        assert!(matches!(err, PropertyViolation::NotNumber { .. }));
    }

    #[test]
    fn bad_root_segment_errors() {
        let p = Property::SetMember {
            field: "neither.foo".into(),
            set: vec![json!(1)],
        };
        let err = p.check(&json!({}), &json!({})).unwrap_err();
        assert!(matches!(err, PropertyViolation::BadPath { .. }));
    }

    #[test]
    fn property_serde_round_trip_set_member() {
        let p = severity_prop();
        let json_str = serde_json::to_string(&p).unwrap();
        let parsed: Property = serde_json::from_str(&json_str).unwrap();
        assert_eq!(p, parsed);
    }

    #[test]
    fn property_serde_round_trip_range() {
        let p = percent_prop();
        let json_str = serde_json::to_string(&p).unwrap();
        let parsed: Property = serde_json::from_str(&json_str).unwrap();
        assert_eq!(p, parsed);
    }

    #[test]
    fn unknown_kind_deserialises_into_unknown_variant() {
        // Forward-compat: a 1.0 reader must not choke on a 1.1 property
        // kind it doesn't recognise.
        let future_json = json!({
            "kind": "regex_match",
            "field": "output.id",
            "pattern": "^[A-Z0-9]+$"
        });
        let p: Property = serde_json::from_value(future_json.clone()).unwrap();
        assert!(p.is_unknown(), "expected Unknown, got {p:?}");
        // Evaluation: skip, not fail.
        let result = p.check(&json!({}), &json!({"id": "ABC123"}));
        assert!(result.is_ok());
        // Type validation: skip, not fail.
        let vresult =
            p.validate_against_types(&crate::types::NType::Any, &crate::types::NType::Any);
        assert!(vresult.is_ok());
    }

    #[test]
    fn property_json_shape_is_tagged_snake_case() {
        let p = severity_prop();
        let v: serde_json::Value = serde_json::to_value(&p).unwrap();
        assert_eq!(v["kind"], json!("set_member"));

        let r = percent_prop();
        let v: serde_json::Value = serde_json::to_value(&r).unwrap();
        assert_eq!(v["kind"], json!("range"));
    }

    // ── validate_against_types tests ────────────────────────────────

    use crate::types::NType;
    use std::collections::BTreeMap as BMap;

    fn record(fields: Vec<(&str, NType)>) -> NType {
        NType::Record(
            fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect::<BMap<_, _>>(),
        )
    }

    #[test]
    fn range_on_numeric_output_field_validates() {
        let p = Property::Range {
            field: "output.soc".into(),
            min: Some(0.0),
            max: Some(100.0),
        };
        let out = record(vec![("soc", NType::Number)]);
        assert!(p.validate_against_types(&NType::Any, &out).is_ok());
    }

    #[test]
    fn range_on_text_field_rejected() {
        // The motivating case from the review: Range on a Text field
        // must not silently pass.
        let p = Property::Range {
            field: "output.severity".into(),
            min: Some(0.0),
            max: Some(1.0),
        };
        let out = record(vec![("severity", NType::Text)]);
        let err = p.validate_against_types(&NType::Any, &out).unwrap_err();
        assert!(matches!(err, PropertyTypeError::RangeOnNonNumber { .. }));
    }

    #[test]
    fn set_member_accepts_any_terminal_type() {
        let p = Property::SetMember {
            field: "output.severity".into(),
            set: vec![json!("HIGH"), json!("LOW")],
        };
        let out = record(vec![("severity", NType::Text)]);
        assert!(p.validate_against_types(&NType::Any, &out).is_ok());
    }

    #[test]
    fn missing_field_rejected() {
        let p = Property::SetMember {
            field: "output.missing".into(),
            set: vec![json!(1)],
        };
        let out = record(vec![("present", NType::Number)]);
        let err = p.validate_against_types(&NType::Any, &out).unwrap_err();
        assert!(matches!(err, PropertyTypeError::FieldNotInType { .. }));
    }

    #[test]
    fn bad_root_segment_rejected() {
        let p = Property::SetMember {
            field: "neither.foo".into(),
            set: vec![json!(1)],
        };
        let err = p
            .validate_against_types(&NType::Any, &NType::Any)
            .unwrap_err();
        assert!(matches!(err, PropertyTypeError::BadPath { .. }));
    }

    #[test]
    fn any_field_accepts_arbitrary_path() {
        // Can't prove absence under Any — we defer to runtime.
        let p = Property::Range {
            field: "output.deeply.nested.thing".into(),
            min: Some(0.0),
            max: None,
        };
        assert!(p.validate_against_types(&NType::Any, &NType::Any).is_ok());
    }

    #[test]
    fn range_on_number_union_accepts() {
        // output type is Number | Null — Range is still valid because
        // at runtime a non-null value must be numeric.
        let p = Property::Range {
            field: "output".into(),
            min: Some(0.0),
            max: None,
        };
        let union = NType::union(vec![NType::Number, NType::Null]);
        assert!(p.validate_against_types(&NType::Any, &union).is_ok());
    }

    #[test]
    fn nested_path_walks_into_records() {
        let p = Property::Range {
            field: "output.battery.soc".into(),
            min: Some(0.0),
            max: Some(100.0),
        };
        let battery = record(vec![("soc", NType::Number)]);
        let out = record(vec![("battery", battery)]);
        assert!(p.validate_against_types(&NType::Any, &out).is_ok());
    }
}
