use crate::effects::EffectSet;
use crate::stage::schema::{SignatureId, StageId, StageSignature};
use crate::types::NType;
use sha2::{Digest, Sha256};

/// Produce the canonical JSON bytes for a StageSignature.
///
/// Uses RFC 8785 (JSON Canonicalization Scheme, JCS) so the byte
/// sequence is independent of the language or struct layout that
/// produced it. Two implementations following RFC 8785 will always
/// produce identical bytes for the same logical value:
///
/// - Object keys sorted lexicographically by code point
/// - No insignificant whitespace
/// - Numbers in I-JSON canonical form
/// - UTF-8 strings with the minimal escape set
///
/// This means a Python or JS client serialising a StageSignature with
/// any RFC 8785 implementation produces the same hash as Rust here.
pub fn canonical_json(sig: &StageSignature) -> Result<Vec<u8>, serde_json::Error> {
    serde_jcs::to_vec(sig)
}

/// Compute the content-addressed StageId from a StageSignature.
///
/// The identity is the hex-encoded SHA-256 of the JCS-canonicalised
/// JSON of the signature.
pub fn compute_stage_id(sig: &StageSignature) -> Result<StageId, serde_json::Error> {
    let bytes = canonical_json(sig)?;
    let hash = Sha256::digest(&bytes);
    Ok(StageId(hex::encode(hash)))
}

/// Compute the signature identity from name + input + output + effects.
///
/// This hash captures *what* a stage does (its interface contract)
/// without the implementation. Two stages with the same
/// [`SignatureId`] are considered versions of the same concept — only
/// one should be Active at a time. Per `STABILITY.md`, signature IDs
/// are stable across the 1.x line even as implementations are bugfixed.
pub fn compute_signature_id(
    name: &str,
    input: &NType,
    output: &NType,
    effects: &EffectSet,
) -> Result<SignatureId, serde_json::Error> {
    let canonical = serde_json::json!({
        "name": name,
        "input": input,
        "output": output,
        "effects": effects,
    });
    let bytes = serde_jcs::to_vec(&canonical)?;
    let hash = Sha256::digest(&bytes);
    Ok(SignatureId(hex::encode(hash)))
}

/// Deprecated name for [`compute_signature_id`]. Kept for back-compat
/// through v0.6.x; removed in v0.7.0.
#[deprecated(since = "0.6.0", note = "renamed to compute_signature_id")]
pub fn compute_canonical_id(
    name: &str,
    input: &NType,
    output: &NType,
    effects: &EffectSet,
) -> Result<SignatureId, serde_json::Error> {
    compute_signature_id(name, input, output, effects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::EffectSet;
    use crate::types::NType;

    fn sample_sig() -> StageSignature {
        StageSignature {
            input: NType::Text,
            output: NType::Number,
            effects: EffectSet::pure(),
            implementation_hash: "abc123".into(),
        }
    }

    #[test]
    fn hash_is_deterministic() {
        let sig = sample_sig();
        let id1 = compute_stage_id(&sig).unwrap();
        let id2 = compute_stage_id(&sig).unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn different_signatures_produce_different_ids() {
        let sig1 = sample_sig();
        let mut sig2 = sample_sig();
        sig2.output = NType::Text;
        let id1 = compute_stage_id(&sig1).unwrap();
        let id2 = compute_stage_id(&sig2).unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn hash_is_64_hex_chars() {
        let id = compute_stage_id(&sample_sig()).unwrap();
        assert_eq!(id.0.len(), 64);
        assert!(id.0.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn canonical_json_round_trip_preserves_hash() {
        let sig = sample_sig();
        let json = canonical_json(&sig).unwrap();
        let deserialized: StageSignature = serde_json::from_slice(&json).unwrap();
        let json2 = canonical_json(&deserialized).unwrap();
        assert_eq!(json, json2);
    }

    /// JCS guarantees identical bytes regardless of input field order.
    /// This is the property that prevents the "client-and-server-disagree-
    /// on-canonical-form" class of bugs cross-language.
    #[test]
    fn jcs_emits_keys_in_lexicographic_order() {
        let sig = sample_sig();
        let bytes = canonical_json(&sig).unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        // Object keys must appear in alphabetical order regardless of
        // struct field order or BTreeMap iteration order.
        let effects_idx = s.find("\"effects\"").unwrap();
        let impl_idx = s.find("\"implementation_hash\"").unwrap();
        let input_idx = s.find("\"input\"").unwrap();
        let output_idx = s.find("\"output\"").unwrap();
        assert!(effects_idx < impl_idx);
        assert!(impl_idx < input_idx);
        assert!(input_idx < output_idx);
    }

    /// Golden test vectors. If these IDs change, every previously-stored
    /// stage in every running registry will be invalidated. Bumping requires
    /// a coordinated migration — never edit these without that plan.
    #[test]
    fn golden_vectors_are_stable() {
        let cases = [
            (
                StageSignature {
                    input: NType::Text,
                    output: NType::Number,
                    effects: EffectSet::pure(),
                    implementation_hash: "abc123".into(),
                },
                "v1:text->number/pure/abc123",
            ),
            (
                StageSignature {
                    input: NType::Bool,
                    output: NType::Bool,
                    effects: EffectSet::pure(),
                    implementation_hash: "identity".into(),
                },
                "v1:bool->bool/pure/identity",
            ),
        ];
        for (sig, label) in cases {
            let id = compute_stage_id(&sig).unwrap();
            // Print so a regression shows the new value next to the old in
            // CI output, making the diff trivial to triage.
            eprintln!("golden {}: {}", label, id.0);
            // Stable across the lifetime of the JCS-based v1 hash format.
            // To regenerate after intentional change: run `cargo test -p
            // noether-core hash::tests::golden_vectors_are_stable -- --nocapture`
            // and paste the new digests below.
            let expected = match label {
                "v1:text->number/pure/abc123" => {
                    "9f66c7c68e0d37b6ec162e1b833b0c9577e463cbf337833076df4be3a5daa3e0"
                }
                "v1:bool->bool/pure/identity" => {
                    "ed852c94fa4b2b0935fd11f715cb5608a8f75780c73bb60ffd52f8ad8301819d"
                }
                _ => unreachable!(),
            };
            assert_eq!(id.0, expected, "golden vector drift for {label}");
        }
    }

    #[test]
    fn signature_id_ignores_implementation() {
        let effects = EffectSet::pure();
        let id1 = compute_signature_id("my_stage", &NType::Text, &NType::Number, &effects).unwrap();
        let id2 = compute_signature_id("my_stage", &NType::Text, &NType::Number, &effects).unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn signature_id_differs_by_name() {
        let effects = EffectSet::pure();
        let id1 = compute_signature_id("stage_a", &NType::Text, &NType::Number, &effects).unwrap();
        let id2 = compute_signature_id("stage_b", &NType::Text, &NType::Number, &effects).unwrap();
        assert_ne!(id1, id2);
    }

    #[test]
    fn signature_id_differs_by_type() {
        let effects = EffectSet::pure();
        let id1 = compute_signature_id("my_stage", &NType::Text, &NType::Number, &effects).unwrap();
        let id2 = compute_signature_id("my_stage", &NType::Text, &NType::Text, &effects).unwrap();
        assert_ne!(id1, id2);
    }
}
