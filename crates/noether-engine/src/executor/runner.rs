use super::{ExecutionError, StageExecutor};
use crate::executor::pure_cache::PureStageCache;
use crate::lagrange::CompositionNode;
use crate::trace::{CompositionTrace, StageStatus, StageTrace, TraceStatus};
use chrono::Utc;
use noether_core::stage::StageId;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::time::Instant;

/// Result of executing a composition graph.
#[derive(Debug)]
pub struct CompositionResult {
    pub output: Value,
    pub trace: CompositionTrace,
    /// Actual cost consumed during this run in cents (sum of declared
    /// `Effect::Cost` for every stage that executed). Zero when no budget
    /// tracking was requested.
    pub spent_cents: u64,
}

/// Execute a composition graph using the provided executor.
///
/// Pass a `PureStageCache` to enable Pure-stage output caching within this run.
pub fn run_composition<E: StageExecutor + Sync>(
    node: &CompositionNode,
    input: &Value,
    executor: &E,
    composition_id: &str,
) -> Result<CompositionResult, ExecutionError> {
    run_composition_with_cache(node, input, executor, composition_id, None)
}

/// Like `run_composition` but accepts an explicit `PureStageCache`.
pub fn run_composition_with_cache<E: StageExecutor + Sync>(
    node: &CompositionNode,
    input: &Value,
    executor: &E,
    composition_id: &str,
    cache: Option<&mut PureStageCache>,
) -> Result<CompositionResult, ExecutionError> {
    let start = Instant::now();
    let mut stage_traces = Vec::new();
    let mut step_counter = 0;

    let mut owned_cache;
    let cache_ref: &mut Option<&mut PureStageCache>;
    let mut none_holder: Option<&mut PureStageCache> = None;

    if let Some(c) = cache {
        owned_cache = Some(c);
        cache_ref = &mut owned_cache;
    } else {
        cache_ref = &mut none_holder;
    }

    let output = execute_node(
        node,
        input,
        executor,
        &mut stage_traces,
        &mut step_counter,
        cache_ref,
    )?;

    let duration_ms = start.elapsed().as_millis() as u64;
    let has_failures = stage_traces
        .iter()
        .any(|t| matches!(t.status, StageStatus::Failed { .. }));

    let trace = CompositionTrace {
        composition_id: composition_id.into(),
        started_at: Utc::now().to_rfc3339(),
        duration_ms,
        status: if has_failures {
            TraceStatus::Failed
        } else {
            TraceStatus::Ok
        },
        stages: stage_traces,
        security_events: Vec::new(),
        warnings: Vec::new(),
    };

    Ok(CompositionResult {
        output,
        trace,
        spent_cents: 0,
    })
}

