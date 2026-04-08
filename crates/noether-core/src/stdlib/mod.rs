mod collections;
mod control;
mod data;
mod internal;
mod io;
mod kv;
mod llm;
mod scalar;
mod text;
mod ui;
mod validation;

use crate::stage::Stage;
use ed25519_dalek::SigningKey;
use sha2::{Digest, Sha256};

/// Deterministic Ed25519 keypair for stdlib signing.
/// Derived from SHA-256 of a fixed seed string.
pub fn stdlib_signing_key() -> SigningKey {
    let seed = Sha256::digest(b"noether-stdlib-signing-key-v0.1.0");
    SigningKey::from_bytes(&seed.into())
}

/// Load all stdlib stages, signed and ready for store insertion.
pub fn load_stdlib() -> Vec<Stage> {
    let key = stdlib_signing_key();
    let mut stages = Vec::new();
    stages.extend(scalar::stages(&key));
    stages.extend(collections::stages(&key));
    stages.extend(control::stages(&key));
    stages.extend(io::stages(&key));
    stages.extend(llm::stages(&key));
    stages.extend(data::stages(&key));
    stages.extend(internal::stages(&key));
    stages.extend(text::stages(&key));
    stages.extend(kv::stages(&key));
    stages.extend(validation::stages(&key));
    stages.extend(ui::stages(&key));
    stages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_stdlib_returns_50_stages() {
        let stages = load_stdlib();
        assert_eq!(stages.len(), 76); // 75 existing + 1 ui (router)
    }

    #[test]
    fn stdlib_ids_are_deterministic() {
        let stages1 = load_stdlib();
        let stages2 = load_stdlib();
        for (s1, s2) in stages1.iter().zip(stages2.iter()) {
            assert_eq!(s1.id, s2.id, "Stage IDs should be deterministic");
        }
    }

    #[test]
    fn all_stdlib_stages_are_active() {
        let stages = load_stdlib();
        for s in &stages {
            assert_eq!(
                s.lifecycle,
                crate::stage::StageLifecycle::Active,
                "Stdlib stage '{}' should be Active",
                s.description
            );
        }
    }

    #[test]
    fn all_stdlib_stages_are_signed() {
        let stages = load_stdlib();
        for s in &stages {
            assert!(
                s.ed25519_signature.is_some(),
                "Stdlib stage '{}' should be signed",
                s.description
            );
        }
    }

    #[test]
    fn no_duplicate_ids() {
        let stages = load_stdlib();
        let mut ids: Vec<_> = stages.iter().map(|s| &s.id).collect();
        let len_before = ids.len();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), len_before, "All stage IDs should be unique");
    }
}
