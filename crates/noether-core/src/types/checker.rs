use crate::types::NType;
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeCompatibility {
    Compatible,
    Incompatible(IncompatibilityReason),
}

impl TypeCompatibility {
    pub fn is_compatible(&self) -> bool {
        matches!(self, TypeCompatibility::Compatible)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncompatibilityReason {
    PrimitiveMismatch {
        expected: String,
        got: String,
    },
    MissingField {
        field: String,
    },
    FieldTypeMismatch {
        field: String,
        reason: Box<IncompatibilityReason>,
    },
    InnerTypeMismatch {
        context: String,
        reason: Box<IncompatibilityReason>,
    },
    UnionNotCovered {
        uncovered: String,
    },
    MapKeyMismatch {
        reason: Box<IncompatibilityReason>,
    },
    MapValueMismatch {
        reason: Box<IncompatibilityReason>,
    },
}

impl fmt::Display for IncompatibilityReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IncompatibilityReason::PrimitiveMismatch { expected, got } => {
                write!(f, "expected {expected}, got {got}")
            }
            IncompatibilityReason::MissingField { field } => {
                write!(f, "missing required field `{field}`")
            }
            IncompatibilityReason::FieldTypeMismatch { field, reason } => {
                write!(f, "field `{field}`: {reason}")
            }
            IncompatibilityReason::InnerTypeMismatch { context, reason } => {
                write!(f, "{context}: {reason}")
            }
            IncompatibilityReason::UnionNotCovered { uncovered } => {
                write!(f, "type {uncovered} is not covered by the expected union")
            }
            IncompatibilityReason::MapKeyMismatch { reason } => {
                write!(f, "map key type: {reason}")
            }
            IncompatibilityReason::MapValueMismatch { reason } => {
                write!(f, "map value type: {reason}")
            }
        }
    }
}

/// True when values of this type may legitimately be absent from a Record
/// value — i.e. the declared type admits `null`. Used by Record subtyping
/// to treat nullable fields as optional.
fn is_nullable(t: &NType) -> bool {
    match t {
        NType::Null | NType::Any => true,
        NType::Union(variants) => variants.iter().any(is_nullable),
        _ => false,
    }
}

