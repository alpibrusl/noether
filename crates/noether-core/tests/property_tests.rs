use noether_core::effects::EffectSet;
use noether_core::stage::{
    compute_stage_id, sign_stage_id, verify_stage_signature, StageSignature,
};
use noether_core::types::{is_subtype_of, NType};
use proptest::prelude::*;
use std::collections::BTreeMap;

// --- Arbitrary NType generator ---

fn arb_ntype(depth: u32) -> impl Strategy<Value = NType> {
    let leaf = prop_oneof![
        Just(NType::Text),
        Just(NType::Number),
        Just(NType::Bool),
        Just(NType::Bytes),
        Just(NType::Null),
    ];

    leaf.prop_recursive(depth, 64, 4, |inner| {
        prop_oneof![
            // List<T>
            inner.clone().prop_map(|t| NType::List(Box::new(t))),
            // Stream<T>
            inner.clone().prop_map(|t| NType::Stream(Box::new(t))),
            // Map<K, V>
            (inner.clone(), inner.clone()).prop_map(|(k, v)| NType::Map {
                key: Box::new(k),
                value: Box::new(v),
            }),
            // Record with 1-4 fields
            prop::collection::btree_map("[a-z]{1,3}", inner.clone(), 1..=4).prop_map(NType::Record),
            // Union with 2-3 variants
            prop::collection::vec(inner.clone(), 2..=3).prop_map(NType::union),
        ]
    })
}

fn arb_ntype_with_any(depth: u32) -> impl Strategy<Value = NType> {
    let leaf = prop_oneof![
        Just(NType::Text),
        Just(NType::Number),
        Just(NType::Bool),
        Just(NType::Bytes),
        Just(NType::Null),
        Just(NType::Any),
    ];

    leaf.prop_recursive(depth, 64, 4, |inner| {
        prop_oneof![
            inner.clone().prop_map(|t| NType::List(Box::new(t))),
            inner.clone().prop_map(|t| NType::Stream(Box::new(t))),
            (inner.clone(), inner.clone()).prop_map(|(k, v)| NType::Map {
                key: Box::new(k),
                value: Box::new(v),
            }),
            prop::collection::btree_map("[a-z]{1,3}", inner.clone(), 1..=4).prop_map(NType::Record),
            prop::collection::vec(inner.clone(), 2..=3).prop_map(NType::union),
        ]
    })
}

// Helper to generate record with extra fields added to an existing record.
// Uses non-overlapping key ranges to ensure the wide record is a true superset.
fn arb_record_with_extra_fields() -> impl Strategy<Value = (NType, NType)> {
    prop::collection::btree_map("a[a-f]{1,2}", arb_ntype(2), 1..=3).prop_flat_map(|base| {
        prop::collection::btree_map("z[a-f]{1,2}", arb_ntype(2), 1..=3).prop_map(move |extra| {
            let narrow = NType::Record(base.clone());
            let mut wide_fields = base.clone();
            wide_fields.extend(extra);
            let wide = NType::Record(wide_fields);
            (wide, narrow)
        })
    })
}

// --- Property: Reflexivity ---
// is_subtype_of(T, T) == Compatible for all T

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn reflexivity(t in arb_ntype(3)) {
        prop_assert!(is_subtype_of(&t, &t).is_compatible(),
            "Type {} should be subtype of itself", t);
    }
}

// --- Property: Any is universal supertype ---
// is_subtype_of(T, Any) == Compatible for all T

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn any_is_universal_supertype(t in arb_ntype(3)) {
        prop_assert!(is_subtype_of(&t, &NType::Any).is_compatible(),
            "Type {} should be subtype of Any", t);
    }
}

// --- Property: Any is universal subtype ---
// is_subtype_of(Any, T) == Compatible for all T

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn any_is_universal_subtype(t in arb_ntype(3)) {
        prop_assert!(is_subtype_of(&NType::Any, &t).is_compatible(),
            "Any should be subtype of {}", t);
    }
}

// --- Property: Record width subtyping ---
// If R2 has all fields of R1 plus more, then R2 <: R1

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn record_width_subtyping((wide, narrow) in arb_record_with_extra_fields()) {
        prop_assert!(is_subtype_of(&wide, &narrow).is_compatible(),
            "Record with extra fields should be subtype of narrower record.\nWide: {}\nNarrow: {}",
            wide, narrow);
    }
}

