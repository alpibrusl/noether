use super::resolver_utils::resolve_and_emit_diagnostics;
use crate::output::{acli_error, acli_error_hints, acli_ok};
use noether_engine::checker::{
    check_capabilities, check_effects, check_graph, collect_effect_warnings, infer_effects,
    verify_signatures, CapabilityPolicy, EffectPolicy,
};
use noether_engine::executor::budget::{build_cost_map, BudgetedExecutor};
use noether_engine::executor::runner::run_composition;
use noether_engine::lagrange::{compute_composition_id, parse_graph, resolve_stage_prefixes};
use noether_engine::planner::plan_graph;
use noether_engine::trace::JsonFileTraceStore;
use noether_store::StageStore;
use serde_json::json;

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
    let composition_id = match compute_composition_id(&graph) {
        Ok(id) => id,
        Err(e) => {
            eprintln!(
                "{}",
                acli_error(&format!("failed to hash composition graph: {e}"))
            );
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
    let executor =
        super::executor_builder::build_executor_with_isolation(store, policies.isolation.clone());
    let result = if let Some(budget) = policies.budget_cents {
        let cost_map = build_cost_map(&graph.root, store);
        let budgeted = BudgetedExecutor::new(executor, cost_map, budget);
        let r = run_composition(&graph.root, input, &budgeted, &composition_id);
        r.map(|mut cr| {
            cr.spent_cents = budgeted.spent_cents();
            cr
        })
    } else {
        run_composition(&graph.root, input, &executor, &composition_id)
    };

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
