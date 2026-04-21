use crate::types::refinement::Refinement;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The core type in Noether's structural type system.
///
/// Types are structural, not nominal: two types are compatible if their
/// structure matches, regardless of how they were named or created.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value")]
pub enum NType {
    // Primitives (ordered by discriminant for stable Ord)
    Any,
    Bool,
    Bytes,
    List(Box<NType>),
    Map {
        key: Box<NType>,
        value: Box<NType>,
    },
    Null,
    Number,
    Record(BTreeMap<String, NType>),
    Stream(Box<NType>),
    Text,
    Union(Vec<NType>),
    /// A virtual DOM node — the output type for UI component stages.
    ///
    /// VNode is opaque in the type system: it does not expose its internal
    /// tag/props/children structure as sub-types. The JS reactive runtime owns
    /// VNode semantics; the type checker only needs to know a VNode is a VNode.
    VNode,
    /// A type variable for parametric polymorphism (M3 slice 2).
    ///
    /// `Var("T")` stands for an unknown type that unification will pin down
    /// to a concrete `NType` at graph-check time. A `Var` is **compatible
    /// with anything** in [`is_subtype_of`](crate::types::is_subtype_of) —
    /// the graph-edge checker treats "has a Var" as "call unification, the
    /// concrete shape will drop out of that pass". Example / JSON-shape
    /// inference treats an unbound `Var` as `Any`.
    ///
    /// Placed at the end of the enum so the discriminant ordering of every
    /// pre-existing variant is preserved — the on-wire form of every stage
    /// in the registry stays byte-identical when no `Var` is used.
    Var(String),
    /// A record with **known fields plus a row-variable tail** (M3 row
    /// polymorphism).
    ///
    /// `RecordWith { fields: { a: Text }, rest: "R" }` reads as "has at
    /// least field `a` of type Text; whatever other fields exist are
    /// captured by row variable `R`." When composed against a concrete
    /// upstream like `Record { a: Text, b: Number, c: Bool }`, unification
    /// binds `R` to `Record { b: Number, c: Bool }`. Downstream stages
    /// declaring `RecordWith { fields: { timestamp: Number }, rest: "R" }`
    /// then output `Record { timestamp: Number, b: Number, c: Bool }` —
    /// the extra fields flow through rather than getting silently dropped.
    ///
    /// # Subtyping
    ///
    /// - `Record(f_sub) <: RecordWith { f_sup, _ }` iff `f_sub` contains
    ///   every key of `f_sup` with subtype values. The row variable is
    ///   a promise that the remaining fields are *captured*; at subtype
    ///   level we accept them without constraint.
    /// - `RecordWith { f_sub, r } <: Record(f_sup)` is a harder check
    ///   (sub has unknown extra fields; sup is closed). Treated as
    ///   `Incompatible` for now — an open record can't silently narrow
    ///   to a closed one without the row variable being provably empty.
    /// - `RecordWith ~ RecordWith` is deferred for a follow-up; common
    ///   agent-composed graphs hit the `Record ~ RecordWith` case.
    ///
    /// # Serialization
    ///
    /// The tagged enum form is
    /// `{"kind": "RecordWith", "value": { "fields": {...}, "rest": "R" }}`
    /// — same outer shape as every other variant. Placed last in the
    /// enum definition so existing stages stay bit-identical on the wire.
    RecordWith {
        fields: BTreeMap<String, NType>,
        rest: String,
    },
    /// A base type with a runtime-checkable predicate attached
    /// (M3 refinement types). `Refined { base: Number, refinement:
    /// Range { min: 0, max: 100 } }` reads as "a Number between 0
    /// and 100 inclusive."
    ///
    /// # Subtyping
    ///
    /// - `Refined { base: T, _ } <: U` iff `T <: U` — dropping a
    ///   refinement is safe (the value is still a T).
    /// - `U <: Refined { base: T, _ }` is **not** automatic — we
    ///   can't prove every T satisfies the predicate without
    ///   value-level reasoning. Current behaviour: compatible only
    ///   when the sub is literally a `Refined` with the same
    ///   refinement and a compatible base.
    ///
    /// # Runtime enforcement
    ///
    /// [`crate::types::refinement::validate`] is the validator
    /// callers invoke to check a `serde_json::Value` against the
    /// refinement. Wiring it into the executor at stage boundaries
    /// is a follow-up — for now, refinements shape the type system
    /// and surface at `stage verify` time but are not auto-enforced
    /// at graph-execute time.
    ///
    /// Placed at the end of the enum so the discriminant ordering
    /// of every pre-existing variant is preserved.
    Refined {
        base: Box<NType>,
        refinement: Refinement,
    },
}

impl NType {
    /// Create a normalized union type.
    ///
    /// Flattens nested unions, deduplicates, and sorts variants.
    /// Returns the inner type if only one variant remains.
    pub fn union(variants: Vec<NType>) -> NType {
        let mut flat = Vec::new();
        for v in variants {
            match v {
                NType::Union(inner) => flat.extend(inner),
                other => flat.push(other),
            }
        }
        flat.sort();
        flat.dedup();
        match flat.len() {
            0 => NType::Null,
            1 => flat.into_iter().next().unwrap(),
            _ => NType::Union(flat),
        }
    }

    /// Convenience for optional types: `T | Null`.
    pub fn optional(inner: NType) -> NType {
        NType::union(vec![inner, NType::Null])
    }

