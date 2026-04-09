use crate::output::{acli_error, acli_error_hint, acli_error_hints, acli_ok};
use ed25519_dalek::SigningKey;
use noether_core::effects::{Effect, EffectSet};
use noether_core::stage::{
    sign_stage_id, verify_stage_signature, Example, Stage, StageBuilder, StageId, StageLifecycle,
};
use noether_core::types::NType;
use noether_engine::index::SemanticIndex;
use noether_store::{StageStore, StoreError};
use serde_json::json;
use std::fs;

/// Convert the simplified type syntax used in stage spec files into the
/// `{"kind": "...", "value": ...}` format that `NType` serde expects.
///
/// Accepted shorthands:
///   `"Text"`                          → `{"kind":"Text"}`
///   `{"List": T}`                     → `{"kind":"List","value": normalize(T)}`
///   `{"Map": [K, V]}`                → `{"kind":"Map","value":{"key":normalize(K),"value":normalize(V)}}`
///   `{"Record": [["f",T], ...]}`     → `{"kind":"Record","value":{"f":normalize(T), ...}}`
///   `{"Union": [T1, T2, ...]}`       → `{"kind":"Union","value":[normalize(T1), ...]}`
///   `{"Stream": T}`                   → `{"kind":"Stream","value": normalize(T)}`
///   already `{"kind": ...}`          → pass through (recursing into value)
fn normalize_type(v: &serde_json::Value) -> serde_json::Value {
    use serde_json::{json, Value};

    match v {
        // String shorthand for primitives: "Text", "Number", "Bool", etc.
        Value::String(s) => {
            match s.as_str() {
                "Any" | "Bool" | "Bytes" | "Null" | "Number" | "Text" | "VNode" => {
                    json!({"kind": s})
                }
                _ => v.clone(), // unknown string, pass through for serde to reject
            }
        }
        Value::Object(map) => {
            // Already in canonical form — pass through, recursing into value.
            if map.contains_key("kind") {
                let mut out = map.clone();
                if let Some(val) = out.get("value").cloned() {
                    out.insert("value".to_string(), normalize_type_value(&val));
                }
                Value::Object(out)
            }
            // {"List": T}
            else if let Some(inner) = map.get("List") {
                json!({"kind": "List", "value": normalize_type(inner)})
            }
            // {"Stream": T}
            else if let Some(inner) = map.get("Stream") {
                json!({"kind": "Stream", "value": normalize_type(inner)})
            }
            // {"Map": [K, V]}
            else if let Some(Value::Array(pair)) = map.get("Map") {
                if pair.len() == 2 {
                    json!({"kind": "Map", "value": {
                        "key": normalize_type(&pair[0]),
                        "value": normalize_type(&pair[1])
                    }})
                } else {
                    v.clone()
                }
            }
            // {"Record": [["field", Type], ...]}
            else if let Some(Value::Array(fields)) = map.get("Record") {
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
            }
            // {"Union": [T1, T2, ...]}
            else if let Some(Value::Array(variants)) = map.get("Union") {
                let normalized: Vec<Value> = variants.iter().map(normalize_type).collect();
                json!({"kind": "Union", "value": normalized})
            } else {
                v.clone()
            }
        }
        _ => v.clone(),
    }
}

/// Recurse into the `"value"` payload of an already-canonical type.
/// Handles arrays (Union value), objects (Record/Map value), and scalar inner types.
fn normalize_type_value(v: &serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match v {
        Value::Array(arr) => Value::Array(arr.iter().map(normalize_type).collect()),
        Value::Object(map) => {
            // Record value: {"field": Type, ...} or Map value: {"key": Type, "value": Type}
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                out.insert(k.clone(), normalize_type(val));
            }
            Value::Object(out)
        }
        _ => normalize_type(v),
    }
}

/// Human-friendly spec format for `noether stage add` — parsed from JSON,
/// not a Rust struct (the `Value` parsing above handles it).
/// Parse the spec file — supports both the simple `StageSpec` format and the
/// full serialised `Stage` format (for importing existing stages).
fn parse_spec(content: &str) -> Result<Stage, String> {
    let v: serde_json::Value =
        serde_json::from_str(content).map_err(|e| format!("invalid JSON: {e}"))?;

    // Simple spec format: has "name" + "implementation" but no "id".
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
                other => {
                    eprintln!("Warning: unknown effect '{other}', ignoring.");
                    None
                }
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

        // Compute the implementation hash from the code (SHA-256 hex).
        let impl_hash = {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(code.as_bytes()))
        };

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

        let stage = builder
            .build_unsigned(impl_hash)
            .map_err(|e| format!("invalid spec: {e}"))?;

        return Ok(stage);
    }

    // Fall back to the full Stage JSON format.
    serde_json::from_str::<Stage>(content).map_err(|e| format!("invalid stage JSON: {e}"))
}

