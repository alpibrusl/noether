use crate::executor::ExecutionError;
use noether_core::stage::StageId;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

fn fail(stage: &str, msg: impl Into<String>) -> ExecutionError {
    ExecutionError::StageFailed {
        stage_id: StageId(stage.into()),
        message: msg.into(),
    }
}

pub fn sort(input: &Value) -> Result<Value, ExecutionError> {
    // Accept either a bare List<Any> or Record{items, key?, descending?}
    let (items, key, descending) = if let Some(arr) = input.as_array() {
        (arr, None, false)
    } else {
        let items = input
            .get("items")
            .and_then(|v| v.as_array())
            .ok_or_else(|| {
                fail(
                    "sort",
                    "input must be an array or Record{items, key?, descending?}",
                )
            })?;
        let key = input.get("key").and_then(|v| v.as_str());
        let descending = input
            .get("descending")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        (items, key, descending)
    };

    let mut sorted = items.clone();
    sorted.sort_by(|a, b| {
        let va = if let Some(k) = key {
            a.get(k).unwrap_or(a)
        } else {
            a
        };
        let vb = if let Some(k) = key {
            b.get(k).unwrap_or(b)
        } else {
            b
        };
        let cmp = compare_values(va, vb);
        if descending {
            cmp.reverse()
        } else {
            cmp
        }
    });
    Ok(Value::Array(sorted))
}

fn compare_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Number(a), Value::Number(b)) => a
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&b.as_f64().unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal),
        (Value::String(a), Value::String(b)) => a.cmp(b),
        (Value::Bool(a), Value::Bool(b)) => a.cmp(b),
        _ => std::cmp::Ordering::Equal,
    }
}

pub fn flatten(input: &Value) -> Result<Value, ExecutionError> {
    let outer = input
        .as_array()
        .ok_or_else(|| fail("flatten", "input must be an array of arrays"))?;
    let mut result = Vec::new();
    for inner in outer {
        if let Some(arr) = inner.as_array() {
            result.extend(arr.iter().cloned());
        } else {
            result.push(inner.clone());
        }
    }
    Ok(Value::Array(result))
}

pub fn zip(input: &Value) -> Result<Value, ExecutionError> {
    let left = input
        .get("left")
        .and_then(|v| v.as_array())
        .ok_or_else(|| fail("zip", "left must be an array"))?;
    let right = input
        .get("right")
        .and_then(|v| v.as_array())
        .ok_or_else(|| fail("zip", "right must be an array"))?;

    let pairs: Vec<Value> = left
        .iter()
        .zip(right.iter())
        .map(|(l, r)| json!({"left": l, "right": r}))
        .collect();
    Ok(Value::Array(pairs))
}

pub fn take(input: &Value) -> Result<Value, ExecutionError> {
    let items = input
        .get("items")
        .and_then(|v| v.as_array())
        .ok_or_else(|| fail("take", "items must be an array"))?;
    let count = input
        .get("count")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| fail("take", "count must be a number"))? as usize;

    let taken: Vec<Value> = items.iter().take(count).cloned().collect();
    Ok(Value::Array(taken))
}

pub fn group_by(input: &Value) -> Result<Value, ExecutionError> {
    let items = input
        .get("items")
        .and_then(|v| v.as_array())
        .ok_or_else(|| fail("group_by", "items must be an array"))?;
    let key = input
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| fail("group_by", "key must be a string"))?;

    let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for item in items {
        let group_key = item
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        groups.entry(group_key).or_default().push(item.clone());
    }

    let mut result = Map::new();
    for (k, v) in groups {
        result.insert(k, Value::Array(v));
    }
    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sort_numbers() {
        let result = sort(&json!({"items": [3, 1, 2], "key": null, "descending": null})).unwrap();
        assert_eq!(result, json!([1, 2, 3]));
    }

    #[test]
    fn test_sort_descending() {
        let result = sort(&json!({"items": [3, 1, 2], "key": null, "descending": true})).unwrap();
        assert_eq!(result, json!([3, 2, 1]));
    }

    #[test]
    fn test_sort_strings() {
        let result =
            sort(&json!({"items": ["b", "a", "c"], "key": null, "descending": null})).unwrap();
        assert_eq!(result, json!(["a", "b", "c"]));
    }

    #[test]
    fn test_flatten() {
        let result = flatten(&json!([[1, 2], [3, 4]])).unwrap();
        assert_eq!(result, json!([1, 2, 3, 4]));
    }

    #[test]
    fn test_zip() {
        let result = zip(&json!({"left": [1, 2], "right": ["a", "b"]})).unwrap();
        assert_eq!(
            result,
            json!([{"left": 1, "right": "a"}, {"left": 2, "right": "b"}])
        );
    }

    #[test]
    fn test_take() {
        let result = take(&json!({"items": [1, 2, 3, 4, 5], "count": 3})).unwrap();
        assert_eq!(result, json!([1, 2, 3]));
    }

    #[test]
    fn test_group_by() {
        let result = group_by(
            &json!({"items": [{"type": "a", "v": 1}, {"type": "b", "v": 2}, {"type": "a", "v": 3}], "key": "type"}),
        )
        .unwrap();
        assert_eq!(
            result,
            json!({"a": [{"type": "a", "v": 1}, {"type": "a", "v": 3}], "b": [{"type": "b", "v": 2}]})
        );
    }
}