/// Check if `sub` is a structural subtype of `sup`.
///
/// Returns `Compatible` if a value of type `sub` can safely be used where
/// `sup` is expected. This is the core check for composition operator `A >> B`:
/// `output(A)` must be a subtype of `input(B)`.
pub fn is_subtype_of(sub: &NType, sup: &NType) -> TypeCompatibility {
    use IncompatibilityReason::*;
    use NType::*;
    use TypeCompatibility::*;

    // Any absorbs everything in both directions
    if matches!(sup, Any) || matches!(sub, Any) {
        return Compatible;
    }

    match (sub, sup) {
        // Identical primitives
        (Text, Text)
        | (Number, Number)
        | (Bool, Bool)
        | (Bytes, Bytes)
        | (Null, Null)
        | (VNode, VNode) => Compatible,

        // List covariance
        (List(s), List(t)) => match is_subtype_of(s, t) {
            Compatible => Compatible,
            Incompatible(r) => Incompatible(InnerTypeMismatch {
                context: "List element".into(),
                reason: Box::new(r),
            }),
        },

        // Stream covariance
        (Stream(s), Stream(t)) => match is_subtype_of(s, t) {
            Compatible => Compatible,
            Incompatible(r) => Incompatible(InnerTypeMismatch {
                context: "Stream element".into(),
                reason: Box::new(r),
            }),
        },

        // Map covariance (both key and value)
        (Map { key: k1, value: v1 }, Map { key: k2, value: v2 }) => {
            if let Incompatible(r) = is_subtype_of(k1, k2) {
                return Incompatible(MapKeyMismatch {
                    reason: Box::new(r),
                });
            }
            match is_subtype_of(v1, v2) {
                Compatible => Compatible,
                Incompatible(r) => Incompatible(MapValueMismatch {
                    reason: Box::new(r),
                }),
            }
        }

        // Record: width + depth subtyping
        // Sub may have MORE fields than sup (width subtyping).
        // Each field in sup must exist in sub with a compatible type (depth).
        //
        // A field typed as nullable in sup (`T | Null`, `Null`, or `Any`) is
        // treated as optional: the value (sub) may omit it entirely, since
        // "absent" is equivalent to "null" for downstream consumers that
        // already have to handle null. This lets stage authors declare
        // config-like fields (`threshold_pct: Number | Null`) without
        // forcing every upstream stage to carry the field through its own
        // output schema.
        (Record(sub_fields), Record(sup_fields)) => {
            for (field_name, sup_type) in sup_fields {
                match sub_fields.get(field_name) {
                    None => {
                        if !is_nullable(sup_type) {
                            return Incompatible(MissingField {
                                field: field_name.clone(),
                            });
                        }
                    }
                    Some(sub_type) => {
                        if let Incompatible(r) = is_subtype_of(sub_type, sup_type) {
                            return Incompatible(FieldTypeMismatch {
                                field: field_name.clone(),
                                reason: Box::new(r),
                            });
                        }
                    }
                }
            }
            Compatible
        }

        // Union as subtype: ALL variants must fit the supertype
        (Union(variants), sup) => {
            for v in variants {
                if let Incompatible(_) = is_subtype_of(v, sup) {
                    return Incompatible(UnionNotCovered {
                        uncovered: format!("{v}"),
                    });
                }
            }
            Compatible
        }

        // Union as supertype: sub must fit at least one variant
        (sub, Union(variants)) => {
            for v in variants {
                if is_subtype_of(sub, v) == Compatible {
                    return Compatible;
                }
            }
            Incompatible(UnionNotCovered {
                uncovered: format!("{sub}"),
            })
        }

        // Everything else: mismatch
        (s, t) => Incompatible(PrimitiveMismatch {
            expected: format!("{t}"),
            got: format!("{s}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::NType;

    fn compatible(sub: &NType, sup: &NType) -> bool {
        is_subtype_of(sub, sup).is_compatible()
    }

    // --- Primitive tests ---

    #[test]
    fn identical_primitives_are_compatible() {
        for t in [
            NType::Text,
            NType::Number,
            NType::Bool,
            NType::Bytes,
            NType::Null,
        ] {
            assert!(compatible(&t, &t));
        }
    }

    #[test]
    fn different_primitives_are_incompatible() {
        assert!(!compatible(&NType::Text, &NType::Number));
        assert!(!compatible(&NType::Bool, &NType::Bytes));
        assert!(!compatible(&NType::Null, &NType::Text));
    }

    // --- Any tests ---

    #[test]
    fn any_is_universal_supertype() {
        assert!(compatible(&NType::Text, &NType::Any));
        assert!(compatible(
            &NType::List(Box::new(NType::Number)),
            &NType::Any
        ));
    }

    #[test]
    fn any_is_universal_subtype() {
        assert!(compatible(&NType::Any, &NType::Text));
        assert!(compatible(
            &NType::Any,
            &NType::List(Box::new(NType::Number))
        ));
    }

    // --- Record tests ---

    #[test]
    fn record_width_subtyping() {
        let wide = NType::record([
            ("name", NType::Text),
            ("age", NType::Number),
            ("email", NType::Text),
        ]);
        let narrow = NType::record([("name", NType::Text), ("age", NType::Number)]);
        assert!(compatible(&wide, &narrow));
        assert!(!compatible(&narrow, &wide));
    }

    #[test]
    fn empty_record_is_universal_supertype() {
        let empty = NType::record([] as [(&str, NType); 0]);
        let full = NType::record([("a", NType::Text), ("b", NType::Number)]);
        assert!(compatible(&full, &empty));
    }

    #[test]
    fn record_depth_subtyping() {
        let sub = NType::record([(
            "data",
            NType::record([("x", NType::Number), ("y", NType::Text)]),
        )]);
        let sup = NType::record([("data", NType::record([("x", NType::Number)]))]);
        assert!(compatible(&sub, &sup));
    }

    #[test]
    fn record_field_type_mismatch() {
        let r1 = NType::record([("name", NType::Text)]);
        let r2 = NType::record([("name", NType::Number)]);
        assert!(!compatible(&r1, &r2));
    }

    // --- List / Stream / Map tests ---

    #[test]
    fn list_covariance() {
        let l1 = NType::List(Box::new(NType::Text));
        let l2 = NType::List(Box::new(NType::Text));
        assert!(compatible(&l1, &l2));

        let l3 = NType::List(Box::new(NType::Number));
        assert!(!compatible(&l1, &l3));
    }

    #[test]
    fn stream_is_not_list() {
        let s = NType::Stream(Box::new(NType::Text));
        let l = NType::List(Box::new(NType::Text));
        assert!(!compatible(&s, &l));
        assert!(!compatible(&l, &s));
    }

    #[test]
    fn map_covariance() {
        let m1 = NType::Map {
            key: Box::new(NType::Text),
            value: Box::new(NType::Number),
        };
        let m2 = NType::Map {
            key: Box::new(NType::Text),
            value: Box::new(NType::Number),
        };
        assert!(compatible(&m1, &m2));
    }

    // --- Union tests ---

    #[test]
    fn type_is_subtype_of_union_containing_it() {
        let u = NType::union(vec![NType::Text, NType::Null]);
        assert!(compatible(&NType::Text, &u));
        assert!(compatible(&NType::Null, &u));
    }

    #[test]
    fn union_is_not_subtype_of_single_variant() {
        let u = NType::union(vec![NType::Text, NType::Null]);
        assert!(!compatible(&u, &NType::Text));
    }

    #[test]
    fn union_subtype_of_wider_union() {
        let narrow = NType::union(vec![NType::Text, NType::Null]);
        let wide = NType::union(vec![NType::Text, NType::Null, NType::Number]);
        assert!(compatible(&narrow, &wide));
        // Wider is NOT subtype of narrower (Number not covered)
        assert!(!compatible(&wide, &narrow));
    }

    // --- Error message tests ---

    #[test]
    fn missing_field_error_is_descriptive() {
        let sub = NType::record([("name", NType::Text)]);
        let sup = NType::record([("name", NType::Text), ("age", NType::Number)]);
        match is_subtype_of(&sub, &sup) {
            TypeCompatibility::Incompatible(reason) => {
                assert!(format!("{reason}").contains("age"));
            }
            TypeCompatibility::Compatible => panic!("expected incompatible"),
        }
    }

    // --- Optional-field tests (nullable = absent) ---

    #[test]
    fn nullable_field_may_be_absent_from_sub() {
        // Required field `foo: Number | Null` in sup: sub may omit it.
        let sup = NType::record([
            ("name", NType::Text),
            ("foo", NType::optional(NType::Number)),
        ]);
        let sub = NType::record([("name", NType::Text)]);
        assert!(compatible(&sub, &sup));
    }

    #[test]
    fn non_nullable_field_still_required() {
        // A non-nullable field omitted from sub is still an error.
        let sup = NType::record([("name", NType::Text), ("age", NType::Number)]);
        let sub = NType::record([("name", NType::Text)]);
        assert!(!compatible(&sub, &sup));
    }

    #[test]
    fn null_typed_field_is_optional() {
        // A field typed literally as `Null` counts as nullable.
        let sup = NType::record([("name", NType::Text), ("marker", NType::Null)]);
        let sub = NType::record([("name", NType::Text)]);
        assert!(compatible(&sub, &sup));
    }

    #[test]
    fn any_typed_field_is_optional() {
        // A field typed as `Any` counts as nullable (Any admits null).
        let sup = NType::record([("name", NType::Text), ("extra", NType::Any)]);
        let sub = NType::record([("name", NType::Text)]);
        assert!(compatible(&sub, &sup));
    }

    #[test]
    fn nullable_field_may_be_present_with_null_value() {
        // Present-with-null still works (unchanged behaviour).
        let sup = NType::record([("foo", NType::optional(NType::Number))]);
        let sub = NType::record([("foo", NType::Null)]);
        assert!(compatible(&sub, &sup));
    }

    #[test]
    fn nullable_field_may_be_present_with_t_value() {
        let sup = NType::record([("foo", NType::optional(NType::Number))]);
        let sub = NType::record([("foo", NType::Number)]);
        assert!(compatible(&sub, &sup));
    }

    #[test]
    fn vnode_is_subtype_of_vnode() {
        assert!(compatible(&NType::VNode, &NType::VNode));
    }

    #[test]
    fn vnode_is_subtype_of_any() {
        assert!(compatible(&NType::VNode, &NType::Any));
        assert!(compatible(&NType::Any, &NType::VNode));
    }

    #[test]
    fn vnode_is_not_compatible_with_other_types() {
        assert!(!compatible(&NType::VNode, &NType::Text));
        assert!(!compatible(
            &NType::VNode,
            &NType::Record(Default::default())
        ));
        assert!(!compatible(&NType::Text, &NType::VNode));
    }
}
