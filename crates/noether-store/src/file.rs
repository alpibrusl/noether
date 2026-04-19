use crate::invariant::{
    duplicate_active_ids_for, duplicate_active_ids_for_incoming, log_auto_deprecation,
};
use crate::lifecycle::validate_transition;
use crate::traits::{StageStore, StoreError, StoreStats};
use noether_core::stage::{Stage, StageId, StageLifecycle};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;

/// File-backed stage store. Persists to JSON on every mutation.
/// Loads stdlib on first creation, then reads from disk on subsequent runs.
pub struct JsonFileStore {
    path: PathBuf,
    stages: HashMap<String, Stage>,
}

/// On-disk format: just a list of stages.
#[derive(Serialize, Deserialize)]
struct StoreFile {
    stages: Vec<Stage>,
}

impl JsonFileStore {
    /// Open or create a store at the given path.
    /// If the file exists, loads from it. Otherwise creates an empty store.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let path = path.into();
        let stages = if path.exists() {
            let content = fs::read_to_string(&path).map_err(|e| StoreError::IoError {
                message: format!("failed to read {}: {e}", path.display()),
            })?;
            if content.trim().is_empty() {
                HashMap::new()
            } else {
                let file: StoreFile =
                    serde_json::from_str(&content).map_err(|e| StoreError::IoError {
                        message: format!("failed to parse {}: {e}", path.display()),
                    })?;
                file.stages
                    .into_iter()
                    .map(|s| (s.id.0.clone(), s))
                    .collect()
            }
        } else {
            HashMap::new()
        };
        Ok(Self { path, stages })
    }

    /// Number of stages in the store.
    pub fn len(&self) -> usize {
        self.stages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    /// Persist current state to disk.
    //
    // NOTE(atomicity): `fs::write` truncates then writes — a mid-write
    // crash can leave `path` empty or half-written. Future hardening:
    // write to `<path>.tmp` and rename in place. Acceptable today
    // because JsonFileStore is a developer-facing local registry, not a
    // hot production path. Track via PR follow-up.
    fn save(&self) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| StoreError::IoError {
                message: format!("failed to create directory {}: {e}", parent.display()),
            })?;
        }
        let file = StoreFile {
            stages: self.stages.values().cloned().collect(),
        };
        let json = serde_json::to_string_pretty(&file).map_err(|e| StoreError::IoError {
            message: format!("serialization failed: {e}"),
        })?;
        fs::write(&self.path, json).map_err(|e| StoreError::IoError {
            message: format!("failed to write {}: {e}", self.path.display()),
        })?;
        Ok(())
    }
}

impl StageStore for JsonFileStore {
    fn put(&mut self, stage: Stage) -> Result<StageId, StoreError> {
        let id = stage.id.clone();
        if self.stages.contains_key(&id.0) {
            return Err(StoreError::AlreadyExists(id));
        }
        // M2.3 invariant: auto-deprecate existing Actives with the
        // same signature_id. See MemoryStore::put for rationale.
        let duplicates = duplicate_active_ids_for_incoming(&self.stages, &stage);
        let signature_id = stage.signature_id.clone();
        self.stages.insert(id.0.clone(), stage);
        for old_id in &duplicates {
            if let Some(existing) = self.stages.get_mut(&old_id.0) {
                existing.lifecycle = StageLifecycle::Deprecated {
                    successor_id: id.clone(),
                };
            }
        }
        log_auto_deprecation(&duplicates, &id, signature_id.as_ref());
        self.save()?;
        Ok(id)
    }

    fn upsert(&mut self, stage: Stage) -> Result<StageId, StoreError> {
        let id = stage.id.clone();
        let duplicates = duplicate_active_ids_for_incoming(&self.stages, &stage);
        let signature_id = stage.signature_id.clone();
        self.stages.insert(id.0.clone(), stage);
        let actually_deprecated: Vec<StageId> = duplicates
            .into_iter()
            .filter(|old_id| *old_id != id)
            .collect();
        for old_id in &actually_deprecated {
            if let Some(existing) = self.stages.get_mut(&old_id.0) {
                existing.lifecycle = StageLifecycle::Deprecated {
                    successor_id: id.clone(),
                };
            }
        }
        log_auto_deprecation(&actually_deprecated, &id, signature_id.as_ref());
        self.save()?;
        Ok(id)
    }

    fn remove(&mut self, id: &StageId) -> Result<(), StoreError> {
        self.stages.remove(&id.0);
        self.save()?;
        Ok(())
    }

    fn get(&self, id: &StageId) -> Result<Option<&Stage>, StoreError> {
        Ok(self.stages.get(&id.0))
    }

    fn contains(&self, id: &StageId) -> bool {
        self.stages.contains_key(&id.0)
    }

    fn list(&self, lifecycle: Option<&StageLifecycle>) -> Vec<&Stage> {
        self.stages
            .values()
            .filter(|s| lifecycle.is_none() || lifecycle == Some(&s.lifecycle))
            .collect()
    }

