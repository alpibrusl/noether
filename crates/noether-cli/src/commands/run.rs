use super::resolver_utils::resolve_and_emit_diagnostics;
use crate::output::{acli_error, acli_error_hints, acli_ok};
use noether_engine::checker::{
    check_capabilities, check_effects, check_graph, collect_effect_warnings, infer_effects,
    verify_signatures, CapabilityPolicy, EffectPolicy,
};
use noether_engine::executor::budget::{build_cost_map, BudgetedExecutor};
use noether_engine::executor::pure_cache::PureStageCache;
use noether_engine::executor::runner::run_composition_with_cache;
use noether_engine::executor::validating::ValidatingExecutor;
use noether_engine::lagrange::{
    compute_composition_id, parse_graph, resolve_stage_prefixes, CompositionGraph,
};
use noether_engine::optimizer::{
    self, canonical_structural::CanonicalStructural, dead_branch::DeadBranchElimination,
    OptimizerReport,
};
use noether_engine::planner::plan_graph;
use noether_engine::trace::JsonFileTraceStore;
use noether_store::StageStore;
use serde_json::json;

/// Hash the graph or return a ready-made ACLI error line.
///
/// Extracted so the error path (contract: stderr message starts
/// with the ACLI error prefix, contains "failed to hash composition
/// graph", and does NOT contain the pre-fix "unknown" placeholder)
/// can be exercised by a unit test without fabricating a
/// serde_jcs-hostile `serde_json::Value` — the test injects a
/// closure that returns `Err` directly.
///
/// The caller is responsible for `eprintln!` + `exit(1)`.
fn compute_composition_id_or_error_line<H>(
    graph: &CompositionGraph,
    hasher: H,
) -> Result<String, String>
where
    H: FnOnce(&CompositionGraph) -> Result<String, serde_json::Error>,
{
    match hasher(graph) {
        Ok(id) => Ok(id),
        Err(e) => Err(acli_error(&format!(
            "failed to hash composition graph: {e}"
        ))),
    }
}

/// Pre-flight policies for a single `noether run` invocation.
pub struct RunPolicies<'a> {
    pub capabilities: &'a CapabilityPolicy,
    pub effects: &'a EffectPolicy,
    /// Maximum total cost in cents. `None` = no limit.
    pub budget_cents: Option<u64>,
    /// Isolation backend applied to every Nix-executed stage.
    pub isolation: noether_engine::executor::isolation::IsolationBackend,
}

