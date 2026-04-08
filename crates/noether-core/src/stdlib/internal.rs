use crate::effects::{Effect, EffectSet};
use crate::stage::{Stage, StageBuilder};
use crate::types::NType;
use ed25519_dalek::SigningKey;
use serde_json::json;

pub fn stages(key: &SigningKey) -> Vec<Stage> {
    vec![
        StageBuilder::new("store_search")
            .input(NType::record([
                ("query", NType::Text),
                ("limit", NType::optional(NType::Number)),
            ]))
            .output(NType::List(Box::new(NType::record([
                ("id", NType::Text),
                ("description", NType::Text),
                ("input", NType::Text),
                ("output", NType::Text),
                ("score", NType::Number),
            ]))))
            .effects(EffectSet::new([Effect::Pure, Effect::Fallible]))
            .description("Search the stage store by semantic query")
            .example(json!({"query": "convert text to number", "limit": 5}), json!([{"id": "abc123", "description": "Parse a value as a number", "input": "Text", "output": "Number", "score": 0.95}]))
            .example(json!({"query": "http request", "limit": null}), json!([{"id": "def456", "description": "Make an HTTP GET request", "input": "Record", "output": "Record", "score": 0.88}]))
            .example(json!({"query": "sort list", "limit": 3}), json!([{"id": "ghi789", "description": "Sort a list", "input": "Record", "output": "List", "score": 0.92}]))
            .example(json!({"query": "no matches", "limit": 10}), json!([]))
            .example(json!({"query": "text processing", "limit": null}), json!([{"id": "jkl012", "description": "Split text", "input": "Record", "output": "List", "score": 0.85}]))
            .tag("internal").tag("meta").tag("search")
            .alias("search_stages").alias("find_stage").alias("stage_search")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("store_add")
            .input(NType::record([("spec", NType::Any)]))
            .output(NType::record([
                ("id", NType::Text),
                ("lifecycle", NType::Text),
            ]))
            .effects(EffectSet::new([Effect::Fallible]))
            .description("Register a new stage in the store")
            .example(json!({"spec": {"input": "Text", "output": "Number"}}), json!({"id": "abc123def456", "lifecycle": "draft"}))
            .example(json!({"spec": {"input": "Any", "output": "Text"}}), json!({"id": "789ghi012jkl", "lifecycle": "draft"}))
            .example(json!({"spec": {"input": "List", "output": "List"}}), json!({"id": "mno345pqr678", "lifecycle": "draft"}))
            .example(json!({"spec": {"input": "Record", "output": "Bool"}}), json!({"id": "stu901vwx234", "lifecycle": "draft"}))
            .example(json!({"spec": {"input": "Bytes", "output": "Text"}}), json!({"id": "yza567bcd890", "lifecycle": "draft"}))
            .tag("internal").tag("meta").tag("registry")
            .alias("add_stage").alias("register_stage").alias("publish_stage")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("composition_verify")
            .input(NType::record([
                ("stages", NType::List(Box::new(NType::Text))),
                ("operators", NType::List(Box::new(NType::Text))),
            ]))
            .output(NType::record([
                ("valid", NType::Bool),
                ("errors", NType::List(Box::new(NType::Text))),
                ("warnings", NType::List(Box::new(NType::Text))),
            ]))
            .pure()
            .description("Verify that a composition graph type-checks correctly")
            .example(json!({"stages": ["s1", "s2"], "operators": ["sequential"]}), json!({"valid": true, "errors": [], "warnings": []}))
            .example(json!({"stages": ["s1", "s2"], "operators": ["sequential"]}), json!({"valid": false, "errors": ["type mismatch at edge s1->s2"], "warnings": []}))
            .example(json!({"stages": ["s1"], "operators": []}), json!({"valid": true, "errors": [], "warnings": []}))
            .example(json!({"stages": ["s1", "s2", "s3"], "operators": ["sequential", "sequential"]}), json!({"valid": true, "errors": [], "warnings": ["s2 is deprecated"]}))
            .example(json!({"stages": [], "operators": []}), json!({"valid": true, "errors": [], "warnings": ["empty composition"]}))
            .tag("internal").tag("meta").tag("verification").tag("pure")
            .alias("verify_graph").alias("check_composition").alias("type_check_graph")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("trace_read")
            .input(NType::record([("composition_id", NType::Text)]))
            .output(NType::record([("trace", NType::Any)]))
            .effects(EffectSet::new([Effect::Pure, Effect::Fallible]))
            .description("Retrieve the execution trace of a past composition")
            .example(json!({"composition_id": "comp_abc123"}), json!({"trace": {"stages": [{"id": "s1", "status": "ok", "duration_ms": 12}]}}))
            .example(json!({"composition_id": "comp_def456"}), json!({"trace": {"stages": [{"id": "s1", "status": "failed", "error": "timeout"}]}}))
            .example(json!({"composition_id": "comp_ghi789"}), json!({"trace": {"stages": []}}))
            .example(json!({"composition_id": "comp_jkl012"}), json!({"trace": {"stages": [{"id": "s1", "status": "ok", "duration_ms": 5}, {"id": "s2", "status": "ok", "duration_ms": 8}]}}))
            .example(json!({"composition_id": "comp_mno345"}), json!({"trace": {"stages": [{"id": "s1", "status": "ok", "duration_ms": 100}]}}))
            .tag("internal").tag("meta").tag("debugging")
            .alias("read_trace").alias("get_trace").alias("execution_log")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("stage_describe")
            .input(NType::record([("id", NType::Text)]))
            .output(NType::record([
                ("id", NType::Text),
                ("description", NType::Text),
                ("input", NType::Text),
                ("output", NType::Text),
                ("effects", NType::List(Box::new(NType::Text))),
                ("lifecycle", NType::Text),
            ]))
            .effects(EffectSet::new([Effect::Pure, Effect::Fallible]))
            .description("Get detailed information about a stage by its ID")
            .example(json!({"id": "abc123"}), json!({"id": "abc123", "description": "Convert to text", "input": "Any", "output": "Text", "effects": ["Pure"], "lifecycle": "active"}))
            .example(json!({"id": "def456"}), json!({"id": "def456", "description": "HTTP GET", "input": "Record", "output": "Record", "effects": ["Network", "Fallible"], "lifecycle": "active"}))
            .example(json!({"id": "ghi789"}), json!({"id": "ghi789", "description": "Sort list", "input": "Record", "output": "List", "effects": ["Pure"], "lifecycle": "active"}))
            .example(json!({"id": "old123"}), json!({"id": "old123", "description": "Legacy stage", "input": "Text", "output": "Text", "effects": ["Unknown"], "lifecycle": "deprecated"}))
            .example(json!({"id": "new456"}), json!({"id": "new456", "description": "Draft stage", "input": "Number", "output": "Bool", "effects": ["Pure"], "lifecycle": "draft"}))
            .tag("internal").tag("meta").tag("reflection").tag("pure")
            .alias("stage_info").alias("describe_stage").alias("get_stage")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("type_check")
            .input(NType::record([
                ("sub", NType::Any),
                ("sup", NType::Any),
            ]))
            .output(NType::record([
                ("compatible", NType::Bool),
                ("reason", NType::optional(NType::Text)),
            ]))
            .pure()
            .description("Check if one type is a structural subtype of another")
            .example(json!({"sub": "Text", "sup": "Text"}), json!({"compatible": true, "reason": null}))
            .example(json!({"sub": "Text", "sup": "Number"}), json!({"compatible": false, "reason": "expected Number, got Text"}))
            .example(json!({"sub": "Text", "sup": "Any"}), json!({"compatible": true, "reason": null}))
            .example(json!({"sub": "Any", "sup": "Text"}), json!({"compatible": true, "reason": null}))
            .example(json!({"sub": "Number", "sup": "Text|Number"}), json!({"compatible": true, "reason": null}))
            .tag("internal").tag("meta").tag("types").tag("pure")
            .alias("is_subtype").alias("type_compatible").alias("structural_subtype")
            .build_stdlib(key)
            .unwrap(),
    ]
}
