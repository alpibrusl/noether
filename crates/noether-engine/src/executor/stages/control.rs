use crate::executor::{ExecutionError, StageExecutor};
use noether_core::stage::StageId;
use noether_core::types::{is_subtype_of, NType, TypeCompatibility};
use serde_json::{json, Value};

fn fail(stage: &str, msg: impl Into<String>) -> ExecutionError {
    ExecutionError::StageFailed {
        stage_id: StageId(stage.into()),
        message: msg.into(),
    }
}

/// Select between if_true / if_false based on a boolean condition.
pub fn branch(input: &Value) -> Result<Value, ExecutionError> {
    let cond = input
        .get("condition")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| fail("branch", "missing boolean field 'condition'"))?;
    let chosen = if cond {
        input.get("if_true")
    } else {
        input.get("if_false")
    };
    Ok(chosen.cloned().unwrap_or(Value::Null))
}

/// Check structural subtyping between two NType descriptions.
/// Input: {sub: Any, sup: Any}  (types as serialized NType JSON values)
pub fn is_subtype(input: &Value) -> Result<Value, ExecutionError> {
    let sub_val = input
        .get("sub")
        .ok_or_else(|| fail("is_subtype", "missing field 'sub'"))?;
    let sup_val = input
        .get("sup")
        .ok_or_else(|| fail("is_subtype", "missing field 'sup'"))?;

    let sub: NType = serde_json::from_value(sub_val.clone())
        .map_err(|e| fail("is_subtype", format!("invalid 'sub' type: {e}")))?;
    let sup: NType = serde_json::from_value(sup_val.clone())
        .map_err(|e| fail("is_subtype", format!("invalid 'sup' type: {e}")))?;

    let result = is_subtype_of(&sub, &sup);
    Ok(json!({
        "compatible": matches!(result, TypeCompatibility::Compatible),
        "reason": null,
    }))
}

/// Try a list of stage IDs in order; return first success.
/// Delegates execution back to the parent executor.
pub fn fallback<E: StageExecutor>(executor: &E, input: &Value) -> Result<Value, ExecutionError> {
    let stage_ids = input
        .get("stages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| fail("fallback", "missing array field 'stages'"))?;
    let inner_input = input
        .get("input")
        .ok_or_else(|| fail("fallback", "missing field 'input'"))?;

    let mut last_err: Option<ExecutionError> = None;
    for sid_val in stage_ids {
        let sid = sid_val
            .as_str()
            .ok_or_else(|| fail("fallback", "stage IDs must be strings"))?;
        match executor.execute(&StageId(sid.into()), inner_input) {
            Ok(out) => return Ok(out),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| fail("fallback", "no stages provided")))
}

/// Run N stages on N inputs concurrently (sequentially in this impl).
pub fn parallel_n<E: StageExecutor>(executor: &E, input: &Value) -> Result<Value, ExecutionError> {
    let stages = input
        .get("stages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| fail("parallel", "missing array field 'stages'"))?;
    let inputs = input
        .get("inputs")
        .and_then(|v| v.as_array())
        .ok_or_else(|| fail("parallel", "missing array field 'inputs'"))?;

    if stages.len() != inputs.len() {
        return Err(fail(
            "parallel",
            format!(
                "stages ({}) and inputs ({}) must have the same length",
                stages.len(),
                inputs.len()
            ),
        ));
    }

    let results: Result<Vec<Value>, ExecutionError> = stages
        .iter()
        .zip(inputs.iter())
        .map(|(sid_val, inp)| {
            let sid = sid_val
                .as_str()
                .ok_or_else(|| fail("parallel", "stage IDs must be strings"))?;
            executor.execute(&StageId(sid.into()), inp)
        })
        .collect();
    Ok(Value::Array(results?))
}

/// Retry a stage up to `max_attempts` times with an optional `delay_ms` between attempts.
/// Input: { stage_id: Text, input: Any, max_attempts: Number, delay_ms: Number? }
pub fn retry_hof<E: StageExecutor>(executor: &E, input: &Value) -> Result<Value, ExecutionError> {
    let stage_id = input["stage_id"]
        .as_str()
        .ok_or_else(|| fail("retry", "missing string field 'stage_id'"))?;
    let inner_input = input
        .get("input")
        .ok_or_else(|| fail("retry", "missing field 'input'"))?;
    let max_attempts = input["max_attempts"]
        .as_u64()
        .unwrap_or(1)
        .max(1) as usize;
    let delay_ms = input["delay_ms"].as_u64().unwrap_or(0);

    let sid = StageId(stage_id.into());
    let mut last_err: Option<ExecutionError> = None;

    for attempt in 0..max_attempts {
        if attempt > 0 && delay_ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(delay_ms));
        }
        match executor.execute(&sid, inner_input) {
            Ok(out) => return Ok(out),
            Err(e) => last_err = Some(e),
        }
    }

    let _ = last_err; // consumed by RetryExhausted below
    Err(ExecutionError::RetryExhausted {
        stage_id: sid,
        attempts: max_attempts as u32,
    })
}

