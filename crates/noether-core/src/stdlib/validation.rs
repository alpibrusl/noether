//! Validation pipeline stages for the stage submission workflow.
//!
//! These 5 stages are Pure, Rust-native (executed by `InlineExecutor`),
//! and form the `stage_submission_validation` composition graph used by the
//! `noether-cloud` registry on every `POST /stages` request.

use crate::stage::{Stage, StageBuilder};
use crate::types::NType;
use ed25519_dalek::SigningKey;
use serde_json::json;

/// Check output type shared by the four parallel checks.
/// `{ passed: Bool, error?: Text|Null, warning?: Text|Null, ... }`
fn check_out() -> NType {
    NType::Map {
        key: Box::new(NType::Text),
        value: Box::new(NType::Any),
    }
}

/// ValidationReport type: `{ passed: Bool, errors: [Text], warnings: [Text] }`
fn report_out() -> NType {
    NType::record([
        ("passed", NType::Bool),
        ("errors", NType::List(Box::new(NType::Text))),
        ("warnings", NType::List(Box::new(NType::Text))),
    ])
}

pub fn stages(key: &SigningKey) -> Vec<Stage> {
    vec![
        // ── 1. Content-hash check ─────────────────────────────────────────
        StageBuilder::new("verify_stage_content_hash")
            .description("Verify that a stage's content hash matches its declared ID")
            .input(NType::Any)
            .output(check_out())
            .pure()
            .example(
                json!({"id": "abc123", "signature": {"input": "Text", "output": "Number", "effects": [], "implementation_hash": "deadbeef"}}),
                json!({"passed": true, "stage_id": "abc123", "computed": "abc123", "error": null}),
            )
            .example(
                json!({"id": "bad_id", "signature": {"input": "Any", "output": "Text", "effects": [], "implementation_hash": "cafebabe"}}),
                json!({"passed": false, "stage_id": "bad_id", "computed": "actual_hash", "error": "content hash mismatch: stage.id=bad_id computed=actual_hash"}),
            )
            .example(
                json!({"id": "", "signature": {}}),
                json!({"passed": false, "stage_id": "", "computed": "e3b0c44298fc1c149afbf4c8996fb924", "error": "content hash mismatch: stage.id= computed=e3b0c44298fc1c149afbf4c8996fb924"}),
            )
            .example(
                json!({"id": "f00d1234", "signature": {"input": "List", "output": "List", "effects": ["Pure"], "implementation_hash": "0011aabb"}}),
                json!({"passed": false, "stage_id": "f00d1234", "computed": "computed_hash_here", "error": "content hash mismatch: stage.id=f00d1234 computed=computed_hash_here"}),
            )
            .example(
                json!({"id": "valid_id", "signature": {"input": "Bool", "output": "Bool", "effects": ["Pure"], "implementation_hash": "identity_fn"}}),
                json!({"passed": true, "stage_id": "valid_id", "computed": "valid_id", "error": null}),
            )
            .tag("validation").tag("security").tag("integrity").tag("pure")
            .alias("check_hash").alias("verify_id")
            .build_stdlib(key)
            .unwrap(),

        // ── 2. Ed25519 signature check ────────────────────────────────────
        StageBuilder::new("verify_stage_ed25519")
            .description("Verify the Ed25519 signature of a stage, if present")
            .input(NType::Any)
            .output(check_out())
            .pure()
            .example(
                json!({"id": "abc123", "ed25519_signature": "cafebabe", "signer_public_key": "deadbeef"}),
                json!({"passed": false, "signed": true, "warning": "Ed25519 signature verification failed — stage may have been tampered with"}),
            )
            .example(
                json!({"id": "abc123"}),
                json!({"passed": true, "signed": false, "warning": "stage is unsigned — consider signing before promoting to Active"}),
            )
            .example(
                json!({"id": "abc123", "ed25519_signature": null, "signer_public_key": null}),
                json!({"passed": true, "signed": false, "warning": "stage is unsigned — consider signing before promoting to Active"}),
            )
            .example(
                json!({"id": "abc123", "ed25519_signature": "sig_hex", "signer_public_key": "pub_hex"}),
                json!({"passed": false, "signed": true, "warning": "signature decode error: invalid hex character"}),
            )
            .example(
                json!({"id": "abc123", "ed25519_signature": "valid_sig", "signer_public_key": "valid_pub"}),
                json!({"passed": true, "signed": true, "warning": null}),
            )
            .tag("validation").tag("security").tag("cryptography").tag("pure")
            .alias("verify_signature").alias("check_sig")
            .build_stdlib(key)
            .unwrap(),

        // ── 3. Description check ──────────────────────────────────────────
        StageBuilder::new("check_stage_description")
            .description("Check that a stage description is non-empty")
            .input(NType::Any)
            .output(check_out())
            .pure()
            .example(
                json!({"description": "Convert text to uppercase"}),
                json!({"passed": true, "error": null}),
            )
            .example(
                json!({"description": ""}),
                json!({"passed": false, "error": "stage description must not be empty"}),
            )
            .example(
                json!({"description": "   "}),
                json!({"passed": false, "error": "stage description must not be empty"}),
            )
            .example(
                json!({}),
                json!({"passed": false, "error": "stage description must not be empty"}),
            )
            .example(
                json!({"description": "A well-described stage that does something useful"}),
                json!({"passed": true, "error": null}),
            )
            .tag("validation").tag("quality").tag("pure")
            .alias("validate_description")
            .build_stdlib(key)
            .unwrap(),

        // ── 4. Examples check ─────────────────────────────────────────────
        StageBuilder::new("check_stage_examples")
            .description("Check that a stage has at least one example")
            .input(NType::Any)
            .output(check_out())
            .pure()
            .example(
                json!({"examples": [{"input": 1, "output": "1"}]}),
                json!({"passed": true, "count": 1, "warning": null}),
            )
            .example(
                json!({"examples": []}),
                json!({"passed": true, "count": 0, "warning": "no examples provided — semantic search quality will be reduced"}),
            )
            .example(
                json!({}),
                json!({"passed": true, "count": 0, "warning": "no examples provided — semantic search quality will be reduced"}),
            )
            .example(
                json!({"examples": [{"input": "a", "output": "A"}, {"input": "b", "output": "B"}]}),
                json!({"passed": true, "count": 2, "warning": null}),
            )
            .example(
                json!({"examples": [{"input": null, "output": null}, {"input": 42, "output": "42"}, {"input": true, "output": "true"}]}),
                json!({"passed": true, "count": 3, "warning": null}),
            )
            .tag("validation").tag("quality").tag("pure")
            .alias("validate_examples")
            .build_stdlib(key)
            .unwrap(),

        // ── 5. Aggregation stage ──────────────────────────────────────────
        StageBuilder::new("merge_validation_checks")
            .description("Aggregate stage validation check results into a report")
            .input(NType::Map { key: Box::new(NType::Text), value: Box::new(NType::Any) })
            .output(report_out())
            .pure()
            .example(
                json!({"hash_check": {"passed": true, "error": null}, "sig_check": {"passed": true, "signed": false, "warning": "unsigned"}, "desc_check": {"passed": true, "error": null}, "examples_check": {"passed": true, "count": 2, "warning": null}}),
                json!({"passed": true, "errors": [], "warnings": ["unsigned"]}),
            )
            .example(
                json!({"hash_check": {"passed": false, "error": "hash mismatch"}, "sig_check": {"passed": true, "signed": false, "warning": null}, "desc_check": {"passed": true, "error": null}, "examples_check": {"passed": true, "count": 1, "warning": null}}),
                json!({"passed": false, "errors": ["hash mismatch"], "warnings": []}),
            )
            .example(
                json!({"hash_check": {"passed": true, "error": null}, "sig_check": {"passed": false, "signed": true, "warning": "sig failed"}, "desc_check": {"passed": false, "error": "empty description"}, "examples_check": {"passed": true, "count": 0, "warning": "no examples"}}),
                json!({"passed": false, "errors": ["sig failed", "empty description"], "warnings": ["no examples"]}),
            )
            .example(
                json!({"hash_check": {"passed": true, "error": null}, "sig_check": {"passed": true, "signed": true, "warning": null}, "desc_check": {"passed": true, "error": null}, "examples_check": {"passed": true, "count": 5, "warning": null}}),
                json!({"passed": true, "errors": [], "warnings": []}),
            )
            .example(
                json!({"hash_check": {"passed": false, "error": "mismatch"}, "sig_check": {"passed": false, "signed": true, "warning": "bad sig"}, "desc_check": {"passed": false, "error": "no desc"}, "examples_check": {"passed": true, "count": 0, "warning": "no examples"}}),
                json!({"passed": false, "errors": ["mismatch", "bad sig", "no desc"], "warnings": ["no examples"]}),
            )
            .tag("validation").tag("aggregation").tag("pure")
            .alias("merge_checks").alias("collect_validation")
            .build_stdlib(key)
            .unwrap(),
    ]
}
