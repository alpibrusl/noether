pub mod cache;
pub mod embedding;
pub mod search;
pub mod text;

use embedding::{EmbeddingError, EmbeddingProvider};
use noether_core::stage::{Stage, StageId, StageLifecycle};
use noether_store::StageStore;
use search::SubIndex;
use std::collections::BTreeMap;

/// Configuration for search result fusion weights.
pub struct IndexConfig {
    /// Weight for type signature similarity (default: 0.3).
    pub signature_weight: f32,
    /// Weight for description similarity (default: 0.5).
    pub semantic_weight: f32,
    /// Weight for example similarity (default: 0.2).
    pub example_weight: f32,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            signature_weight: 0.3,
            semantic_weight: 0.5,
            example_weight: 0.2,
        }
    }
}

/// A search result with fused scores from all three indexes.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub stage_id: StageId,
    pub score: f32,
    pub signature_score: f32,
    pub semantic_score: f32,
    pub example_score: f32,
}

/// Three-index semantic search over the stage store.
pub struct SemanticIndex {
    provider: Box<dyn EmbeddingProvider>,
    signature_index: SubIndex,
    semantic_index: SubIndex,
    example_index: SubIndex,
    config: IndexConfig,
}

impl SemanticIndex {
    /// Build the index from all non-tombstoned stages in a store.
    pub fn build(
        store: &dyn StageStore,
        provider: Box<dyn EmbeddingProvider>,
        config: IndexConfig,
    ) -> Result<Self, EmbeddingError> {
        let mut index = Self {
            provider,
            signature_index: SubIndex::new(),
            semantic_index: SubIndex::new(),
            example_index: SubIndex::new(),
            config,
        };
        for stage in store.list(None) {
            if matches!(stage.lifecycle, StageLifecycle::Tombstone) {
                continue;
            }
            index.add_stage(stage)?;
        }
        Ok(index)
    }

    /// Build using a CachedEmbeddingProvider for persistent embedding cache.
    pub fn build_cached(
        store: &dyn StageStore,
        mut cached_provider: cache::CachedEmbeddingProvider,
        config: IndexConfig,
    ) -> Result<Self, EmbeddingError> {
        let mut signature_index = SubIndex::new();
        let mut semantic_index = SubIndex::new();
        let mut example_index = SubIndex::new();

        for stage in store.list(None) {
            if matches!(stage.lifecycle, StageLifecycle::Tombstone) {
                continue;
            }
            let sig_emb = cached_provider.embed_cached(&text::signature_text(stage))?;
            let desc_emb = cached_provider.embed_cached(&text::description_text(stage))?;
            let ex_emb = cached_provider.embed_cached(&text::examples_text(stage))?;

            signature_index.add(stage.id.clone(), sig_emb);
            semantic_index.add(stage.id.clone(), desc_emb);
            example_index.add(stage.id.clone(), ex_emb);
        }

        cached_provider.flush();

        // Wrap the inner provider for future queries
        let provider: Box<dyn EmbeddingProvider> = Box::new(cached_provider);

        Ok(Self {
            provider,
            signature_index,
            semantic_index,
            example_index,
            config,
        })
    }

    /// Add a single stage to all three indexes.
    pub fn add_stage(&mut self, stage: &Stage) -> Result<(), EmbeddingError> {
        let sig_text = text::signature_text(stage);
        let desc_text = text::description_text(stage);
        let ex_text = text::examples_text(stage);

        let sig_emb = self.provider.embed(&sig_text)?;
        let desc_emb = self.provider.embed(&desc_text)?;
        let ex_emb = self.provider.embed(&ex_text)?;

        self.signature_index.add(stage.id.clone(), sig_emb);
        self.semantic_index.add(stage.id.clone(), desc_emb);
        self.example_index.add(stage.id.clone(), ex_emb);

        Ok(())
    }

    /// Remove a stage from all three indexes.
    pub fn remove_stage(&mut self, stage_id: &StageId) {
        self.signature_index.remove(stage_id);
        self.semantic_index.remove(stage_id);
        self.example_index.remove(stage_id);
    }

