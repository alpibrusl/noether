//! Declarative properties that a stage claims to satisfy for every
//! `(input, output)` pair it accepts. Properties are the M2 answer to
//! the question *"what does this stage actually guarantee, beyond its
//! type signature?"*. Types say `output: Number`; properties say
//! `output >= 0 and output <= 100`.
//!
//! ## Scope
//!
//! The v0.6 DSL shipped with two variants; M2.5 (v0.7) added five more
//! for relational and type-shape constraints:
//!
//! - [`Property::SetMember`] — a named field is one of a fixed set of
//!   JSON values.
//! - [`Property::Range`] — a named numeric field is within `[min, max]`
//!   (either bound optional).
//! - [`Property::FieldLengthEq`] — two fields have the same length
//!   (string UTF-8 chars / array length / object key count).
//! - [`Property::FieldLengthMax`] — `subject_field` length ≤
//!   `bound_field` length.
//! - [`Property::SubsetOf`] — every element / key of `subject_field`
//!   appears in `super_field`.
//! - [`Property::Equals`] — two fields are JSON-value-equal.
//! - [`Property::FieldTypeIn`] — the runtime JSON type at `field` is
//!   one of the allowed set.
//!
//! Higher-order predicates (implications over examples, quantifiers,
//! type-class predicates) remain explicit non-goals for 1.0.
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

/// The six JSON runtime-value kinds. Used by
/// [`Property::FieldTypeIn`] to declare a field's acceptable kinds.
///
/// Typed rather than a free-form `String` so that wire-format typos
/// (`"bolean"`, `"interger"`) fail at deserialisation with a clear
/// serde error rather than silently making every example violate
/// the property. Wire format is unchanged from M2.5 round-1:
/// `"allowed": ["number", "string"]` — snake_case strings matching
/// the serde-renamed variant names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JsonKind {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

impl JsonKind {
    /// Return the kind of a given JSON value.
    pub fn of(v: &serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => JsonKind::Null,
            serde_json::Value::Bool(_) => JsonKind::Bool,
            serde_json::Value::Number(_) => JsonKind::Number,
            serde_json::Value::String(_) => JsonKind::String,
            serde_json::Value::Array(_) => JsonKind::Array,
            serde_json::Value::Object(_) => JsonKind::Object,
        }
    }

    /// The wire-format string for this kind — same bytes that
    /// serde emits. Used for human-readable error messages.
    pub fn as_str(&self) -> &'static str {
        match self {
            JsonKind::Null => "null",
            JsonKind::Bool => "bool",
            JsonKind::Number => "number",
            JsonKind::String => "string",
            JsonKind::Array => "array",
            JsonKind::Object => "object",
        }
    }
}

