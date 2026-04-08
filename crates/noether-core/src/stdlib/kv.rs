use crate::effects::{Effect, EffectSet};
use crate::stage::{Stage, StageBuilder};
use crate::types::NType;
use ed25519_dalek::SigningKey;
use serde_json::json;

fn ns_field() -> (&'static str, NType) {
    ("namespace", NType::optional(NType::Text))
}

pub fn stages(key: &SigningKey) -> Vec<Stage> {
    vec![
        // ── kv_set ──────────────────────────────────────────────────────────
        StageBuilder::new("kv_set")
            .input(NType::record([
                ("key", NType::Text),
                ("value", NType::Any),
                ns_field(),
            ]))
            .output(NType::Text)
            .effects(EffectSet::new([Effect::Fallible]))
            .description("Store a JSON value under a key in the persistent key-value store; returns \"ok\"")
            .example(
                json!({"key": "last_query", "value": "rust async", "namespace": null}),
                json!("ok"),
            )
            .example(
                json!({"key": "counter", "value": 42, "namespace": "session:abc"}),
                json!("ok"),
            )
            .example(
                json!({"key": "results", "value": [1, 2, 3], "namespace": null}),
                json!("ok"),
            )
            .example(
                json!({"key": "config", "value": {"debug": true}, "namespace": "app"}),
                json!("ok"),
            )
            .example(
                json!({"key": "flag", "value": true, "namespace": null}),
                json!("ok"),
            )
            .tag("kv").tag("storage").tag("state").tag("persistence")
            .alias("set_key").alias("store_value").alias("put").alias("cache_set")
            .build_stdlib(key)
            .unwrap(),

        // ── kv_get ──────────────────────────────────────────────────────────
        StageBuilder::new("kv_get")
            .input(NType::record([
                ("key", NType::Text),
                ns_field(),
            ]))
            .output(NType::Any)
            .effects(EffectSet::new([Effect::Fallible]))
            .description("Retrieve a JSON value by key from the persistent key-value store; returns null if not found")
            .example(
                json!({"key": "last_query", "namespace": null}),
                json!("rust async"),
            )
            .example(
                json!({"key": "counter", "namespace": "session:abc"}),
                json!(42),
            )
            .example(
                json!({"key": "missing_key", "namespace": null}),
                json!(null),
            )
            .example(
                json!({"key": "results", "namespace": null}),
                json!([1, 2, 3]),
            )
            .example(
                json!({"key": "config", "namespace": "app"}),
                json!({"debug": true}),
            )
            .tag("kv").tag("storage").tag("state").tag("persistence")
            .alias("get_key").alias("load_value").alias("fetch_key").alias("cache_get")
            .build_stdlib(key)
            .unwrap(),

        // ── kv_delete ───────────────────────────────────────────────────────
        StageBuilder::new("kv_delete")
            .input(NType::record([
                ("key", NType::Text),
                ns_field(),
            ]))
            .output(NType::Bool)
            .effects(EffectSet::new([Effect::Fallible]))
            .description("Delete a key from the persistent key-value store; returns true if the key existed")
            .example(
                json!({"key": "last_query", "namespace": null}),
                json!(true),
            )
            .example(
                json!({"key": "missing_key", "namespace": null}),
                json!(false),
            )
            .example(
                json!({"key": "counter", "namespace": "session:abc"}),
                json!(true),
            )
            .example(
                json!({"key": "config", "namespace": "app"}),
                json!(true),
            )
            .example(
                json!({"key": "empty", "namespace": null}),
                json!(false),
            )
            .tag("kv").tag("storage").tag("state")
            .alias("delete_key").alias("remove_key").alias("del").alias("evict")
            .build_stdlib(key)
            .unwrap(),

        // ── kv_exists ───────────────────────────────────────────────────────
        StageBuilder::new("kv_exists")
            .input(NType::record([
                ("key", NType::Text),
                ns_field(),
            ]))
            .output(NType::Bool)
            .pure()
            .description("Check whether a key exists in the persistent key-value store")
            .example(
                json!({"key": "last_query", "namespace": null}),
                json!(true),
            )
            .example(
                json!({"key": "missing_key", "namespace": null}),
                json!(false),
            )
            .example(
                json!({"key": "counter", "namespace": "session:abc"}),
                json!(true),
            )
            .example(
                json!({"key": "config", "namespace": "app"}),
                json!(true),
            )
            .example(
                json!({"key": "empty", "namespace": null}),
                json!(false),
            )
            .tag("kv").tag("storage").tag("state").tag("pure")
            .alias("has_key").alias("key_exists").alias("contains_key")
            .build_stdlib(key)
            .unwrap(),

        // ── kv_list ─────────────────────────────────────────────────────────
        StageBuilder::new("kv_list")
            .input(NType::record([
                ("prefix", NType::Text),
                ns_field(),
            ]))
            .output(NType::List(Box::new(NType::Text)))
            .effects(EffectSet::new([Effect::Fallible]))
            .description("List all keys in the persistent key-value store that start with a given prefix")
            .example(
                json!({"prefix": "", "namespace": null}),
                json!(["config", "counter", "last_query"]),
            )
            .example(
                json!({"prefix": "session:", "namespace": null}),
                json!(["session:abc", "session:xyz"]),
            )
            .example(
                json!({"prefix": "cache:", "namespace": "app"}),
                json!(["cache:users", "cache:products"]),
            )
            .example(
                json!({"prefix": "no_match_", "namespace": null}),
                json!([]),
            )
            .example(
                json!({"prefix": "c", "namespace": null}),
                json!(["cache:users", "config", "counter"]),
            )
            .tag("kv").tag("storage").tag("state")
            .alias("list_keys").alias("scan_keys").alias("prefix_scan")
            .build_stdlib(key)
            .unwrap(),
    ]
}
