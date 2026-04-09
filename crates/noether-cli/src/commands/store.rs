use crate::output::{acli_error, acli_ok};
use noether_core::effects::{Effect, EffectSet};
use noether_core::stage::{verify_stage_signature, StageLifecycle};
use noether_engine::index::SemanticIndex;
use noether_engine::llm::{LlmConfig, LlmProvider, Message};
use noether_store::StageStore;
use serde_json::json;

pub fn cmd_stats(store: &dyn StageStore, index: &SemanticIndex) {
    let stats = store.stats();
    let near_duplicate_pairs = index.find_near_duplicates(0.92).len();
    println!(
        "{}",
        acli_ok(json!({
            "total": stats.total,
            "by_lifecycle": stats.by_lifecycle,
            "by_effect": stats.by_effect,
            "near_duplicate_pairs": near_duplicate_pairs,
            "dedup_rate_pct": if stats.total > 0 {
                (near_duplicate_pairs * 2 * 100) / stats.total
            } else {
                0
            },
        }))
    );
}

pub fn cmd_retro(
    store: &mut dyn StageStore,
    index: &SemanticIndex,
    dry_run: bool,
    apply: bool,
    threshold: f32,
) {
    let pairs = index.find_near_duplicates(threshold);

    if pairs.is_empty() {
        println!(
            "{}",
            acli_ok(json!({
                "threshold": threshold,
                "near_duplicate_pairs": 0,
                "pairs": [],
                "action": "none",
                "message": "Store is clean — no near-duplicate stages found.",
            }))
        );
        return;
    }

    // Resolve descriptions for display
    let pair_details: Vec<serde_json::Value> = pairs
        .iter()
        .map(|(a, b, sim)| {
            let desc_a = store
                .get(a)
                .ok()
                .flatten()
                .map(|s| s.description.clone())
                .unwrap_or_default();
            let desc_b = store
                .get(b)
                .ok()
                .flatten()
                .map(|s| s.description.clone())
                .unwrap_or_default();
            json!({
                "stage_a": { "id": a.0, "description": desc_a },
                "stage_b": { "id": b.0, "description": desc_b },
                "similarity": sim,
                "recommendation": "deprecate stage_b, use stage_a as canonical",
            })
        })
        .collect();

    if dry_run || !apply {
        println!(
            "{}",
            acli_ok(json!({
                "threshold": threshold,
                "near_duplicate_pairs": pair_details.len(),
                "pairs": pair_details,
                "action": "dry-run",
                "message": format!(
                    "Found {} near-duplicate pair(s). Run with --apply to deprecate.",
                    pair_details.len()
                ),
            }))
        );
        return;
    }

    // --apply: deprecate stage_b in favour of stage_a for each pair
    // We only process pairs where stage_b is not already deprecated/tombstoned.
    let mut deprecated_count = 0;
    let mut skipped_count = 0;
    let mut errors: Vec<String> = Vec::new();

    for (a, b, _sim) in &pairs {
        let lifecycle_b = store.get(b).ok().flatten().map(|s| s.lifecycle.clone());

        match lifecycle_b {
            Some(noether_core::stage::StageLifecycle::Active) => {
                let deprecated = noether_core::stage::StageLifecycle::Deprecated {
                    successor_id: a.clone(),
                };
                match store.update_lifecycle(b, deprecated) {
                    Ok(_) => {
                        deprecated_count += 1;
                    }
                    Err(e) => errors.push(format!("failed to deprecate {}: {e}", b.0)),
                }
            }
            _ => {
                skipped_count += 1;
            }
        }
    }

    if !errors.is_empty() {
        eprintln!(
            "{}",
            acli_error(&format!(
                "retro completed with {} error(s): {}",
                errors.len(),
                errors.join("; ")
            ))
        );
    }

    println!(
        "{}",
        acli_ok(json!({
            "threshold": threshold,
            "near_duplicate_pairs": pairs.len(),
            "deprecated": deprecated_count,
            "skipped": skipped_count,
            "errors": errors,
            "action": "applied",
            "message": format!(
                "Deprecated {} stage(s), skipped {} (already inactive).",
                deprecated_count, skipped_count
            ),
        }))
    );
}