// --- Property: Union absorption ---
// is_subtype_of(T, T | U) == Compatible

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn union_absorption(t in arb_ntype(2), u in arb_ntype(2)) {
        let union = NType::union(vec![t.clone(), u]);
        prop_assert!(is_subtype_of(&t, &union).is_compatible(),
            "Type {} should be subtype of union containing it: {}", t, union);
    }
}

// --- Property: Union strictness ---
// If T and U are incompatible primitives, then T|U is NOT subtype of U alone

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn union_not_subtype_of_single_variant(
        t in prop_oneof![Just(NType::Text), Just(NType::Number), Just(NType::Bool)],
        u in prop_oneof![Just(NType::Bytes), Just(NType::Null)],
    ) {
        let union = NType::union(vec![t, u.clone()]);
        prop_assert!(!is_subtype_of(&union, &u).is_compatible(),
            "Union {} should NOT be subtype of single variant {}", union, u);
    }
}

// --- Property: Serde round-trip ---
// serialize(T) -> deserialize -> T

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn serde_round_trip(t in arb_ntype_with_any(3)) {
        let json = serde_json::to_vec(&t).unwrap();
        let deserialized: NType = serde_json::from_slice(&json).unwrap();
        prop_assert_eq!(&t, &deserialized);
    }
}

// --- Property: Hash determinism ---
// compute_stage_id(sig) == compute_stage_id(sig)

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn hash_determinism(
        input in arb_ntype(2),
        output in arb_ntype(2),
        impl_hash in "[a-f0-9]{16}",
    ) {
        let sig = StageSignature {
            input,
            output,
            effects: EffectSet::pure(),
            implementation_hash: impl_hash,
        };
        let id1 = compute_stage_id("test", &sig).unwrap();
        let id2 = compute_stage_id("test", &sig).unwrap();
        prop_assert_eq!(id1, id2);
    }
}

// --- Property: Hash canonical JSON round-trip ---
// serialize -> deserialize -> re-serialize produces same bytes

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn hash_json_round_trip(
        input in arb_ntype(2),
        output in arb_ntype(2),
    ) {
        let sig = StageSignature {
            input,
            output,
            effects: EffectSet::unknown(),
            implementation_hash: "test".into(),
        };
        let json1 = serde_json::to_vec(&sig).unwrap();
        let deserialized: StageSignature = serde_json::from_slice(&json1).unwrap();
        let json2 = serde_json::to_vec(&deserialized).unwrap();
        prop_assert_eq!(json1, json2);
    }
}

// --- Property: Sign/verify round-trip ---

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]

    #[test]
    fn sign_verify_round_trip(id_str in "[a-f0-9]{64}") {
        use ed25519_dalek::SigningKey;
        use rand::rngs::OsRng;
        use noether_core::stage::StageId;

        let signing_key = SigningKey::generate(&mut OsRng);
        let stage_id = StageId(id_str);
        let signature = sign_stage_id(&stage_id, &signing_key);
        let public_key = hex::encode(signing_key.verifying_key().to_bytes());

        let valid = verify_stage_signature(&stage_id, &signature, &public_key).unwrap();
        prop_assert!(valid, "Signature should verify for stage {}", stage_id.0);
    }
}

// --- Property: Transitivity (sampled) ---
// If A <: B and B <: C, then A <: C

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn transitivity_sampled(
        a in arb_ntype(2),
        b in arb_ntype(2),
        c in arb_ntype(2),
    ) {
        if is_subtype_of(&a, &b).is_compatible() && is_subtype_of(&b, &c).is_compatible() {
            prop_assert!(is_subtype_of(&a, &c).is_compatible(),
                "Transitivity violated: {} <: {} and {} <: {} but {} is not <: {}",
                a, b, b, c, a, c);
        }
    }
}

// --- Property: List<Any> accepts any List ---

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn list_any_accepts_any_list(t in arb_ntype(2)) {
        let list_t = NType::List(Box::new(t));
        let list_any = NType::List(Box::new(NType::Any));
        prop_assert!(is_subtype_of(&list_t, &list_any).is_compatible());
    }
}

// --- Property: Empty record accepts any record ---

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn empty_record_accepts_any_record(
        fields in prop::collection::btree_map("[a-z]{1,3}", arb_ntype(2), 0..=4),
    ) {
        let record = NType::Record(fields);
        let empty = NType::Record(BTreeMap::new());
        prop_assert!(is_subtype_of(&record, &empty).is_compatible(),
            "Any record should be subtype of empty record, but {} wasn't", record);
    }
}
