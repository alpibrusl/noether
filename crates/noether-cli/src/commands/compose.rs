use crate::output::{acli_error, acli_error_hints, acli_ok, acli_ok_cached};
use acli::output::CacheMeta;
use noether_engine::agent::SynthesisResult;
use noether_engine::checker::{
    check_capabilities, check_graph, collect_effect_warnings, verify_signatures, CapabilityPolicy,
};
use noether_engine::composition_cache::CompositionCache;
use noether_engine::executor::runner::run_composition;
use noether_engine::index::SemanticIndex;
use noether_engine::lagrange::{compute_composition_id, serialize_graph, CompositionGraph};
use noether_engine::llm::{LlmConfig, LlmProvider};
use noether_engine::planner::plan_graph;
use noether_store::StageStore;
use serde_json::json;
use std::path::Path;

pub struct ComposeOptions<'a> {
    pub model: &'a str,
    pub dry_run: bool,
    pub input: &'a serde_json::Value,
    pub force: bool,
    pub cache_path: &'a Path,
    pub policy: &'a CapabilityPolicy,
    /// Maximum total cost in cents. `None` = no limit.
    pub budget_cents: Option<u64>,
}

pub fn cmd_compose(
    store: &mut dyn StageStore,
    index: &mut SemanticIndex,
    llm: &dyn LlmProvider,
    problem: &str,
    opts: ComposeOptions<'_>,
) {
    let mut cache = CompositionCache::open(opts.cache_path);

    // ── Cache lookup ──────────────────────────────────────────────────────────
    if !opts.force {
        if let Some(cached) = cache.get(problem) {
            let age_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
                .saturating_sub(cached.cached_at);
            eprintln!(
                "Cache hit (model: {}, composed: {}s ago). Use --force to recompose.",
                cached.model, age_secs,
            );
            emit_result(
                store,
                EmitCtx {
                    model: opts.model,
                    dry_run: opts.dry_run,
                    input: opts.input,
                    from_cache: true,
                    cache_age_secs: age_secs,
                    attempts: 0,
                    synthesized: &[],
                    graph: &cached.graph.clone(),
                    policy: opts.policy,
                    budget_cents: opts.budget_cents,
                },
            );
            return;
        }
    }

    // ── LLM composition ───────────────────────────────────────────────────────
    let llm_config = LlmConfig {
        model: opts.model.into(),
        max_tokens: 4096,
        temperature: 0.2,
    };

    let mut agent = noether_engine::agent::CompositionAgent::new(index, llm, llm_config, 3);
    let result = match agent.compose(problem, store) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}", acli_error(&format!("composition failed: {e}")));
            std::process::exit(2);
        }
    };

    // Cache only graphs that don't depend on freshly synthesized stage IDs.
    if result.synthesized.is_empty() {
        cache.insert(problem, result.graph.clone(), opts.model);
    }

    let (graph, synthesized, attempts) = (result.graph, result.synthesized, result.attempts);
    emit_result(
        store,
        EmitCtx {
            model: opts.model,
            dry_run: opts.dry_run,
            input: opts.input,
            from_cache: false,
            cache_age_secs: 0,
            attempts,
            synthesized: &synthesized,
            graph: &graph,
            policy: opts.policy,
            budget_cents: opts.budget_cents,
        },
    );
}

struct EmitCtx<'a> {
    #[allow(dead_code)] // Used by cache key; may be read in future executor config
    model: &'a str,
    dry_run: bool,
    input: &'a serde_json::Value,
    from_cache: bool,
    /// Age of the cached entry in seconds (0 when not from cache).
    cache_age_secs: u64,
    attempts: u32,
    synthesized: &'a [SynthesisResult],
    graph: &'a CompositionGraph,
    policy: &'a CapabilityPolicy,
    budget_cents: Option<u64>,
}