/// Infer and record effects for all stages currently marked `Unknown`.
///
/// In Noether's content-addressed model, changing a stage's declared effects
/// changes its identity hash. This command therefore:
/// 1. Identifies all Active stages with Unknown effects that have implementation code.
/// 2. Uses the LLM to infer the likely effects from description + code snippet.
/// 3. In `--dry-run` mode: reports the plan, no mutations.
/// 4. Without `--dry-run`: creates a new stage with inferred effects (new hash)
///    and deprecates the original, preserving the full audit chain.
pub fn cmd_migrate_effects(store: &mut dyn StageStore, llm: &dyn LlmProvider, dry_run: bool) {
    let candidates: Vec<_> = store
        .list(None)
        .into_iter()
        .filter(|s| {
            matches!(s.lifecycle, StageLifecycle::Active)
                && s.signature.effects.is_unknown()
                && s.implementation_code.is_some()
        })
        .cloned()
        .collect();

    if candidates.is_empty() {
        println!(
            "{}",
            acli_ok(json!({
                "candidates": 0,
                "action": if dry_run { "dry-run" } else { "none" },
                "message": "No Unknown-effects stages with implementation code found.",
                "migrations": [],
            }))
        );
        return;
    }

    let mut migrations: Vec<serde_json::Value> = Vec::new();

    for stage in &candidates {
        let code = stage.implementation_code.as_deref().unwrap_or("");
        let inferred = infer_effects_with_llm(llm, &stage.description, code);

        migrations.push(json!({
            "stage_id": stage.id.0,
            "description": stage.description,
            "inferred_effects": inferred.iter().map(|e| format!("{e:?}")).collect::<Vec<_>>(),
            "applied": !dry_run,
        }));

        if dry_run {
            continue;
        }

        // Build new stage with inferred effects.
        // We keep all metadata identical except the effects field in the signature.
        let mut new_stage = stage.clone();
        new_stage.signature.effects = inferred;
        // Recompute ID for the new signature.
        match noether_core::stage::compute_stage_id(&new_stage.signature) {
            Ok(new_id) => {
                new_stage.id = new_id.clone();
                // Re-sign with the same signer key if present — we can't do that
                // without the private key, so clear signatures and let the caller
                // re-sign via `stage add` if needed.
                new_stage.ed25519_signature = None;
                new_stage.signer_public_key = None;

                // Insert the updated stage.
                if let Err(e) = store.put(new_stage) {
                    eprintln!(
                        "Warning: failed to insert migrated stage for {}: {e}",
                        stage.id.0
                    );
                    continue;
                }

                // Deprecate the original with the new stage as successor.
                let deprecated = StageLifecycle::Deprecated {
                    successor_id: new_id,
                };
                if let Err(e) = store.update_lifecycle(&stage.id, deprecated) {
                    eprintln!(
                        "Warning: failed to deprecate original {} after migration: {e}",
                        stage.id.0
                    );
                }
            }
            Err(e) => {
                eprintln!(
                    "Warning: could not compute new stage ID for {}: {e}",
                    stage.id.0
                );
            }
        }
    }

    println!(
        "{}",
        acli_ok(json!({
            "candidates": candidates.len(),
            "action": if dry_run { "dry-run" } else { "applied" },
            "message": if dry_run {
                format!(
                    "Would migrate {} stage(s). Run without --dry-run to apply.",
                    candidates.len()
                )
            } else {
                format!("Migrated {} stage(s).", candidates.len())
            },
            "migrations": migrations,
        }))
    );
}

