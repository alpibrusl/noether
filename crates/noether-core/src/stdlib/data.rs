use crate::effects::{Effect, EffectSet};
use crate::stage::{Stage, StageBuilder};
use crate::types::NType;
use ed25519_dalek::SigningKey;
use serde_json::json;

pub fn stages(key: &SigningKey) -> Vec<Stage> {
    vec![
        StageBuilder::new("csv_parse")
            .input(NType::record([
                ("text", NType::Text),
                ("has_header", NType::optional(NType::Bool)),
                ("delimiter", NType::optional(NType::Text)),
            ]))
            .output(NType::List(Box::new(NType::Map {
                key: Box::new(NType::Text),
                value: Box::new(NType::Text),
            })))
            .effects(EffectSet::new([Effect::Pure, Effect::Fallible]))
            .description("Parse CSV text into a list of row maps")
            .example(json!({"text": "name,age\nAlice,30\nBob,25", "has_header": true, "delimiter": null}), json!([{"name": "Alice", "age": "30"}, {"name": "Bob", "age": "25"}]))
            .example(json!({"text": "a;b\n1;2", "has_header": true, "delimiter": ";"}), json!([{"a": "1", "b": "2"}]))
            .example(json!({"text": "x,y\n1,2\n3,4", "has_header": true, "delimiter": null}), json!([{"x": "1", "y": "2"}, {"x": "3", "y": "4"}]))
            .example(json!({"text": "a,b", "has_header": true, "delimiter": null}), json!([]))
            .example(json!({"text": "h1,h2\nv1,v2", "has_header": true, "delimiter": ","}), json!([{"h1": "v1", "h2": "v2"}]))
            .tag("data").tag("csv").tag("parsing").tag("pure")
            .alias("parse_csv").alias("read_csv").alias("csv_decode")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("csv_write")
            .input(NType::record([
                (
                    "records",
                    NType::List(Box::new(NType::Map {
                        key: Box::new(NType::Text),
                        value: Box::new(NType::Text),
                    })),
                ),
                ("delimiter", NType::optional(NType::Text)),
            ]))
            .output(NType::Text)
            .pure()
            .description("Serialize a list of row maps to CSV text")
            .example(json!({"records": [{"name": "Alice", "age": "30"}], "delimiter": null}), json!("age,name\n30,Alice"))
            .example(json!({"records": [{"a": "1", "b": "2"}], "delimiter": ";"}), json!("a;b\n1;2"))
            .example(json!({"records": [], "delimiter": null}), json!(""))
            .example(json!({"records": [{"x": "1"}, {"x": "2"}], "delimiter": null}), json!("x\n1\n2"))
            .example(json!({"records": [{"k": "v"}], "delimiter": ","}), json!("k\nv"))
            .tag("data").tag("csv").tag("pure")
            .alias("write_csv").alias("serialize_csv").alias("csv_encode")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("json_schema_validate")
            .input(NType::record([
                ("data", NType::Any),
                ("schema", NType::Any),
            ]))
            .output(NType::record([
                ("valid", NType::Bool),
                ("errors", NType::List(Box::new(NType::Text))),
            ]))
            .pure()
            .description("Validate data against a JSON schema; returns validation results")
            .example(json!({"data": {"name": "Alice"}, "schema": {"type": "object"}}), json!({"valid": true, "errors": []}))
            .example(json!({"data": 42, "schema": {"type": "string"}}), json!({"valid": false, "errors": ["expected string, got number"]}))
            .example(json!({"data": "hello", "schema": {"type": "string"}}), json!({"valid": true, "errors": []}))
            .example(json!({"data": [1, 2], "schema": {"type": "array"}}), json!({"valid": true, "errors": []}))
            .example(json!({"data": null, "schema": {"type": "null"}}), json!({"valid": true, "errors": []}))
            .tag("data").tag("validation").tag("json").tag("pure")
            .alias("jsonschema").alias("schema_check").alias("validate_json")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("arrow_from_records")
            .input(NType::List(Box::new(NType::Any)))
            .output(NType::Bytes)
            .effects(EffectSet::new([Effect::Pure, Effect::Fallible]))
            .description("Convert a list of records to Apache Arrow IPC bytes")
            .example(json!([{"a": 1}, {"a": 2}]), json!("QVJST1dfSVBD"))
            .example(json!([{"x": "hello"}]), json!("QVJST1dfSVBD"))
            .example(json!([]), json!("QVJST1dfRU1QVFk="))
            .example(json!([{"k": true}]), json!("QVJST1dfSVBD"))
            .example(json!([{"n": 9.81}]), json!("QVJST1dfSVBD"))
            .tag("data").tag("arrow").tag("analytics").tag("serialization")
            .alias("to_arrow").alias("records_to_ipc").alias("dataframe")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("records_to_arrow")
            .input(NType::Bytes)
            .output(NType::List(Box::new(NType::Map {
                key: Box::new(NType::Text),
                value: Box::new(NType::Any),
            })))
            .effects(EffectSet::new([Effect::Pure, Effect::Fallible]))
            .description("Decode Apache Arrow IPC bytes to a list of record maps")
            .example(json!("QVJST1dfSVBD"), json!([{"a": 1}, {"a": 2}]))
            .example(json!("QVJST1dfSVBD"), json!([{"x": "hello"}]))
            .example(json!("QVJST1dfRU1QVFk="), json!([]))
            .example(json!("QVJST1dfSVBD"), json!([{"k": true}]))
            .example(json!("QVJST1dfSVBD"), json!([{"n": 9.81}]))
            .tag("data").tag("arrow").tag("analytics").tag("serialization")
            .alias("from_arrow").alias("ipc_to_records").alias("decode_arrow")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("json_merge")
            .input(NType::record([
                ("base", NType::Any),
                ("patch", NType::Any),
            ]))
            .output(NType::Any)
            .pure()
            .description("Deep merge two JSON values; patch values override base")
            .example(json!({"base": {"a": 1}, "patch": {"b": 2}}), json!({"a": 1, "b": 2}))
            .example(json!({"base": {"a": 1}, "patch": {"a": 2}}), json!({"a": 2}))
            .example(json!({"base": {}, "patch": {"x": "y"}}), json!({"x": "y"}))
            .example(json!({"base": {"a": {"b": 1}}, "patch": {"a": {"c": 2}}}), json!({"a": {"b": 1, "c": 2}}))
            .example(json!({"base": [1, 2], "patch": [3]}), json!([3]))
            .tag("data").tag("json").tag("pure")
            .alias("merge_json").alias("deep_merge").alias("patch_json")
            .build_stdlib(key)
            .unwrap(),
        StageBuilder::new("json_path")
            .input(NType::record([
                ("data", NType::Any),
                ("path", NType::Text),
            ]))
            .output(NType::Any)
            .effects(EffectSet::new([Effect::Pure, Effect::Fallible]))
            .description("Extract a value from JSON data using a JSONPath expression")
            .example(json!({"data": {"a": {"b": 42}}, "path": "$.a.b"}), json!(42))
            .example(json!({"data": {"items": [1, 2, 3]}, "path": "$.items[0]"}), json!(1))
            .example(json!({"data": {"x": "hello"}, "path": "$.x"}), json!("hello"))
            .example(json!({"data": [10, 20, 30], "path": "$[1]"}), json!(20))
            .example(json!({"data": {"a": {"b": {"c": true}}}, "path": "$.a.b.c"}), json!(true))
            .tag("data").tag("json").tag("query").tag("pure")
            .alias("jsonpath").alias("jq").alias("json_query").alias("extract_field")
            .build_stdlib(key)
            .unwrap(),
    ]
}
