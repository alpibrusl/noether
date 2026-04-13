//! Parser for the human-friendly stage spec format used by `noether stage add`,
//! `noether stage sync`, and the noether-cloud registry's boot-time loader.
//!
//! Two input forms are accepted:
//!
//! - **Simple spec** — top-level `name`, `description`, `input`, `output`,
//!   `effects`, `language`, `implementation`, `examples`, `tags`, `aliases`.
//!   Types may use the simplified shorthand (`"Text"`, `{"Record": [...]}`)
//!   or the canonical `{"kind": ..., "value": ...}` form.
//! - **Full Stage JSON** — the serialised [`Stage`] struct, used when
//!   re-importing already-built stages.
//!
//! Returns an unsigned [`Stage`] (no `ed25519_signature` / `signer_public_key`
//! set). The caller is responsible for signing — typically with the local
//! author key for CLI submissions or the stdlib key for trusted curated
//! content loaded from disk by a registry.

use crate::effects::{Effect, EffectSet};
use crate::stage::{Example, Stage, StageBuilder};
use crate::types::NType;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Convert the simplified type syntax used in stage spec files into the
/// `{"kind": "...", "value": ...}` format that `NType` serde expects.
///
/// Accepted shorthands:
/// - `"Text"`                          → `{"kind":"Text"}`
/// - `{"List": T}`                     → `{"kind":"List","value": normalize(T)}`
/// - `{"Map": [K, V]}`                 → `{"kind":"Map","value":{"key":..,"value":..}}`
/// - `{"Record": [["f",T], ...]}`      → `{"kind":"Record","value":{"f":..,...}}`
/// - `{"Union": [T1, T2, ...]}`        → `{"kind":"Union","value":[..]}`
/// - `{"Stream": T}`                   → `{"kind":"Stream","value": normalize(T)}`
/// - already `{"kind": ...}`           → pass through (recursing into value)
pub fn normalize_type(v: &Value) -> Value {
    use serde_json::json;

    match v {
        Value::String(s) => match s.as_str() {
            "Any" | "Bool" | "Bytes" | "Null" | "Number" | "Text" | "VNode" => {
                json!({"kind": s})
            }
            _ => v.clone(),
        },
        Value::Object(map) => {
            if map.contains_key("kind") {
                let mut out = map.clone();
                if let Some(val) = out.get("value").cloned() {
                    out.insert("value".to_string(), normalize_type_value(&val));
                }
                Value::Object(out)
            } else if let Some(inner) = map.get("List") {
                json!({"kind": "List", "value": normalize_type(inner)})
            } else if let Some(inner) = map.get("Stream") {
                json!({"kind": "Stream", "value": normalize_type(inner)})
            } else if let Some(Value::Array(pair)) = map.get("Map") {
                if pair.len() == 2 {
                    json!({"kind": "Map", "value": {
                        "key": normalize_type(&pair[0]),
                        "value": normalize_type(&pair[1])
                    }})
                } else {
                    v.clone()
                }
            } else if let Some(Value::Array(fields)) = map.get("Record") {
                let mut record = serde_json::Map::new();
                for field in fields {
                    if let Value::Array(pair) = field {
                        if pair.len() == 2 {
                            if let Some(name) = pair[0].as_str() {
                                record.insert(name.to_string(), normalize_type(&pair[1]));
                            }
                        }
                    }
                }
                json!({"kind": "Record", "value": Value::Object(record)})
            } else if let Some(Value::Array(variants)) = map.get("Union") {
                let normalized: Vec<Value> = variants.iter().map(normalize_type).collect();
                json!({"kind": "Union", "value": normalized})
            } else {
                v.clone()
            }
        }
        _ => v.clone(),
    }
}

fn normalize_type_value(v: &Value) -> Value {
    match v {
        Value::Array(arr) => Value::Array(arr.iter().map(normalize_type).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                out.insert(k.clone(), normalize_type(val));
            }
            Value::Object(out)
        }
        _ => normalize_type(v),
    }
}

/// Parse a stage spec from JSON text.
///
/// Accepts both the simple spec format (with top-level `name` and
/// `implementation`) and the full serialised `Stage` JSON. Returns an
/// unsigned `Stage` — caller signs it.
pub fn parse_simple_spec(content: &str) -> Result<Stage, String> {
    let v: Value = serde_json::from_str(content).map_err(|e| format!("invalid JSON: {e}"))?;

    if v.get("name").is_some() && v.get("implementation").is_some() && v.get("id").is_none() {
        let name = v["name"].as_str().ok_or("missing 'name'")?.to_string();
        let description = v
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or(&name)
            .to_string();
        let input: NType = serde_json::from_value(normalize_type(&v["input"]))
            .map_err(|e| format!("invalid input type: {e}"))?;
        let output: NType = serde_json::from_value(normalize_type(&v["output"]))
            .map_err(|e| format!("invalid output type: {e}"))?;

        let effects_raw: Vec<String> = v
            .get("effects")
            .and_then(|e| e.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let effects: Vec<Effect> = effects_raw
            .iter()
            .filter_map(|s| match s.as_str() {
                "Pure" => Some(Effect::Pure),
                "Network" => Some(Effect::Network),
                "Fallible" => Some(Effect::Fallible),
                "NonDeterministic" => Some(Effect::NonDeterministic),
                "Process" => Some(Effect::Process),
                _ => None,
            })
            .collect();

        let effect_set = if effects.is_empty() {
            EffectSet::pure()
        } else {
            EffectSet::new(effects)
        };

        let code = if v["implementation"].is_string() {
            v["implementation"]
                .as_str()
                .ok_or("missing implementation code")?
        } else {
            v["implementation"]["code"]
                .as_str()
                .ok_or("missing implementation.code")?
        };
        let language = if v["implementation"].is_string() {
            v.get("language")
                .and_then(|l| l.as_str())
                .unwrap_or("python")
        } else {
            v["implementation"]["language"]
                .as_str()
                .ok_or("missing implementation.language")?
        };

        let impl_hash = hex::encode(Sha256::digest(code.as_bytes()));

        let examples: Vec<Example> = v
            .get("examples")
            .and_then(|e| e.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|ex| serde_json::from_value::<Example>(ex.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();

        let mut builder = StageBuilder::new(&name)
            .description(&description)
            .input(input)
            .output(output)
            .effects(effect_set)
            .implementation_code(code, language);

        for ex in examples {
            builder = builder.example(ex.input, ex.output);
        }

        if let Some(tags) = v.get("tags").and_then(|t| t.as_array()) {
            for tag in tags {
                if let Some(t) = tag.as_str() {
                    builder = builder.tag(t);
                }
            }
        }
        if let Some(aliases) = v.get("aliases").and_then(|a| a.as_array()) {
            for alias in aliases {
                if let Some(a) = alias.as_str() {
                    builder = builder.alias(a);
                }
            }
        }

        return builder
            .build_unsigned(impl_hash)
            .map_err(|e| format!("invalid spec: {e}"));
    }

    serde_json::from_str::<Stage>(content).map_err(|e| format!("invalid stage JSON: {e}"))
}
