use crate::effects::{Effect, EffectSet};
use crate::stage::{Stage, StageBuilder};
use crate::types::NType;
use ed25519_dalek::SigningKey;
use serde_json::json;

pub fn stages(key: &SigningKey) -> Vec<Stage> {
    vec![
        StageBuilder::new("branch")
            .input(NType::record([
                ("condition", NType::Bool),
                ("if_true", NType::Any),
                ("if_false", NType::Any),
            ]))
            .output(NType::Any)
            .pure()
            .description("Select between two values based on a boolean condition")
            .example(
                json!({"condition": true, "if_true": "yes", "if_false": "no"}),
                json!("yes"),
            )
            .example(
                json!({"condition": false, "if_true": "yes", "if_false": "no"}),
                json!("no"),
            )
            .example(
                json!({"condition": true, "if_true": 1, "if_false": 2}),
                json!(1),
            )
            .example(
                json!({"condition": false, "if_true": null, "if_false": "default"}),
                json!("default"),
            )
            .example(
                json!({"condition": true, "if_true": [1, 2], "if_false": []}),
                json!([1, 2]),
            )
            .tag("control")
            .tag("conditional")
            .tag("pure")
            .alias("if_else")
            .alias("ternary")
            .alias("cond")
            .alias("conditional")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("retry")
            .input(NType::record([
                ("stage_id", NType::Text),
                ("input", NType::Any),
                ("max_attempts", NType::Number),
                ("delay_ms", NType::optional(NType::Number)),
            ]))
            .output(NType::Any)
            .effects(EffectSet::new([Effect::Fallible]))
            .description(
                "Retry a fallible stage up to N times with optional delay between attempts",
            )
            .example(
                json!({"stage_id": "abc", "input": "data", "max_attempts": 3, "delay_ms": 100}),
                json!("result"),
            )
            .example(
                json!({"stage_id": "abc", "input": 42, "max_attempts": 1, "delay_ms": null}),
                json!(42),
            )
            .example(
                json!({"stage_id": "def", "input": null, "max_attempts": 5, "delay_ms": 500}),
                json!("ok"),
            )
            .example(
                json!({"stage_id": "ghi", "input": "test", "max_attempts": 2, "delay_ms": 0}),
                json!("test"),
            )
            .example(
                json!({"stage_id": "jkl", "input": [1], "max_attempts": 3, "delay_ms": null}),
                json!([1]),
            )
            .tag("control")
            .tag("resilience")
            .tag("error-handling")
            .alias("retry_on_failure")
            .alias("with_retries")
            .alias("backoff")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("fallback")
            .input(NType::record([
                ("stages", NType::List(Box::new(NType::Text))),
                ("input", NType::Any),
            ]))
            .output(NType::Any)
            .effects(EffectSet::new([Effect::Fallible]))
            .description("Try stages in order until one succeeds; fails if all fail")
            .example(
                json!({"stages": ["primary", "secondary"], "input": "data"}),
                json!("result"),
            )
            .example(json!({"stages": ["a", "b", "c"], "input": 42}), json!(42))
            .example(json!({"stages": ["fast"], "input": "x"}), json!("x"))
            .example(
                json!({"stages": ["s1", "s2"], "input": null}),
                json!("from_s2"),
            )
            .example(
                json!({"stages": ["main", "backup"], "input": [1, 2]}),
                json!([1, 2]),
            )
            .tag("control")
            .tag("resilience")
            .tag("error-handling")
            .alias("try_catch")
            .alias("with_fallback")
            .alias("or_else")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("timeout")
            .input(NType::record([
                ("stage_id", NType::Text),
                ("input", NType::Any),
                ("timeout_ms", NType::Number),
            ]))
            .output(NType::Any)
            .effects(EffectSet::new([Effect::Fallible]))
            .description("Run a stage with a deadline; fails if the stage exceeds the timeout")
            .example(
                json!({"stage_id": "fast", "input": "data", "timeout_ms": 5000}),
                json!("result"),
            )
            .example(
                json!({"stage_id": "slow", "input": 42, "timeout_ms": 100}),
                json!(42),
            )
            .example(
                json!({"stage_id": "s1", "input": null, "timeout_ms": 1000}),
                json!(null),
            )
            .example(
                json!({"stage_id": "s2", "input": "test", "timeout_ms": 10000}),
                json!("done"),
            )
            .example(
                json!({"stage_id": "s3", "input": [1], "timeout_ms": 500}),
                json!([1]),
            )
            .tag("control")
            .tag("resilience")
            .tag("deadline")
            .alias("with_timeout")
            .alias("deadline")
            .alias("max_duration")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("race")
            .input(NType::record([
                ("stages", NType::List(Box::new(NType::Text))),
                ("input", NType::Any),
            ]))
            .output(NType::Any)
            .effects(EffectSet::new([Effect::Fallible, Effect::NonDeterministic]))
            .description("Run multiple stages concurrently; return the first to complete")
            .example(
                json!({"stages": ["fast", "slow"], "input": "data"}),
                json!("fast_result"),
            )
            .example(json!({"stages": ["a", "b", "c"], "input": 42}), json!(42))
            .example(json!({"stages": ["s1"], "input": "x"}), json!("x"))
            .example(
                json!({"stages": ["p1", "p2"], "input": null}),
                json!("winner"),
            )
            .example(json!({"stages": ["r1", "r2"], "input": [1]}), json!([1]))
            .tag("control")
            .tag("concurrent")
            .tag("parallel")
            .alias("first_success")
            .alias("any_of")
            .alias("fastest")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("parallel")
            .input(NType::record([
                ("stages", NType::List(Box::new(NType::Text))),
                ("inputs", NType::List(Box::new(NType::Any))),
            ]))
            .output(NType::List(Box::new(NType::Any)))
            .effects(EffectSet::new([Effect::Fallible]))
            .description("Run N stages concurrently on N inputs; collect all results")
            .example(
                json!({"stages": ["s1", "s2"], "inputs": ["a", "b"]}),
                json!(["r1", "r2"]),
            )
            .example(json!({"stages": ["s1"], "inputs": [42]}), json!([42]))
            .example(json!({"stages": [], "inputs": []}), json!([]))
            .example(
                json!({"stages": ["a", "b", "c"], "inputs": [1, 2, 3]}),
                json!([1, 2, 3]),
            )
            .example(
                json!({"stages": ["x", "y"], "inputs": [null, null]}),
                json!([null, null]),
            )
            .tag("control")
            .tag("concurrent")
            .tag("parallel")
            .alias("concurrent_map")
            .alias("run_all")
            .alias("fan_out")
            .build_stdlib(key)
            .unwrap(),
    ]
}
