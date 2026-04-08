//! Apache Arrow IPC serialisation stages.
//!
//! Both stages are feature-gated under `native` (they depend on the `arrow`
//! and `base64` crates which are not compiled for wasm32).

use crate::executor::ExecutionError;
use noether_core::stage::StageId;
use serde_json::{json, Value};

fn fail(msg: impl Into<String>) -> ExecutionError {
    ExecutionError::StageFailed {
        stage_id: StageId("arrow".into()),
        message: msg.into(),
    }
}

/// Convert a list of JSON records → Apache Arrow IPC bytes (base64-encoded).
///
/// Input:  `List<Any>`  (array of JSON objects; non-object values are skipped)
/// Output: `Bytes`      (base64 string — Arrow IPC stream format)
pub fn arrow_from_records(input: &Value) -> Result<Value, ExecutionError> {
    use arrow::{
        array::{ArrayRef, BooleanBuilder, Float64Builder, StringBuilder},
        datatypes::{DataType, Field, Schema},
        ipc::writer::StreamWriter,
        record_batch::RecordBatch,
    };
    use base64::{engine::general_purpose::STANDARD, Engine};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    let records = input
        .as_array()
        .ok_or_else(|| fail("input must be a JSON array"))?;

    if records.is_empty() {
        // Return a minimal valid Arrow IPC stream with no columns.
        let schema = Arc::new(Schema::new(vec![] as Vec<Field>));
        let batch = RecordBatch::new_empty(schema.clone());
        let mut buf = vec![];
        let mut writer = StreamWriter::try_new(&mut buf, &schema)
            .map_err(|e| fail(format!("arrow writer error: {e}")))?;
        writer
            .write(&batch)
            .map_err(|e| fail(format!("arrow write error: {e}")))?;
        writer
            .finish()
            .map_err(|e| fail(format!("arrow finish error: {e}")))?;
        return Ok(json!(STANDARD.encode(&buf)));
    }

    // Collect all column names and infer types from the first non-null value.
    // Column type inference: Bool < Number < Text (widest wins).
    let mut column_types: BTreeMap<String, ColumnType> = BTreeMap::new();

    for row in records {
        if let Some(obj) = row.as_object() {
            for (k, v) in obj {
                let t = value_type(v);
                let current = column_types.entry(k.clone()).or_insert(ColumnType::Bool);
                *current = (*current).wider(t);
            }
        }
    }

    // Build Arrow schema
    let fields: Vec<Field> = column_types
        .keys()
        .map(|name| {
            let dtype = match column_types[name] {
                ColumnType::Bool => DataType::Boolean,
                ColumnType::Number => DataType::Float64,
                ColumnType::Text => DataType::Utf8,
            };
            Field::new(name.as_str(), dtype, true)
        })
        .collect();
    let schema = Arc::new(Schema::new(fields.clone()));

    // Build column arrays
    let columns: Vec<ArrayRef> = fields
        .iter()
        .map(|field| {
            let col_type = &column_types[field.name()];
            match col_type {
                ColumnType::Bool => {
                    let mut builder = BooleanBuilder::new();
                    for row in records {
                        match row.get(field.name()) {
                            Some(v) => builder.append_value(v.as_bool().unwrap_or(false)),
                            None => builder.append_null(),
                        }
                    }
                    Arc::new(builder.finish()) as ArrayRef
                }
                ColumnType::Number => {
                    let mut builder = Float64Builder::new();
                    for row in records {
                        match row.get(field.name()) {
                            Some(v) => builder.append_value(v.as_f64().unwrap_or(0.0)),
                            None => builder.append_null(),
                        }
                    }
                    Arc::new(builder.finish()) as ArrayRef
                }
                ColumnType::Text => {
                    let mut builder = StringBuilder::new();
                    for row in records {
                        match row.get(field.name()) {
                            Some(v) => {
                                let s = match v {
                                    Value::String(s) => s.clone(),
                                    other => other.to_string(),
                                };
                                builder.append_value(s);
                            }
                            None => builder.append_null(),
                        }
                    }
                    Arc::new(builder.finish()) as ArrayRef
                }
            }
        })
        .collect();

    let batch = RecordBatch::try_new(schema.clone(), columns)
        .map_err(|e| fail(format!("record batch error: {e}")))?;

    let mut buf = vec![];
    let mut writer = StreamWriter::try_new(&mut buf, &schema)
        .map_err(|e| fail(format!("arrow writer error: {e}")))?;
    writer
        .write(&batch)
        .map_err(|e| fail(format!("arrow write error: {e}")))?;
    writer
        .finish()
        .map_err(|e| fail(format!("arrow finish error: {e}")))?;

    Ok(json!(STANDARD.encode(&buf)))
}

