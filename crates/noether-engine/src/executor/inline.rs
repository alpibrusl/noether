use super::stages::{execute_executor_stage, find_implementation, is_executor_stage, StageFn};
use super::{ExecutionError, StageExecutor};
use noether_core::stage::StageId;
use noether_store::StageStore;
use serde_json::Value;
use std::collections::HashMap;

/// Real executor that runs Pure stage implementations inline.
/// Handles higher-order stages (map, filter, reduce) by recursively calling itself.
/// Falls back to returning the first example output for unimplemented stages.
pub struct InlineExecutor {
    implementations: HashMap<String, StageFn>,
    fallback_outputs: HashMap<String, Value>,
    /// Descriptions keyed by stage ID — used to detect HOF stages.
    descriptions: HashMap<String, String>,
}

impl InlineExecutor {
    /// Build from a store: registers real implementations for known stages,
    /// and caches first-example outputs as fallbacks for unimplemented stages.
    pub fn from_store(store: &(impl StageStore + ?Sized)) -> Self {
        let mut implementations = HashMap::new();
        let mut fallback_outputs = HashMap::new();
        let mut descriptions = HashMap::new();

        for stage in store.list(None) {
            if let Some(func) = find_implementation(&stage.description) {
                implementations.insert(stage.id.0.clone(), func);
            }
            if let Some(example) = stage.examples.first() {
                fallback_outputs.insert(stage.id.0.clone(), example.output.clone());
            }
            descriptions.insert(stage.id.0.clone(), stage.description.clone());
        }

        Self {
            implementations,
            fallback_outputs,
            descriptions,
        }
    }

    /// Check if a stage has a real implementation (not just a fallback).
    pub fn has_implementation(&self, stage_id: &StageId) -> bool {
        self.implementations.contains_key(&stage_id.0)
            || self.is_hof_stage(stage_id)
            || self.is_csv_stage(stage_id)
            || self.is_executor_hof(stage_id)
    }

    fn description_of(&self, stage_id: &StageId) -> Option<&str> {
        self.descriptions.get(&stage_id.0).map(|s| s.as_str())
    }

    fn is_hof_stage(&self, stage_id: &StageId) -> bool {
        matches!(
            self.description_of(stage_id),
            Some("Apply a stage to each element of a list")
                | Some("Keep only elements where the predicate stage returns true")
                | Some(
                    "Reduce a list to a single value by applying a stage to accumulator and each element"
                )
        )
    }

    fn is_csv_stage(&self, stage_id: &StageId) -> bool {
        matches!(
            self.description_of(stage_id),
            Some("Parse CSV text into a list of row maps")
                | Some("Serialize a list of row maps to CSV text")
        )
    }

    fn is_executor_hof(&self, stage_id: &StageId) -> bool {
        self.description_of(stage_id)
            .map(is_executor_stage)
            .unwrap_or(false)
    }

    fn execute_hof(&self, stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        let desc = self.description_of(stage_id).unwrap_or("");
        match desc {
            "Apply a stage to each element of a list" => self.execute_map(input),
            "Keep only elements where the predicate stage returns true" => {
                self.execute_filter(input)
            }
            "Reduce a list to a single value by applying a stage to accumulator and each element" => {
                self.execute_reduce(input)
            }
            _ => unreachable!(),
        }
    }

