//! Refinement predicates for [`NType::Refined`](super::NType::Refined).
//!
//! A **refinement** attaches a runtime-checkable predicate to a
//! [base `NType`](super::NType). `NType::Refined { base: Number,
//! refinement: Refinement::Range { min: 0, max: 100 } }` reads as
//! "a `Number` that is between 0 and 100 inclusive."
//!
//! # Scope of this module
//!
//! Three refinement kinds cover the cases agent-composed graphs hit
//! most often:
//!
//! - [`Refinement::Range`] — numeric bounds.
//! - [`Refinement::OneOf`] — closed-set / enum-like membership.
//! - [`Refinement::NonEmpty`] — string / array non-emptiness.
//!
//! The DSL is deliberately narrow. Adding more predicates — regex
//! match, length bounds, etc. — is additive; the existing entries
//! keep their meaning across 1.x per `STABILITY.md`.
//!
//! # What this module does NOT do (yet)
//!
//! - **Subtype-level predicate implication.** Proving that
//!   `Range { min: 0, max: 10 }` refines `Range { min: 0, max: 100 }`
//!   is a valid subtyping step but requires predicate-logic
//!   reasoning. Current [`is_subtype_of`](super::is_subtype_of)
//!   treats `Refined` structurally: sub can drop a refinement to
//!   match a non-refined sup, and two refinements must be equal to
//!   be compatible.
//! - **Automatic runtime enforcement.** `noether run` wraps its
//!   executor in `ValidatingExecutor` by default, which calls
//!   [`validate`] on inputs and outputs at every stage boundary. A
//!   violation fails the stage with an `input refinement violation` or
//!   `output refinement violation` message. Embedders using the
//!   library directly opt in by building their own
//!   `ValidatingExecutor`; the `NOETHER_NO_REFINEMENT_CHECK=1` env
//!   var is the CLI-level opt-out.

use super::NType;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The predicate attached to a refinement type.
///
/// Every variant is serde-tagged so the wire form is uniform with
/// [`NType`](super::NType) and [`crate::stage::property::Property`].
///
/// `Eq` / `Hash` / `Ord` are implemented via the serialized JSON form
/// — `f64` and `serde_json::Value` don't derive `Eq` / `Hash` / `Ord`
/// cleanly (NaN ordering, float-bit equality, etc.), and the wire form
/// is already the canonical identity we ship anyway. This keeps
/// [`NType`](super::NType)'s existing trait bounds working when it
/// nests a `Refinement` inside a `Refined` variant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Refinement {
    /// Numeric value within `[min, max]`. Either bound may be
    /// omitted for an open-ended interval. Applies to `Number`
    /// values; non-numeric values fail validation with
    /// `"Range requires a Number"`.
    Range {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
    },
    /// Value must JSON-equal one of the listed options. Works on
    /// any JSON scalar or object. Equivalent to an enum at the type
    /// level.
    OneOf { options: Vec<Value> },
    /// Value must be non-empty. Applies to:
    ///
    /// - `String` — at least one codepoint.
    /// - `Array` — at least one element.
    /// - `Object` — at least one key.
    ///
    /// Non-measurable values (numbers, booleans, null) fail with
    /// `"NonEmpty expects a string / array / object"`.
    NonEmpty,
}

// ── Eq / Hash / Ord via canonical wire form ─────────────────────────
//
// `f64` and `serde_json::Value` don't cleanly derive `Eq` / `Hash` /
// `Ord`. Hashing / comparing via the serialized JSON form is the
// standard workaround when a container needs those traits on a type
// whose components lack them — trade an allocation per comparison
// for correctness and consistency with the wire format. `NType`
// needs `Eq + Hash + Ord` for its existing uses, so `Refined` wrapping
// a `Refinement` inherits these impls.

impl Eq for Refinement {}

impl std::hash::Hash for Refinement {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        serde_json::to_string(self).unwrap_or_default().hash(state);
    }
}

