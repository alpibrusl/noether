use crate::stage::schema::Stage;
use crate::types::{is_subtype_of, NType};
use std::collections::BTreeMap;
use std::fmt;

/// Infer the structural NType from a serde_json::Value.
pub fn infer_type(value: &serde_json::Value) -> NType {
    infer_type_with_hint(value, None)
}

/// For Union hints like `Map<Text,Text> | Null`, extract the non-Null variant
/// to use as the effective hint for non-null values.
fn resolve_union_hint(hint: &NType) -> Option<NType> {
    if let NType::Union(variants) = hint {
        let non_null: Vec<&NType> = variants
            .iter()
            .filter(|v| !matches!(v, NType::Null))
            .collect();
        if non_null.len() == 1 {
            return Some(non_null[0].clone());
        }
    }
    None
}

/// Infer NType with an optional type hint.
///
/// When the hint is `Map<K, V>`, a JSON object is inferred as `Map<Text, V'>`
/// rather than `Record { ... }`. This resolves the JSON Map/Record ambiguity.
pub fn infer_type_with_hint(value: &serde_json::Value, hint: Option<&NType>) -> NType {
    use serde_json::Value;

    // Resolve the effective hint: if hint is a Union, extract the most
    // specific non-Null variant to use as the real hint.
    let resolved_hint = hint.and_then(resolve_union_hint);
    let hint = resolved_hint.as_ref().or(hint);

    match value {
        Value::Null => NType::Null,
        Value::Bool(_) => NType::Bool,
        Value::Number(_) => NType::Number,
        // If the hint says Bytes, treat JSON strings as Bytes
        Value::String(_) if matches!(hint, Some(NType::Bytes)) => NType::Bytes,
        Value::String(_) => NType::Text,
        Value::Array(items) => {
            let element_hint = match hint {
                Some(NType::List(inner)) => Some(inner.as_ref()),
                _ => None,
            };
            if items.is_empty() {
                NType::List(Box::new(NType::Any))
            } else {
                let element_types: Vec<NType> = items
                    .iter()
                    .map(|v| infer_type_with_hint(v, element_hint))
                    .collect();
                let element_type = NType::union(element_types);
                NType::List(Box::new(element_type))
            }
        }
        Value::Object(map) => {
            // If the hint says VNode, and the value has a "tag" key, treat as VNode
            if matches!(hint, Some(NType::VNode)) {
                return NType::VNode;
            }
            // If the hint says Map<K,V>, infer as Map rather than Record
            if let Some(NType::Map { value: hint_v, .. }) = hint {
                let value_types: Vec<NType> = map
                    .values()
                    .map(|v| infer_type_with_hint(v, Some(hint_v)))
                    .collect();
                let value_type = if value_types.is_empty() {
                    NType::Any
                } else {
                    NType::union(value_types)
                };
                NType::Map {
                    key: Box::new(NType::Text),
                    value: Box::new(value_type),
                }
            } else {
                // Infer field hints from Record hint if available
                let field_hints: Option<&BTreeMap<String, NType>> = match hint {
                    Some(NType::Record(fields)) => Some(fields),
                    _ => None,
                };
                let fields: BTreeMap<String, NType> = map
                    .iter()
                    .map(|(k, v)| {
                        let field_hint = field_hints.and_then(|fh| fh.get(k));
                        (k.clone(), infer_type_with_hint(v, field_hint))
                    })
                    .collect();
                NType::Record(fields)
            }
        }
    }
}

#[derive(Debug)]
pub struct ValidationResult {
    pub stage_description: String,
    pub errors: Vec<ValidationError>,
}

impl ValidationResult {
    pub fn is_ok(&self) -> bool {
        self.errors.is_empty()
    }
}

#[derive(Debug)]
pub enum ValidationError {
    InputTypeMismatch {
        index: usize,
        inferred: NType,
        declared: NType,
        reason: String,
    },
    OutputTypeMismatch {
        index: usize,
        inferred: NType,
        declared: NType,
        reason: String,
    },
    TooFewExamples {
        min: usize,
        got: usize,
    },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ValidationError::InputTypeMismatch {
                index,
                inferred,
                declared,
                reason,
            } => write!(
                f,
                "example {index}: input type {inferred} is not subtype of declared {declared}: {reason}"
            ),
            ValidationError::OutputTypeMismatch {
                index,
                inferred,
                declared,
                reason,
            } => write!(
                f,
                "example {index}: output type {inferred} is not subtype of declared {declared}: {reason}"
            ),
            ValidationError::TooFewExamples { min, got } => {
                write!(f, "too few examples: need at least {min}, got {got}")
            }
        }
    }
}