/// Run a stage with a deadline. Fails with a timeout error if it exceeds `timeout_ms`.
/// Input: { stage_id: Text, input: Any, timeout_ms: Number }
///
/// Measures wall-clock time on the calling thread. This is correct for synchronous
/// executors. The check fires *after* the stage returns — it does not interrupt
/// a running stage.
pub fn timeout_hof<E: StageExecutor>(
    executor: &E,
    input: &Value,
) -> Result<Value, ExecutionError> {
    let stage_id = input["stage_id"]
        .as_str()
        .ok_or_else(|| fail("timeout", "missing string field 'stage_id'"))?;
    let inner_input = input
        .get("input")
        .cloned()
        .ok_or_else(|| fail("timeout", "missing field 'input'"))?;
    let timeout_ms = input["timeout_ms"].as_u64().unwrap_or(5000);

    let sid = StageId(stage_id.into());
    let sid_clone = sid.clone();

    // We can't clone the executor in general, so we box it behind a reference and
    // use a scoped approach: run on the current thread if possible, else fall back
    // to a best-effort timeout check.
    //
    // For now, execute directly and measure wall time. This is correct for single-
    // threaded workloads and test harnesses. A future version can use rayon or tokio.
    let start = std::time::Instant::now();
    let result = executor.execute(&sid_clone, &inner_input);
    let elapsed = start.elapsed().as_millis() as u64;

    if elapsed > timeout_ms {
        return Err(ExecutionError::StageFailed {
            stage_id: sid,
            message: format!(
                "stage exceeded timeout of {}ms (took {}ms)",
                timeout_ms, elapsed
            ),
        });
    }

    result
}

/// Run multiple stage IDs with the same input; return the first that succeeds.
/// Input: { stages: List<Text>, input: Any }
pub fn race_hof<E: StageExecutor>(executor: &E, input: &Value) -> Result<Value, ExecutionError> {
    let stage_ids = input
        .get("stages")
        .and_then(|v| v.as_array())
        .ok_or_else(|| fail("race", "missing array field 'stages'"))?;
    let inner_input = input
        .get("input")
        .ok_or_else(|| fail("race", "missing field 'input'"))?;

    if stage_ids.is_empty() {
        return Err(fail("race", "no stages provided"));
    }

    let mut last_err: Option<ExecutionError> = None;
    for sid_val in stage_ids {
        let sid = sid_val
            .as_str()
            .ok_or_else(|| fail("race", "stage IDs must be strings"))?;
        match executor.execute(&StageId(sid.into()), inner_input) {
            Ok(out) => return Ok(out),
            Err(e) => last_err = Some(e),
        }
    }

    Err(last_err.unwrap_or_else(|| fail("race", "all stages failed")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::{ExecutionError, StageExecutor};
    use crate::executor::mock::MockExecutor;
    use noether_core::stage::StageId;
    use serde_json::json;

    fn ok_executor() -> MockExecutor {
        MockExecutor::default()
    }

    #[test]
    fn retry_succeeds_on_first_attempt() {
        let exec = ok_executor();
        let result = retry_hof(
            &exec,
            &json!({"stage_id": "any", "input": "x", "max_attempts": 3, "delay_ms": null}),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn retry_exhausted_returns_error() {
        struct AlwaysFail;
        impl StageExecutor for AlwaysFail {
            fn execute(
                &self,
                id: &StageId,
                _input: &serde_json::Value,
            ) -> Result<serde_json::Value, ExecutionError> {
                Err(ExecutionError::StageFailed {
                    stage_id: id.clone(),
                    message: "forced fail".into(),
                })
            }
        }
        let result = retry_hof(
            &AlwaysFail,
            &json!({"stage_id": "s", "input": null, "max_attempts": 2, "delay_ms": null}),
        );
        assert!(matches!(result, Err(ExecutionError::RetryExhausted { .. })));
    }

    #[test]
    fn race_returns_first_success() {
        let exec = ok_executor();
        let result = race_hof(
            &exec,
            &json!({"stages": ["a", "b"], "input": "x"}),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn race_empty_stages_fails() {
        let exec = ok_executor();
        let result = race_hof(&exec, &json!({"stages": [], "input": "x"}));
        assert!(result.is_err());
    }

    #[test]
    fn timeout_within_deadline_passes() {
        let exec = ok_executor();
        let result = timeout_hof(
            &exec,
            &json!({"stage_id": "fast", "input": null, "timeout_ms": 5000}),
        );
        assert!(result.is_ok());
    }
}