    /// Number of stages indexed.
    pub fn len(&self) -> usize {
        self.signature_index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Search across all three indexes and return ranked results.
    pub fn search(&self, query: &str, top_k: usize) -> Result<Vec<SearchResult>, EmbeddingError> {
        let query_emb = self.provider.embed(query)?;
        let fetch_k = top_k * 2;

        let sig_results = self.signature_index.search(&query_emb, fetch_k);
        let sem_results = self.semantic_index.search(&query_emb, fetch_k);
        let ex_results = self.example_index.search(&query_emb, fetch_k);

        // Collect scores per stage_id
        let mut scores: BTreeMap<String, (f32, f32, f32)> = BTreeMap::new();
        for r in &sig_results {
            scores.entry(r.stage_id.0.clone()).or_default().0 = r.score;
        }
        for r in &sem_results {
            scores.entry(r.stage_id.0.clone()).or_default().1 = r.score;
        }
        for r in &ex_results {
            scores.entry(r.stage_id.0.clone()).or_default().2 = r.score;
        }

        // Fuse scores
        let mut results: Vec<SearchResult> = scores
            .into_iter()
            .map(|(id, (sig, sem, ex))| {
                let fused = self.config.signature_weight * sig.max(0.0)
                    + self.config.semantic_weight * sem.max(0.0)
                    + self.config.example_weight * ex.max(0.0);
                SearchResult {
                    stage_id: StageId(id),
                    score: fused,
                    signature_score: sig,
                    semantic_score: sem,
                    example_score: ex,
                }
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(top_k);
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedding::MockEmbeddingProvider;
    use noether_core::effects::EffectSet;
    use noether_core::stage::{CostEstimate, StageSignature};
    use noether_core::types::NType;
    use noether_store::MemoryStore;
    use std::collections::BTreeSet;

    fn make_stage(id: &str, desc: &str, input: NType, output: NType) -> Stage {
        Stage {
            id: StageId(id.into()),
            signature: StageSignature {
                input,
                output,
                effects: EffectSet::pure(),
                implementation_hash: format!("impl_{id}"),
            },
            capabilities: BTreeSet::new(),
            cost: CostEstimate {
                time_ms_p50: None,
                tokens_est: None,
                memory_mb: None,
            },
            description: desc.into(),
            examples: vec![],
            lifecycle: StageLifecycle::Active,
            ed25519_signature: None,
            signer_public_key: None,
        }
    }

    fn test_store() -> MemoryStore {
        let mut store = MemoryStore::new();
        store
            .put(make_stage(
                "s1",
                "convert text to number",
                NType::Text,
                NType::Number,
            ))
            .unwrap();
        store
            .put(make_stage(
                "s2",
                "make http request",
                NType::Text,
                NType::Text,
            ))
            .unwrap();
        store
            .put(make_stage(
                "s3",
                "sort a list of items",
                NType::List(Box::new(NType::Any)),
                NType::List(Box::new(NType::Any)),
            ))
            .unwrap();
        store
    }

    #[test]
    fn build_indexes_all_stages() {
        let store = test_store();
        let index = SemanticIndex::build(
            &store,
            Box::new(MockEmbeddingProvider::new(32)),
            IndexConfig::default(),
        )
        .unwrap();
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn add_stage_increments_count() {
        let store = test_store();
        let mut index = SemanticIndex::build(
            &store,
            Box::new(MockEmbeddingProvider::new(32)),
            IndexConfig::default(),
        )
        .unwrap();
        assert_eq!(index.len(), 3);
        index
            .add_stage(&make_stage("s4", "new stage", NType::Bool, NType::Text))
            .unwrap();
        assert_eq!(index.len(), 4);
    }

    #[test]
    fn remove_stage_decrements_count() {
        let store = test_store();
        let mut index = SemanticIndex::build(
            &store,
            Box::new(MockEmbeddingProvider::new(32)),
            IndexConfig::default(),
        )
        .unwrap();
        index.remove_stage(&StageId("s1".into()));
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn search_returns_results() {
        let store = test_store();
        let index = SemanticIndex::build(
            &store,
            Box::new(MockEmbeddingProvider::new(32)),
            IndexConfig::default(),
        )
        .unwrap();
        let results = index.search("convert text", 10).unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn search_respects_top_k() {
        let store = test_store();
        let index = SemanticIndex::build(
            &store,
            Box::new(MockEmbeddingProvider::new(32)),
            IndexConfig::default(),
        )
        .unwrap();
        let results = index.search("anything", 2).unwrap();
        assert!(results.len() <= 2);
    }

    #[test]
    fn search_self_is_top_result() {
        let store = test_store();
        let index = SemanticIndex::build(
            &store,
            Box::new(MockEmbeddingProvider::new(128)),
            IndexConfig::default(),
        )
        .unwrap();
        // Searching with exact description should return that stage highly ranked
        let results = index.search("convert text to number", 3).unwrap();
        assert!(!results.is_empty());
        // With mock embeddings, the exact description match should have the highest
        // semantic score (identical hash → identical embedding → cosine sim = 1.0)
        let top = &results[0];
        assert!(
            top.semantic_score > 0.9,
            "Expected high semantic score for exact match, got {}",
            top.semantic_score
        );
    }

    #[test]
    fn tombstoned_stages_not_indexed() {
        let mut store = MemoryStore::new();
        let mut s = make_stage("s1", "active stage", NType::Text, NType::Text);
        store.put(s.clone()).unwrap();
        s.id = StageId("s2".into());
        s.description = "tombstoned stage".into();
        s.lifecycle = StageLifecycle::Tombstone;
        store.put(s).unwrap();

        let index = SemanticIndex::build(
            &store,
            Box::new(MockEmbeddingProvider::new(32)),
            IndexConfig::default(),
        )
        .unwrap();
        assert_eq!(index.len(), 1);
    }
}