    fn execute_map(&self, input: &Value) -> Result<Value, ExecutionError> {
        let items = input
            .get("items")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ExecutionError::StageFailed {
                stage_id: StageId("map".into()),
                message: "items must be an array".into(),
            })?;
        let child_id = input
            .get("stage_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ExecutionError::StageFailed {
                stage_id: StageId("map".into()),
                message: "stage_id must be a string".into(),
            })?;
        let child = StageId(child_id.into());

        let mut results = Vec::with_capacity(items.len());
        for item in items {
            results.push(self.execute(&child, item)?);
        }
        Ok(Value::Array(results))
    }

    fn execute_filter(&self, input: &Value) -> Result<Value, ExecutionError> {
        let items = input
            .get("items")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ExecutionError::StageFailed {
                stage_id: StageId("filter".into()),
                message: "items must be an array".into(),
            })?;
        let child_id = input
            .get("stage_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ExecutionError::StageFailed {
                stage_id: StageId("filter".into()),
                message: "stage_id must be a string".into(),
            })?;
        let child = StageId(child_id.into());

        let mut results = Vec::new();
        for item in items {
            let predicate_result = self.execute(&child, item)?;
            let keep = match &predicate_result {
                Value::Bool(b) => *b,
                _ => false,
            };
            if keep {
                results.push(item.clone());
            }
        }
        Ok(Value::Array(results))
    }

    fn execute_reduce(&self, input: &Value) -> Result<Value, ExecutionError> {
        let items = input
            .get("items")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ExecutionError::StageFailed {
                stage_id: StageId("reduce".into()),
                message: "items must be an array".into(),
            })?;
        let child_id = input
            .get("stage_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ExecutionError::StageFailed {
                stage_id: StageId("reduce".into()),
                message: "stage_id must be a string".into(),
            })?;
        let initial = input.get("initial").cloned().unwrap_or(Value::Null);
        let child = StageId(child_id.into());

        let mut accumulator = initial;
        for item in items {
            let reducer_input = serde_json::json!({
                "accumulator": accumulator,
                "item": item,
            });
            accumulator = self.execute(&child, &reducer_input)?;
        }
        Ok(accumulator)
    }

    fn execute_csv(&self, stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        let desc = self.description_of(stage_id).unwrap_or("");
        match desc {
            "Parse CSV text into a list of row maps" => csv_parse(input),
            "Serialize a list of row maps to CSV text" => csv_write(input),
            _ => unreachable!(),
        }
    }
}

fn csv_parse(input: &Value) -> Result<Value, ExecutionError> {
    let text =
        input
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ExecutionError::StageFailed {
                stage_id: StageId("csv_parse".into()),
                message: "text must be a string".into(),
            })?;
    let has_header = input
        .get("has_header")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let delimiter = input
        .get("delimiter")
        .and_then(|v| v.as_str())
        .unwrap_or(",");

    let mut lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Ok(Value::Array(vec![]));
    }

    let headers: Vec<&str> = if has_header {
        let header_line = lines.remove(0);
        header_line.split(delimiter).collect()
    } else {
        // Generate numeric headers: col0, col1, ...
        let first = lines.first().unwrap_or(&"");
        let count = first.split(delimiter).count();
        (0..count)
            .map(|i| Box::leak(format!("col{i}").into_boxed_str()) as &str)
            .collect()
    };

    let mut rows = Vec::new();
    for line in &lines {
        if line.trim().is_empty() {
            continue;
        }
        let values: Vec<&str> = line.split(delimiter).collect();
        let mut row = serde_json::Map::new();
        for (i, header) in headers.iter().enumerate() {
            let val = values.get(i).unwrap_or(&"");
            row.insert(header.to_string(), Value::String(val.to_string()));
        }
        rows.push(Value::Object(row));
    }
    Ok(Value::Array(rows))
}

fn csv_write(input: &Value) -> Result<Value, ExecutionError> {
    let records = input
        .get("records")
        .and_then(|v| v.as_array())
        .ok_or_else(|| ExecutionError::StageFailed {
            stage_id: StageId("csv_write".into()),
            message: "records must be an array".into(),
        })?;
    let delimiter = input
        .get("delimiter")
        .and_then(|v| v.as_str())
        .unwrap_or(",");

    if records.is_empty() {
        return Ok(Value::String(String::new()));
    }

    // Collect all headers from first record (sorted for determinism)
    let mut headers: Vec<String> = records
        .first()
        .and_then(|r| r.as_object())
        .map(|obj| obj.keys().cloned().collect())
        .unwrap_or_default();
    headers.sort();

    let mut lines = Vec::new();
    // Header line
    lines.push(headers.join(delimiter));

    // Data lines
    for record in records {
        if let Some(obj) = record.as_object() {
            let values: Vec<String> = headers
                .iter()
                .map(|h| {
                    obj.get(h)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                })
                .collect();
            lines.push(values.join(delimiter));
        }
    }

    Ok(Value::String(lines.join("\n")))
}