    fn update_lifecycle(
        &mut self,
        id: &StageId,
        lifecycle: StageLifecycle,
    ) -> Result<(), StoreError> {
        let current = self
            .stages
            .get(&id.0)
            .ok_or_else(|| StoreError::NotFound(id.clone()))?;

        validate_transition(&current.lifecycle, &lifecycle)
            .map_err(|reason| StoreError::InvalidTransition { reason })?;

        if let StageLifecycle::Deprecated { ref successor_id } = lifecycle {
            if !self.stages.contains_key(&successor_id.0) {
                return Err(StoreError::InvalidSuccessor {
                    reason: format!("successor {successor_id:?} not found in store"),
                });
            }
        }

        // M2.3 invariant: promoting to Active deprecates any other
        // Active stage with the same signature_id.
        let (duplicates, signature_id) = if matches!(lifecycle, StageLifecycle::Active) {
            let sig = current.signature_id.clone();
            (
                duplicate_active_ids_for(&self.stages, id, sig.as_ref()),
                sig,
            )
        } else {
            (Vec::new(), None)
        };

        self.stages.get_mut(&id.0).unwrap().lifecycle = lifecycle;
        for old_id in &duplicates {
            if let Some(existing) = self.stages.get_mut(&old_id.0) {
                existing.lifecycle = StageLifecycle::Deprecated {
                    successor_id: id.clone(),
                };
            }
        }
        log_auto_deprecation(&duplicates, id, signature_id.as_ref());
        self.save()?;
        Ok(())
    }

    fn stats(&self) -> StoreStats {
        let mut by_lifecycle: BTreeMap<String, usize> = BTreeMap::new();
        let mut by_effect: BTreeMap<String, usize> = BTreeMap::new();

        for stage in self.stages.values() {
            let lc_name = match &stage.lifecycle {
                StageLifecycle::Draft => "draft",
                StageLifecycle::Active => "active",
                StageLifecycle::Deprecated { .. } => "deprecated",
                StageLifecycle::Tombstone => "tombstone",
            };
            *by_lifecycle.entry(lc_name.into()).or_default() += 1;

            for effect in stage.signature.effects.iter() {
                let effect_name = format!("{effect:?}");
                *by_effect.entry(effect_name).or_default() += 1;
            }
        }

        StoreStats {
            total: self.stages.len(),
            by_lifecycle,
            by_effect,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use noether_core::effects::EffectSet;
    use noether_core::stage::{CostEstimate, StageSignature};
    use noether_core::types::NType;
    use std::collections::BTreeSet;
    use tempfile::NamedTempFile;

    fn make_stage(id: &str) -> Stage {
        Stage {
            id: StageId(id.into()),
            signature_id: None,
            signature: StageSignature {
                input: NType::Text,
                output: NType::Number,
                effects: EffectSet::pure(),
                implementation_hash: format!("impl_{id}"),
            },
            capabilities: BTreeSet::new(),
            cost: CostEstimate {
                time_ms_p50: None,
                tokens_est: None,
                memory_mb: None,
            },
            description: "test stage".into(),
            examples: vec![],
            lifecycle: StageLifecycle::Active,
            ed25519_signature: None,
            signer_public_key: None,
            implementation_code: None,
            implementation_language: None,
            ui_style: None,
            tags: vec![],
            aliases: vec![],
            name: None,
            properties: Vec::new(),
        }
    }

    #[test]
    fn create_and_reload() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        // Create and add a stage
        {
            let mut store = JsonFileStore::open(&path).unwrap();
            store.put(make_stage("abc123")).unwrap();
            assert_eq!(store.len(), 1);
        }

        // Reload from disk
        {
            let store = JsonFileStore::open(&path).unwrap();
            assert_eq!(store.len(), 1);
            let stage = store.get(&StageId("abc123".into())).unwrap().unwrap();
            assert_eq!(stage.description, "test stage");
        }
    }

    #[test]
    fn persists_lifecycle_changes() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        {
            let mut store = JsonFileStore::open(&path).unwrap();
            store.put(make_stage("old")).unwrap();
            store.put(make_stage("new")).unwrap();
            store
                .update_lifecycle(
                    &StageId("old".into()),
                    StageLifecycle::Deprecated {
                        successor_id: StageId("new".into()),
                    },
                )
                .unwrap();
        }

        {
            let store = JsonFileStore::open(&path).unwrap();
            let stage = store.get(&StageId("old".into())).unwrap().unwrap();
            assert!(matches!(stage.lifecycle, StageLifecycle::Deprecated { .. }));
        }
    }

    #[test]
    fn auto_deprecation_persists_across_reload() {
        // Put two Active stages with the same signature through
        // JsonFileStore; after reopening the file, the first must be
        // Deprecated with the second as successor. Guards against a
        // save() that writes stale state or forgets the deprecation.
        use noether_core::stage::SignatureId;
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        {
            let mut store = JsonFileStore::open(&path).unwrap();
            let mut a = make_stage("impl_a");
            a.signature_id = Some(SignatureId("sig".into()));
            let mut b = make_stage("impl_b");
            b.signature_id = Some(SignatureId("sig".into()));
            store.put(a).unwrap();
            store.put(b).unwrap();
        }

        {
            let store = JsonFileStore::open(&path).unwrap();
            let stored_a = store.get(&StageId("impl_a".into())).unwrap().unwrap();
            match &stored_a.lifecycle {
                StageLifecycle::Deprecated { successor_id } => {
                    assert_eq!(successor_id.0, "impl_b");
                }
                other => panic!("expected Deprecated on reload, got {other:?}"),
            }
            let stored_b = store.get(&StageId("impl_b".into())).unwrap().unwrap();
            assert!(matches!(stored_b.lifecycle, StageLifecycle::Active));
        }
    }

    #[test]
    fn empty_file_creates_empty_store() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();
        // Delete the file so open() creates empty
        fs::remove_file(&path).ok();

        let store = JsonFileStore::open(&path).unwrap();
        assert_eq!(store.len(), 0);
    }
}
