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