impl std::fmt::Display for JsonKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

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
    /// The length of the value at `left_field` equals the length of the
    /// value at `right_field`. "Length" is:
    ///
    /// - **String**: UTF-8 **code-point** count (`str::chars().count()`),
    ///   not byte count and not grapheme cluster count. `"a̐"` has
    ///   length 2 (two codepoints: `a` + combining accent); `"👨‍👩‍👧"`
    ///   has length 5 (three emoji + two zero-width joiners).
    /// - **Array**: element count.
    /// - **Object**: key count.
    /// - **Other scalars (number, bool, null)**: not measurable; the
    ///   property fails with [`PropertyViolation::NotMeasurable`].
    ///
    /// Added in M2.5 to express length-preservation invariants
    /// (`text_reverse`, `text_upper`, `map`, etc.).
    ///
    /// **Cross-kind comparisons are allowed but rarely useful.**
    /// `FieldLengthEq { left: array, right: string }` compares the
    /// array's element count against the string's code-point count
    /// — mechanically defined, but almost never what an author
    /// means. Prefer paths of the same JSON kind.
    FieldLengthEq {
        left_field: String,
        right_field: String,
    },
    /// The length of `subject_field` is less than or equal to the
    /// length of `bound_field`. Useful for stages like `filter`,
    /// `take`, `list_dedup` where the output is bounded by the
    /// input's size but not equal to it.
    ///
    /// Added in M2.5.
    FieldLengthMax {
        subject_field: String,
        bound_field: String,
    },
    /// Every element / key / character of `subject_field` appears in
    /// `super_field`. Semantics depend on the runtime JSON shape of
    /// the two values — all three branches are useful in practice,
    /// but they're different checks:
    ///
    /// - **Array vs Array**: every element of `subject` appears (by
    ///   JSON-value equality) in `super`. Duplicates allowed as long
    ///   as the value is present. Useful for `sort`, `filter`,
    ///   `project` — output elements are drawn from input.
    /// - **Object vs Object**: every `(key, value)` of `subject`
    ///   appears with an equal value in `super` — *stricter* than
    ///   key-presence. `{"a": 1}` is NOT a subset of `{"a": 2}`.
    /// - **String vs String**: `subject` is a **contiguous
    ///   substring** of `super` — not a character set. So
    ///   `SubsetOf { subject: "abc", super: "bac" }` is false (`abc`
    ///   is not a substring of `bac`) even though every character in
    ///   `abc` appears somewhere in `bac`. This matches the
    ///   "output string is a quote from the input" use case; declare
    ///   a `Range` or `FieldLengthMax` if you want a weaker claim.
    ///
    /// Mixed-type pairs (`Array` vs `Object`, scalar vs anything) and
    /// scalar-only pairs produce [`PropertyViolation::NotCollectionForSubset`].
    ///
    /// Added in M2.5.
    SubsetOf {
        subject_field: String,
        super_field: String,
    },
    /// `left_field` and `right_field` are equal by JSON-value
    /// equality. The most common use is reflexivity (`output ==
    /// input` for identity stages) and content preservation (output
    /// body bytes match a source field).
    ///
    /// Added in M2.5.
    Equals {
        left_field: String,
        right_field: String,
    },
    /// The runtime JSON type at `field` is one of the allowed kinds.
    /// Kinds are enumerated by [`JsonKind`] (typed rather than
    /// free-form strings, so wire-format typos fail at
    /// deserialisation). Bridges the gap between the structural type
    /// system's compile-time view and the actual runtime shape.
    ///
    /// Added in M2.5.
    FieldTypeIn {
        field: String,
        allowed: Vec<JsonKind>,
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

    // ── M2.5 violation variants ────────────────────────────────────────
    #[error(
        "field `{path}` has no measurable length ({actual}); expected a \
         string, array, or object for a length check"
    )]
    NotMeasurable {
        path: String,
        actual: serde_json::Value,
    },

    #[error(
        "length check failed: `{left}` has length {left_len}, `{right}` \
         has length {right_len}; expected equal"
    )]
    LengthMismatch {
        left: String,
        left_len: usize,
        right: String,
        right_len: usize,
    },

    #[error(
        "length bound violated: `{subject}` has length {subject_len} but \
         `{bound}` has length {bound_len}; expected subject ≤ bound"
    )]
    LengthExceedsBound {
        subject: String,
        subject_len: usize,
        bound: String,
        bound_len: usize,
    },

    #[error(
        "field `{subject}` is not a subset of `{super_field}`: element \
         {element} appears in subject but not in super"
    )]
    NotSubset {
        subject: String,
        super_field: String,
        element: serde_json::Value,
    },

    #[error(
        "subset check needs arrays, objects, or strings; `{path}` is \
         {actual}"
    )]
    NotCollectionForSubset {
        path: String,
        actual: serde_json::Value,
    },

    #[error(
        "equality check failed: `{left}` is {left_value}; `{right}` is \
         {right_value}"
    )]
    NotEqual {
        left: String,
        left_value: serde_json::Value,
        right: String,
        right_value: serde_json::Value,
    },

    #[error(
        "field `{path}` is of JSON type `{actual}`; expected one of: \
         {allowed:?}"
    )]
    TypeNotInAllowed {
        path: String,
        actual: JsonKind,
        allowed: Vec<JsonKind>,
    },
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
    /// The primary field path this property constrains. Used by
    /// callers that want to group properties by target field for
    /// reporting.
    ///
    /// For relational variants (`FieldLengthEq`, `FieldLengthMax`,
    /// `SubsetOf`, `Equals`) this returns the left/subject side.
    /// Returns an empty string for [`Property::Unknown`].
    pub fn field(&self) -> &str {
        match self {
            Property::SetMember { field, .. }
            | Property::Range { field, .. }
            | Property::FieldTypeIn { field, .. } => field,
            Property::FieldLengthEq { left_field, .. } | Property::Equals { left_field, .. } => {
                left_field
            }
            Property::FieldLengthMax { subject_field, .. }
            | Property::SubsetOf { subject_field, .. } => subject_field,
            Property::Unknown { .. } => "",
        }
    }

    /// True if this is an `Unknown` variant — the reader doesn't know
    /// how to evaluate the property. Callers that aggregate results
    /// should treat these as skips, not passes or failures.
    pub fn is_unknown(&self) -> bool {
        matches!(self, Property::Unknown { .. })
    }

    /// If this property came out as [`Property::Unknown`] but its raw
    /// `"kind"` matches a KNOWN variant name, return the known kind
    /// string. This captures the case where a user typo'd a field
    /// *within* a known kind (e.g. `allowed: ["bolean"]` inside a
    /// valid `field_type_in`) and serde fell through to the
    /// forward-compat `Unknown` variant instead of surfacing the
    /// error. Downstream ingest paths (stage add, `validate_spec`)
    /// should treat `Some(...)` here as a hard error — the user
    /// intended a known property kind but wrote something malformed,
    /// and the `Unknown` escape hatch would silently drop the check.
    pub fn shadowed_known_kind(&self) -> Option<&str> {
        const KNOWN_KINDS: &[&str] = &[
            "set_member",
            "range",
            "field_length_eq",
            "field_length_max",
            "subset_of",
            "equals",
            "field_type_in",
        ];
        match self {
            Property::Unknown { raw } => raw
                .get("kind")
                .and_then(|k| k.as_str())
                .filter(|k| KNOWN_KINDS.contains(k)),
            _ => None,
        }
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
                // A free type variable is shape-opaque (M3): we can't prove
                // the field is absent under an unknown-type cursor. Same
                // treatment as Any — accept and defer to runtime.
                NType::Var(_) => {
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
            // we're permissive here). A free type variable is accepted on
            // the same grounds as Any: we can't prove at stage-registration
            // time that the eventual binding isn't Number.
            Property::Range { .. } => match terminal {
                NType::Number | NType::Any | NType::Var(_) => Ok(()),
                NType::Union(variants) => {
                    if variants
                        .iter()
                        .any(|v| matches!(v, NType::Number | NType::Any | NType::Var(_)))
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
            // M2.5 relational variants: type-aware validation of
            // these pairs would require resolving BOTH paths, which
            // validate_against_types doesn't do today (it validates
            // one path). Accept them structurally; runtime
            // evaluation still catches real shape mismatches.
            Property::FieldLengthEq { .. }
            | Property::FieldLengthMax { .. }
            | Property::SubsetOf { .. }
            | Property::Equals { .. }
            | Property::FieldTypeIn { .. } => Ok(()),
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
        match self {
            Property::Unknown { .. } => Ok(()),
            Property::SetMember { field, set } => {
                let value = resolve_path(field, input, output)?;
                if set.iter().any(|allowed| allowed == value) {
                    Ok(())
                } else {
                    Err(PropertyViolation::NotInSet {
                        path: field.clone(),
                        actual: value.clone(),
                        expected: set.clone(),
                    })
                }
            }
            Property::Range { field, min, max } => {
                let value = resolve_path(field, input, output)?;
                let n = coerce_number(value).ok_or_else(|| PropertyViolation::NotNumber {
                    path: field.clone(),
                    actual: value.clone(),
                })?;
                if let Some(lo) = min {
                    if n < *lo {
                        return Err(PropertyViolation::BelowMin {
                            path: field.clone(),
                            actual: n,
                            min: *lo,
                        });
                    }
                }
                if let Some(hi) = max {
                    if n > *hi {
                        return Err(PropertyViolation::AboveMax {
                            path: field.clone(),
                            actual: n,
                            max: *hi,
                        });
                    }
                }
                Ok(())
            }
            Property::FieldLengthEq {
                left_field,
                right_field,
            } => {
                let left = resolve_path(left_field, input, output)?;
                let right = resolve_path(right_field, input, output)?;
                let left_len =
                    measurable_length(left).ok_or_else(|| PropertyViolation::NotMeasurable {
                        path: left_field.clone(),
                        actual: left.clone(),
                    })?;
                let right_len =
                    measurable_length(right).ok_or_else(|| PropertyViolation::NotMeasurable {
                        path: right_field.clone(),
                        actual: right.clone(),
                    })?;
                if left_len == right_len {
                    Ok(())
                } else {
                    Err(PropertyViolation::LengthMismatch {
                        left: left_field.clone(),
                        left_len,
                        right: right_field.clone(),
                        right_len,
                    })
                }
            }
            Property::FieldLengthMax {
                subject_field,
                bound_field,
            } => {
                let subject = resolve_path(subject_field, input, output)?;
                let bound = resolve_path(bound_field, input, output)?;
                let subject_len =
                    measurable_length(subject).ok_or_else(|| PropertyViolation::NotMeasurable {
                        path: subject_field.clone(),
                        actual: subject.clone(),
                    })?;
                let bound_len =
                    measurable_length(bound).ok_or_else(|| PropertyViolation::NotMeasurable {
                        path: bound_field.clone(),
                        actual: bound.clone(),
                    })?;
                if subject_len <= bound_len {
                    Ok(())
                } else {
                    Err(PropertyViolation::LengthExceedsBound {
                        subject: subject_field.clone(),
                        subject_len,
                        bound: bound_field.clone(),
                        bound_len,
                    })
                }
            }
            Property::SubsetOf {
                subject_field,
                super_field,
            } => {
                let subject = resolve_path(subject_field, input, output)?;
                let superset = resolve_path(super_field, input, output)?;
                check_subset(subject_field, subject, super_field, superset)
            }
            Property::Equals {
                left_field,
                right_field,
            } => {
                let left = resolve_path(left_field, input, output)?;
                let right = resolve_path(right_field, input, output)?;
                if left == right {
                    Ok(())
                } else {
                    Err(PropertyViolation::NotEqual {
                        left: left_field.clone(),
                        left_value: left.clone(),
                        right: right_field.clone(),
                        right_value: right.clone(),
                    })
                }
            }
            Property::FieldTypeIn { field, allowed } => {
                let value = resolve_path(field, input, output)?;
                let actual = JsonKind::of(value);
                if allowed.contains(&actual) {
                    Ok(())
                } else {
                    Err(PropertyViolation::TypeNotInAllowed {
                        path: field.clone(),
                        actual,
                        allowed: allowed.clone(),
                    })
                }
            }
        }
    }
}

fn coerce_number(v: &serde_json::Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_i64().map(|i| i as f64))
        .or_else(|| v.as_u64().map(|u| u as f64))
}

/// Return the length of a JSON value for length-based property
/// checks. UTF-8 chars for strings, element count for arrays, key
/// count for objects. `None` for non-measurable types (numbers,
/// bools, null).
fn measurable_length(v: &serde_json::Value) -> Option<usize> {
    match v {
        serde_json::Value::String(s) => Some(s.chars().count()),
        serde_json::Value::Array(a) => Some(a.len()),
        serde_json::Value::Object(o) => Some(o.len()),
        _ => None,
    }
}

/// Element-wise subset check used by `Property::SubsetOf`. Works on
/// arrays, objects (key set), and strings (substring).
fn check_subset(
    subject_path: &str,
    subject: &serde_json::Value,
    super_path: &str,
    superset: &serde_json::Value,
) -> Result<(), PropertyViolation> {
    match (subject, superset) {
        (serde_json::Value::Array(sub), serde_json::Value::Array(sup)) => {
            for element in sub {
                if !sup.iter().any(|s| s == element) {
                    return Err(PropertyViolation::NotSubset {
                        subject: subject_path.to_string(),
                        super_field: super_path.to_string(),
                        element: element.clone(),
                    });
                }
            }
            Ok(())
        }
        (serde_json::Value::Object(sub), serde_json::Value::Object(sup)) => {
            // Object subset = every key in subject appears in super
            // with an equal value.
            for (key, val) in sub {
                match sup.get(key) {
                    Some(sup_val) if sup_val == val => {}
                    _ => {
                        return Err(PropertyViolation::NotSubset {
                            subject: subject_path.to_string(),
                            super_field: super_path.to_string(),
                            element: serde_json::json!({ key.as_str(): val }),
                        });
                    }
                }
            }
            Ok(())
        }
        (serde_json::Value::String(sub), serde_json::Value::String(sup)) => {
            if sup.contains(sub.as_str()) {
                Ok(())
            } else {
                Err(PropertyViolation::NotSubset {
                    subject: subject_path.to_string(),
                    super_field: super_path.to_string(),
                    element: serde_json::Value::String(sub.clone()),
                })
            }
        }
        // Cross-type or scalar subject/super: not a meaningful
        // subset check. Blame whichever side is *not* a collection
        // (array / object / string) so the error points the operator
        // at the malformed field rather than the well-formed one.
        (_, _) => {
            let subject_is_collection = matches!(
                subject,
                serde_json::Value::Array(_)
                    | serde_json::Value::Object(_)
                    | serde_json::Value::String(_)
            );
            let (path, actual) = if subject_is_collection {
                (super_path.to_string(), superset.clone())
            } else {
                (subject_path.to_string(), subject.clone())
            };
            Err(PropertyViolation::NotCollectionForSubset { path, actual })
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

    // ── M2.5 new variants ──────────────────────────────────────────

    #[test]
    fn field_length_eq_passes_on_equal_string_lengths() {
        let p = Property::FieldLengthEq {
            left_field: "output".into(),
            right_field: "input".into(),
        };
        assert!(p.check(&json!("abc"), &json!("cba")).is_ok());
    }

    #[test]
    fn field_length_eq_fails_on_different_lengths() {
        let p = Property::FieldLengthEq {
            left_field: "output".into(),
            right_field: "input".into(),
        };
        let err = p.check(&json!("abc"), &json!("abcd")).unwrap_err();
        assert!(matches!(err, PropertyViolation::LengthMismatch { .. }));
    }

    #[test]
    fn field_length_eq_handles_arrays() {
        let p = Property::FieldLengthEq {
            left_field: "output.items".into(),
            right_field: "input.items".into(),
        };
        assert!(p
            .check(
                &json!({"items": [1, 2, 3]}),
                &json!({"items": ["a", "b", "c"]})
            )
            .is_ok());
    }

    #[test]
    fn field_length_eq_rejects_non_measurable() {
        let p = Property::FieldLengthEq {
            left_field: "output".into(),
            right_field: "input".into(),
        };
        let err = p.check(&json!(42), &json!("abc")).unwrap_err();
        assert!(matches!(err, PropertyViolation::NotMeasurable { .. }));
    }

    #[test]
    fn field_length_max_passes_when_subject_bounded() {
        let p = Property::FieldLengthMax {
            subject_field: "output.items".into(),
            bound_field: "input.items".into(),
        };
        assert!(p
            .check(&json!({"items": [1, 2]}), &json!({"items": [10]}))
            .is_ok());
        assert!(p
            .check(
                &json!({"items": [1, 2, 3]}),
                &json!({"items": [10, 20, 30]})
            )
            .is_ok());
    }

    #[test]
    fn field_length_max_fails_when_subject_exceeds_bound() {
        let p = Property::FieldLengthMax {
            subject_field: "output.items".into(),
            bound_field: "input.items".into(),
        };
        let err = p
            .check(&json!({"items": [10, 20]}), &json!({"items": [1, 2, 3, 4]}))
            .unwrap_err();
        assert!(matches!(err, PropertyViolation::LengthExceedsBound { .. }));
    }

    #[test]
    fn subset_of_passes_on_array_subset() {
        let p = Property::SubsetOf {
            subject_field: "output".into(),
            super_field: "input".into(),
        };
        assert!(p.check(&json!([1, 2, 3, 4]), &json!([1, 3])).is_ok());
    }

    #[test]
    fn subset_of_fails_on_non_subset() {
        let p = Property::SubsetOf {
            subject_field: "output".into(),
            super_field: "input".into(),
        };
        let err = p.check(&json!([1, 2, 3]), &json!([99, 100])).unwrap_err();
        assert!(matches!(err, PropertyViolation::NotSubset { .. }));
    }

    #[test]
    fn subset_of_objects_uses_key_subset() {
        let p = Property::SubsetOf {
            subject_field: "output".into(),
            super_field: "input".into(),
        };
        assert!(p
            .check(&json!({"a": 1, "b": 2, "c": 3}), &json!({"a": 1, "b": 2}))
            .is_ok());
    }

    #[test]
    fn subset_of_strings_is_substring_not_character_set() {
        // Pinning test for the round-2 review finding: `SubsetOf`
        // on strings means **contiguous substring**, not a
        // character-set subset. This test contrasts the two
        // interpretations so the chosen semantics can't drift.
        //
        // "abc" ⊂ "bac" under character-subset (every char of "abc"
        //   appears in "bac") — but we DO NOT want that match.
        // "abc" ⊂ "zabcz" under both substring and char-subset.
        let p = Property::SubsetOf {
            subject_field: "output".into(),
            super_field: "input".into(),
        };

        // input = "bac" contains the chars a,b,c but not the
        // substring "abc". Property must fail — the docstring's
        // substring contract is the authoritative one.
        let err = p
            .check(&json!("bac"), &json!("abc"))
            .expect_err("expected substring check to reject 'abc' ⊄ 'bac'");
        assert!(
            matches!(err, PropertyViolation::NotSubset { .. }),
            "expected NotSubset, got {err:?}"
        );

        // input = "zabcz" contains the substring "abc" — passes.
        assert!(p.check(&json!("zabcz"), &json!("abc")).is_ok());

        // Identity (trivial substring of self) passes.
        assert!(p.check(&json!("abc"), &json!("abc")).is_ok());
    }

    #[test]
    fn subset_of_rejects_mixed_collection_kinds() {
        // Array-vs-object and collection-vs-scalar both surface as
        // NotCollectionForSubset rather than a false pass/fail.
        let p = Property::SubsetOf {
            subject_field: "output".into(),
            super_field: "input".into(),
        };
        let err = p
            .check(&json!({"a": 1}), &json!([1, 2]))
            .expect_err("array subject vs object super must fail");
        assert!(matches!(
            err,
            PropertyViolation::NotCollectionForSubset { .. }
        ));

        let err = p
            .check(&json!(42), &json!([1, 2, 3]))
            .expect_err("scalar super vs array subject must fail");
        assert!(matches!(
            err,
            PropertyViolation::NotCollectionForSubset { .. }
        ));
    }

    #[test]
    fn equals_passes_on_identical_values() {
        let p = Property::Equals {
            left_field: "output".into(),
            right_field: "input".into(),
        };
        assert!(p.check(&json!({"a": 1}), &json!({"a": 1})).is_ok());
    }

    #[test]
    fn equals_fails_on_different_values() {
        let p = Property::Equals {
            left_field: "output".into(),
            right_field: "input".into(),
        };
        let err = p.check(&json!({"a": 1}), &json!({"a": 2})).unwrap_err();
        assert!(matches!(err, PropertyViolation::NotEqual { .. }));
    }

    #[test]
    fn field_type_in_passes_on_allowed_type() {
        let p = Property::FieldTypeIn {
            field: "output.value".into(),
            allowed: vec![JsonKind::Number, JsonKind::Null],
        };
        assert!(p.check(&json!(null), &json!({"value": 42})).is_ok());
        assert!(p.check(&json!(null), &json!({"value": null})).is_ok());
    }

    #[test]
    fn field_type_in_fails_on_disallowed_type() {
        let p = Property::FieldTypeIn {
            field: "output.value".into(),
            allowed: vec![JsonKind::Number],
        };
        let err = p
            .check(&json!(null), &json!({"value": "oops"}))
            .unwrap_err();
        assert!(matches!(err, PropertyViolation::TypeNotInAllowed { .. }));
    }

    #[test]
    fn field_type_in_typo_surfaces_as_shadowed_known_kind() {
        // The `JsonKind` enum catches typos at the Rust API level
        // (code that constructs `Property::FieldTypeIn { allowed: ... }`
        // can't pass a bad string). But wire-format typos fall
        // through to `Property::Unknown` via serde's forward-compat
        // untagged fallback — we don't want to break loading
        // v0.8 graphs in a v0.7 reader just because a new kind
        // was added.
        //
        // `shadowed_known_kind` splits the two cases: an unknown
        // variant with a truly unknown `kind` is fine (v0.8
        // forward-compat); an unknown variant with a KNOWN `kind`
        // means the user mistyped inside a known property and
        // should be rejected at ingest time.
        let bad = r#"{
            "kind": "field_type_in",
            "field": "output.x",
            "allowed": ["bolean"]
        }"#;
        let p: Property = serde_json::from_str(bad).unwrap();
        assert!(
            matches!(p, Property::Unknown { .. }),
            "typo falls through to Unknown via serde untagged fallback"
        );
        assert_eq!(
            p.shadowed_known_kind(),
            Some("field_type_in"),
            "shadow of a known kind must be detectable; ingest paths \
             should reject properties that report this non-None"
        );
    }

    #[test]
    fn genuinely_unknown_kind_does_not_shadow() {
        // A hypothetical v0.8 kind that v0.7 doesn't know about
        // must NOT be flagged as shadowing — forward compat.
        let future = r#"{
            "kind": "v08_implication",
            "premise": "input.x",
            "conclusion": "output.y"
        }"#;
        let p: Property = serde_json::from_str(future).unwrap();
        assert!(matches!(p, Property::Unknown { .. }));
        assert_eq!(
            p.shadowed_known_kind(),
            None,
            "truly unknown kinds must not be flagged as shadowing"
        );
    }

    #[test]
    fn field_type_in_empty_allowlist_always_fails() {
        // Edge case: an empty `allowed` list is vacuously unsatisfiable.
        // Pins the intended behavior so a future refactor can't
        // silently short-circuit `allowed.is_empty() => Ok(())`.
        let p = Property::FieldTypeIn {
            field: "output".into(),
            allowed: vec![],
        };
        let err = p.check(&json!(null), &json!("x")).unwrap_err();
        assert!(matches!(err, PropertyViolation::TypeNotInAllowed { .. }));
    }

    #[test]
    fn subset_of_empty_subject_is_vacuously_true() {
        // Every side is a subset of anything when the subject is
        // empty — trivially true for arrays, objects, and strings.
        // Protects against a refactor that accidentally special-cases
        // empty inputs as violations.
        let p = Property::SubsetOf {
            subject_field: "output".into(),
            super_field: "input".into(),
        };
        assert!(p.check(&json!([1, 2, 3]), &json!([])).is_ok());
        assert!(p.check(&json!({"a": 1, "b": 2}), &json!({})).is_ok());
        assert!(p.check(&json!("abc"), &json!("")).is_ok());
    }

    #[test]
    fn field_type_in_wire_format_is_snake_case_strings() {
        // Regression guard on the `#[serde(rename_all = "snake_case")]`
        // contract — the byte-count argument in the round-1 review
        // depends on `JsonKind::Number` serialising as `"number"`,
        // not `"Number"` or some other casing.
        let p = Property::FieldTypeIn {
            field: "output.x".into(),
            allowed: vec![JsonKind::Bool, JsonKind::Null],
        };
        let json = serde_json::to_value(&p).unwrap();
        assert_eq!(json["allowed"], serde_json::json!(["bool", "null"]));
    }

    #[test]
    fn new_variants_serde_roundtrip() {
        let cases = vec![
            Property::FieldLengthEq {
                left_field: "output".into(),
                right_field: "input".into(),
            },
            Property::FieldLengthMax {
                subject_field: "output".into(),
                bound_field: "input.items".into(),
            },
            Property::SubsetOf {
                subject_field: "output.keys".into(),
                super_field: "input.keys".into(),
            },
            Property::Equals {
                left_field: "output".into(),
                right_field: "input".into(),
            },
            Property::FieldTypeIn {
                field: "output.id".into(),
                allowed: vec![JsonKind::String],
            },
        ];
        for p in cases {
            let serialised = serde_json::to_string(&p).unwrap();
            let parsed: Property = serde_json::from_str(&serialised).unwrap();
            assert_eq!(p, parsed, "round-trip failed: {serialised}");
        }
    }

    #[test]
    fn new_variants_json_shape_is_tagged_snake_case() {
        let cases = vec![
            (
                Property::FieldLengthEq {
                    left_field: "output".into(),
                    right_field: "input".into(),
                },
                "field_length_eq",
            ),
            (
                Property::FieldLengthMax {
                    subject_field: "output".into(),
                    bound_field: "input".into(),
                },
                "field_length_max",
            ),
            (
                Property::SubsetOf {
                    subject_field: "output".into(),
                    super_field: "input".into(),
                },
                "subset_of",
            ),
            (
                Property::Equals {
                    left_field: "output".into(),
                    right_field: "input".into(),
                },
                "equals",
            ),
            (
                Property::FieldTypeIn {
                    field: "output".into(),
                    allowed: vec![JsonKind::String],
                },
                "field_type_in",
            ),
        ];
        for (p, expected_kind) in cases {
            let v: serde_json::Value = serde_json::to_value(&p).unwrap();
            assert_eq!(v["kind"], json!(expected_kind));
        }
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