pub fn cmd_run(
    store: &dyn StageStore,
    trace_store: &mut JsonFileTraceStore,
    graph_path: &str,
    dry_run: bool,
    input: &serde_json::Value,
    policies: RunPolicies<'_>,
) {
    // 1. Read and parse
    let content = match std::fs::read_to_string(graph_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "{}",
                acli_error(&format!("failed to read {graph_path}: {e}"))
            );
            std::process::exit(1);
        }
    };

    let mut graph = match parse_graph(&content) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{}", acli_error(&format!("invalid graph JSON: {e}")));
            std::process::exit(1);
        }
    };

    // Composition identity comes from the **pre-resolution canonical form** —
    // the graph as authored, after structural canonicalisation but before any
    // store-dependent rewrite. Hashing after resolve_pinning /
    // resolve_deprecated would make the same source graph produce different
    // composition IDs on different days (as Active implementations change),
    // which contradicts the M1 "canonical form is identity" contract.
    //
    // Trace correlation against specific implementations uses `execution_id`
    // on the trace record (computed after resolution, not yet wired — see
    // #28 follow-up).
    // Propagate hash failures loudly rather than falling back to
    // "unknown" — a silent stringly-typed fallback in the ACLI
    // envelope would rob the caller of any breadcrumb to the cause.
    // Matches the post-#32 compose.rs shape.
    let composition_id = match compute_composition_id_or_error_line(&graph, compute_composition_id)
    {
        Ok(id) => id,
        Err(line) => {
            eprintln!("{line}");
            std::process::exit(1);
        }
    };

    // 1a. Resolve stage ID prefixes against the store. Hand-authored graphs
    //     can use the 8-char prefixes that `noether stage list` prints; the
    //     resolver expands them to full SHA-256 IDs (or fails clearly if
    //     ambiguous / not found).
    if let Err(e) = resolve_stage_prefixes(&mut graph.root, store) {
        eprintln!("{}", acli_error(&format!("stage reference: {e}")));
        std::process::exit(1);
    }

    // 1b+1c. Resolve pinning + deprecated stages in one pass, printing
    //        rewrite diagnostics and invariant-violation warnings. See
    //        `resolver_utils` for the canonical preamble used by every
    //        Lagrange-ingest entry point.
    if let Err(msg) = resolve_and_emit_diagnostics(&mut graph, store) {
        eprintln!("{}", acli_error(&msg));
        std::process::exit(1);
    }

    // 2. Type check
    let type_result = check_graph(&graph.root, store);
    if let Err(errors) = &type_result {
        let error_msgs: Vec<String> = errors.iter().map(|e| format!("{e}")).collect();
        eprintln!(
            "{}",
            acli_error(&format!(
                "type check failed:\n  {}",
                error_msgs.join("\n  ")
            ))
        );
        std::process::exit(2);
    }
    let check = type_result.unwrap();

    // 3. Capability pre-flight
    let violations = check_capabilities(&graph.root, store, policies.capabilities);
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

    // 4. Signature verification pre-flight
    let sig_violations = verify_signatures(&graph.root, store);
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

    // 5. Effect inference — collect the union of all effects in the composition.
    let inferred = infer_effects(&graph.root, store);
    let inferred_kinds: Vec<String> = inferred.iter().map(|e| e.kind().to_string()).collect();

    // 6. Effect policy pre-flight — block if any inferred effect is not allowed.
    let effect_violations = check_effects(&graph.root, store, policies.effects);
    if !effect_violations.is_empty() {
        let msgs: Vec<String> = effect_violations.iter().map(|v| format!("{v}")).collect();
        eprintln!(
            "{}",
            acli_error_hints(
                &format!("{} effect violation(s)", msgs.len()),
                None,
                Some(msgs),
            )
        );
        std::process::exit(2);
    }

    // 7. Effect warnings (includes budget enforcement if requested)
    let effect_warnings = collect_effect_warnings(&graph.root, store, policies.budget_cents);
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

    // 7a. Optimize — structural rewrites that preserve semantics
    //     (M3, see crates/noether-engine/src/optimizer/). Runs after
    //     type-check so the passes see a well-formed graph, and
    //     before the planner so the plan is built from the rewritten
    //     tree. `composition_id` was computed on the pre-resolution
    //     canonical form (step 1b), so it stays stable across
    //     optimization. Env-gate `NOETHER_NO_OPTIMIZE=1` disables for
    //     anyone who needs the literal graph to reach the executor
    //     (trace debugging, bug repros).
    let opt_report: OptimizerReport = if std::env::var("NOETHER_NO_OPTIMIZE")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    {
        OptimizerReport {
            passes_applied: Vec::new(),
            iterations: 0,
            hit_iteration_cap: false,
        }
    } else {
        // Pass ordering matters: CanonicalStructural first so
        // DeadBranchElimination sees the flattened form and can
        // fold Branches that were hidden inside a collapsible
        // singleton Sequential wrapper.
        let (rewritten, report) = optimizer::optimize(
            graph.root,
            &[&CanonicalStructural, &DeadBranchElimination],
            optimizer::DEFAULT_MAX_ITERATIONS,
        );
        graph.root = rewritten;
        if report.hit_iteration_cap {
            eprintln!(
                "[noether] optimizer hit iteration cap ({} iters, passes touched: {:?}) — \
                 treat the rewritten graph as suspect and re-run with NOETHER_NO_OPTIMIZE=1 to compare",
                report.iterations, report.passes_applied
            );
        }
        report
    };

    // 8. Plan
    let plan = plan_graph(&graph.root, store);

    let warning_strings: Vec<String> = effect_warnings.iter().map(|w| format!("{w}")).collect();

    if dry_run {
        let expected_cost: u64 = if policies.budget_cents.is_some() {
            let cost_map = build_cost_map(&graph.root, store);
            cost_map.values().sum()
        } else {
            0
        };

        let mut resp = json!({
            "mode": "dry-run",
            "composition_id": composition_id,
            "description": graph.description,
            "type_check": {
                "input": format!("{}", check.resolved.input),
                "output": format!("{}", check.resolved.output),
            },
            "effects": inferred_kinds,
            "optimizer": {
                "passes_applied": opt_report.passes_applied,
                "iterations": opt_report.iterations,
                "hit_cap": opt_report.hit_iteration_cap,
            },
            "plan": {
                "steps": plan.steps.len(),
                "parallel_groups": plan.parallel_groups.len(),
                "cost": plan.cost,
            },
            "warnings": warning_strings,
        });
        if expected_cost > 0 {
            resp["expected_cost_cents"] = json!(expected_cost);
        }
        println!("{}", acli_ok(resp));
        return;
    }

    // 9. Execute — wrap with BudgetedExecutor when a budget is set so cost
    //    is tracked and enforced at runtime (not just statically pre-flight).
    //    A PureStageCache is built from the store so repeated (stage, input)
    //    pairs within a single run skip re-execution (M3 memoize_pure).
    //    Opt out via NOETHER_NO_MEMOIZE=1 for benchmarks or bug repros
    //    where the literal dispatch path matters.
    let executor =
        super::executor_builder::build_executor_with_isolation(store, policies.isolation.clone());
    let memoize_disabled = std::env::var("NOETHER_NO_MEMOIZE")
        .ok()
        .is_some_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    let mut pure_cache = if memoize_disabled {
        None
    } else {
        Some(PureStageCache::from_store(store))
    };
    // Refinement enforcement wraps whichever executor chain we end up with,
    // so the check fires once per stage call regardless of how many layers
    // (budget, memoize) sit above it. Opt out with NOETHER_NO_REFINEMENT_CHECK=1
    // for repros where you need the executor to see out-of-band values —
    // `ValidatingExecutor` itself provides the env-var convention.
    let skip_refinement =
        ValidatingExecutor::<noether_engine::executor::composite::CompositeExecutor>::is_disabled();
    let result = if let Some(budget) = policies.budget_cents {
        let cost_map = build_cost_map(&graph.root, store);
        let budgeted = BudgetedExecutor::new(executor, cost_map, budget);
        if skip_refinement {
            let r = run_composition_with_cache(
                &graph.root,
                input,
                &budgeted,
                &composition_id,
                pure_cache.as_mut(),
            );
            r.map(|mut cr| {
                cr.spent_cents = budgeted.spent_cents();
                cr
            })
        } else {
            let validated = ValidatingExecutor::from_store(budgeted, store);
            let r = run_composition_with_cache(
                &graph.root,
                input,
                &validated,
                &composition_id,
                pure_cache.as_mut(),
            );
            r.map(|mut cr| {
                cr.spent_cents = validated.inner().spent_cents();
                cr
            })
        }
    } else if skip_refinement {
        run_composition_with_cache(
            &graph.root,
            input,
            &executor,
            &composition_id,
            pure_cache.as_mut(),
        )
    } else {
        let validated = ValidatingExecutor::from_store(executor, store);
        run_composition_with_cache(
            &graph.root,
            input,
            &validated,
            &composition_id,
            pure_cache.as_mut(),
        )
    };
    let pure_cache_stats = pure_cache
        .as_ref()
        .map(|c| (c.hits, c.misses))
        .unwrap_or((0, 0));

    match result {
        Ok(result) => {
            // Persist the trace so `noether trace <id>` can retrieve it later.
            trace_store.put(result.trace.clone());

            let mut resp = json!({
                "composition_id": composition_id,
                "output": result.output,
                "effects": inferred_kinds,
                "trace": result.trace,
                "warnings": warning_strings,
            });
            if result.spent_cents > 0 {
                resp["spent_cents"] = json!(result.spent_cents);
            }
            // Cache stats are informational — only surface them when
            // they say something useful (i.e. at least one hit or the
            // feature was explicitly disabled). A pure-composition
            // run with zero hits and zero misses would otherwise
            // clutter the envelope with a `memoize` field that
            // conveys nothing.
            if pure_cache_stats.0 > 0 || memoize_disabled {
                resp["memoize"] = json!({
                    "enabled": !memoize_disabled,
                    "hits": pure_cache_stats.0,
                    "misses": pure_cache_stats.1,
                });
            }
            println!("{}", acli_ok(resp));
        }
        Err(noether_engine::executor::ExecutionError::BudgetExceeded {
            spent_cents,
            budget_cents,
        }) => {
            eprintln!(
                "{}",
                acli_error(&format!(
                    "cost budget exceeded at runtime: spent {spent_cents}¢ of {budget_cents}¢"
                ))
            );
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("{}", acli_error(&format!("execution failed: {e}")));
            std::process::exit(3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noether_core::stage::StageId;
    use noether_engine::lagrange::{CompositionNode, Pinning};

    fn dummy_graph() -> CompositionGraph {
        CompositionGraph::new(
            "t",
            CompositionNode::Stage {
                id: StageId("abc".into()),
                pinning: Pinning::Signature,
                config: None,
            },
        )
    }

    // Contract: on hash failure, the CLI surfaces a single ACLI
    // error line mentioning "failed to hash composition graph" and
    // never the pre-fix literal "unknown". The caller then
    // `exit(1)`s; this helper's return value is the full line
    // operators will see on stderr. See PR #40 review for why
    // "mock compute_composition_id to return Err" is the chosen
    // test strategy (serde_jcs-hostile `serde_json::Value`s can't
    // be constructed through `Number::from_f64`).
    #[test]
    fn hash_failure_produces_acli_error_line() {
        let graph = dummy_graph();
        let failing_hasher =
            |_: &CompositionGraph| Err(serde_json::from_str::<()>("not json").unwrap_err());

        let outcome = compute_composition_id_or_error_line(&graph, failing_hasher);
        let line = outcome.expect_err("hash should have failed");

        assert!(
            line.contains("failed to hash composition graph"),
            "line should identify the failure: {line}"
        );
        assert!(
            !line.contains("unknown"),
            "line should not stringly-type to 'unknown': {line}"
        );
        // ACLI error envelope contract: ok=false + error field.
        assert!(
            line.contains("\"ok\":false") || line.contains("\"ok\": false"),
            "expected ACLI error envelope shape: {line}"
        );
        assert!(
            line.contains("\"error\""),
            "expected ACLI error field: {line}"
        );
    }

    #[test]
    fn hash_success_returns_non_empty_non_unknown_id() {
        let graph = dummy_graph();
        let outcome = compute_composition_id_or_error_line(&graph, compute_composition_id);
        let id = outcome.expect("real hasher should succeed on a trivial graph");
        assert!(!id.is_empty());
        assert_ne!(id, "unknown");
    }
}
