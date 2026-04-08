use crate::output::{acli_error, acli_error_hints, acli_ok};
use noether_engine::checker::{
    check_capabilities, check_effects, check_graph, collect_effect_warnings, infer_effects,
    verify_signatures, CapabilityPolicy, EffectPolicy,
};
use noether_engine::executor::runner::run_composition;
use noether_engine::lagrange::{compute_composition_id, parse_graph};
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

    let graph = match parse_graph(&content) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{}", acli_error(&format!("invalid graph JSON: {e}")));
            std::process::exit(1);
        }
    };

    let composition_id = compute_composition_id(&graph).unwrap_or_else(|_| "unknown".into());

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
        println!(
            "{}",
            acli_ok(json!({
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
            }))
        );
        return;
    }

    // 9. Execute — use CompositeExecutor so synthesized stages run via Nix.
    let executor = super::executor_builder::build_executor(store);
    match run_composition(&graph.root, input, &executor, &composition_id) {
        Ok(result) => {
            // Persist the trace so `noether trace <id>` can retrieve it later.
            trace_store.put(result.trace.clone());

            println!(
                "{}",
                acli_ok(json!({
                    "composition_id": composition_id,
                    "output": result.output,
                    "effects": inferred_kinds,
                    "trace": result.trace,
                    "warnings": warning_strings,
                }))
            );
        }
        Err(e) => {
            eprintln!("{}", acli_error(&format!("execution failed: {e}")));
            std::process::exit(3);
        }
    }
}
