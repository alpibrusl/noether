//! Generic (polymorphic) stdlib stages — M3 slice 3.
//!
//! These stages carry [`NType::Var`] in their signatures. The M3 slice 2b
//! changes to [`crate::types::unification`] and the engine's `check_graph`
//! mean that composing one of these with a concrete upstream (e.g.
//! `text_to_number >> identity`) type-checks end-to-end with the
//! concrete type flowing through — the resolved output is `Number`, not
//! `<T>`.
//!
//! All four are `Pure`. `head` is additionally `Fallible` — an empty
//! input list has no first element, so the runtime surfaces the error
//! rather than returning a surprising default.
//!
//! None of these are higher-order — `map` / `filter` stay in
//! `collections.rs` because they take a stage id as input; a generic
//! version needs proper higher-order type support which is a later
//! milestone.

use crate::effects::{Effect, EffectSet};
use crate::stage::property::Property;
use crate::stage::{Stage, StageBuilder};
use crate::types::NType;
use ed25519_dalek::SigningKey;
use serde_json::json;

pub fn stages(key: &SigningKey) -> Vec<Stage> {
    vec![
        // identity : <T> -> <T>
        // Trivial polymorphic stage. Useful as a test probe for the
        // type checker (does slice 2b's substitution threading actually
        // bind <T> at the edge?) and as a no-op placeholder in graphs
        // where a stage is expected but none is needed.
        StageBuilder::new("identity")
            .input(NType::var("T"))
            .output(NType::var("T"))
            .pure()
            .description("Return the input unchanged. Polymorphic: <T> -> <T>.")
            .example(json!("hello"), json!("hello"))
            .example(json!(42), json!(42))
            .example(json!(true), json!(true))
            .example(json!([1, 2, 3]), json!([1, 2, 3]))
            .example(json!({ "a": 1 }), json!({ "a": 1 }))
            .tag("generic")
            .tag("polymorphic")
            .tag("pure")
            .alias("id")
            .alias("pass_through")
            .alias("no_op")
            .build_stdlib(key)
            .unwrap(),
        // head : List<<T>> -> <T>
        // First element of a list. Empty list -> typed execution error;
        // that's the Fallible effect.
        StageBuilder::new("head")
            .input(NType::List(Box::new(NType::var("T"))))
            .output(NType::var("T"))
            .effects(EffectSet::new([Effect::Pure, Effect::Fallible]))
            .description("Return the first element of a list. Empty list is a Fallible error.")
            .example(json!([1, 2, 3]), json!(1))
            .example(json!(["a"]), json!("a"))
            .example(json!([true, false]), json!(true))
            .example(json!([[1, 2], [3, 4]]), json!([1, 2]))
            .example(json!([null, 1]), json!(null))
            .tag("generic")
            .tag("polymorphic")
            .tag("list")
            .tag("fallible")
            .tag("pure")
            .alias("first")
            .alias("car")
            .alias("list_head")
            .build_stdlib(key)
            .unwrap(),
        // tail : List<<T>> -> List<<T>>
        // All but the first element. Total: empty list -> empty list.
        StageBuilder::new("tail")
            .input(NType::List(Box::new(NType::var("T"))))
            .output(NType::List(Box::new(NType::var("T"))))
            .pure()
            .description(
                "Return every element of a list except the first. Empty list -> empty list.",
            )
            // Output length is always (input length - 1), clamped at 0.
            // `FieldLengthMax` pins "output no longer than input" — the
            // weaker half of that invariant, but enough to rule out an
            // implementation that invents elements.
            .property(Property::FieldLengthMax {
                subject_field: "output".into(),
                bound_field: "input".into(),
            })
            // Every element of the output came from the input (it's a
            // suffix of it). `SubsetOf` catches an implementation that
            // rewrites elements.
            .property(Property::SubsetOf {
                subject_field: "output".into(),
                super_field: "input".into(),
            })
            .example(json!([1, 2, 3]), json!([2, 3]))
            .example(json!(["a", "b"]), json!(["b"]))
            .example(json!([true]), json!([]))
            .example(json!([]), json!([]))
            .example(json!([1, 2, 3, 4, 5]), json!([2, 3, 4, 5]))
            .tag("generic")
            .tag("polymorphic")
            .tag("list")
            .tag("pure")
            .alias("rest")
            .alias("cdr")
            .alias("list_tail")
            .build_stdlib(key)
            .unwrap(),
        // (A separate polymorphic `list_length: List<<T>> -> Number` was
        // considered here. Skipped: the existing `list_length` in
        // `collections.rs` has signature `List<Any> -> Number`, which
        // already resolves a concrete upstream via the Any-is-compatible
        // rule, so an additional Var-shaped copy would be a name-clashing
        // duplicate without buying new semantics. The point of slice 3
        // is demonstrated by `identity` / `head` / `tail` — each of which
        // declares a Var the existing stdlib does not.)
    ]
}
