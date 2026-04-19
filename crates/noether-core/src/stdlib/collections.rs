use crate::effects::{Effect, EffectSet};
use crate::stage::property::Property;
use crate::stage::{Stage, StageBuilder};
use crate::types::NType;
use ed25519_dalek::SigningKey;
use serde_json::json;

pub fn stages(key: &SigningKey) -> Vec<Stage> {
    vec![
        StageBuilder::new("map")
            .input(NType::record([
                ("items", NType::List(Box::new(NType::Any))),
                ("stage_id", NType::Text),
            ]))
            .output(NType::List(Box::new(NType::Any)))
            .pure()
            .description("Apply a stage to each element of a list")
            .example(json!({"items": [1, 2, 3], "stage_id": "abc123"}), json!([2, 4, 6]))
            .example(json!({"items": ["a", "b"], "stage_id": "def456"}), json!(["A", "B"]))
            .example(json!({"items": [], "stage_id": "abc123"}), json!([]))
            .example(json!({"items": [true], "stage_id": "ghi789"}), json!([false]))
            .example(json!({"items": [1], "stage_id": "abc123"}), json!([2]))
            .tag("collections").tag("list").tag("functional").tag("pure")
            .alias("list_map").alias("array_map").alias("transform")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("filter")
            .input(NType::record([
                ("items", NType::List(Box::new(NType::Any))),
                ("stage_id", NType::Text),
            ]))
            .output(NType::List(Box::new(NType::Any)))
            .pure()
            .description("Keep only elements where the predicate stage returns true")
            .example(json!({"items": [1, 2, 3, 4], "stage_id": "is_even"}), json!([2, 4]))
            .example(json!({"items": ["a", "bb", "ccc"], "stage_id": "len_gt_1"}), json!(["bb", "ccc"]))
            .example(json!({"items": [], "stage_id": "any"}), json!([]))
            .example(json!({"items": [1, 2, 3], "stage_id": "always_true"}), json!([1, 2, 3]))
            .example(json!({"items": [1, 2, 3], "stage_id": "always_false"}), json!([]))
            .tag("collections").tag("list").tag("functional").tag("pure")
            .alias("array_filter").alias("list_filter").alias("where").alias("select")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("reduce")
            .input(NType::record([
                ("items", NType::List(Box::new(NType::Any))),
                ("stage_id", NType::Text),
                ("initial", NType::Any),
            ]))
            .output(NType::Any)
            .pure()
            .description("Reduce a list to a single value by applying a stage to accumulator and each element")
            .example(json!({"items": [1, 2, 3], "stage_id": "sum", "initial": 0}), json!(6))
            .example(json!({"items": ["a", "b", "c"], "stage_id": "concat", "initial": ""}), json!("abc"))
            .example(json!({"items": [], "stage_id": "sum", "initial": 0}), json!(0))
            .example(json!({"items": [5], "stage_id": "sum", "initial": 10}), json!(15))
            .example(json!({"items": [1, 2], "stage_id": "multiply", "initial": 1}), json!(2))
            .tag("collections").tag("list").tag("functional").tag("pure")
            .alias("fold").alias("aggregate").alias("foldl")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("sort")
            .input(NType::union(vec![
                NType::List(Box::new(NType::Any)),
                NType::record([
                    ("items", NType::List(Box::new(NType::Any))),
                    ("key", NType::optional(NType::Text)),
                    ("descending", NType::optional(NType::Bool)),
                ]),
            ]))
            .output(NType::List(Box::new(NType::Any)))
            .pure()
            .description("Sort a list; optionally by a field name and/or in descending order")
            .example(json!({"items": [3, 1, 2], "key": null, "descending": null}), json!([1, 2, 3]))
            .example(json!({"items": [3, 1, 2], "key": null, "descending": true}), json!([3, 2, 1]))
            .example(json!({"items": ["b", "a", "c"], "key": null, "descending": null}), json!(["a", "b", "c"]))
            .example(json!({"items": [], "key": null, "descending": null}), json!([]))
            .example(json!([3, 1, 2]), json!([1, 2, 3]))
            .tag("collections").tag("list").tag("pure")
            .alias("order_by").alias("array_sort").alias("list_sort")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("group_by")
            .input(NType::record([
                ("items", NType::List(Box::new(NType::Any))),
                ("key", NType::Text),
            ]))
            .output(NType::Map {
                key: Box::new(NType::Text),
                value: Box::new(NType::List(Box::new(NType::Any))),
            })
            .pure()
            .description("Group list items by the value of a named field")
            .example(
                json!({"items": [{"type": "a", "v": 1}, {"type": "b", "v": 2}, {"type": "a", "v": 3}], "key": "type"}),
                json!({"a": [{"type": "a", "v": 1}, {"type": "a", "v": 3}], "b": [{"type": "b", "v": 2}]}),
            )
            .example(json!({"items": [], "key": "x"}), json!({}))
            .example(
                json!({"items": [{"k": "x"}, {"k": "x"}], "key": "k"}),
                json!({"x": [{"k": "x"}, {"k": "x"}]}),
            )
            .example(
                json!({"items": [{"c": "a"}, {"c": "b"}, {"c": "c"}], "key": "c"}),
                json!({"a": [{"c": "a"}], "b": [{"c": "b"}], "c": [{"c": "c"}]}),
            )
            .example(
                json!({"items": [{"g": "1", "v": "a"}, {"g": "1", "v": "b"}], "key": "g"}),
                json!({"1": [{"g": "1", "v": "a"}, {"g": "1", "v": "b"}]}),
            )
            .tag("collections").tag("list").tag("pure")
            .alias("groupby").alias("partition").alias("bucket")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("flatten")
            .input(NType::List(Box::new(NType::List(Box::new(NType::Any)))))
            .output(NType::List(Box::new(NType::Any)))
            .pure()
            .description("Flatten a list of lists into a single list")
            .example(json!([[1, 2], [3, 4]]), json!([1, 2, 3, 4]))
            .example(json!([["a"], ["b", "c"]]), json!(["a", "b", "c"]))
            .example(json!([[], [1]]), json!([1]))
            .example(json!([[], []]), json!([]))
            .example(json!([[1]]), json!([1]))
            .tag("collections").tag("list").tag("pure")
            .alias("flat").alias("flat_map").alias("concat_lists")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("zip")
            .input(NType::record([
                ("left", NType::List(Box::new(NType::Any))),
                ("right", NType::List(Box::new(NType::Any))),
            ]))
            .output(NType::List(Box::new(NType::record([
                ("left", NType::Any),
                ("right", NType::Any),
            ]))))
            .pure()
            .description("Combine two lists into a list of pairs, truncating to the shorter list")
            .example(json!({"left": [1, 2, 3], "right": ["a", "b", "c"]}), json!([{"left": 1, "right": "a"}, {"left": 2, "right": "b"}, {"left": 3, "right": "c"}]))
            .example(json!({"left": [1, 2], "right": ["a"]}), json!([{"left": 1, "right": "a"}]))
            .example(json!({"left": [], "right": []}), json!([]))
            .example(json!({"left": [1], "right": [2]}), json!([{"left": 1, "right": 2}]))
            .example(json!({"left": ["x", "y"], "right": [true, false]}), json!([{"left": "x", "right": true}, {"left": "y", "right": false}]))
            .tag("collections").tag("list").tag("pure")
            .alias("zip_lists").alias("pair").alias("combine_lists")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("take")
            .input(NType::record([
                ("items", NType::List(Box::new(NType::Any))),
                ("count", NType::Number),
            ]))
            .output(NType::List(Box::new(NType::Any)))
            .pure()
            .description("Take the first N elements from a list")
            .example(json!({"items": [1, 2, 3, 4, 5], "count": 3}), json!([1, 2, 3]))
            .example(json!({"items": [1, 2], "count": 5}), json!([1, 2]))
            .example(json!({"items": [], "count": 3}), json!([]))
            .example(json!({"items": ["a", "b", "c"], "count": 0}), json!([]))
            .example(json!({"items": [1], "count": 1}), json!([1]))
            .tag("collections").tag("list").tag("pure")
            .alias("head").alias("limit").alias("first_n").alias("slice")
            .build_stdlib(key)
            .unwrap(),
        // ── Numeric aggregation stages ─────────────────────────────────────────
        StageBuilder::new("num_sum")
            .input(NType::List(Box::new(NType::Number)))
            .output(NType::Number)
            .pure()
            .description("Sum all numbers in a list")
            .example(json!([1.0, 2.0, 3.0]), json!(6.0))
            .example(json!([10.0, -5.0, 3.0]), json!(8.0))
            .example(json!([]), json!(0.0))
            .example(json!([42.0]), json!(42.0))
            .example(json!([0.1, 0.2, 0.3]), json!(0.6000000000000001))
            .tag("collections").tag("math").tag("aggregation").tag("pure")
            .alias("sum").alias("total").alias("add_all")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("num_avg")
            .input(NType::List(Box::new(NType::Number)))
            .output(NType::Number)
            .effects(EffectSet::new([Effect::Pure, Effect::Fallible]))
            .description("Compute the arithmetic mean of a list of numbers; fails on empty list")
            .example(json!([1.0, 2.0, 3.0]), json!(2.0))
            .example(json!([10.0, 20.0]), json!(15.0))
            .example(json!([5.0]), json!(5.0))
            .example(json!([0.0, 0.0, 0.0]), json!(0.0))
            .example(json!([-1.0, 1.0]), json!(0.0))
            .tag("collections").tag("math").tag("aggregation").tag("pure")
            .alias("mean").alias("average").alias("arithmetic_mean")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("num_min")
            .input(NType::List(Box::new(NType::Number)))
            .output(NType::Number)
            .effects(EffectSet::new([Effect::Pure, Effect::Fallible]))
            .description("Return the minimum value in a list of numbers; fails on empty list")
            .example(json!([3.0, 1.0, 4.0, 1.0, 5.0]), json!(1.0))
            .example(json!([42.0]), json!(42.0))
            .example(json!([-5.0, 0.0, 5.0]), json!(-5.0))
            .example(json!([1.0, 1.0, 1.0]), json!(1.0))
            .example(json!([100.0, 0.001]), json!(0.001))
            .tag("collections").tag("math").tag("aggregation").tag("pure")
            .alias("minimum").alias("min_value")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("num_max")
            .input(NType::List(Box::new(NType::Number)))
            .output(NType::Number)
            .effects(EffectSet::new([Effect::Pure, Effect::Fallible]))
            .description("Return the maximum value in a list of numbers; fails on empty list")
            .example(json!([3.0, 1.0, 4.0, 1.0, 5.0]), json!(5.0))
            .example(json!([42.0]), json!(42.0))
            .example(json!([-5.0, 0.0, 5.0]), json!(5.0))
            .example(json!([1.0, 1.0, 1.0]), json!(1.0))
            .example(json!([0.001, 100.0]), json!(100.0))
            .tag("collections").tag("math").tag("aggregation").tag("pure")
            .alias("maximum").alias("max_value")
            .build_stdlib(key)
            .unwrap(),
        // ── List utility stages ────────────────────────────────────────────────
        StageBuilder::new("list_dedup")
            .input(NType::List(Box::new(NType::Any)))
            .output(NType::List(Box::new(NType::Any)))
            .pure()
            .description("Remove duplicate values from a list, preserving first-occurrence order")
            .example(json!([1.0, 2.0, 1.0, 3.0, 2.0]), json!([1.0, 2.0, 3.0]))
            .example(json!(["a", "b", "a", "c"]), json!(["a", "b", "c"]))
            .example(json!([1.0, 1.0, 1.0]), json!([1.0]))
            .example(json!([]), json!([]))
            .example(json!([1.0, 2.0, 3.0]), json!([1.0, 2.0, 3.0]))
            .tag("collections").tag("list").tag("pure")
            .alias("unique").alias("deduplicate").alias("distinct").alias("uniq")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("list_length")
            .input(NType::List(Box::new(NType::Any)))
            .output(NType::Number)
            .pure()
            .description("Return the number of elements in a list")
            .example(json!([1.0, 2.0, 3.0]), json!(3.0))
            .example(json!([]), json!(0.0))
            .example(json!(["a"]), json!(1.0))
            .example(json!([1.0, 2.0, 3.0, 4.0, 5.0]), json!(5.0))
            .example(json!([null, false, 0.0]), json!(3.0))
            .tag("collections").tag("list").tag("pure")
            .alias("count").alias("size").alias("array_length").alias("len")
            .property(Property::Range {
                field: "output".into(),
                min: Some(0.0),
                max: None,
            })
            .build_stdlib(key)
            .unwrap(),
    ]
}