/// Ask the LLM to classify effects from a stage description and code snippet.
/// Falls back to `Unknown` (empty) on any error.
fn infer_effects_with_llm(llm: &dyn LlmProvider, description: &str, code: &str) -> EffectSet {
    let code_snippet = if code.len() > 800 { &code[..800] } else { code };

    let prompt = format!(
        r#"You are classifying the side-effects of a Noether stage.

Stage description: {description}

Implementation (first 800 chars):
```
{code_snippet}
```

Reply with a JSON array containing ONLY the applicable effects from this list:
- "Pure"             — deterministic, no side-effects, safe to cache forever
- "Network"          — makes HTTP or socket calls
- "Llm"              — calls a language model
- "Fallible"         — may fail for non-type reasons (network errors, invalid input, etc.)
- "NonDeterministic" — same input may give different output (implies not Pure)

Reply ONLY with a JSON array, e.g.: ["Pure"] or ["Network","Fallible"] or ["Llm","NonDeterministic"].
No explanation, no markdown, just the JSON array."#
    );

    let messages = vec![Message::user(prompt)];
    let cfg = LlmConfig::default();
    match llm.complete(&messages, &cfg) {
        Ok(response) => parse_effects_response(&response),
        Err(e) => {
            eprintln!("Warning: LLM effect inference failed: {e}. Keeping Unknown.");
            EffectSet::default()
        }
    }
}

fn parse_effects_response(response: &str) -> EffectSet {
    // Extract the JSON array from the response (handle markdown fences etc.)
    let trimmed = response.trim();
    let json_start = trimmed.find('[').unwrap_or(0);
    let json_end = trimmed.rfind(']').map(|i| i + 1).unwrap_or(trimmed.len());
    let json_str = &trimmed[json_start..json_end];

    let names: Vec<String> = serde_json::from_str(json_str).unwrap_or_default();
    let effects: Vec<Effect> = names
        .iter()
        .filter_map(|name| match name.as_str() {
            "Pure" => Some(Effect::Pure),
            "Network" => Some(Effect::Network),
            "Fallible" => Some(Effect::Fallible),
            "NonDeterministic" => Some(Effect::NonDeterministic),
            "Llm" => Some(Effect::Llm {
                model: "unknown".into(),
            }),
            // FsRead / FsWrite are Capabilities, not Effects — silently ignore.
            _ => None,
        })
        .collect();

    if effects.is_empty() {
        EffectSet::unknown()
    } else {
        EffectSet::new(effects)
    }
}

