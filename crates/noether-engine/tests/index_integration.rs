use noether_core::stdlib::load_stdlib;
use noether_engine::index::embedding::MockEmbeddingProvider;
use noether_engine::index::{IndexConfig, SemanticIndex};
use noether_store::{MemoryStore, StageStore};
use std::time::Instant;

fn init_store() -> MemoryStore {
    let mut store = MemoryStore::new();
    for stage in load_stdlib() {
        store.put(stage).unwrap();
    }
    store
}

fn build_index(store: &MemoryStore) -> SemanticIndex {
    SemanticIndex::build(
        store,
        Box::new(MockEmbeddingProvider::new(128)),
        IndexConfig::default(),
    )
    .unwrap()
}

#[test]
fn index_all_stdlib_stages() {
    let store = init_store();
    let index = build_index(&store);
    assert_eq!(index.len(), 80); // 76 existing + 4 process stages
}

#[test]
fn search_returns_results_for_any_query() {
    let store = init_store();
    let index = build_index(&store);
    let results = index.search("convert text to number", 10).unwrap();
    assert!(!results.is_empty());
    assert!(results.len() <= 10);
}

#[test]
fn search_exact_description_ranks_high() {
    let store = init_store();
    let index = build_index(&store);

    // Search with the full text that the `to_text` stage embeds (description + aliases + tags).
    // MockEmbeddingProvider hashes the exact string, so using the same text as the index produces
    // cosine similarity = 1.0.
    let full_text = "Convert any value to its text representation\nAliases: stringify, to_string, num_to_str\nTags: scalar, conversion, pure";
    let results = index.search(full_text, 5).unwrap();
    assert!(!results.is_empty());
    // Top result must have a high semantic score
    assert!(
        results[0].semantic_score > 0.8,
        "Expected high semantic score for exact match, got {}",
        results[0].semantic_score
    );
}

#[test]
fn search_respects_top_k() {
    let store = init_store();
    let index = build_index(&store);
    let results = index.search("anything", 5).unwrap();
    assert!(results.len() <= 5);
}

#[test]
fn search_results_are_sorted_by_score() {
    let store = init_store();
    let index = build_index(&store);
    let results = index.search("text processing", 20).unwrap();
    for i in 1..results.len() {
        assert!(
            results[i - 1].score >= results[i].score,
            "Results should be sorted descending by score"
        );
    }
}

#[test]
fn search_performance() {
    let store = init_store();
    let index = build_index(&store);

    let start = Instant::now();
    for _ in 0..100 {
        let _ = index.search("convert text to number", 20).unwrap();
    }
    let elapsed = start.elapsed();
    assert!(
        elapsed.as_millis() < 500,
        "100 searches should complete in < 500ms, took {}ms",
        elapsed.as_millis()
    );
}

#[test]
fn empty_store_search() {
    let store = MemoryStore::new();
    let index = SemanticIndex::build(
        &store,
        Box::new(MockEmbeddingProvider::new(128)),
        IndexConfig::default(),
    )
    .unwrap();
    let results = index.search("anything", 10).unwrap();
    assert!(results.is_empty());
}

#[test]
fn search_all_scores_are_non_negative() {
    let store = init_store();
    let index = build_index(&store);
    let results = index.search("sort filter map", 20).unwrap();
    for r in &results {
        assert!(
            r.score >= 0.0,
            "Fused score should be >= 0, got {}",
            r.score
        );
    }
}
