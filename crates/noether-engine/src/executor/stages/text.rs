use crate::executor::ExecutionError;
use noether_core::stage::StageId;
use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

fn get_field<'a>(input: &'a Value, field: &str) -> Result<&'a Value, ExecutionError> {
    input.get(field).ok_or_else(|| ExecutionError::StageFailed {
        stage_id: StageId("text".into()),
        message: format!("missing field '{field}'"),
    })
}

fn get_str<'a>(input: &'a Value, field: &str) -> Result<&'a str, ExecutionError> {
    get_field(input, field)?
        .as_str()
        .ok_or_else(|| ExecutionError::StageFailed {
            stage_id: StageId("text".into()),
            message: format!("field '{field}' must be a string"),
        })
}

pub fn text_split(input: &Value) -> Result<Value, ExecutionError> {
    let text = get_str(input, "text")?;
    let delimiter = get_str(input, "delimiter")?;
    let parts: Vec<Value> = text.split(delimiter).map(|s| json!(s)).collect();
    Ok(Value::Array(parts))
}

pub fn text_join(input: &Value) -> Result<Value, ExecutionError> {
    let items =
        get_field(input, "items")?
            .as_array()
            .ok_or_else(|| ExecutionError::StageFailed {
                stage_id: StageId("text_join".into()),
                message: "items must be an array".into(),
            })?;
    let delimiter = get_str(input, "delimiter")?;
    let strings: Vec<&str> = items.iter().filter_map(|v| v.as_str()).collect();
    Ok(json!(strings.join(delimiter)))
}

pub fn regex_match(input: &Value) -> Result<Value, ExecutionError> {
    let text = get_str(input, "text")?;
    let pattern = get_str(input, "pattern")?;

    let re = Regex::new(pattern).map_err(|e| ExecutionError::StageFailed {
        stage_id: StageId("regex_match".into()),
        message: format!("invalid regex: {e}"),
    })?;

    if let Some(caps) = re.captures(text) {
        let full_match = caps.get(0).map(|m| m.as_str()).unwrap_or("");
        let groups: Vec<Value> = (1..caps.len())
            .map(|i| match caps.get(i) {
                Some(m) => json!(m.as_str()),
                None => Value::Null,
            })
            .collect();
        Ok(json!({
            "matched": true,
            "full_match": full_match,
            "groups": groups,
        }))
    } else {
        Ok(json!({
            "matched": false,
            "full_match": null,
            "groups": [],
        }))
    }
}

pub fn regex_replace(input: &Value) -> Result<Value, ExecutionError> {
    let text = get_str(input, "text")?;
    let pattern = get_str(input, "pattern")?;
    let replacement = get_str(input, "replacement")?;

    let re = Regex::new(pattern).map_err(|e| ExecutionError::StageFailed {
        stage_id: StageId("regex_replace".into()),
        message: format!("invalid regex: {e}"),
    })?;
    Ok(json!(re.replace_all(text, replacement).as_ref()))
}

pub fn text_template(input: &Value) -> Result<Value, ExecutionError> {
    let template = get_str(input, "template")?;
    let variables =
        get_field(input, "variables")?
            .as_object()
            .ok_or_else(|| ExecutionError::StageFailed {
                stage_id: StageId("text_template".into()),
                message: "variables must be an object".into(),
            })?;

    let mut result = template.to_string();
    for (key, value) in variables {
        let val_str = value.as_str().unwrap_or("");
        result = result.replace(&format!("{{{{{key}}}}}"), val_str);
    }
    Ok(json!(result))
}

pub fn text_hash(input: &Value) -> Result<Value, ExecutionError> {
    let text = get_str(input, "text")?;
    let algorithm = get_field(input, "algorithm")
        .ok()
        .and_then(|v| v.as_str())
        .unwrap_or("sha256");

    let hash = match algorithm {
        "sha256" => {
            let digest = Sha256::digest(text.as_bytes());
            hex::encode(digest)
        }
        other => {
            return Err(ExecutionError::StageFailed {
                stage_id: StageId("text_hash".into()),
                message: format!("unsupported algorithm: {other}"),
            })
        }
    };

    Ok(json!({
        "hash": hash,
        "algorithm": algorithm,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_split() {
        let result = text_split(&json!({"text": "a,b,c", "delimiter": ","})).unwrap();
        assert_eq!(result, json!(["a", "b", "c"]));
    }

    #[test]
    fn test_text_join() {
        let result = text_join(&json!({"items": ["a", "b", "c"], "delimiter": ","})).unwrap();
        assert_eq!(result, json!("a,b,c"));
    }

    #[test]
    fn test_regex_replace() {
        let result = regex_replace(
            &json!({"text": "hello world", "pattern": "world", "replacement": "rust"}),
        )
        .unwrap();
        assert_eq!(result, json!("hello rust"));

        let result = regex_replace(
            &json!({"text": "hello   world   foo", "pattern": r"\s+", "replacement": " "}),
        )
        .unwrap();
        assert_eq!(result, json!("hello world foo"));

        let result = regex_replace(
            &json!({"text": "abc123def456", "pattern": r"\d+", "replacement": "NUM"}),
        )
        .unwrap();
        assert_eq!(result, json!("abcNUMdefNUM"));

        assert!(
            regex_replace(&json!({"text": "x", "pattern": "[invalid", "replacement": ""})).is_err()
        );
    }

    #[test]
    fn test_regex_match() {
        let result = regex_match(&json!({"text": "hello 42 world", "pattern": r"\d+"})).unwrap();
        assert_eq!(result["matched"], true);
        assert_eq!(result["full_match"], "42");

        let result = regex_match(&json!({"text": "no digits here", "pattern": r"\d+"})).unwrap();
        assert_eq!(result["matched"], false);

        // Capture groups
        let result =
            regex_match(&json!({"text": "2026-04-06", "pattern": r"(\d{4})-(\d{2})-(\d{2})"}))
                .unwrap();
        assert_eq!(result["matched"], true);
        assert_eq!(result["groups"][0], "2026");
        assert_eq!(result["groups"][1], "04");
        assert_eq!(result["groups"][2], "06");
    }

    #[test]
    fn test_text_template() {
        let result =
            text_template(&json!({"template": "Hello, {{name}}!", "variables": {"name": "Alice"}}))
                .unwrap();
        assert_eq!(result, json!("Hello, Alice!"));
    }

    #[test]
    fn test_text_hash() {
        let result = text_hash(&json!({"text": "hello", "algorithm": "sha256"})).unwrap();
        assert_eq!(
            result["hash"],
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_eq!(result["algorithm"], "sha256");
    }

    #[test]
    fn test_text_hash_default_algorithm() {
        let result = text_hash(&json!({"text": "hello", "algorithm": null})).unwrap();
        assert_eq!(result["algorithm"], "sha256");
    }
}