/// Surface near-duplicate stages above `threshold` cosine similarity.
///
/// Unlike `retro` (which deprecates), `dedup` tombstones the lower-relevance
/// stage in each pair — permanent removal, not soft deprecation.
/// A stage is "lower" if it has fewer examples or was registered later (shorter ID by alpha).
pub fn cmd_dedup(store: &mut dyn StageStore, index: &SemanticIndex, threshold: f32, apply: bool) {
    let pairs = index.find_near_duplicates(threshold);

    if pairs.is_empty() {
        println!(
            "{}",
            acli_ok(json!({
                "threshold": threshold,
                "pairs_found": 0,
                "action": if apply { "none" } else { "dry-run" },
                "message": format!("No near-duplicate pairs above {:.0}% similarity.", threshold * 100.0),
            }))
        );
        return;
    }

    let pair_details: Vec<serde_json::Value> = pairs
        .iter()
        .map(|(a, b, sim)| {
            let stage_a = store.get(a).ok().flatten();
            let stage_b = store.get(b).ok().flatten();
            let (desc_a, ex_a) = stage_a
                .as_ref()
                .map(|s| (s.description.clone(), s.examples.len()))
                .unwrap_or_default();
            let (desc_b, ex_b) = stage_b
                .as_ref()
                .map(|s| (s.description.clone(), s.examples.len()))
                .unwrap_or_default();
            // The stage with fewer examples is the "redundant" one.
            // If tied, prefer keeping the alphabetically smaller ID (stable, deterministic).
            let (keep_id, remove_id, keep_desc, remove_desc) = if ex_a >= ex_b && (ex_a != ex_b || a.0 <= b.0) {
                (a, b, &desc_a, &desc_b)
            } else {
                (b, a, &desc_b, &desc_a)
            };
            json!({
                "keep":   { "id": &keep_id.0[..8.min(keep_id.0.len())], "description": keep_desc, "examples": ex_a.max(ex_b) },
                "remove": { "id": &remove_id.0[..8.min(remove_id.0.len())], "description": remove_desc, "examples": ex_a.min(ex_b) },
                "similarity": sim,
            })
        })
        .collect();

    if !apply {
        println!(
            "{}",
            acli_ok(json!({
                "threshold": threshold,
                "pairs_found": pair_details.len(),
                "action": "dry-run",
                "pairs": pair_details,
                "message": format!(
                    "Found {} near-duplicate pair(s). Run with --apply to deprecate the redundant stages (with successor_id pointing to the kept stage).",
                    pair_details.len()
                ),
            }))
        );
        return;
    }

    let mut tombstoned = 0;
    let mut skipped = 0;
    let mut errors: Vec<String> = Vec::new();

    for (a, b, _sim) in &pairs {
        let stage_a = store.get(a).ok().flatten();
        let stage_b = store.get(b).ok().flatten();
        let (ex_a, ex_b) = (
            stage_a.as_ref().map(|s| s.examples.len()).unwrap_or(0),
            stage_b.as_ref().map(|s| s.examples.len()).unwrap_or(0),
        );
        let (keep_id, remove_id) = if ex_a >= ex_b && (ex_a != ex_b || a.0 <= b.0) {
            (a, b)
        } else {
            (b, a)
        };

        let lifecycle = store
            .get(remove_id)
            .ok()
            .flatten()
            .map(|s| s.lifecycle.clone());
        match lifecycle {
            Some(noether_core::stage::StageLifecycle::Active) => {
                // Deprecate (not tombstone) so graphs referencing the old ID
                // get a clear error pointing to the replacement stage.
                let new_lc = noether_core::stage::StageLifecycle::Deprecated {
                    successor_id: keep_id.clone(),
                };
                match store.update_lifecycle(remove_id, new_lc) {
                    Ok(_) => tombstoned += 1,
                    Err(e) => {
                        errors.push(format!("could not deprecate {}: {e}", &remove_id.0[..8]))
                    }
                }
            }
            _ => skipped += 1,
        }
    }

    println!(
        "{}",
        acli_ok(json!({
            "threshold": threshold,
            "pairs_found": pairs.len(),
            "tombstoned": tombstoned,
            "skipped": skipped,
            "errors": errors,
            "action": "applied",
            "message": format!("Deprecated {} duplicate stage(s) (with successor_id), skipped {} (already inactive).", tombstoned, skipped),
        }))
    );
}