fn execute_node<E: StageExecutor + Sync>(
    node: &CompositionNode,
    input: &Value,
    executor: &E,
    traces: &mut Vec<StageTrace>,
    step_counter: &mut usize,
    cache: &mut Option<&mut PureStageCache>,
) -> Result<Value, ExecutionError> {
    match node {
        CompositionNode::Stage {
            id,
            pinning: _, // resolved upstream by checker / planner
            config,
        } => {
            let merged = if let Some(cfg) = config {
                let mut obj = match input {
                    Value::Object(map) => map.clone(),
                    other => {
                        let mut m = serde_json::Map::new();
                        let data_key = [
                            "items", "text", "data", "input", "records", "train", "document",
                            "html", "csv", "json_str",
                        ]
                        .iter()
                        .find(|k| !cfg.contains_key(**k))
                        .unwrap_or(&"items");
                        m.insert(data_key.to_string(), other.clone());
                        m
                    }
                };
                for (k, v) in cfg {
                    obj.insert(k.clone(), v.clone());
                }
                Value::Object(obj)
            } else {
                input.clone()
            };
            execute_stage(id, &merged, executor, traces, step_counter, cache)
        }
        CompositionNode::Const { value } => Ok(value.clone()),
        CompositionNode::Sequential { stages } => {
            let mut current = input.clone();
            for stage in stages {
                current = execute_node(stage, &current, executor, traces, step_counter, cache)?;
            }
            Ok(current)
        }
        CompositionNode::Parallel { branches } => {
            // Resolve each branch's input before spawning (pure field lookup).
            // If the input is a Record containing the branch name as a key,
            // that field's value is passed to the branch. Otherwise the full
            // input is passed — this lets Stage branches receive the pipeline
            // input naturally while Const branches ignore it entirely.
            let branch_data: Vec<(&str, &CompositionNode, Value)> = branches
                .iter()
                .map(|(name, branch)| {
                    let branch_input = if let Value::Object(ref obj) = input {
                        obj.get(name).cloned().unwrap_or_else(|| input.clone())
                    } else {
                        input.clone()
                    };
                    (name.as_str(), branch, branch_input)
                })
                .collect();

            // Execute all branches concurrently. Each branch gets its own
            // trace list; the Pure cache is NOT shared across parallel branches
            // to avoid any locking overhead.
            let branch_results = std::thread::scope(|s| {
                let handles: Vec<_> = branch_data
                    .iter()
                    .map(|(name, branch, branch_input)| {
                        s.spawn(move || {
                            let mut branch_traces = Vec::new();
                            let mut branch_counter = 0usize;
                            let result = execute_node(
                                branch,
                                branch_input,
                                executor,
                                &mut branch_traces,
                                &mut branch_counter,
                                &mut None,
                            );
                            (*name, result, branch_traces)
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| h.join().expect("parallel branch panicked"))
                    .collect::<Vec<_>>()
            });

            let mut output_fields = serde_json::Map::new();
            for (name, result, branch_traces) in branch_results {
                let branch_output = result?;
                output_fields.insert(name.to_string(), branch_output);
                traces.extend(branch_traces);
            }
            Ok(Value::Object(output_fields))
        }
        CompositionNode::Branch {
            predicate,
            if_true,
            if_false,
        } => {
            let pred_result =
                execute_node(predicate, input, executor, traces, step_counter, cache)?;
            let condition = match &pred_result {
                Value::Bool(b) => *b,
                _ => false,
            };
            if condition {
                execute_node(if_true, input, executor, traces, step_counter, cache)
            } else {
                execute_node(if_false, input, executor, traces, step_counter, cache)
            }
        }
        CompositionNode::Fanout { source, targets } => {
            let source_output = execute_node(source, input, executor, traces, step_counter, cache)?;
            let mut results = Vec::new();
            for target in targets {
                let result = execute_node(
                    target,
                    &source_output,
                    executor,
                    traces,
                    step_counter,
                    cache,
                )?;
                results.push(result);
            }
            Ok(Value::Array(results))
        }
        CompositionNode::Merge { sources, target } => {
            let mut merged = serde_json::Map::new();
            for (i, source) in sources.iter().enumerate() {
                let source_input = if let Value::Object(ref obj) = input {
                    obj.get(&format!("source_{i}"))
                        .cloned()
                        .unwrap_or(Value::Null)
                } else {
                    input.clone()
                };
                let result =
                    execute_node(source, &source_input, executor, traces, step_counter, cache)?;
                merged.insert(format!("source_{i}"), result);
            }
            execute_node(
                target,
                &Value::Object(merged),
                executor,
                traces,
                step_counter,
                cache,
            )
        }
        CompositionNode::Retry {
            stage,
            max_attempts,
            ..
        } => {
            let mut last_err = None;
            for _ in 0..*max_attempts {
                match execute_node(stage, input, executor, traces, step_counter, cache) {
                    Ok(output) => return Ok(output),
                    Err(e) => last_err = Some(e),
                }
            }
            Err(last_err.unwrap_or(ExecutionError::RetryExhausted {
                stage_id: StageId("unknown".into()),
                attempts: *max_attempts,
            }))
        }
        CompositionNode::RemoteStage { url, .. } => execute_remote_stage(url, input),
        CompositionNode::Let { bindings, body } => {
            // Execute bindings concurrently — each receives the outer input.
            // Then merge: outer-input record fields + binding name → output.
            let bindings_vec: Vec<(&str, &CompositionNode)> =
                bindings.iter().map(|(n, b)| (n.as_str(), b)).collect();

            let binding_results = std::thread::scope(|s| {
                let handles: Vec<_> = bindings_vec
                    .iter()
                    .map(|(name, node)| {
                        s.spawn(move || {
                            let mut bt = Vec::new();
                            let mut bc = 0usize;
                            let r =
                                execute_node(node, input, executor, &mut bt, &mut bc, &mut None);
                            (*name, r, bt)
                        })
                    })
                    .collect();
                handles
                    .into_iter()
                    .map(|h| h.join().expect("Let binding panicked"))
                    .collect::<Vec<_>>()
            });

            // Start the merged record from the outer input (when it is one).
            let mut merged = match input {
                Value::Object(map) => map.clone(),
                _ => serde_json::Map::new(),
            };
            for (name, result, branch_traces) in binding_results {
                let value = result?;
                merged.insert(name.to_string(), value);
                traces.extend(branch_traces);
            }

            let body_input = Value::Object(merged);
            execute_node(body, &body_input, executor, traces, step_counter, cache)
        }
    }
}

fn execute_stage<E: StageExecutor + Sync>(
    id: &StageId,
    input: &Value,
    executor: &E,
    traces: &mut Vec<StageTrace>,
    step_counter: &mut usize,
    cache: &mut Option<&mut PureStageCache>,
) -> Result<Value, ExecutionError> {
    let step_index = *step_counter;
    *step_counter += 1;
    let start = Instant::now();

    let input_hash = hash_value(input);

    // Pure cache check: skip execution if we have a cached output for this stage + input.
    if let Some(ref mut c) = cache {
        if let Some(cached_output) = c.get(id, input) {
            let output = cached_output.clone();
            let duration_ms = start.elapsed().as_millis() as u64;
            traces.push(StageTrace {
                stage_id: id.clone(),
                step_index,
                status: StageStatus::Ok,
                duration_ms,
                input_hash: Some(input_hash),
                output_hash: Some(hash_value(&output)),
            });
            return Ok(output);
        }
    }

    match executor.execute(id, input) {
        Ok(output) => {
            let output_hash = hash_value(&output);
            let duration_ms = start.elapsed().as_millis() as u64;
            traces.push(StageTrace {
                stage_id: id.clone(),
                step_index,
                status: StageStatus::Ok,
                duration_ms,
                input_hash: Some(input_hash),
                output_hash: Some(output_hash),
            });
            // Store result in Pure cache for future calls within this run.
            if let Some(ref mut c) = cache {
                c.put(id, input, output.clone());
            }
            Ok(output)
        }
        Err(e) => {
            let duration_ms = start.elapsed().as_millis() as u64;
            traces.push(StageTrace {
                stage_id: id.clone(),
                step_index,
                status: StageStatus::Failed {
                    code: "EXECUTION_ERROR".into(),
                    message: format!("{e}"),
                },
                duration_ms,
                input_hash: Some(input_hash),
                output_hash: None,
            });
            Err(e)
        }
    }
}

fn hash_value(value: &Value) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let hash = Sha256::digest(&bytes);
    hex::encode(hash)
}

/// Execute a remote Noether API call via HTTP POST.
///
/// Sends `{"input": <value>}` to `url` and extracts the output from the
/// ACLI response envelope `{"data": {"output": <value>}}`.
///
/// In native builds this uses `reqwest::blocking`. In WASM builds this
/// function returns an error — remote calls are handled by the JS runtime.
fn execute_remote_stage(url: &str, input: &Value) -> Result<Value, ExecutionError> {
    #[cfg(feature = "native")]
    {
        use reqwest::blocking::Client;

        let client = Client::new();
        let body = serde_json::json!({ "input": input });
        let resp =
            client
                .post(url)
                .json(&body)
                .send()
                .map_err(|e| ExecutionError::RemoteCallFailed {
                    url: url.to_string(),
                    reason: e.to_string(),
                })?;

        let resp_json: Value = resp.json().map_err(|e| ExecutionError::RemoteCallFailed {
            url: url.to_string(),
            reason: format!("invalid JSON response: {e}"),
        })?;

        // ACLI envelope: {"ok": true, "data": {"output": ...}} on success,
        // {"ok": false, "error": "..."} on failure. Check `ok` first so a
        // worker-side error (e.g. stage not found) surfaces verbatim
        // instead of being masked as "missing data.output".
        if resp_json.get("ok") == Some(&Value::Bool(false)) {
            let reason = resp_json
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("remote reported ok=false without error message")
                .to_string();
            return Err(ExecutionError::RemoteCallFailed {
                url: url.to_string(),
                reason,
            });
        }
        resp_json
            .get("data")
            .and_then(|d| d.get("output"))
            .cloned()
            .ok_or_else(|| ExecutionError::RemoteCallFailed {
                url: url.to_string(),
                reason: "response missing data.output field".to_string(),
            })
    }
    #[cfg(not(feature = "native"))]
    {
        let _ = (url, input);
        Err(ExecutionError::RemoteCallFailed {
            url: url.to_string(),
            reason: "remote calls are handled by the JS runtime in WASM builds".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::mock::MockExecutor;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn stage(id: &str) -> CompositionNode {
        CompositionNode::Stage {
            id: StageId(id.into()),
            pinning: crate::lagrange::Pinning::Signature,
            config: None,
        }
    }

    #[test]
    fn run_single_stage() {
        let executor = MockExecutor::new().with_output(&StageId("a".into()), json!(42));
        let result = run_composition(&stage("a"), &json!("input"), &executor, "test_comp").unwrap();
        assert_eq!(result.output, json!(42));
        assert_eq!(result.trace.stages.len(), 1);
        assert!(matches!(result.trace.status, TraceStatus::Ok));
    }

    #[test]
    fn run_sequential() {
        let executor = MockExecutor::new()
            .with_output(&StageId("a".into()), json!("mid"))
            .with_output(&StageId("b".into()), json!("final"));
        let node = CompositionNode::Sequential {
            stages: vec![stage("a"), stage("b")],
        };
        let result = run_composition(&node, &json!("start"), &executor, "test").unwrap();
        assert_eq!(result.output, json!("final"));
        assert_eq!(result.trace.stages.len(), 2);
    }

    #[test]
    fn run_parallel() {
        let executor = MockExecutor::new()
            .with_output(&StageId("s1".into()), json!("r1"))
            .with_output(&StageId("s2".into()), json!("r2"));
        let node = CompositionNode::Parallel {
            branches: BTreeMap::from([("left".into(), stage("s1")), ("right".into(), stage("s2"))]),
        };
        let result = run_composition(&node, &json!({}), &executor, "test").unwrap();
        assert_eq!(result.output, json!({"left": "r1", "right": "r2"}));
    }

    #[test]
    fn run_branch_true() {
        let executor = MockExecutor::new()
            .with_output(&StageId("pred".into()), json!(true))
            .with_output(&StageId("yes".into()), json!("YES"))
            .with_output(&StageId("no".into()), json!("NO"));
        let node = CompositionNode::Branch {
            predicate: Box::new(stage("pred")),
            if_true: Box::new(stage("yes")),
            if_false: Box::new(stage("no")),
        };
        let result = run_composition(&node, &json!("input"), &executor, "test").unwrap();
        assert_eq!(result.output, json!("YES"));
    }

    #[test]
    fn run_branch_false() {
        let executor = MockExecutor::new()
            .with_output(&StageId("pred".into()), json!(false))
            .with_output(&StageId("yes".into()), json!("YES"))
            .with_output(&StageId("no".into()), json!("NO"));
        let node = CompositionNode::Branch {
            predicate: Box::new(stage("pred")),
            if_true: Box::new(stage("yes")),
            if_false: Box::new(stage("no")),
        };
        let result = run_composition(&node, &json!("input"), &executor, "test").unwrap();
        assert_eq!(result.output, json!("NO"));
    }

    #[test]
    fn run_fanout() {
        let executor = MockExecutor::new()
            .with_output(&StageId("src".into()), json!("data"))
            .with_output(&StageId("t1".into()), json!("r1"))
            .with_output(&StageId("t2".into()), json!("r2"));
        let node = CompositionNode::Fanout {
            source: Box::new(stage("src")),
            targets: vec![stage("t1"), stage("t2")],
        };
        let result = run_composition(&node, &json!("in"), &executor, "test").unwrap();
        assert_eq!(result.output, json!(["r1", "r2"]));
    }

    #[test]
    fn trace_has_input_output_hashes() {
        let executor = MockExecutor::new().with_output(&StageId("a".into()), json!(42));
        let result = run_composition(&stage("a"), &json!("input"), &executor, "test").unwrap();
        assert!(result.trace.stages[0].input_hash.is_some());
        assert!(result.trace.stages[0].output_hash.is_some());
    }
}