pub fn cmd_add(
    store: &mut dyn StageStore,
    spec_path: &str,
    author_key: &SigningKey,
    index: &SemanticIndex,
) {
    let content = match fs::read_to_string(spec_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}", acli_error(&format!("failed to read file: {e}")));
            std::process::exit(1);
        }
    };

    let mut stage: Stage = match parse_spec(&content) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{}", acli_error(&e));
            std::process::exit(1);
        }
    };

    // ── Pre-insertion dedup check ─────────────────────────────────────────
    // Reject if a semantically near-identical stage with the same type
    // signature already exists; otherwise emit a warning and allow.
    if let Ok(similar) = index.search(&stage.description, 3) {
        for hit in &similar {
            if hit.score < 0.92 {
                break; // results are sorted descending
            }
            if let Ok(Some(existing)) = store.get(&hit.stage_id) {
                let types_match = existing.signature.input == stage.signature.input
                    && existing.signature.output == stage.signature.output;
                if types_match {
                    eprintln!(
                        "{}",
                        acli_error_hints(
                            "near-duplicate stage already exists (similarity ≥ 0.92, same types)",
                            Some("Use the existing stage or change the type signature to register a distinct variant."),
                            Some(vec![
                                format!("existing id:   {}", existing.id.0),
                                format!("similarity:    {:.3}", hit.score),
                                format!("description:   {}", existing.description),
                            ]),
                        )
                    );
                    std::process::exit(1);
                } else {
                    // Same semantics, different types → allowed, but warn
                    eprintln!(
                        "Warning: similar stage exists (similarity {:.3}) but types differ — \
                         registering as a distinct variant (id: {}).",
                        hit.score,
                        &existing.id.0[..8.min(existing.id.0.len())],
                    );
                }
            }
        }
    }

    // ── Signing pipeline ──────────────────────────────────────────────────
    match (&stage.ed25519_signature, &stage.signer_public_key) {
        (Some(sig_hex), Some(pub_hex)) => {
            // Stage is pre-signed — verify before accepting.
            match verify_stage_signature(&stage.id, sig_hex, pub_hex) {
                Ok(true) => {} // valid, proceed
                Ok(false) => {
                    eprintln!(
                        "{}",
                        acli_error_hints(
                            "signature verification failed",
                            Some("The stage may have been tampered with after signing."),
                            Some(vec![
                                format!("stage id:    {}", stage.id.0),
                                format!("signer key:  {pub_hex}"),
                            ]),
                        )
                    );
                    std::process::exit(2);
                }
                Err(e) => {
                    eprintln!(
                        "{}",
                        acli_error(&format!("could not decode signature: {e}"))
                    );
                    std::process::exit(2);
                }
            }
        }
        (None, None) => {
            // Unsigned stage — sign with the local author key.
            let sig_hex = sign_stage_id(&stage.id, author_key);
            let pub_hex = hex::encode(author_key.verifying_key().to_bytes());
            eprintln!(
                "Stage is unsigned — signing with local author key (public: {}).",
                &pub_hex[..16]
            );
            stage.ed25519_signature = Some(sig_hex);
            stage.signer_public_key = Some(pub_hex);
        }
        _ => {
            eprintln!(
                "{}",
                acli_error(
                    "malformed stage: exactly one of ed25519_signature / signer_public_key is set; \
                     both must be present or both must be absent"
                )
            );
            std::process::exit(2);
        }
    }

    // ── Canonical dedup: auto-deprecate previous version ────────────────
    // If a stage with the same canonical_id exists and is Active, deprecate
    // it with the new stage as successor. This ensures only one active
    // version per concept (name + types + effects).
    let mut deprecated_id: Option<String> = None;
    if let Some(ref canonical) = stage.canonical_id {
        for existing in store.list(Some(&StageLifecycle::Active)) {
            if existing.canonical_id.as_ref() == Some(canonical) && existing.id != stage.id {
                deprecated_id = Some(existing.id.0.clone());
                // Don't deprecate yet — wait until the new stage is inserted successfully.
                break;
            }
        }
    }

    match store.put(stage) {
        Ok(id) => {
            // Now deprecate the old version, pointing to the new one.
            if let Some(ref old_id) = deprecated_id {
                let old_stage_id = StageId(old_id.clone());
                let new_lc = StageLifecycle::Deprecated {
                    successor_id: id.clone(),
                };
                if store.update_lifecycle(&old_stage_id, new_lc).is_ok() {
                    eprintln!(
                        "Auto-deprecated previous version {} → successor {}",
                        &old_id[..8.min(old_id.len())],
                        &id.0[..8.min(id.0.len())]
                    );
                }
            }
            let mut result = json!({"id": id.0});
            if let Some(old) = deprecated_id {
                result["supersedes"] = json!(old);
            }
            println!("{}", acli_ok(result));
        }
        Err(StoreError::AlreadyExists(id)) => {
            println!("{}", acli_ok(json!({"id": id.0, "note": "already exists"})));
        }
        Err(e) => {
            eprintln!("{}", acli_error(&format!("{e}")));
            std::process::exit(1);
        }
    }
}