/// Decode Apache Arrow IPC bytes (base64-encoded) → list of JSON record maps.
///
/// Input:  `Bytes`          (base64 string — Arrow IPC stream)
/// Output: `List<Map<Text, Any>>`
pub fn records_to_arrow(input: &Value) -> Result<Value, ExecutionError> {
    use arrow::{
        array::{Array, BooleanArray, Float64Array, StringArray},
        datatypes::DataType,
        ipc::reader::StreamReader,
    };
    use base64::{engine::general_purpose::STANDARD, Engine};
    use std::io::Cursor;

    let b64 = input
        .as_str()
        .ok_or_else(|| fail("input must be a base64 string"))?;

    let bytes = STANDARD
        .decode(b64)
        .map_err(|e| fail(format!("base64 decode error: {e}")))?;

    let cursor = Cursor::new(bytes);
    let mut reader =
        StreamReader::try_new(cursor, None).map_err(|e| fail(format!("arrow reader error: {e}")))?;

    let mut rows: Vec<Value> = vec![];

    for batch_result in reader.by_ref() {
        let batch = batch_result.map_err(|e| fail(format!("arrow batch error: {e}")))?;
        let schema = batch.schema();
        let num_rows = batch.num_rows();

        for row_idx in 0..num_rows {
            let mut obj = serde_json::Map::new();
            for (col_idx, field) in schema.fields().iter().enumerate() {
                let col = batch.column(col_idx);
                let v = if col.is_null(row_idx) {
                    Value::Null
                } else {
                    match field.data_type() {
                        DataType::Boolean => {
                            let arr = col.as_any().downcast_ref::<BooleanArray>().unwrap();
                            json!(arr.value(row_idx))
                        }
                        DataType::Float64 => {
                            let arr = col.as_any().downcast_ref::<Float64Array>().unwrap();
                            json!(arr.value(row_idx))
                        }
                        DataType::Utf8 => {
                            let arr = col.as_any().downcast_ref::<StringArray>().unwrap();
                            json!(arr.value(row_idx))
                        }
                        other => json!(format!("<unsupported type: {other}>")),
                    }
                };
                obj.insert(field.name().clone(), v);
            }
            rows.push(Value::Object(obj));
        }
    }

    Ok(Value::Array(rows))
}

// ── Column type inference ────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq)]
enum ColumnType {
    Bool,
    Number,
    Text,
}

impl ColumnType {
    fn wider(self, other: ColumnType) -> ColumnType {
        use ColumnType::*;
        match (self, other) {
            (Text, _) | (_, Text) => Text,
            (Number, _) | (_, Number) => Number,
            _ => Bool,
        }
    }
}

fn value_type(v: &Value) -> ColumnType {
    match v {
        Value::Bool(_) => ColumnType::Bool,
        Value::Number(_) => ColumnType::Number,
        _ => ColumnType::Text,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrip_records() {
        let records = json!([
            {"name": "alice", "score": 95.5, "active": true},
            {"name": "bob",   "score": 87.0, "active": false},
        ]);

        let arrow_bytes = arrow_from_records(&records).unwrap();
        assert!(arrow_bytes.is_string());

        let decoded = records_to_arrow(&arrow_bytes).unwrap();
        let rows = decoded.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["name"].as_str().unwrap(), "alice");
        assert!((rows[0]["score"].as_f64().unwrap() - 95.5).abs() < f64::EPSILON);
        assert_eq!(rows[0]["active"].as_bool().unwrap(), true);
        assert_eq!(rows[1]["name"].as_str().unwrap(), "bob");
    }

    #[test]
    fn empty_list_roundtrip() {
        let empty = json!([]);
        let arrow_bytes = arrow_from_records(&empty).unwrap();
        let decoded = records_to_arrow(&arrow_bytes).unwrap();
        assert_eq!(decoded.as_array().unwrap().len(), 0);
    }

    #[test]
    fn non_array_input_fails() {
        assert!(arrow_from_records(&json!("not an array")).is_err());
    }

    #[test]
    fn invalid_base64_fails() {
        assert!(records_to_arrow(&json!("!!!notbase64!!!")).is_err());
    }
}
