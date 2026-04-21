//! Implementations for the polymorphic stdlib stages (M3 slice 3).
//!
//! Rust-native, no Nix. Each function operates on `&serde_json::Value`
//! — at runtime `<T>` is just a JSON value of whatever concrete shape
//! the upstream produced. The type checker ensures the shape is
//! consistent before the graph executes.
//!
//! Mapping to stage descriptions (see `find_implementation` in this
//! module's `mod.rs`):
//!
//! - "Return the input unchanged. Polymorphic: <T> -> <T>." → [`identity`]
//! - "Return the first element of a list. Empty list is a Fallible error." → [`head`]
//! - "Return every element of a list except the first. Empty list -> empty list." → [`tail`]

use crate::executor::ExecutionError;
use noether_core::stage::StageId;
use serde_json::Value;

/// `identity: <T> -> <T>` — pass through.
pub fn identity(input: &Value) -> Result<Value, ExecutionError> {
    Ok(input.clone())
}

/// `head: List<<T>> -> <T>` — first element, or a typed error on an
/// empty list. Fallible-effect: callers wrap in `Retry` or surface
/// the error through their composition.
pub fn head(input: &Value) -> Result<Value, ExecutionError> {
    let arr = input
        .as_array()
        .ok_or_else(|| ExecutionError::StageFailed {
            stage_id: StageId("head".into()),
            message: format!("expected list, got {input}"),
        })?;
    arr.first()
        .cloned()
        .ok_or_else(|| ExecutionError::StageFailed {
            stage_id: StageId("head".into()),
            message: "cannot take head of empty list".into(),
        })
}

/// `tail: List<<T>> -> List<<T>>` — every element except the first.
/// Total: empty input yields empty output, matching the declared example.
pub fn tail(input: &Value) -> Result<Value, ExecutionError> {
    let arr = input
        .as_array()
        .ok_or_else(|| ExecutionError::StageFailed {
            stage_id: StageId("tail".into()),
            message: format!("expected list, got {input}"),
        })?;
    let out: Vec<Value> = arr.iter().skip(1).cloned().collect();
    Ok(Value::Array(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identity_passes_through_any_json_shape() {
        assert_eq!(identity(&json!(42)).unwrap(), json!(42));
        assert_eq!(identity(&json!("hello")).unwrap(), json!("hello"));
        assert_eq!(identity(&json!([1, 2, 3])).unwrap(), json!([1, 2, 3]));
        assert_eq!(identity(&json!({"a": 1})).unwrap(), json!({"a": 1}));
        assert_eq!(identity(&json!(null)).unwrap(), json!(null));
    }

    #[test]
    fn head_of_non_empty_list() {
        assert_eq!(head(&json!([1, 2, 3])).unwrap(), json!(1));
        assert_eq!(head(&json!(["a", "b"])).unwrap(), json!("a"));
        assert_eq!(head(&json!([null])).unwrap(), json!(null));
    }

    #[test]
    fn head_of_empty_list_is_stage_failed() {
        let err = head(&json!([])).unwrap_err();
        assert!(matches!(err, ExecutionError::StageFailed { .. }));
    }

    #[test]
    fn head_of_non_list_is_stage_failed() {
        let err = head(&json!(42)).unwrap_err();
        assert!(matches!(err, ExecutionError::StageFailed { .. }));
    }

    #[test]
    fn tail_of_non_empty_list() {
        assert_eq!(tail(&json!([1, 2, 3])).unwrap(), json!([2, 3]));
        assert_eq!(tail(&json!(["a", "b"])).unwrap(), json!(["b"]));
        assert_eq!(tail(&json!([true])).unwrap(), json!([]));
    }

    #[test]
    fn tail_of_empty_list_is_empty_list() {
        assert_eq!(tail(&json!([])).unwrap(), json!([]));
    }
}