pub fn cmd_get(store: &dyn StageStore, hash: &str) {
    let id = StageId(hash.into());
    match store.get(&id) {
        Ok(Some(stage)) => {
            let json = serde_json::to_value(stage).unwrap();
            println!("{}", acli_ok(json));
        }
        Ok(None) => {
            // Try to find a prefix match for a useful hint
            let hint = find_prefix_hint(store, hash);
            eprintln!(
                "{}",
                acli_error_hint(&format!("stage {hash} not found"), hint.as_deref(),)
            );
            std::process::exit(1);
        }
        Err(e) => {
            eprintln!("{}", acli_error(&format!("{e}")));
            std::process::exit(1);
        }
    }
}

pub fn cmd_activate(store: &mut dyn StageStore, hash: &str) {
    // Resolve prefix to full ID if needed.
    let full_id = match resolve_stage_id(store, hash) {
        Some(id) => id,
        None => {
            let hint = find_prefix_hint(store, hash);
            eprintln!(
                "{}",
                acli_error_hint(&format!("stage {hash} not found"), hint.as_deref())
            );
            std::process::exit(1);
        }
    };

    match store.update_lifecycle(&full_id, StageLifecycle::Active) {
        Ok(()) => {
            println!(
                "{}",
                acli_ok(json!({"id": full_id.0, "lifecycle": "active"}))
            );
        }
        Err(e) => {
            eprintln!("{}", acli_error(&format!("{e}")));
            std::process::exit(1);
        }
    }
}

/// Resolve a hash prefix to a full StageId. Returns None if no match or ambiguous.
fn resolve_stage_id(store: &dyn StageStore, prefix: &str) -> Option<StageId> {
    // Try exact match first.
    let exact = StageId(prefix.into());
    if store.get(&exact).ok().flatten().is_some() {
        return Some(exact);
    }
    // Try prefix match.
    let matches: Vec<_> = store
        .list(None)
        .into_iter()
        .filter(|s| s.id.0.starts_with(prefix))
        .collect();
    if matches.len() == 1 {
        Some(matches[0].id.clone())
    } else {
        None
    }
}

/// Return a hint string if a stage ID starts with `prefix`.
fn find_prefix_hint(store: &dyn StageStore, prefix: &str) -> Option<String> {
    if prefix.len() < 4 {
        return None;
    }
    let matches: Vec<_> = store
        .list(None)
        .into_iter()
        .filter(|s| s.id.0.starts_with(prefix))
        .take(3)
        .collect();
    if matches.is_empty() {
        Some(
            "No stage with that ID. Try `noether stage search \"<description>\"` \
             or `noether stage list` to browse all stages."
                .into(),
        )
    } else {
        let ids: Vec<_> = matches
            .iter()
            .map(|s| &s.id.0[..16.min(s.id.0.len())])
            .collect();
        Some(format!("Did you mean one of: {}?", ids.join(", ")))
    }
}

