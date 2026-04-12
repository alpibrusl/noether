//! Rust-native implementations for the stage submission validation pipeline.
//!
//! These stages execute entirely within the Rust process (via `InlineExecutor`)
//! — no Python, no Nix subprocess.  Each stage operates on the raw JSON
//! representation of a `Stage` so they can be composed into a standard Noether
//! `Parallel + Sequential` graph without any special casing in the registry.

use super::super::ExecutionError;
use noether_core::stage::{compute_stage_id, StageId, StageSignature};
use serde_json::{json, Value};

// ── helpers ────────────────────────────────────────────────────────────────

fn err(name: &str, message: impl Into<String>) -> ExecutionError {
    ExecutionError::StageFailed {
        stage_id: StageId(name.into()),
        message: message.into(),
    }
}

// ── individual check stages ─────────────────────────────────────────────────

/// Verify that the stage's `id` field equals SHA-256 of its canonical
/// `signature` JSON (the Noether content-addressing invariant).
pub fn verify_stage_content_hash(input: &Value) -> Result<Value, ExecutionError> {
    let stage_id = input["id"].as_str().unwrap_or("");

    // Re-hashing must go through the StageSignature struct, NOT through the
    // raw JSON Value. serde_json::to_string on a Value emits map keys in
    // alphabetical order, while serde_json::to_vec on a struct emits them in
    // struct field-declaration order (input, output, effects,
    // implementation_hash). The two serialisations produce different bytes
    // and therefore different SHA-256 digests — which is precisely the
    // "content hash mismatch" bug clients hit when the JSON they POST has
    // any field order other than the struct's own.
    let sig: StageSignature = serde_json::from_value(input["signature"].clone()).map_err(|e| {
        err(
            "verify_stage_content_hash",
            format!("cannot parse signature as StageSignature: {e}"),
        )
    })?;

    let computed = compute_stage_id(&sig)
        .map_err(|e| {
            err(
                "verify_stage_content_hash",
                format!("cannot canonicalise signature: {e}"),
            )
        })?
        .0;

    if stage_id == computed {
        Ok(json!({
            "passed": true,
            "stage_id": stage_id,
            "computed": computed,
            "error": null
        }))
    } else {
        Ok(json!({
            "passed": false,
            "stage_id": stage_id,
            "computed": computed,
            "error": format!(
                "content hash mismatch: stage.id={} computed={}",
                stage_id, computed
            )
        }))
    }
}

/// Verify the Ed25519 signature of a stage.
///
/// If the stage is unsigned, the check **passes** (with a warning) — unsigned
/// stages are allowed; promotion to Active is blocked by the lifecycle rules.
pub fn verify_stage_ed25519(input: &Value) -> Result<Value, ExecutionError> {
    let sig_hex = input["ed25519_signature"].as_str();
    let pub_hex = input["signer_public_key"].as_str();

    match (sig_hex, pub_hex) {
        (Some(sig), Some(pub_key)) => {
            let stage_id = StageId(input["id"].as_str().unwrap_or("").to_string());
            match noether_core::stage::verify_stage_signature(&stage_id, sig, pub_key) {
                Ok(true) => Ok(json!({ "passed": true, "signed": true, "warning": null })),
                Ok(false) => Ok(json!({
                    "passed": false,
                    "signed": true,
                    "warning": "Ed25519 signature verification failed — stage may have been tampered with"
                })),
                Err(e) => Ok(json!({
                    "passed": false,
                    "signed": true,
                    "warning": format!("signature decode error: {e}")
                })),
            }
        }
        (None, None) => Ok(json!({
            "passed": true,
            "signed": false,
            "warning": "stage is unsigned — consider signing before promoting to Active"
        })),
        _ => Ok(json!({
            "passed": false,
            "signed": false,
            "warning": "malformed: exactly one of ed25519_signature / signer_public_key is set"
        })),
    }
}

