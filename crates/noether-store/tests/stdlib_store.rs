use noether_core::stage::StageLifecycle;
use noether_core::stdlib::load_stdlib;
use noether_store::{MemoryStore, StageStore};

#[test]
fn load_all_stdlib_into_store() {
    let mut store = MemoryStore::new();
    let stages = load_stdlib();
    for stage in stages {
        store.put(stage).unwrap();
    }
    assert_eq!(store.len(), 85); // + 3 generic (slice 3) + 1 row-poly (mark_done)
}

#[test]
fn all_stdlib_stages_are_active_in_store() {
    let mut store = MemoryStore::new();
    for stage in load_stdlib() {
        store.put(stage).unwrap();
    }
    let active = store.list(Some(&StageLifecycle::Active));
    assert_eq!(active.len(), 85); // + 3 generic (slice 3) + 1 row-poly (mark_done)
}

#[test]
fn store_stats_after_stdlib_load() {
    let mut store = MemoryStore::new();
    for stage in load_stdlib() {
        store.put(stage).unwrap();
    }
    let stats = store.stats();
    assert_eq!(stats.total, 85);
    assert_eq!(stats.by_lifecycle.get("active"), Some(&85));
}

#[test]
fn lifecycle_transition_draft_to_active() {
    let mut store = MemoryStore::new();
    for stage in load_stdlib() {
        store.put(stage).unwrap();
    }

    // Add a draft stage
    let stdlib = load_stdlib();
    let mut draft = stdlib[0].clone();
    draft.id = noether_core::stage::StageId("draft_stage_id".into());
    draft.lifecycle = StageLifecycle::Draft;
    store.put(draft).unwrap();

    // Transition to active
    store
        .update_lifecycle(
            &noether_core::stage::StageId("draft_stage_id".into()),
            StageLifecycle::Active,
        )
        .unwrap();
}

#[test]
fn deprecation_with_successor() {
    let mut store = MemoryStore::new();
    for stage in load_stdlib() {
        store.put(stage).unwrap();
    }

    let stages = load_stdlib();
    let old_id = stages[0].id.clone();
    let new_id = stages[1].id.clone();

    // Deprecate the first stage, pointing to the second
    store
        .update_lifecycle(
            &old_id,
            StageLifecycle::Deprecated {
                successor_id: new_id,
            },
        )
        .unwrap();
}

#[test]
fn invalid_deprecation_fails() {
    let mut store = MemoryStore::new();
    for stage in load_stdlib() {
        store.put(stage).unwrap();
    }

    let stages = load_stdlib();
    let result = store.update_lifecycle(
        &stages[0].id,
        StageLifecycle::Deprecated {
            successor_id: noether_core::stage::StageId("nonexistent".into()),
        },
    );
    assert!(result.is_err());
}
