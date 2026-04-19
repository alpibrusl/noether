use crate::invariant::{
    duplicate_active_ids_for, duplicate_active_ids_for_incoming, log_auto_deprecation,
};
use crate::lifecycle::validate_transition;
use crate::traits::{StageStore, StoreError, StoreStats};
use noether_core::stage::{Stage, StageId, StageLifecycle};
use std::collections::{BTreeMap, HashMap};

/// In-memory stage store for testing and development.
#[derive(Debug, Default)]
pub struct MemoryStore {
    stages: HashMap<String, Stage>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.stages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    /// Test-only escape hatch: insert a stage without the
    /// invariant-enforcement path. Exists so tests can set up
    /// deliberately-broken store states (e.g. two Active stages with
    /// the same `signature_id`) that can't be created through the
    /// public API. Do NOT use from production code.
    #[doc(hidden)]
    pub fn inject_raw_for_testing(&mut self, stage: Stage) {
        self.stages.insert(stage.id.0.clone(), stage);
    }
}

impl StageStore for MemoryStore {
    fn put(&mut self, stage: Stage) -> Result<StageId, StoreError> {
        let id = stage.id.clone();
        if self.stages.contains_key(&id.0) {
            return Err(StoreError::AlreadyExists(id));
        }

        // M2.3 invariant: at most one Active stage per signature_id.
        // If the incoming stage is Active and shares a signature_id with
        // other Active stages, auto-deprecate them with the new stage as
        // the successor. This enforces the constraint the resolver's
        // `get_by_signature` assumes — previously only the `stage add`
        // CLI path did this check; direct library `put` could violate
        // it silently.
        let duplicates = duplicate_active_ids_for_incoming(&self.stages, &stage);
        let signature_id = stage.signature_id.clone();

        self.stages.insert(id.0.clone(), stage);

        // Deprecate duplicates by direct mutation. Skipping
        // validate_transition is intentional: Active → Deprecated is
        // always valid, and the successor (just-inserted new stage)
        // is already in the store.
        for old_id in &duplicates {
            if let Some(existing) = self.stages.get_mut(&old_id.0) {
                existing.lifecycle = StageLifecycle::Deprecated {
                    successor_id: id.clone(),
                };
            }
        }
        log_auto_deprecation(&duplicates, &id, signature_id.as_ref());

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
        Ok(id)
    }