impl StageExecutor for InlineExecutor {
    fn execute(&self, stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        // HOF stages need recursive executor access
        if self.is_hof_stage(stage_id) {
            return self.execute_hof(stage_id, input);
        }
        // Executor-HOF stages (fallback, parallel_n) also need recursive access
        if self.is_executor_hof(stage_id) {
            let desc = self.description_of(stage_id).unwrap_or("");
            return execute_executor_stage(self, desc, input);
        }
        // CSV stages
        if self.is_csv_stage(stage_id) {
            return self.execute_csv(stage_id, input);
        }
        // Simple stage implementations
        if let Some(func) = self.implementations.get(&stage_id.0) {
            return func(input);
        }
        // Fall back to example output
        if let Some(output) = self.fallback_outputs.get(&stage_id.0) {
            return Ok(output.clone());
        }
        // Unknown stage
        Err(ExecutionError::StageNotFound(stage_id.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noether_core::stdlib::load_stdlib;
    use noether_store::{MemoryStore, StageStore};
    use serde_json::json;

    fn init_store() -> MemoryStore {
        let mut store = MemoryStore::new();
        for stage in load_stdlib() {
            store.put(stage).unwrap();
        }
        store
    }

    fn find_id(store: &MemoryStore, desc: &str) -> StageId {
        store
            .list(None)
            .into_iter()
            .find(|s| s.description.contains(desc))
            .unwrap()
            .id
            .clone()
    }

    #[test]
    fn inline_to_text() {
        let store = init_store();
        let executor = InlineExecutor::from_store(&store);
        let id = find_id(&store, "Convert any value to its text");
        assert!(executor.has_implementation(&id));
        let result = executor.execute(&id, &json!(42)).unwrap();
        assert_eq!(result, json!("42"));
    }

    #[test]
    fn inline_parse_json() {
        let store = init_store();
        let executor = InlineExecutor::from_store(&store);
        let id = find_id(&store, "Parse a JSON string");
        let result = executor.execute(&id, &json!(r#"{"a":1}"#)).unwrap();
        assert_eq!(result, json!({"a": 1}));
    }

    #[test]
    fn inline_text_split() {
        let store = init_store();
        let executor = InlineExecutor::from_store(&store);
        let id = find_id(&store, "Split text by a delimiter");
        let result = executor
            .execute(&id, &json!({"text": "a,b,c", "delimiter": ","}))
            .unwrap();
        assert_eq!(result, json!(["a", "b", "c"]));
    }

    #[test]
    fn inline_text_hash() {
        let store = init_store();
        let executor = InlineExecutor::from_store(&store);
        let id = find_id(&store, "Compute a cryptographic hash");
        let result = executor
            .execute(&id, &json!({"text": "hello", "algorithm": "sha256"}))
            .unwrap();
        assert_eq!(
            result["hash"],
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn inline_sort() {
        let store = init_store();
        let executor = InlineExecutor::from_store(&store);
        let id = find_id(&store, "Sort a list");
        let result = executor
            .execute(
                &id,
                &json!({"items": [3, 1, 2], "key": null, "descending": false}),
            )
            .unwrap();
        assert_eq!(result, json!([1, 2, 3]));
    }

    #[test]
    fn inline_json_merge() {
        let store = init_store();
        let executor = InlineExecutor::from_store(&store);
        let id = find_id(&store, "Deep merge two JSON");
        let result = executor
            .execute(&id, &json!({"base": {"a": 1}, "patch": {"b": 2}}))
            .unwrap();
        assert_eq!(result, json!({"a": 1, "b": 2}));
    }

    #[test]
    fn fallback_for_unimplemented() {
        let store = init_store();
        let executor = InlineExecutor::from_store(&store);
        // LLM stages are still unimplemented (require external API credentials)
        let id = find_id(&store, "Generate text completion using a language model");
        assert!(!executor.has_implementation(&id));
        let result = executor.execute(&id, &json!(null)).unwrap();
        assert!(result.is_object());
    }

    // --- HOF stage tests ---

    #[test]
    fn inline_map_with_to_text() {
        let store = init_store();
        let executor = InlineExecutor::from_store(&store);
        let map_id = find_id(&store, "Apply a stage to each element");
        let to_text_id = find_id(&store, "Convert any value to its text");

        let result = executor
            .execute(
                &map_id,
                &json!({"items": [1, 2, 3], "stage_id": to_text_id.0}),
            )
            .unwrap();
        assert_eq!(result, json!(["1", "2", "3"]));
    }

    #[test]
    fn inline_filter_with_to_bool() {
        let store = init_store();
        let executor = InlineExecutor::from_store(&store);
        let filter_id = find_id(&store, "Keep only elements where");
        let to_bool_id = find_id(&store, "Convert a value to boolean");

        // to_bool: 0 → false, 1 → true, "" → false, "x" → true
        let result = executor
            .execute(
                &filter_id,
                &json!({"items": [0, 1, 2, 0, 3], "stage_id": to_bool_id.0}),
            )
            .unwrap();
        assert_eq!(result, json!([1, 2, 3]));
    }

    #[test]
    fn inline_map_empty_list() {
        let store = init_store();
        let executor = InlineExecutor::from_store(&store);
        let map_id = find_id(&store, "Apply a stage to each element");
        let to_text_id = find_id(&store, "Convert any value to its text");

        let result = executor
            .execute(&map_id, &json!({"items": [], "stage_id": to_text_id.0}))
            .unwrap();
        assert_eq!(result, json!([]));
    }

    // --- CSV tests ---

    #[test]
    fn inline_csv_parse() {
        let store = init_store();
        let executor = InlineExecutor::from_store(&store);
        let id = find_id(&store, "Parse CSV text into a list");

        let result = executor
            .execute(
                &id,
                &json!({"text": "name,age\nAlice,30\nBob,25", "has_header": true, "delimiter": null}),
            )
            .unwrap();
        let rows = result.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"], "Alice");
        assert_eq!(rows[0]["age"], "30");
        assert_eq!(rows[1]["name"], "Bob");
    }

    #[test]
    fn inline_csv_write() {
        let store = init_store();
        let executor = InlineExecutor::from_store(&store);
        let id = find_id(&store, "Serialize a list of row maps");

        let result = executor
            .execute(
                &id,
                &json!({"records": [{"name": "Alice", "age": "30"}, {"name": "Bob", "age": "25"}], "delimiter": null}),
            )
            .unwrap();
        let text = result.as_str().unwrap();
        assert!(text.contains("Alice"));
        assert!(text.contains("Bob"));
        assert!(text.contains("age"));
    }

    #[test]
    fn inline_csv_roundtrip() {
        let store = init_store();
        let executor = InlineExecutor::from_store(&store);
        let parse_id = find_id(&store, "Parse CSV text into a list");
        let write_id = find_id(&store, "Serialize a list of row maps");

        let csv_text = "name,age\nAlice,30\nBob,25";
        let parsed = executor
            .execute(
                &parse_id,
                &json!({"text": csv_text, "has_header": true, "delimiter": null}),
            )
            .unwrap();

        let written = executor
            .execute(&write_id, &json!({"records": parsed, "delimiter": null}))
            .unwrap();
        let text = written.as_str().unwrap();
        // Should contain all data (order may differ due to sorted headers)
        assert!(text.contains("Alice"));
        assert!(text.contains("Bob"));
        assert!(text.contains("30"));
        assert!(text.contains("25"));
    }

    #[test]
    fn has_implementations_count() {
        let store = init_store();
        let executor = InlineExecutor::from_store(&store);
        let count = store
            .list(None)
            .iter()
            .filter(|s| executor.has_implementation(&s.id))
            .count();
        // scalar(5) + text(6) + collections(5+3 HOF) + data(3) + csv(2) = 24
        assert!(
            count >= 22,
            "Expected at least 22 real implementations, got {count}"
        );
    }
}