pub fn cmd_list(store: &dyn StageStore, tag_filter: Option<&str>) {
    let stages = store.list(Some(&StageLifecycle::Active));
    let mut sorted: Vec<&Stage> = stages;
    sorted.sort_by(|a, b| a.description.cmp(&b.description));

    if let Some(tag) = tag_filter {
        sorted.retain(|s| s.tags.iter().any(|t| t == tag));
    }

    let entries: Vec<serde_json::Value> = sorted
        .iter()
        .map(|s| {
            let mut entry = json!({
                "id": &s.id.0[..8.min(s.id.0.len())],
                "description": s.description,
                "signature": format!("{} → {}", s.signature.input, s.signature.output),
                "lifecycle": format!("{:?}", s.lifecycle),
            });
            if !s.tags.is_empty() {
                entry["tags"] = serde_json::json!(s.tags);
            }
            entry
        })
        .collect();

    println!(
        "{}",
        acli_ok(json!({"stages": entries, "count": entries.len()}))
    );
}

pub fn cmd_search(store: &dyn StageStore, index: &SemanticIndex, query: &str, tag: Option<&str>) {
    let results = match index.search_filtered(query, 20, tag) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}", acli_error(&format!("search failed: {e}")));
            std::process::exit(1);
        }
    };

    let entries: Vec<serde_json::Value> = results
        .iter()
        .filter_map(|r| {
            let stage = store.get(&r.stage_id).ok()??;
            let mut entry = json!({
                "id": &stage.id.0[..8.min(stage.id.0.len())],
                "description": stage.description,
                "signature": format!("{} → {}", stage.signature.input, stage.signature.output),
                "score": format!("{:.3}", r.score),
                "scores": {
                    "signature": format!("{:.3}", r.signature_score),
                    "semantic": format!("{:.3}", r.semantic_score),
                    "example": format!("{:.3}", r.example_score),
                }
            });
            if !stage.tags.is_empty() {
                entry["tags"] = serde_json::json!(stage.tags);
            }
            Some(entry)
        })
        .collect();

    let mut out = json!({"query": query, "results": entries, "count": entries.len()});
    if let Some(t) = tag {
        out["tag_filter"] = serde_json::json!(t);
    }
    println!("{}", acli_ok(out));
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── normalize_type tests ─────────────────────────────────────────────

    #[test]
    fn normalize_primitive_strings() {
        for name in &["Text", "Number", "Bool", "Bytes", "Null", "Any", "VNode"] {
            let input = json!(name);
            let expected = json!({"kind": name});
            assert_eq!(normalize_type(&input), expected, "failed for {name}");
        }
    }

    #[test]
    fn normalize_unknown_string_passes_through() {
        let input = json!("FooBar");
        assert_eq!(normalize_type(&input), json!("FooBar"));
    }

    #[test]
    fn normalize_list() {
        let input = json!({"List": "Text"});
        let expected = json!({"kind": "List", "value": {"kind": "Text"}});
        assert_eq!(normalize_type(&input), expected);
    }

    #[test]
    fn normalize_stream() {
        let input = json!({"Stream": "Number"});
        let expected = json!({"kind": "Stream", "value": {"kind": "Number"}});
        assert_eq!(normalize_type(&input), expected);
    }

    #[test]
    fn normalize_map() {
        let input = json!({"Map": ["Text", "Number"]});
        let expected =
            json!({"kind": "Map", "value": {"key": {"kind": "Text"}, "value": {"kind": "Number"}}});
        assert_eq!(normalize_type(&input), expected);
    }

    #[test]
    fn normalize_record() {
        let input = json!({"Record": [["name", "Text"], ["age", "Number"]]});
        let expected = json!({"kind": "Record", "value": {"name": {"kind": "Text"}, "age": {"kind": "Number"}}});
        assert_eq!(normalize_type(&input), expected);
    }

    #[test]
    fn normalize_union() {
        let input = json!({"Union": ["Text", "Null"]});
        let expected = json!({"kind": "Union", "value": [{"kind": "Text"}, {"kind": "Null"}]});
        assert_eq!(normalize_type(&input), expected);
    }

    #[test]
    fn normalize_nested_record_with_list() {
        let input = json!({"Record": [["items", {"List": "Text"}], ["count", "Number"]]});
        let expected = json!({"kind": "Record", "value": {
            "items": {"kind": "List", "value": {"kind": "Text"}},
            "count": {"kind": "Number"}
        }});
        assert_eq!(normalize_type(&input), expected);
    }

    #[test]
    fn normalize_passthrough_canonical_format() {
        let input = json!({"kind": "Text"});
        assert_eq!(normalize_type(&input), json!({"kind": "Text"}));

        let input = json!({"kind": "Record", "value": {"name": {"kind": "Text"}}});
        assert_eq!(normalize_type(&input), input);
    }

    #[test]
    fn normalize_canonical_with_simplified_inner() {
        // A kind/value wrapper around a simplified inner type
        let input = json!({"kind": "List", "value": "Text"});
        let expected = json!({"kind": "List", "value": {"kind": "Text"}});
        assert_eq!(normalize_type(&input), expected);
    }

    // ── parse_spec tests ─────────────────────────────────────────────────

    #[test]
    fn parse_spec_with_tags_and_aliases() {
        let spec = json!({
            "name": "test_stage",
            "description": "A test stage",
            "input": "Text",
            "output": "Text",
            "effects": ["Pure"],
            "language": "python",
            "implementation": "import sys, json\ndata = json.load(sys.stdin)\nprint(json.dumps(data))",
            "examples": [
                {"input": "hello", "output": "hello"}
            ],
            "tags": ["test", "pure"],
            "aliases": ["test_alias", "another_alias"]
        });

        let stage = parse_spec(&spec.to_string()).expect("parse_spec should succeed");
        assert_eq!(stage.tags, vec!["test", "pure"]);
        assert_eq!(stage.aliases, vec!["test_alias", "another_alias"]);
    }

    #[test]
    fn parse_spec_without_tags_has_empty_vecs() {
        let spec = json!({
            "name": "bare_stage",
            "input": "Any",
            "output": "Any",
            "implementation": "import sys; print(sys.stdin.read())"
        });

        let stage = parse_spec(&spec.to_string()).expect("parse_spec should succeed");
        assert!(stage.tags.is_empty());
        assert!(stage.aliases.is_empty());
    }

    #[test]
    fn parse_spec_simplified_record_types() {
        let spec = json!({
            "name": "typed_stage",
            "description": "Stage with record input",
            "input": {"Record": [["url", "Text"], ["headers", {"Union": ["Text", "Null"]}]]},
            "output": {"Record": [["status", "Number"], ["body", "Text"]]},
            "effects": ["Network", "Fallible"],
            "implementation": "pass"
        });

        let stage = parse_spec(&spec.to_string()).expect("parse_spec should succeed");
        // Verify the types deserialized correctly
        assert_eq!(
            stage.signature.input,
            NType::record([
                ("url", NType::Text),
                ("headers", NType::Union(vec![NType::Text, NType::Null])),
            ])
        );
        assert_eq!(
            stage.signature.output,
            NType::record([("status", NType::Number), ("body", NType::Text)])
        );
    }

    #[test]
    fn parse_spec_full_doc_example() {
        let spec = r#"{
            "name": "html_extract_text",
            "description": "Extract all visible text from an HTML string, stripping tags",
            "input": {"Record": [["html", "Text"]]},
            "output": "Text",
            "effects": ["Fallible"],
            "language": "python",
            "implementation": "from bs4 import BeautifulSoup\nimport sys, json\ndata = json.load(sys.stdin)\nsoup = BeautifulSoup(data['html'], 'html.parser')\nprint(json.dumps(soup.get_text(separator=' ', strip=True)))",
            "examples": [
                {"input": {"html": "<h1>Hello</h1><p>World</p>"}, "output": "Hello World"},
                {"input": {"html": ""}, "output": ""},
                {"input": {"html": "no tags"}, "output": "no tags"},
                {"input": {"html": "<p>  spaces  </p>"}, "output": "spaces"},
                {"input": {"html": "<div><span>a</span></div>"}, "output": "a"}
            ],
            "tags": ["web", "html", "text"],
            "aliases": ["html_to_text", "strip_html"]
        }"#;

        let stage = parse_spec(spec).expect("doc example should parse");
        assert_eq!(
            stage.description,
            "Extract all visible text from an HTML string, stripping tags"
        );
        assert_eq!(
            stage.signature.input,
            NType::record([("html", NType::Text)])
        );
        assert_eq!(stage.signature.output, NType::Text);
        assert_eq!(stage.examples.len(), 5);
        assert_eq!(stage.tags, vec!["web", "html", "text"]);
        assert_eq!(stage.aliases, vec!["html_to_text", "strip_html"]);
    }
}