impl Ord for Refinement {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let a = serde_json::to_string(self).unwrap_or_default();
        let b = serde_json::to_string(other).unwrap_or_default();
        a.cmp(&b)
    }
}

impl PartialOrd for Refinement {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl std::fmt::Display for Refinement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refinement::Range { min, max } => match (min, max) {
                (Some(lo), Some(hi)) => write!(f, "Range({lo}..={hi})"),
                (Some(lo), None) => write!(f, "Range({lo}..)"),
                (None, Some(hi)) => write!(f, "Range(..={hi})"),
                (None, None) => write!(f, "Range(..)"),
            },
            Refinement::OneOf { options } => {
                write!(f, "OneOf[")?;
                for (i, v) in options.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", serde_json::to_string(v).unwrap_or_default())?;
                }
                write!(f, "]")
            }
            Refinement::NonEmpty => write!(f, "NonEmpty"),
        }
    }
}

/// Check whether `value` satisfies `refinement`.
///
/// Returns `Ok(())` when the value is in the refinement set, or
/// `Err(reason)` with a short human-readable message otherwise.
/// Callers (the executor runtime, stage-verification CI, etc.)
/// decide how to surface the reason.
pub fn validate(value: &Value, refinement: &Refinement) -> Result<(), String> {
    match refinement {
        Refinement::Range { min, max } => {
            let n = value.as_f64().ok_or_else(|| {
                format!("Range requires a Number, got {}", short_type_name(value))
            })?;
            if let Some(lo) = min {
                if n < *lo {
                    return Err(format!("value {n} below minimum {lo}"));
                }
            }
            if let Some(hi) = max {
                if n > *hi {
                    return Err(format!("value {n} above maximum {hi}"));
                }
            }
            Ok(())
        }
        Refinement::OneOf { options } => {
            if options.iter().any(|o| o == value) {
                Ok(())
            } else {
                Err(format!(
                    "value {} is not one of the allowed options",
                    serde_json::to_string(value).unwrap_or_default()
                ))
            }
        }
        Refinement::NonEmpty => match value {
            Value::String(s) if !s.is_empty() => Ok(()),
            Value::Array(a) if !a.is_empty() => Ok(()),
            Value::Object(o) if !o.is_empty() => Ok(()),
            Value::String(_) => Err("string is empty".into()),
            Value::Array(_) => Err("array is empty".into()),
            Value::Object(_) => Err("object is empty".into()),
            _ => Err(format!(
                "NonEmpty expects a string / array / object, got {}",
                short_type_name(value)
            )),
        },
    }
}

/// Collect every refinement layer attached along a `Refined` chain,
/// outermost-first — the order the validator applies them.
///
/// `Refined { base: Refined { base: Number, r1 }, r2 }` yields
/// `[r2, r1]`. Non-`Refined` types return an empty slice.
///
/// Callers that also need the stripped base type can pair this with
/// [`strip_refinements`].
pub fn refinements_of(ty: &NType) -> Vec<&Refinement> {
    let mut out = Vec::new();
    let mut current = ty;
    while let NType::Refined { base, refinement } = current {
        out.push(refinement);
        current = base;
    }
    out
}

/// Peel [`NType::Refined`] wrappers off `ty` and return the concrete
/// base type underneath. Non-refined types are returned unchanged.
pub fn strip_refinements(ty: &NType) -> &NType {
    let mut current = ty;
    while let NType::Refined { base, .. } = current {
        current = base;
    }
    current
}

