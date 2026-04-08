use crate::output::{acli_error, acli_error_hint, acli_error_hints, acli_ok};
use ed25519_dalek::SigningKey;
use noether_core::effects::{Effect, EffectSet};
use noether_core::stage::{
    sign_stage_id, verify_stage_signature, Example, Stage, StageBuilder, StageId,
};
use noether_core::types::NType;
use noether_engine::index::SemanticIndex;
use noether_store::{StageStore, StoreError};
use serde_json::json;
use std::fs;

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
        let input: NType = serde_json::from_value(v["input"].clone())
            .map_err(|e| format!("invalid input type: {e}"))?;
        let output: NType = serde_json::from_value(v["output"].clone())
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

    match store.put(stage) {
        Ok(id) => println!("{}", acli_ok(json!({"id": id.0}))),
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
    let stages = store.list(None);
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