/// Audit store health: check signatures, lifecycle integrity, example coverage, and orphan stages.
///
/// Returns a structured report with categories of issues:
/// - **unsigned**: Active non-stdlib stages without an Ed25519 signature.
/// - **invalid_signature**: Stages whose signature fails cryptographic verification.
/// - **no_examples**: Active stages with no usage examples (reduces semantic search quality).
/// - **unknown_effects**: Active stages whose effects are Unknown (can't be safely cached or planned).
/// - **deprecated_no_successor**: Deprecated stages with no valid successor stage in store.
/// - **tombstoned_active**: (reserved) tombstone invariant check placeholder.
///
/// This command is read-only — it never modifies the store.
pub fn cmd_health(store: &dyn StageStore) {
    let all_stages = store.list(None);
    let stdlib_ids: std::collections::HashSet<String> = noether_core::stdlib::load_stdlib()
        .iter()
        .map(|s| s.id.0.clone())
        .collect();

    let mut unsigned: Vec<serde_json::Value> = Vec::new();
    let mut invalid_sig: Vec<serde_json::Value> = Vec::new();
    let mut no_examples: Vec<serde_json::Value> = Vec::new();
    let mut unknown_effects: Vec<serde_json::Value> = Vec::new();
    let mut deprecated_no_successor: Vec<serde_json::Value> = Vec::new();

    for stage in &all_stages {
        let is_stdlib = stdlib_ids.contains(&stage.id.0);
        let short_id = &stage.id.0[..8.min(stage.id.0.len())];

        match &stage.lifecycle {
            StageLifecycle::Active => {
                // 1. Unsigned non-stdlib stages
                if !is_stdlib && stage.ed25519_signature.is_none() {
                    unsigned.push(json!({
                        "id": short_id,
                        "description": stage.description,
                        "fix": "re-add with `noether stage add` to sign with local author key"
                    }));
                }

                // 2. Invalid signatures
                if let (Some(sig), Some(pub_key)) =
                    (&stage.ed25519_signature, &stage.signer_public_key)
                {
                    match verify_stage_signature(&stage.id, sig, pub_key) {
                        Ok(false) => {
                            invalid_sig.push(json!({
                                "id": short_id,
                                "description": stage.description,
                                "signer": &pub_key[..16.min(pub_key.len())],
                                "fix": "stage may have been tampered with; tombstone and re-register"
                            }));
                        }
                        Err(e) => {
                            invalid_sig.push(json!({
                                "id": short_id,
                                "description": stage.description,
                                "error": e.to_string(),
                                "fix": "signature could not be decoded; tombstone and re-register"
                            }));
                        }
                        Ok(true) => {} // valid
                    }
                }

                // 3. No examples
                if stage.examples.is_empty() {
                    no_examples.push(json!({
                        "id": short_id,
                        "description": stage.description,
                        "fix": "add at least one example to improve semantic search quality"
                    }));
                }

                // 4. Unknown effects
                if stage.signature.effects.is_unknown() {
                    unknown_effects.push(json!({
                        "id": short_id,
                        "description": stage.description,
                        "fix": "run `noether store migrate-effects` to infer effects via LLM"
                    }));
                }
            }
            StageLifecycle::Deprecated { successor_id } => {
                // 5. Deprecated pointing to a non-existent or tombstoned successor
                let successor_ok = store
                    .get(successor_id)
                    .ok()
                    .flatten()
                    .map(|s| !matches!(s.lifecycle, StageLifecycle::Tombstone))
                    .unwrap_or(false);
                if !successor_ok {
                    deprecated_no_successor.push(json!({
                        "id": short_id,
                        "description": stage.description,
                        "successor_id": &successor_id.0[..8.min(successor_id.0.len())],
                        "fix": "update successor_id to a valid active stage, or tombstone this stage"
                    }));
                }
            }
            StageLifecycle::Draft | StageLifecycle::Tombstone => {}
        }
    }

    let total_issues = unsigned.len()
        + invalid_sig.len()
        + no_examples.len()
        + unknown_effects.len()
        + deprecated_no_successor.len();

    let status = if invalid_sig.is_empty() && unsigned.is_empty() {
        "healthy"
    } else {
        "needs_attention"
    };

    println!(
        "{}",
        acli_ok(json!({
            "status": status,
            "total_stages": all_stages.len(),
            "total_issues": total_issues,
            "categories": {
                "unsigned": { "count": unsigned.len(), "items": unsigned },
                "invalid_signature": { "count": invalid_sig.len(), "items": invalid_sig },
                "no_examples": { "count": no_examples.len(), "items": no_examples },
                "unknown_effects": { "count": unknown_effects.len(), "items": unknown_effects },
                "deprecated_no_successor": { "count": deprecated_no_successor.len(), "items": deprecated_no_successor },
            },
            "summary": if total_issues == 0 {
                "Store is healthy. All signatures valid, all active stages have examples.".to_string()
            } else {
                format!(
                    "{} issue(s) found: {} unsigned, {} invalid signature(s), {} missing examples, {} unknown effects, {} broken deprecations.",
                    total_issues,
                    unsigned.len(),
                    invalid_sig.len(),
                    no_examples.len(),
                    unknown_effects.len(),
                    deprecated_no_successor.len()
                )
            },
        }))
    );
}