    fn remove(&mut self, id: &StageId) -> Result<(), StoreError> {
        self.stages.remove(&id.0);
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
        // Validate all preconditions before taking a mutable borrow
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

        // M2.3 invariant: when promoting to Active, check whether any
        // other Active stage shares this one's signature_id and
        // auto-deprecate it.
        let (duplicates, signature_id) = if matches!(lifecycle, StageLifecycle::Active) {
            let sig = current.signature_id.clone();
            (
                duplicate_active_ids_for(&self.stages, id, sig.as_ref()),
                sig,
            )
        } else {
            (Vec::new(), None)
        };

        // Now safe to mutate
        self.stages.get_mut(&id.0).unwrap().lifecycle = lifecycle;
        for old_id in &duplicates {
            if let Some(existing) = self.stages.get_mut(&old_id.0) {
                existing.lifecycle = StageLifecycle::Deprecated {
                    successor_id: id.clone(),
                };
            }
        }
        log_auto_deprecation(&duplicates, id, signature_id.as_ref());
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
    fn put_and_get() {
        let mut store = MemoryStore::new();
        let stage = make_stage("abc123");
        store.put(stage.clone()).unwrap();
        let retrieved = store.get(&StageId("abc123".into())).unwrap().unwrap();
        assert_eq!(retrieved.id, stage.id);
    }

    #[test]
    fn duplicate_put_fails() {
        let mut store = MemoryStore::new();
        store.put(make_stage("abc123")).unwrap();
        assert!(store.put(make_stage("abc123")).is_err());
    }

    #[test]
    fn valid_lifecycle_transition() {
        let mut store = MemoryStore::new();
        let mut draft = make_stage("abc123");
        draft.lifecycle = StageLifecycle::Draft;
        store.put(draft).unwrap();
        store
            .update_lifecycle(&StageId("abc123".into()), StageLifecycle::Active)
            .unwrap();
        let stage = store.get(&StageId("abc123".into())).unwrap().unwrap();
        assert_eq!(stage.lifecycle, StageLifecycle::Active);
    }

    #[test]
    fn invalid_lifecycle_transition_fails() {
        let mut store = MemoryStore::new();
        let mut draft = make_stage("abc123");
        draft.lifecycle = StageLifecycle::Draft;
        store.put(draft).unwrap();
        // Draft → Tombstone is invalid
        let result = store.update_lifecycle(&StageId("abc123".into()), StageLifecycle::Tombstone);
        assert!(result.is_err());
    }

    #[test]
    fn deprecation_requires_valid_successor() {
        let mut store = MemoryStore::new();
        store.put(make_stage("old")).unwrap();
        // Try to deprecate pointing to a nonexistent successor
        let result = store.update_lifecycle(
            &StageId("old".into()),
            StageLifecycle::Deprecated {
                successor_id: StageId("nonexistent".into()),
            },
        );
        assert!(result.is_err());

        // Now add the successor and try again
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

    #[test]
    fn get_by_signature_returns_active_impl() {
        use noether_core::stage::SignatureId;
        let mut store = MemoryStore::new();
        let mut stage = make_stage("impl_a");
        stage.signature_id = Some(SignatureId("sig_one".into()));
        store.put(stage).unwrap();

        let found = store.get_by_signature(&SignatureId("sig_one".into()));
        assert!(found.is_some(), "stage pinned by signature should resolve");
        assert_eq!(found.unwrap().id, StageId("impl_a".into()));

        assert!(store
            .get_by_signature(&SignatureId("sig_missing".into()))
            .is_none());
    }

    #[test]
    fn get_by_signature_skips_deprecated() {
        use noether_core::stage::SignatureId;
        let mut store = MemoryStore::new();
        // Old implementation of "sig" goes Active, new Active stage
        // becomes successor. The M2.3 invariant enforcement in
        // MemoryStore::put auto-deprecates the old one.
        let mut old = make_stage("impl_old");
        old.signature_id = Some(SignatureId("sig".into()));
        store.put(old).unwrap();
        let mut new = make_stage("impl_new");
        new.signature_id = Some(SignatureId("sig".into()));
        store.put(new).unwrap();

        // old should already be Deprecated — no manual update_lifecycle
        // needed.
        assert!(matches!(
            store
                .get(&StageId("impl_old".into()))
                .unwrap()
                .unwrap()
                .lifecycle,
            StageLifecycle::Deprecated { .. }
        ));

        let found = store.get_by_signature(&SignatureId("sig".into())).unwrap();
        assert_eq!(found.id, StageId("impl_new".into()));
    }

    #[test]
    fn put_enforces_one_active_per_signature() {
        use noether_core::stage::SignatureId;
        let mut store = MemoryStore::new();
        let mut a = make_stage("impl_a");
        a.signature_id = Some(SignatureId("sig".into()));
        let mut b = make_stage("impl_b");
        b.signature_id = Some(SignatureId("sig".into()));

        store.put(a).unwrap();
        store.put(b).unwrap();

        // impl_a must have been auto-deprecated with impl_b as successor.
        let stored_a = store.get(&StageId("impl_a".into())).unwrap().unwrap();
        match &stored_a.lifecycle {
            StageLifecycle::Deprecated { successor_id } => {
                assert_eq!(successor_id.0, "impl_b");
            }
            other => panic!("expected Deprecated, got {other:?}"),
        }

        // impl_b stays Active.
        let stored_b = store.get(&StageId("impl_b".into())).unwrap().unwrap();
        assert!(matches!(stored_b.lifecycle, StageLifecycle::Active));
    }

    #[test]
    fn put_draft_does_not_trigger_deprecation() {
        // Only an Active incoming stage should auto-deprecate. A Draft
        // put must leave existing Actives alone.
        use noether_core::stage::SignatureId;
        let mut store = MemoryStore::new();
        let mut active = make_stage("impl_active");
        active.signature_id = Some(SignatureId("sig".into()));
        let mut draft = make_stage("impl_draft");
        draft.signature_id = Some(SignatureId("sig".into()));
        draft.lifecycle = StageLifecycle::Draft;

        store.put(active).unwrap();
        store.put(draft).unwrap();

        let stored = store.get(&StageId("impl_active".into())).unwrap().unwrap();
        assert!(
            matches!(stored.lifecycle, StageLifecycle::Active),
            "draft put must not deprecate existing Active"
        );
    }

    #[test]
    fn update_lifecycle_to_active_deprecates_existing() {
        // Promoting a Draft to Active must also trigger the invariant
        // check, not just put.
        use noether_core::stage::SignatureId;
        let mut store = MemoryStore::new();
        let mut existing = make_stage("impl_existing");
        existing.signature_id = Some(SignatureId("sig".into()));
        let mut draft = make_stage("impl_new");
        draft.signature_id = Some(SignatureId("sig".into()));
        draft.lifecycle = StageLifecycle::Draft;

        store.put(existing).unwrap();
        store.put(draft).unwrap();

        // Promote draft → Active.
        store
            .update_lifecycle(&StageId("impl_new".into()), StageLifecycle::Active)
            .unwrap();

        let stored_existing = store
            .get(&StageId("impl_existing".into()))
            .unwrap()
            .unwrap();
        assert!(
            matches!(stored_existing.lifecycle, StageLifecycle::Deprecated { .. }),
            "existing Active must be auto-deprecated when another stage \
             with the same signature is promoted to Active"
        );
    }

    #[test]
    fn upsert_enforces_one_active_per_signature() {
        // Same invariant as `put`, but via the `upsert` codepath. An
        // upsert writing a new Active with the same signature as an
        // existing Active must auto-deprecate the existing one.
        use noether_core::stage::SignatureId;
        let mut store = MemoryStore::new();
        let mut a = make_stage("impl_a");
        a.signature_id = Some(SignatureId("sig".into()));
        let mut b = make_stage("impl_b");
        b.signature_id = Some(SignatureId("sig".into()));

        store.upsert(a).unwrap();
        store.upsert(b).unwrap();

        let stored_a = store.get(&StageId("impl_a".into())).unwrap().unwrap();
        match &stored_a.lifecycle {
            StageLifecycle::Deprecated { successor_id } => {
                assert_eq!(successor_id.0, "impl_b");
            }
            other => panic!("expected Deprecated, got {other:?}"),
        }

        let stored_b = store.get(&StageId("impl_b".into())).unwrap().unwrap();
        assert!(matches!(stored_b.lifecycle, StageLifecycle::Active));
    }

    #[test]
    fn upsert_replacing_self_does_not_deprecate_self() {
        // Upserting a stage onto itself (same id) must leave it Active.
        // Regression check: an earlier draft of the invariant helper
        // would have flagged the stage as a duplicate of itself.
        use noether_core::stage::SignatureId;
        let mut store = MemoryStore::new();
        let mut stage = make_stage("impl");
        stage.signature_id = Some(SignatureId("sig".into()));
        store.upsert(stage.clone()).unwrap();
        store.upsert(stage).unwrap();

        let stored = store.get(&StageId("impl".into())).unwrap().unwrap();
        assert!(
            matches!(stored.lifecycle, StageLifecycle::Active),
            "self-upsert must not auto-deprecate"
        );
    }

    #[test]
    fn list_filters_by_lifecycle() {
        let mut store = MemoryStore::new();
        store.put(make_stage("a")).unwrap();
        let mut draft = make_stage("b");
        draft.lifecycle = StageLifecycle::Draft;
        store.put(draft).unwrap();

        let active = store.list(Some(&StageLifecycle::Active));
        assert_eq!(active.len(), 1);
        let all = store.list(None);
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn stats_returns_counts() {
        let mut store = MemoryStore::new();
        store.put(make_stage("a")).unwrap();
        store.put(make_stage("b")).unwrap();
        let mut draft = make_stage("c");
        draft.lifecycle = StageLifecycle::Draft;
        store.put(draft).unwrap();

        let stats = store.stats();
        assert_eq!(stats.total, 3);
        assert_eq!(stats.by_lifecycle.get("active"), Some(&2));
        assert_eq!(stats.by_lifecycle.get("draft"), Some(&1));
    }
}