/// Check that the stage description field is non-empty.
pub fn check_stage_description(input: &Value) -> Result<Value, ExecutionError> {
    let desc = input["description"].as_str().unwrap_or("").trim();
    if desc.is_empty() {
        Ok(json!({ "passed": false, "error": "stage description must not be empty" }))
    } else {
        Ok(json!({ "passed": true, "error": null }))
    }
}

/// Check that the stage has at least one example.
///
/// Examples are optional but strongly recommended for semantic search quality.
/// Missing examples produce a warning, not a hard error.
pub fn check_stage_examples(input: &Value) -> Result<Value, ExecutionError> {
    let count = input["examples"].as_array().map(|a| a.len()).unwrap_or(0);
    let warning: Value = if count == 0 {
        Value::String("no examples provided — semantic search quality will be reduced".into())
    } else {
        Value::Null
    };
    Ok(json!({ "passed": true, "count": count, "warning": warning }))
}

// ── aggregation stage ───────────────────────────────────────────────────────

/// Aggregate the results of the four parallel validation checks into a single
/// `ValidationReport`.
///
/// Input: a Record produced by the `Parallel` operator, with keys
/// `hash_check`, `sig_check`, `desc_check`, `examples_check`.
///
/// Output: `{ passed: Bool, errors: [Text], warnings: [Text] }`
pub fn merge_validation_checks(input: &Value) -> Result<Value, ExecutionError> {
    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    for key in &["hash_check", "sig_check", "desc_check", "examples_check"] {
        let check = &input[key];

        let passed = check["passed"].as_bool().unwrap_or(false);

        if !passed {
            // Hard error — collect from "error" or "warning" field.
            for field in &["error", "warning"] {
                if let Some(msg) = check[field].as_str() {
                    if !msg.is_empty() {
                        errors.push(msg.to_string());
                    }
                }
            }
        } else {
            // Soft warning — collect from "warning" field.
            if let Some(msg) = check["warning"].as_str() {
                if !msg.is_empty() {
                    warnings.push(msg.to_string());
                }
            }
        }
    }

    Ok(json!({
        "passed": errors.is_empty(),
        "errors": errors,
        "warnings": warnings
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression: a stage submitted with signature fields in alphabetical
    /// order (effects, implementation_hash, input, output) must validate.
    /// The earlier implementation re-serialised the raw JSON Value, which
    /// emitted alphabetically sorted keys, while clients hash the
    /// StageSignature struct (input, output, effects, implementation_hash).
    /// The two byte sequences differ — and used to produce different IDs.
    #[test]
    fn content_hash_check_is_field_order_independent() {
        // JCS-canonicalised id for the signature below. Under RFC 8785 the
        // bytes are identical regardless of the order keys appear in the
        // source JSON, so this id stays valid whether the client emitted
        // {input,output,effects,impl_hash} or alphabetical (as here).
        let raw = serde_json::json!({
            "id": "279804424b7e12b55ec2ed135d9f0c62b1af95b9b1a937895fe69da0f5a42c38",
            "signature": {
                "effects": {"effects": [{"effect": "Fallible"}]},
                "implementation_hash": "1eb75086add21d5ea28d2cf6c79a5c08a40e322517958ad328f19ce4f9d46658",
                "input":  {"kind": "Record", "value": {"manifest": {"kind": "Record", "value": {"apiVersion": {"kind": "Text"}, "name": {"kind": "Text"}}}}},
                "output": {"kind": "Record", "value": {"errors": {"kind": "List", "value": {"kind": "Text"}}, "hash": {"kind": "Text"}, "name": {"kind": "Text"}, "valid": {"kind": "Bool"}, "version": {"kind": "Text"}}}
            }
        });
        let result = verify_stage_content_hash(&raw).unwrap();
        assert_eq!(
            result["passed"],
            serde_json::Value::Bool(true),
            "alphabetically ordered signature should validate; got {result:?}"
        );
    }
}
