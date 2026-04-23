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

/// Verify that a Python stage implementation defines a top-level
/// `def execute(...)` function. The Noether runtime wraps user code and
/// calls `execute(parsed_input)` — without it, stages fail with a confusing
/// runtime error. We catch this at `stage add` time with a clear message.
///
/// Returns `Err` with the message to display, or `Ok(())` if a top-level
/// `def execute` is present (no Python parser involved — a regex match on
/// non-indented lines is sufficient and avoids a runtime dependency).
fn validate_python_execute(code: &str) -> Result<(), String> {
    let mut has_execute = false;
    let mut bad_main: Option<usize> = None;
    let mut bad_stdin: Option<usize> = None;

    for (lineno, line) in code.lines().enumerate() {
        // Only consider module-level (column 0) statements.
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let trimmed = line.trim_start();

        if trimmed.starts_with("def execute(") || trimmed.starts_with("async def execute(") {
            has_execute = true;
        }

        // Reject a top-level `if __name__ == "__main__":` block. The Noether
        // runtime synthesizes its own `__main__` block in the wrapper to
        // call `execute(parsed_input)` and emit the JSON result on stdout.
        // A user `__main__` block — particularly one that reads stdin —
        // races the runtime's own stdin consumer; whichever runs first
        // drains the pipe and the other gets an empty input and silent
        // wrong-results.
        if bad_main.is_none()
            && trimmed.starts_with("if __name__")
            && (trimmed.contains("\"__main__\"") || trimmed.contains("'__main__'"))
        {
            bad_main = Some(lineno + 1);
        }

        // Reject module-level stdin reads — same race condition. A reference
        // to sys.stdin or a bare input() call here means the user is trying
        // to do their own I/O, which conflicts with the wrapper.
        if bad_stdin.is_none()
            && (trimmed.contains("sys.stdin")
                || trimmed.starts_with("input(")
                || trimmed.contains(" input("))
        {
            bad_stdin = Some(lineno + 1);
        }
    }

    if !has_execute {
        return Err(
            "Python stage implementation must define a top-level function \
             `def execute(input): ...` that takes the parsed input dict and \
             returns the output dict. Do not read from stdin or print to stdout — \
             the Noether runtime handles I/O for you."
                .into(),
        );
    }

    if let Some(line) = bad_main {
        return Err(format!(
            "Python stage at line {line} has a top-level \
             `if __name__ == \"__main__\":` block. The Noether runtime \
             synthesizes its own __main__ block to call execute(input) — \
             a user-level one races the runtime's stdin consumer and \
             produces silent wrong results. Move that code into the body \
             of `def execute(input):` instead."
        ));
    }

    if let Some(line) = bad_stdin {
        return Err(format!(
            "Python stage at line {line} reads stdin or calls input() \
             at module level. The Noether runtime parses stdin for you \
             and passes the result to `def execute(input)`. Direct stdin \
             reads race the runtime and drain the pipe. Use the `input` \
             argument to your execute function instead."
        ));
    }

    Ok(())
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

        // Accept TitleCase (canonical), snake_case (CLI form), and bare
        // lowercase. Llm/Cost without parameters get sensible defaults
        // (Llm{model:"unknown"}, Cost{cents:0}). Old v0.2 specs that
        // shipped {"effect": "Llm"} now decode correctly instead of
        // silently dropping with the cryptic 'unknown effect' warning.
        let effects: Vec<Effect> = effects_raw
            .iter()
            .filter_map(|s| match s.as_str() {
                "Pure" | "pure" => Some(Effect::Pure),
                "Network" | "network" => Some(Effect::Network),
                "Fallible" | "fallible" => Some(Effect::Fallible),
                "NonDeterministic" | "non-deterministic" | "nondeterministic" => {
                    Some(Effect::NonDeterministic)
                }
                "Process" | "process" => Some(Effect::Process),
                "Llm" | "llm" => Some(Effect::Llm {
                    model: "unknown".into(),
                }),
                "Cost" | "cost" => Some(Effect::Cost { cents: 0 }),
                "Unknown" | "unknown" => Some(Effect::Unknown),
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

        // For Python stages, fail fast if the implementation does not define
        // a top-level `def execute(input)`. The Nix wrapper requires it.
        if matches!(language, "python" | "python3") {
            validate_python_execute(code)?;
        }

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
    auto_activate: bool,
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

    // ── Signature dedup: auto-deprecate previous version ────────────────
    // If a stage with the same signature_id exists and is Active, deprecate
    // it with the new stage as successor. This ensures only one active
    // version per concept (name + types + effects).
    let mut deprecated_id: Option<String> = None;
    if let Some(ref sig) = stage.signature_id {
        for existing in store.list(Some(&StageLifecycle::Active)) {
            if existing.signature_id.as_ref() == Some(sig) && existing.id != stage.id {
                deprecated_id = Some(existing.id.0.clone());
                break;
            }
        }
    }

    match store.put(stage) {
        Ok(id) => {
            // Auto-promote Draft → Active unless caller asked for --draft.
            // This matches user expectation: `stage add` is the publish step.
            let mut activated = false;
            if auto_activate && store.update_lifecycle(&id, StageLifecycle::Active).is_ok() {
                activated = true;
            }
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
            result["lifecycle"] = json!(if activated { "active" } else { "draft" });
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

/// Bulk-import every `*.json` file in `directory` as a stage spec. Idempotent:
/// existing stages (same content hash) are skipped. Per-file failures are
/// reported but do not abort the overall sync.
pub fn cmd_sync(
    store: &mut dyn StageStore,
    directory: &str,
    author_key: &SigningKey,
    index: &SemanticIndex,
    auto_activate: bool,
) {
    let dir = std::path::Path::new(directory);
    if !dir.is_dir() {
        eprintln!("{}", acli_error(&format!("not a directory: {directory}")));
        std::process::exit(1);
    }

    let mut paths: Vec<std::path::PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("json"))
            .collect(),
        Err(e) => {
            eprintln!("{}", acli_error(&format!("failed to read directory: {e}")));
            std::process::exit(1);
        }
    };
    paths.sort();

    let mut imported = Vec::new();
    let mut skipped = Vec::new();
    let mut failed = Vec::new();

    for path in &paths {
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                failed.push(json!({"file": path.display().to_string(), "error": format!("{e}")}));
                continue;
            }
        };
        let mut stage = match parse_spec(&content) {
            Ok(s) => s,
            Err(e) => {
                failed.push(json!({"file": path.display().to_string(), "error": e}));
                continue;
            }
        };

        // Sign unsigned stages with the local author key (matches cmd_add).
        if stage.ed25519_signature.is_none() && stage.signer_public_key.is_none() {
            let sig_hex = sign_stage_id(&stage.id, author_key);
            let pub_hex = hex::encode(author_key.verifying_key().to_bytes());
            stage.ed25519_signature = Some(sig_hex);
            stage.signer_public_key = Some(pub_hex);
        }

        // Skip if the same content hash is already in the store.
        if matches!(store.get(&stage.id), Ok(Some(_))) {
            skipped.push(json!({
                "file": path.display().to_string(),
                "id": stage.id.0,
            }));
            continue;
        }

        let stage_id = stage.id.clone();
        match store.put(stage) {
            Ok(id) => {
                let mut lifecycle = "draft";
                if auto_activate && store.update_lifecycle(&id, StageLifecycle::Active).is_ok() {
                    lifecycle = "active";
                }
                imported.push(json!({
                    "file": path.display().to_string(),
                    "id": id.0,
                    "lifecycle": lifecycle,
                }));
            }
            Err(StoreError::AlreadyExists(id)) => {
                skipped.push(json!({"file": path.display().to_string(), "id": id.0}));
            }
            Err(e) => {
                failed.push(json!({
                    "file": path.display().to_string(),
                    "id": stage_id.0,
                    "error": format!("{e}"),
                }));
            }
        }
    }

    // Touch index var so unused-variable warning stays quiet (sync currently
    // doesn't perform similarity dedup — that's add-only behaviour).
    let _ = index;

    let result = json!({
        "directory": directory,
        "imported": imported,
        "skipped": skipped,
        "failed": failed,
        "counts": {
            "imported": imported.len(),
            "skipped": skipped.len(),
            "failed": failed.len(),
        }
    });
    println!("{}", acli_ok(result));
    if !failed.is_empty() {
        std::process::exit(1);
    }
}

pub fn cmd_get(store: &dyn StageStore, hash: &str) {
    // Accept any unambiguous prefix — same resolution as `stage activate`,
    // matching the 8-char form `stage list` prints. Prior versions required
    // an exact-length match here, so passing a prefix returned
    // `stage X not found  Did you mean: X` (the hint echoed the same input
    // because the prefix was the right answer all along).
    let resolved = match resolve_stage_id(store, hash) {
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
    match store.get(&resolved) {
        Ok(Some(stage)) => {
            let json = serde_json::to_value(stage).unwrap();
            println!("{}", acli_ok(json));
        }
        Ok(None) => {
            // resolve_stage_id said this exists; if get() now disagrees the
            // store changed under us. Surface as a real error.
            eprintln!(
                "{}",
                acli_error(&format!(
                    "stage {} disappeared between resolution and fetch",
                    resolved.0
                ))
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

/// Return a hint string when stage resolution failed.
///
/// Three outcomes:
/// 1. No stage starts with `prefix` → "search the catalogue" hint.
/// 2. Exactly one stage starts with `prefix` → resolution would have
///    succeeded; this branch is unreachable from the call sites that go
///    through `resolve_stage_id` first.
/// 3. Multiple stages start with `prefix` → "ambiguous" hint that prints
///    each matching ID at a length **longer than the input** so the user
///    can see why they collide. Earlier versions truncated to 16 chars
///    even when the user already typed 16, producing the famously useless
///    "Did you mean: <exact-input>?" non-hint.
fn find_prefix_hint(store: &dyn StageStore, prefix: &str) -> Option<String> {
    if prefix.len() < 4 {
        return None;
    }
    let matches: Vec<_> = store
        .list(None)
        .into_iter()
        .filter(|s| s.id.0.starts_with(prefix))
        .take(5)
        .collect();
    if matches.is_empty() {
        return Some(
            "No stage with that ID. Try `noether stage search \"<description>\"` \
             or `noether stage list` to browse all stages."
                .into(),
        );
    }
    // Show enough characters to actually differentiate. Take the user's
    // prefix length plus 8, capped at the full ID length.
    let display_len = (prefix.len() + 8).min(64);
    let ids: Vec<_> = matches
        .iter()
        .map(|s| &s.id.0[..display_len.min(s.id.0.len())])
        .collect();
    Some(format!(
        "prefix '{prefix}' is ambiguous; matches {}: {}",
        matches.len(),
        ids.join(", ")
    ))
}

/// Options for `noether stage list`. Defaults reproduce historical behavior
/// (active stages only, 8-char ID prefixes, no signer/tag filter).
pub struct ListOptions<'a> {
    pub tag: Option<&'a str>,
    pub signed_by: Option<&'a str>,
    pub lifecycle: Option<&'a str>,
    pub full_ids: bool,
}

pub fn cmd_list(store: &dyn StageStore, opts: ListOptions<'_>) {
    // Resolve the lifecycle filter.
    let lifecycle_filter: Option<StageLifecycle> = match opts.lifecycle {
        None => Some(StageLifecycle::Active), // default: active only
        Some("all") | Some("any") => None,
        Some("draft") => Some(StageLifecycle::Draft),
        Some("active") => Some(StageLifecycle::Active),
        Some("tombstone") => Some(StageLifecycle::Tombstone),
        Some("deprecated") => {
            // store.list takes an exact lifecycle; fetch all and filter below.
            None
        }
        Some(other) => {
            eprintln!(
                "{}",
                acli_error(&format!(
                    "unknown lifecycle '{other}' — use draft|active|deprecated|tombstone|all"
                ))
            );
            std::process::exit(1);
        }
    };

    let mut sorted: Vec<&Stage> = store.list(lifecycle_filter.as_ref());
    if matches!(opts.lifecycle, Some("deprecated")) {
        sorted.retain(|s| matches!(s.lifecycle, StageLifecycle::Deprecated { .. }));
    }
    sorted.sort_by(|a, b| a.description.cmp(&b.description));

    if let Some(tag) = opts.tag {
        sorted.retain(|s| s.tags.iter().any(|t| t == tag));
    }

    if let Some(who) = opts.signed_by {
        let stdlib_pub = {
            use ed25519_dalek::SigningKey as _Sk;
            let sk: _Sk = noether_core::stdlib::stdlib_signing_key();
            hex::encode(sk.verifying_key().to_bytes())
        };
        match who {
            "stdlib" => {
                sorted.retain(|s| s.signer_public_key.as_deref() == Some(stdlib_pub.as_str()))
            }
            "custom" => {
                sorted.retain(|s| s.signer_public_key.as_deref() != Some(stdlib_pub.as_str()))
            }
            prefix => sorted.retain(|s| {
                s.signer_public_key
                    .as_deref()
                    .map(|k| k.starts_with(prefix))
                    .unwrap_or(false)
            }),
        }
    }

    let entries: Vec<serde_json::Value> = sorted
        .iter()
        .map(|s| {
            let id_str: &str = if opts.full_ids {
                &s.id.0
            } else {
                &s.id.0[..8.min(s.id.0.len())]
            };
            let mut entry = json!({
                "id": id_str,
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

/// `noether stage test [ID_PREFIX]` — run each stage against its own
/// `examples` and compare actual output to declared output via canonical
/// JSON hashing. Skips `Network` / `Llm` / `NonDeterministic` stages
/// with a clear reason.
///
/// Exit code: 0 on success (all non-skipped stages pass), 1 if any stage
/// has at least one mismatched example.
pub fn cmd_test(
    store: &dyn StageStore,
    executor: &impl noether_engine::executor::StageExecutor,
    id_prefix: Option<&str>,
) {
    use noether_engine::stage_test::{verify_stage, ReportOutcome};

    let stages: Vec<&Stage> = if let Some(prefix) = id_prefix {
        let matches: Vec<&Stage> = store
            .list(None)
            .into_iter()
            .filter(|s| s.id.0.starts_with(prefix))
            .collect();
        match matches.len() {
            0 => {
                eprintln!(
                    "{}",
                    acli_error(&format!("no stage matches prefix '{prefix}'"))
                );
                std::process::exit(1);
            }
            1 => matches,
            _ => {
                eprintln!(
                    "{}",
                    acli_error(&format!(
                        "prefix '{prefix}' is ambiguous — {} matches",
                        matches.len()
                    ))
                );
                std::process::exit(1);
            }
        }
    } else {
        store.list(Some(&StageLifecycle::Active))
    };

    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    let mut entries: Vec<serde_json::Value> = Vec::with_capacity(stages.len());

    for stage in &stages {
        let report = verify_stage(stage, executor);
        let (status, details) = match &report.outcome {
            ReportOutcome::Skipped { reason } => {
                skipped += 1;
                ("skipped", json!({"reason": reason.to_string()}))
            }
            ReportOutcome::Tested { examples } => {
                let all_ok = examples
                    .iter()
                    .all(|e| matches!(e, noether_engine::stage_test::ExampleOutcome::Ok));
                if all_ok {
                    passed += 1;
                    (
                        "passed",
                        json!({"examples": examples.len(), "all_match": true}),
                    )
                } else {
                    failed += 1;
                    let mismatches: Vec<serde_json::Value> = examples
                        .iter()
                        .enumerate()
                        .filter_map(|(i, outcome)| match outcome {
                            noether_engine::stage_test::ExampleOutcome::Ok => None,
                            noether_engine::stage_test::ExampleOutcome::Mismatch {
                                expected,
                                actual,
                            } => Some(json!({
                                "index": i,
                                "kind": "mismatch",
                                "expected": expected,
                                "actual": actual,
                            })),
                            noether_engine::stage_test::ExampleOutcome::Errored { message } => {
                                Some(json!({
                                    "index": i,
                                    "kind": "error",
                                    "message": message,
                                }))
                            }
                        })
                        .collect();
                    (
                        "failed",
                        json!({
                            "examples": examples.len(),
                            "failures": mismatches,
                        }),
                    )
                }
            }
        };
        entries.push(json!({
            "id": &report.stage_id[..8.min(report.stage_id.len())],
            "description": report.description,
            "status": status,
            "details": details,
        }));
    }

    let result = json!({
        "total": stages.len(),
        "passed": passed,
        "failed": failed,
        "skipped": skipped,
        "stages": entries,
    });
    println!("{}", acli_ok(result));

    if failed > 0 {
        std::process::exit(1);
    }
}

/// Verify a stage's Ed25519 signature and/or its declarative
/// properties against examples.
///
/// With `id_prefix`, check just the matching stage; otherwise walk
/// every Active stage. `check_signatures` and `check_properties`
/// control which checks run — at least one should be true.
///
/// Emits an ACLI error envelope (not ok) when any stage fails, so
/// agents branching on `ok: true` reliably catch violations.
pub fn cmd_verify(
    store: &dyn StageStore,
    id_prefix: Option<&str>,
    check_signatures: bool,
    check_properties: bool,
) {
    use noether_core::stage::{verify_stage_signature, CheckPropertiesError};

    let stages: Vec<&Stage> = if let Some(prefix) = id_prefix {
        let matches: Vec<&Stage> = store
            .list(None)
            .into_iter()
            .filter(|s| s.id.0.starts_with(prefix))
            .collect();
        match matches.len() {
            0 => {
                eprintln!(
                    "{}",
                    acli_error(&format!("no stage matches prefix '{prefix}'"))
                );
                std::process::exit(1);
            }
            1 => matches,
            _ => {
                eprintln!(
                    "{}",
                    acli_error(&format!(
                        "prefix '{prefix}' is ambiguous — {} matches",
                        matches.len()
                    ))
                );
                std::process::exit(1);
            }
        }
    } else {
        store.list(Some(&StageLifecycle::Active))
    };

    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut skipped = 0usize;
    let mut entries: Vec<serde_json::Value> = Vec::with_capacity(stages.len());

    for stage in &stages {
        let short_id = &stage.id.0[..8.min(stage.id.0.len())];
        let mut failures: Vec<serde_json::Value> = Vec::new();
        let mut skip_reasons: Vec<&'static str> = Vec::new();
        let mut ran_something = false;

        // ── Ed25519 signature check ─────────────────────────────────
        if check_signatures {
            match (&stage.ed25519_signature, &stage.signer_public_key) {
                (Some(sig_hex), Some(pub_hex)) => {
                    ran_something = true;
                    match verify_stage_signature(&stage.id, sig_hex, pub_hex) {
                        Ok(true) => {}
                        Ok(false) => failures.push(json!({
                            "check": "signature",
                            "violation": "Ed25519 signature does not match stage ID",
                        })),
                        Err(e) => failures.push(json!({
                            "check": "signature",
                            "violation": format!("signature verify error: {e}"),
                        })),
                    }
                }
                _ => skip_reasons.push("no signature"),
            }
        }

        // ── Declarative properties check ────────────────────────────
        if check_properties {
            if stage.properties.is_empty() {
                skip_reasons.push("no properties declared");
            } else {
                ran_something = true;
                match stage.check_properties() {
                    Ok(()) => {}
                    Err(CheckPropertiesError::NoExamples { count }) => failures.push(json!({
                        "check": "property",
                        "violation": format!(
                            "stage declares {count} properties but has no examples"
                        ),
                    })),
                    Err(CheckPropertiesError::Violations(violations)) => {
                        for (example_idx, v) in violations {
                            failures.push(json!({
                                "check": "property",
                                "example_index": example_idx,
                                "violation": v.to_string(),
                            }));
                        }
                    }
                }
            }
        }

        let status = if !ran_something {
            skipped += 1;
            "skipped"
        } else if failures.is_empty() {
            passed += 1;
            "passed"
        } else {
            failed += 1;
            "failed"
        };

        let mut details = json!({
            "examples": stage.examples.len(),
            "properties": stage.properties.len(),
        });
        if !failures.is_empty() {
            details["violations"] = json!(failures);
        }
        if !skip_reasons.is_empty() {
            details["skip_reasons"] = json!(skip_reasons);
        }

        entries.push(json!({
            "id": short_id,
            "description": stage.description.clone(),
            "status": status,
            "details": details,
        }));
    }

    let payload = json!({
        "total": stages.len(),
        "passed": passed,
        "failed": failed,
        "skipped": skipped,
        "stages": entries,
    });

    if failed > 0 {
        // ACLI envelope mismatch fix: emit acli_error on failure so
        // agents branching on `ok: true` can't miss violations.
        eprintln!(
            "{}",
            acli_error(&format!(
                "{} of {} stages failed verification",
                failed,
                stages.len()
            ))
        );
        // Still print the full report on stdout so the agent has
        // the structured data.
        println!("{}", acli_ok(payload));
        std::process::exit(1);
    }

    println!("{}", acli_ok(payload));
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
            "implementation": "def execute(input):\n    return input",
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
            "implementation": "def execute(input):\n    return input"
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
            "implementation": "def execute(input):\n    return {\"status\": 200, \"body\": \"\"}"
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

    // ── validate_python_execute tests ────────────────────────────────────

    #[test]
    fn validate_python_accepts_top_level_execute() {
        assert!(validate_python_execute("def execute(x):\n    return x").is_ok());
        assert!(
            validate_python_execute("import json\n\ndef execute(input):\n    return input").is_ok()
        );
        assert!(validate_python_execute("async def execute(input):\n    return input").is_ok());
    }

    #[test]
    fn validate_python_rejects_module_level_io() {
        let bad = "import sys, json\ndata = json.load(sys.stdin)\nprint(json.dumps(data))";
        let err = validate_python_execute(bad).unwrap_err();
        assert!(
            err.contains("def execute"),
            "error should mention contract: {err}"
        );
    }

    #[test]
    fn validate_python_rejects_indented_execute() {
        // A nested def inside a class shouldn't satisfy the contract.
        let bad = "class Foo:\n    def execute(self, x):\n        return x";
        assert!(validate_python_execute(bad).is_err());
    }

    #[test]
    fn validate_python_rejects_user_main_block() {
        // The Noether wrapper synthesizes its own __main__ block; a user
        // one races the runtime's stdin consumer.
        let bad = "import sys\n\ndef execute(input):\n    return input\n\nif __name__ == \"__main__\":\n    print(execute({}))";
        let err = validate_python_execute(bad).unwrap_err();
        assert!(err.contains("__main__"), "got: {err}");
    }

    #[test]
    fn validate_python_accepts_main_inside_string_or_comment() {
        // Mentions of __main__ in strings or comments should not trip the
        // detector — the check looks for an actual `if __name__ == "__main__":` line.
        let ok = "def execute(input):\n    return {'doc': '__main__ pattern not used'}\n";
        assert!(validate_python_execute(ok).is_ok());
    }

    #[test]
    fn validate_python_rejects_module_level_stdin_read() {
        let bad = "import sys, json\n\n_data = json.load(sys.stdin)\n\ndef execute(input):\n    return _data\n";
        let err = validate_python_execute(bad).unwrap_err();
        assert!(
            err.contains("stdin") || err.contains("input("),
            "got: {err}"
        );
    }

    #[test]
    fn validate_python_rejects_module_level_input_call() {
        let bad = "name = input(\"name? \")\n\ndef execute(input):\n    return name\n";
        let err = validate_python_execute(bad).unwrap_err();
        assert!(
            err.contains("input(") || err.contains("stdin"),
            "got: {err}"
        );
    }

    #[test]
    fn parse_spec_rejects_python_without_execute() {
        let spec = json!({
            "name": "bad_stage",
            "input": "Any",
            "output": "Any",
            "implementation": "x = 1\nprint(x)"
        });
        let err = parse_spec(&spec.to_string()).unwrap_err();
        assert!(err.contains("def execute"));
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
            "implementation": "from bs4 import BeautifulSoup\ndef execute(input):\n    soup = BeautifulSoup(input['html'], 'html.parser')\n    return soup.get_text(separator=' ', strip=True)",
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