fn short_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Range ─────────────────────────────────────────────────────

    #[test]
    fn range_accepts_value_inside_bounds() {
        let r = Refinement::Range {
            min: Some(0.0),
            max: Some(100.0),
        };
        validate(&json!(0), &r).unwrap();
        validate(&json!(50.5), &r).unwrap();
        validate(&json!(100), &r).unwrap();
    }

    #[test]
    fn range_rejects_value_below_min() {
        let r = Refinement::Range {
            min: Some(0.0),
            max: None,
        };
        let err = validate(&json!(-1), &r).unwrap_err();
        assert!(err.contains("below minimum"));
    }

    #[test]
    fn range_rejects_value_above_max() {
        let r = Refinement::Range {
            min: None,
            max: Some(100.0),
        };
        let err = validate(&json!(101), &r).unwrap_err();
        assert!(err.contains("above maximum"));
    }

    #[test]
    fn range_unbounded_accepts_any_number() {
        let r = Refinement::Range {
            min: None,
            max: None,
        };
        validate(&json!(-1e9), &r).unwrap();
        validate(&json!(0), &r).unwrap();
        validate(&json!(1e9), &r).unwrap();
    }

    #[test]
    fn range_rejects_non_number() {
        let r = Refinement::Range {
            min: Some(0.0),
            max: Some(100.0),
        };
        let err = validate(&json!("fifty"), &r).unwrap_err();
        assert!(err.contains("Range requires a Number"));
    }

    // ── OneOf ────────────────────────────────────────────────────

    #[test]
    fn one_of_accepts_exact_member() {
        let r = Refinement::OneOf {
            options: vec![json!("low"), json!("medium"), json!("high")],
        };
        validate(&json!("low"), &r).unwrap();
        validate(&json!("high"), &r).unwrap();
    }

    #[test]
    fn one_of_rejects_non_member() {
        let r = Refinement::OneOf {
            options: vec![json!(1), json!(2), json!(3)],
        };
        let err = validate(&json!(4), &r).unwrap_err();
        assert!(err.contains("not one of the allowed options"));
    }

    #[test]
    fn one_of_matches_by_json_value_equality() {
        // Objects compare by field equality, arrays by element equality —
        // same rule as serde_json::Value::PartialEq.
        let r = Refinement::OneOf {
            options: vec![json!({ "status": "ok" }), json!({ "status": "err" })],
        };
        validate(&json!({ "status": "ok" }), &r).unwrap();
        validate(&json!({ "status": "unknown" }), &r).unwrap_err();
    }

    // ── NonEmpty ─────────────────────────────────────────────────

    #[test]
    fn non_empty_accepts_non_empty_string_array_object() {
        validate(&json!("a"), &Refinement::NonEmpty).unwrap();
        validate(&json!([1]), &Refinement::NonEmpty).unwrap();
        validate(&json!({ "k": 1 }), &Refinement::NonEmpty).unwrap();
    }

    #[test]
    fn non_empty_rejects_empty_string_array_object() {
        let r = Refinement::NonEmpty;
        assert!(validate(&json!(""), &r).is_err());
        assert!(validate(&json!([]), &r).is_err());
        assert!(validate(&json!({}), &r).is_err());
    }

    #[test]
    fn non_empty_rejects_non_measurable_value() {
        let r = Refinement::NonEmpty;
        let err = validate(&json!(42), &r).unwrap_err();
        assert!(err.contains("NonEmpty expects"));
    }

    // ── Serde + Display ──────────────────────────────────────────

    #[test]
    fn refinement_round_trips_through_json() {
        let variants = vec![
            Refinement::Range {
                min: Some(0.0),
                max: Some(100.0),
            },
            Refinement::Range {
                min: Some(0.0),
                max: None,
            },
            Refinement::OneOf {
                options: vec![json!("a"), json!(1)],
            },
            Refinement::NonEmpty,
        ];
        for r in variants {
            let json = serde_json::to_string(&r).unwrap();
            let back: Refinement = serde_json::from_str(&json).unwrap();
            assert_eq!(r, back);
        }
    }

    #[test]
    fn display_is_human_readable() {
        let r = Refinement::Range {
            min: Some(0.0),
            max: Some(100.0),
        };
        assert_eq!(r.to_string(), "Range(0..=100)");
        assert_eq!(Refinement::NonEmpty.to_string(), "NonEmpty");
    }
}