/// Validate a stage's examples against its declared type signature.
pub fn validate_stage(stage: &Stage, min_examples: usize) -> ValidationResult {
    let mut errors = Vec::new();

    if stage.examples.len() < min_examples {
        errors.push(ValidationError::TooFewExamples {
            min: min_examples,
            got: stage.examples.len(),
        });
    }

    for (i, example) in stage.examples.iter().enumerate() {
        // Check input
        let inferred_input = infer_type_with_hint(&example.input, Some(&stage.signature.input));
        if let crate::types::TypeCompatibility::Incompatible(reason) =
            is_subtype_of(&inferred_input, &stage.signature.input)
        {
            errors.push(ValidationError::InputTypeMismatch {
                index: i,
                inferred: inferred_input,
                declared: stage.signature.input.clone(),
                reason: format!("{reason}"),
            });
        }

        // Check output
        let inferred_output = infer_type_with_hint(&example.output, Some(&stage.signature.output));
        if let crate::types::TypeCompatibility::Incompatible(reason) =
            is_subtype_of(&inferred_output, &stage.signature.output)
        {
            errors.push(ValidationError::OutputTypeMismatch {
                index: i,
                inferred: inferred_output,
                declared: stage.signature.output.clone(),
                reason: format!("{reason}"),
            });
        }
    }

    ValidationResult {
        stage_description: stage.description.clone(),
        errors,
    }
}

/// Validate all stages in a collection.
pub fn validate_all(stages: &[Stage], min_examples: usize) -> Vec<ValidationResult> {
    stages
        .iter()
        .map(|s| validate_stage(s, min_examples))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn infer_primitives() {
        assert_eq!(infer_type(&json!(null)), NType::Null);
        assert_eq!(infer_type(&json!(true)), NType::Bool);
        assert_eq!(infer_type(&json!(42)), NType::Number);
        assert_eq!(infer_type(&json!("hello")), NType::Text);
    }

    #[test]
    fn infer_array() {
        let t = infer_type(&json!([1, 2, 3]));
        assert_eq!(t, NType::List(Box::new(NType::Number)));
    }

    #[test]
    fn infer_empty_array() {
        let t = infer_type(&json!([]));
        assert_eq!(t, NType::List(Box::new(NType::Any)));
    }

    #[test]
    fn infer_object_as_record() {
        let t = infer_type(&json!({"name": "alice", "age": 30}));
        assert_eq!(
            t,
            NType::record([("age", NType::Number), ("name", NType::Text)])
        );
    }

    #[test]
    fn infer_object_as_map_with_hint() {
        let hint = NType::Map {
            key: Box::new(NType::Text),
            value: Box::new(NType::Text),
        };
        let t = infer_type_with_hint(
            &json!({"x-auth": "token", "content-type": "json"}),
            Some(&hint),
        );
        assert_eq!(
            t,
            NType::Map {
                key: Box::new(NType::Text),
                value: Box::new(NType::Text),
            }
        );
    }

    #[test]
    fn infer_nested_with_record_hint() {
        let hint = NType::record([(
            "data",
            NType::Map {
                key: Box::new(NType::Text),
                value: Box::new(NType::Number),
            },
        )]);
        let t = infer_type_with_hint(&json!({"data": {"a": 1, "b": 2}}), Some(&hint));
        let expected = NType::record([(
            "data",
            NType::Map {
                key: Box::new(NType::Text),
                value: Box::new(NType::Number),
            },
        )]);
        assert_eq!(t, expected);
    }

    #[test]
    fn infer_vnode_with_hint() {
        let vnode = json!({
            "tag": "div",
            "props": {"class": "counter"},
            "children": [{"$text": "Count: 0"}]
        });
        let t = infer_type_with_hint(&vnode, Some(&NType::VNode));
        assert_eq!(t, NType::VNode);
    }

    #[test]
    fn infer_vnode_without_hint_gives_record() {
        // Without VNode hint, the type checker infers Record (the underlying JSON structure).
        let vnode = json!({"tag": "div", "props": {}, "children": []});
        let t = infer_type(&vnode);
        // Should be a Record, not VNode (VNode requires an explicit hint)
        assert!(matches!(t, NType::Record(_)));
    }
}