fn emit_result(store: &mut dyn StageStore, ctx: EmitCtx<'_>) {
    let composition_id = compute_composition_id(ctx.graph).unwrap_or_else(|_| "unknown".into());
    let graph_json = serialize_graph(ctx.graph).unwrap_or_else(|_| "{}".into());

    let synthesized_json: Vec<serde_json::Value> = ctx
        .synthesized
        .iter()
        .map(|s| {
            json!({
                "stage_id": s.stage_id.0,
                "language": s.language,
                "attempts": s.attempts,
                "is_new": s.is_new,
            })
        })
        .collect();

    let check_result = check_graph(&ctx.graph.root, store).ok();
    let plan = plan_graph(&ctx.graph.root, store);

    // Capability pre-flight
    let violations = check_capabilities(&ctx.graph.root, store, ctx.policy);
    if !violations.is_empty() {
        let msgs: Vec<String> = violations.iter().map(|v| format!("{v}")).collect();
        eprintln!(
            "{}",
            acli_error_hints(
                &format!("{} capability violation(s)", msgs.len()),
                None,
                Some(msgs),
            )
        );
        std::process::exit(2);
    }

    // Signature verification pre-flight
    let sig_violations = verify_signatures(&ctx.graph.root, store);
    if !sig_violations.is_empty() {
        let msgs: Vec<String> = sig_violations.iter().map(|v| format!("{v}")).collect();
        eprintln!(
            "{}",
            acli_error_hints(
                &format!("{} signature violation(s)", msgs.len()),
                None,
                Some(msgs),
            )
        );
        std::process::exit(2);
    }

    // Effect warnings (includes budget enforcement)
    let effect_warnings = collect_effect_warnings(&ctx.graph.root, store, ctx.budget_cents);
    let budget_errors: Vec<String> = effect_warnings
        .iter()
        .filter(|w| {
            matches!(
                w,
                noether_engine::checker::EffectWarning::CostBudgetExceeded { .. }
            )
        })
        .map(|w| format!("{w}"))
        .collect();
    if !budget_errors.is_empty() {
        eprintln!(
            "{}",
            acli_error_hints("composition exceeds cost budget", None, Some(budget_errors))
        );
        std::process::exit(2);
    }

    let warning_strings: Vec<String> = {
        let mut ws: Vec<String> = check_result
            .as_ref()
            .map(|r| r.warnings.iter().map(|w| format!("{w}")).collect())
            .unwrap_or_default();
        // Include non-budget effect warnings too
        for w in &effect_warnings {
            let s = format!("{w}");
            if !ws.contains(&s) {
                ws.push(s);
            }
        }
        ws
    };

    if ctx.dry_run {
        println!(
            "{}",
            acli_ok(json!({
                "mode": "dry-run",
                "composition_id": composition_id,
                "attempts": ctx.attempts,
                "from_cache": ctx.from_cache,
                "synthesized": synthesized_json,
                "graph": serde_json::from_str::<serde_json::Value>(&graph_json).unwrap_or(json!(null)),
                "type_check": check_result.as_ref().map(|r| json!({
                    "input": format!("{}", r.resolved.input),
                    "output": format!("{}", r.resolved.output),
                })),
                "plan": {
                    "steps": plan.steps.len(),
                    "parallel_groups": plan.parallel_groups.len(),
                    "cost": plan.cost,
                },
                "warnings": warning_strings,
            }))
        );
        return;
    }

    let mut executor = super::executor_builder::build_executor_with_embeddings(store);
    for syn in ctx.synthesized {
        executor.register_synthesized(
            &syn.stage_id,
            &syn.implementation,
            &syn.language,
            syn.effects.clone(),
        );
    }

    if !ctx.synthesized.is_empty() && !executor.nix_available() {
        eprintln!("Warning: synthesized stages will use fallback execution (nix not available).");
    }

    match run_composition(&ctx.graph.root, ctx.input, &executor, &composition_id) {
        Ok(exec_result) => {
            let data = json!({
                "composition_id": composition_id,
                "attempts": ctx.attempts,
                "from_cache": ctx.from_cache,
                "synthesized": synthesized_json,
                "graph": serde_json::from_str::<serde_json::Value>(&graph_json).unwrap_or(json!(null)),
                "output": exec_result.output,
                "trace": exec_result.trace,
                "warnings": warning_strings,
            });
            if ctx.from_cache {
                println!(
                    "{}",
                    acli_ok_cached(
                        data,
                        CacheMeta {
                            hit: true,
                            key: Some(composition_id.clone()),
                            age_seconds: Some(ctx.cache_age_secs),
                        }
                    )
                );
            } else {
                println!("{}", acli_ok(data));
            }
        }
        Err(e) => {
            eprintln!("{}", acli_error(&format!("execution failed: {e}")));
            std::process::exit(3);
        }
    }
}