    /// Create a Record from field pairs.
    pub fn record(fields: impl IntoIterator<Item = (impl Into<String>, NType)>) -> NType {
        NType::Record(fields.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// Convenience constructor for a type variable.
    pub fn var(name: impl Into<String>) -> NType {
        NType::Var(name.into())
    }

    /// Convenience constructor for an open record with a row variable.
    pub fn record_with(
        fields: impl IntoIterator<Item = (impl Into<String>, NType)>,
        rest: impl Into<String>,
    ) -> NType {
        NType::RecordWith {
            fields: fields.into_iter().map(|(k, v)| (k.into(), v)).collect(),
            rest: rest.into(),
        }
    }

    /// Convenience constructor for a refined type.
    pub fn refined(base: NType, refinement: Refinement) -> NType {
        NType::Refined {
            base: Box::new(base),
            refinement,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_flattens_nested() {
        let inner = NType::Union(vec![NType::Text, NType::Number]);
        let outer = NType::union(vec![inner, NType::Bool]);
        assert_eq!(
            outer,
            NType::Union(vec![NType::Bool, NType::Number, NType::Text])
        );
    }

    #[test]
    fn union_deduplicates() {
        let u = NType::union(vec![NType::Text, NType::Text, NType::Number]);
        assert_eq!(u, NType::Union(vec![NType::Number, NType::Text]));
    }

    #[test]
    fn union_single_variant_unwraps() {
        let u = NType::union(vec![NType::Text]);
        assert_eq!(u, NType::Text);
    }

    #[test]
    fn union_empty_becomes_null() {
        let u = NType::union(vec![]);
        assert_eq!(u, NType::Null);
    }

    #[test]
    fn union_is_sorted() {
        let u = NType::union(vec![NType::Text, NType::Bool, NType::Number]);
        assert_eq!(
            u,
            NType::Union(vec![NType::Bool, NType::Number, NType::Text])
        );
    }

    #[test]
    fn optional_creates_union_with_null() {
        let opt = NType::optional(NType::Text);
        assert_eq!(opt, NType::Union(vec![NType::Null, NType::Text]));
    }

    #[test]
    fn serde_round_trip() {
        let types = vec![
            NType::Text,
            NType::Number,
            NType::List(Box::new(NType::Text)),
            NType::Map {
                key: Box::new(NType::Text),
                value: Box::new(NType::Number),
            },
            NType::record([("name", NType::Text), ("age", NType::Number)]),
            NType::union(vec![NType::Text, NType::Null]),
            NType::Stream(Box::new(NType::Bool)),
            NType::Any,
            NType::VNode,
            NType::Var("T".into()),
        ];
        for t in types {
            let json = serde_json::to_string(&t).unwrap();
            let deserialized: NType = serde_json::from_str(&json).unwrap();
            assert_eq!(t, deserialized);
        }
    }

    #[test]
    fn vnode_ord_after_union() {
        // VNode sorts after Union alphabetically, which keeps Ord stable.
        assert!(NType::VNode > NType::Union(vec![NType::Text]));
    }

    #[test]
    fn var_ord_is_deterministic() {
        // Var is the newest variant and sorts after every pre-existing one
        // (it's last in the enum definition, so it has the highest discriminant).
        // This keeps the ordering of every already-stored signature stable.
        assert!(NType::Var("T".into()) > NType::VNode);
        assert!(NType::Var("T".into()) > NType::Text);
        assert!(NType::Var("A".into()) < NType::Var("B".into()));
    }

    #[test]
    fn var_serde_shape_is_tagged() {
        // Wire-format check: Var serialises as a tagged object so older
        // readers encounter a recognisable shape rather than a bare string.
        let t = NType::Var("T".into());
        let json = serde_json::to_value(&t).unwrap();
        assert_eq!(json, serde_json::json!({ "kind": "Var", "value": "T" }));
    }

    #[test]
    fn record_with_round_trips_through_json() {
        // M3 row polymorphism: the RecordWith variant must round-trip
        // cleanly via the existing `#[serde(tag = "kind", content = "value")]`
        // shape so non-Rust consumers see the same schema form.
        let t = NType::record_with([("id", NType::Text), ("age", NType::Number)], "R");
        let json = serde_json::to_string(&t).unwrap();
        let back: NType = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
        assert!(json.contains("\"kind\":\"RecordWith\""));
        assert!(json.contains("\"rest\":\"R\""));
    }

    #[test]
    fn record_with_ord_is_deterministic() {
        // RecordWith is the newest variant (after Var) and has the
        // highest discriminant — so every pre-existing stage's on-wire
        // ordering stays stable.
        let rw = NType::record_with([("a", NType::Text)], "R");
        assert!(rw > NType::Var("T".into()));
        assert!(rw > NType::Text);
    }

    // ── Refinement types (M3 refinement slice) ─────────────────────

    #[test]
    fn refined_round_trips_through_json() {
        // Refinement variants all round-trip via `#[serde(tag = "kind")]`.
        let t = NType::refined(
            NType::Number,
            Refinement::Range {
                min: Some(0.0),
                max: Some(100.0),
            },
        );
        let json = serde_json::to_string(&t).unwrap();
        let back: NType = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
    }

    #[test]
    fn refined_ord_is_deterministic() {
        // Refined is the newest variant (after RecordWith) so its
        // discriminant sorts last. Pre-existing variants keep their
        // slots.
        let refined = NType::refined(NType::Number, Refinement::NonEmpty);
        assert!(refined > NType::record_with(Vec::<(String, NType)>::new(), "R"));
        assert!(refined > NType::Var("T".into()));
    }
}
