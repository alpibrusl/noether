use crate::output::{acli_error, acli_ok};
use noether_engine::agent::CompositionAgent;
use noether_engine::checker::check_graph;
use noether_engine::executor::composite::CompositeExecutor;
use noether_engine::executor::runner::run_composition;
use noether_engine::index::SemanticIndex;
use noether_engine::lagrange::{compute_composition_id, serialize_graph};
use noether_engine::llm::{LlmConfig, LlmProvider};
use noether_engine::planner::plan_graph;
use noether_engine::providers;
use noether_store::StageStore;
use serde_json::json;

pub fn cmd_compose(
    store: &mut dyn StageStore,
    index: &mut SemanticIndex,
    llm: &dyn LlmProvider,
    problem: &str,
    model: &str,
    dry_run: bool,
    input: &serde_json::Value,
) {
    let llm_config = LlmConfig {
        model: model.into(),
        max_tokens: 4096,
        temperature: 0.2,
    };

    let mut agent = CompositionAgent::new(index, llm, llm_config, 3);

    let result = match agent.compose(problem, store) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{}", acli_error(&format!("composition failed: {e}")));
            std::process::exit(2);
        }
    };

    let graph = &result.graph;
    let composition_id = compute_composition_id(graph).unwrap_or_else(|_| "unknown".into());
    let graph_json = serialize_graph(graph).unwrap_or_else(|_| "{}".into());

    let synthesized_json: Vec<serde_json::Value> = result
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

    // Type check (should pass since agent validates, but confirm)
    let resolved = check_graph(&graph.root, store).ok();
    let plan = plan_graph(&graph.root, store);

    if dry_run {
        println!(
            "{}",
            acli_ok(json!({
                "mode": "dry-run",
                "composition_id": composition_id,
                "attempts": result.attempts,
                "synthesized": synthesized_json,
                "graph": serde_json::from_str::<serde_json::Value>(&graph_json).unwrap_or(json!(null)),
                "type_check": resolved.as_ref().map(|r| json!({
                    "input": format!("{}", r.input),
                    "output": format!("{}", r.output),
                })),
                "plan": {
                    "steps": plan.steps.len(),
                    "parallel_groups": plan.parallel_groups.len(),
                    "cost": plan.cost,
                },
            }))
        );
        return;
    }

    // Build executor — CompositeExecutor picks up synthesized stages from the store,
    // then we register any freshly synthesized stages for immediate execution.
    // Also wire in the LLM so llm_* stdlib stages actually run.
    let mut executor = CompositeExecutor::from_store(store).with_llm(
        providers::build_llm_provider().0,
        LlmConfig {
            model: model.into(),
            max_tokens: 4096,
            temperature: 0.2,
        },
    );
    for syn in &result.synthesized {
        executor.register_synthesized(&syn.stage_id, &syn.implementation, &syn.language);
    }

    if !result.synthesized.is_empty() && !executor.nix_available() {
        eprintln!("Warning: synthesized stages will use fallback execution (nix not available).");
    }

    match run_composition(&graph.root, input, &executor, &composition_id) {
        Ok(exec_result) => {
            println!(
                "{}",
                acli_ok(json!({
                    "composition_id": composition_id,
                    "attempts": result.attempts,
                    "synthesized": synthesized_json,
                    "graph": serde_json::from_str::<serde_json::Value>(&graph_json).unwrap_or(json!(null)),
                    "output": exec_result.output,
                    "trace": exec_result.trace,
                }))
            );
        }
        Err(e) => {
            eprintln!("{}", acli_error(&format!("execution failed: {e}")));
            std::process::exit(3);
        }
    }
}
