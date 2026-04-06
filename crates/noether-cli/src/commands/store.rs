use crate::output::{acli_error, acli_ok};
use noether_engine::index::SemanticIndex;
use noether_store::StageStore;
use serde_json::json;

pub fn cmd_stats(store: &impl StageStore) {
    let stats = store.stats();
    println!(
        "{}",
        acli_ok(json!({
            "total": stats.total,
            "by_lifecycle": stats.by_lifecycle,
            "by_effect": stats.by_effect,
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
            let desc_a = store.get(a).ok().flatten().map(|s| s.description.clone()).unwrap_or_default();
            let desc_b = store.get(b).ok().flatten().map(|s| s.description.clone()).unwrap_or_default();
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
        let lifecycle_b = store
            .get(b)
            .ok()
            .flatten()
            .map(|s| s.lifecycle.clone());

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
