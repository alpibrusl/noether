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

/// Collapse a `Var` hint to `None` (no useful shape guidance). In practice
/// unification would have replaced the Var with a concrete NType before we
/// got here; if it hasn't, we have no grounds to push a shape onto the JSON
/// value, so we drop the hint. Called at every hint lookup in `infer_type_with_hint`.
fn strip_var_hint(hint: Option<&NType>) -> Option<&NType> {
    match hint {
        Some(NType::Var(_)) => None,
        other => other,
    }
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
    // If the effective hint is a Var, strip it — an unbound variable carries
    // no shape information, so we fall back to inferring from the JSON value
    // alone (effectively treating the hint as Any).
    let hint = strip_var_hint(hint);

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
    /// A property in the stage's `properties` array deserialised into
    /// [`Property::Unknown`] with a `kind` string that names a
    /// variant this reader DOES know about. That signals a user typo
    /// inside a known property kind (e.g. `allowed: ["bolean"]`
    /// inside a `field_type_in`) rather than a genuinely unknown
    /// future kind. Rejecting at ingest prevents the typo'd property
    /// from silently becoming a no-op check.
    ///
    /// See [`Property::shadowed_known_kind`] for the detection
    /// semantics.
    ///
    /// [`Property::Unknown`]: crate::stage::property::Property::Unknown
    /// [`Property::shadowed_known_kind`]: crate::stage::property::Property::shadowed_known_kind
    ShadowedKnownKind {
        /// Position in the stage's `properties` array.
        index: usize,
        /// The known-kind string that the malformed property
        /// reported (e.g. `"field_type_in"`).
        kind: String,
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
            ValidationError::ShadowedKnownKind { index, kind } => write!(
                f,
                "property[{index}]: looks like a `{kind}` but failed to \
                 deserialise into that variant (likely a typo in one of \
                 its fields). Fix the property — registering it as-is \
                 would silently drop the check at runtime."
            ),
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

    // Reject properties that came out as `Property::Unknown` but name
    // a known kind — see ValidationError::ShadowedKnownKind. Running
    // before example-type-checking keeps the error message close to
    // "your property JSON is malformed" rather than burying it under
    // an output-type mismatch.
    for (i, prop) in stage.properties.iter().enumerate() {
        if let Some(kind) = prop.shadowed_known_kind() {
            errors.push(ValidationError::ShadowedKnownKind {
                index: i,
                kind: kind.to_string(),
            });
        }
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

    #[test]
    fn validate_stage_rejects_shadowed_known_kind() {
        // End-to-end: a stage spec with a typo inside a known
        // property kind (e.g. `allowed: ["bolean"]` in a
        // `field_type_in`) deserialises as `Property::Unknown` via
        // serde's untagged fallback. `validate_stage` must surface
        // that as a ShadowedKnownKind error so ingest (stage add,
        // validate_spec) rejects it.
        use crate::effects::EffectSet;
        use crate::stage::property::Property;
        use crate::stage::schema::{
            CostEstimate, Example, Stage, StageId, StageLifecycle, StageSignature,
        };
        use std::collections::BTreeSet;

        // Hand-construct the Unknown shape serde would produce for
        // a shadowed typo — round-tripping through JSON keeps this
        // test honest about what a real ingest path sees.
        let typo_json = json!({
            "kind": "field_type_in",
            "field": "output.x",
            "allowed": ["bolean"]
        });
        let typo: Property = serde_json::from_value(typo_json).unwrap();
        assert!(matches!(typo, Property::Unknown { .. }));
        assert_eq!(typo.shadowed_known_kind(), Some("field_type_in"));

        let stage = Stage {
            id: StageId("abc".into()),
            signature_id: None,
            signature: StageSignature {
                input: NType::Text,
                output: NType::Text,
                effects: EffectSet::pure(),
                implementation_hash: "h".into(),
            },
            capabilities: BTreeSet::new(),
            cost: CostEstimate {
                time_ms_p50: None,
                tokens_est: None,
                memory_mb: None,
            },
            description: "t".into(),
            examples: vec![Example {
                input: json!("x"),
                output: json!("x"),
            }],
            lifecycle: StageLifecycle::Active,
            ed25519_signature: None,
            signer_public_key: None,
            implementation_code: None,
            implementation_language: None,
            ui_style: None,
            tags: vec![],
            aliases: vec![],
            name: None,
            properties: vec![typo],
        };

        let result = validate_stage(&stage, 0);
        assert!(
            result.errors.iter().any(|e| matches!(
                e,
                ValidationError::ShadowedKnownKind { kind, .. } if kind == "field_type_in"
            )),
            "expected ShadowedKnownKind error, got: {:?}",
            result.errors
        );
    }
}
