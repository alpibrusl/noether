use crate::output::{acli_error, acli_ok};
use noether_engine::checker::check_graph;
use noether_engine::executor::composite::CompositeExecutor;
use noether_engine::executor::runner::run_composition;
use noether_engine::lagrange::{compute_composition_id, parse_graph};
use noether_engine::planner::plan_graph;
use noether_store::StageStore;
use serde_json::json;

pub fn cmd_run(
    store: &impl StageStore,
    graph_path: &str,
    dry_run: bool,
    input: &serde_json::Value,
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
    let resolved = type_result.unwrap();

    // 3. Plan
    let plan = plan_graph(&graph.root, store);

    if dry_run {
        println!(
            "{}",
            acli_ok(json!({
                "mode": "dry-run",
                "composition_id": composition_id,
                "description": graph.description,
                "type_check": {
                    "input": format!("{}", resolved.input),
                    "output": format!("{}", resolved.output),
                },
                "plan": {
                    "steps": plan.steps.len(),
                    "parallel_groups": plan.parallel_groups.len(),
                    "cost": plan.cost,
                },
            }))
        );
        return;
    }

    // 4. Execute — use CompositeExecutor so synthesized stages run via Nix.
    let executor = CompositeExecutor::from_store(store);
    match run_composition(&graph.root, input, &executor, &composition_id) {
        Ok(result) => {
            println!(
                "{}",
                acli_ok(json!({
                    "composition_id": composition_id,
                    "output": result.output,
                    "trace": result.trace,
                }))
            );
        }
        Err(e) => {
            eprintln!("{}", acli_error(&format!("execution failed: {e}")));
            std::process::exit(3);
        }
    }
}
