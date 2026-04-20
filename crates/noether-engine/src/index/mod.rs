#![warn(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod cache;
pub mod embedding;
pub mod search;
pub mod text;

use embedding::{EmbeddingError, EmbeddingProvider};
use noether_core::stage::{Stage, StageId, StageLifecycle};
use noether_store::StageStore;
use search::SubIndex;
use std::collections::BTreeMap;
use std::collections::HashMap;

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
    /// Exact-match tag → stage IDs lookup for fast `search_filtered` pre-filtering.
    tag_map: HashMap<String, Vec<StageId>>,
}

impl SemanticIndex {
    /// Build the index from an owned list of stages (useful in async contexts
    /// where holding a `&dyn StageStore` across `.await` is not possible).
    pub fn from_stages(
        stages: Vec<Stage>,
        provider: Box<dyn EmbeddingProvider>,
        config: IndexConfig,
    ) -> Result<Self, EmbeddingError> {
        let mut index = Self {
            provider,
            signature_index: SubIndex::new(),
            semantic_index: SubIndex::new(),
            example_index: SubIndex::new(),
            config,
            tag_map: HashMap::new(),
        };
        for stage in &stages {
            if matches!(stage.lifecycle, StageLifecycle::Tombstone) {
                continue;
            }
            index.add_stage(stage)?;
        }
        Ok(index)
    }

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
            tag_map: HashMap::new(),
        };
        for stage in store.list(None) {
            if matches!(stage.lifecycle, StageLifecycle::Tombstone) {
                continue;
            }
            index.add_stage(stage)?;
        }
        Ok(index)
    }

    /// Build the index in a single pass: collect every signature/description/
    /// example text upfront, dispatch all cache misses through
    /// `inner.embed_batch` in chunks of `chunk_size`, then assemble the three
    /// sub-indexes. Used by noether-cloud's registry on cold start so that
    /// 486 stages × 3 texts = 1458 individual API calls collapse into ~46
    /// batch calls of 32 texts each — well within typical rate limits.
    pub fn from_stages_batched(
        stages: Vec<Stage>,
        cached_provider: cache::CachedEmbeddingProvider,
        config: IndexConfig,
        chunk_size: usize,
    ) -> Result<Self, EmbeddingError> {
        Self::from_stages_batched_paced(
            stages,
            cached_provider,
            config,
            chunk_size,
            std::time::Duration::ZERO,
        )
    }

    /// Like `from_stages_batched`, but waits `inter_batch_delay` between
    /// successive batch calls and commits cache entries to disk after each
    /// batch. Use this with rate-limited remote providers (e.g. Mistral
    /// free tier ≈ 1 req/s → pass ~1100 ms).
    pub fn from_stages_batched_paced(
        stages: Vec<Stage>,
        mut cached_provider: cache::CachedEmbeddingProvider,
        config: IndexConfig,
        chunk_size: usize,
        inter_batch_delay: std::time::Duration,
    ) -> Result<Self, EmbeddingError> {
        // Filter active stages once and pre-compute all three texts per stage.
        let active: Vec<&Stage> = stages
            .iter()
            .filter(|s| !matches!(s.lifecycle, StageLifecycle::Tombstone))
            .collect();

        let mut all_texts: Vec<String> = Vec::with_capacity(active.len() * 3);
        for s in &active {
            all_texts.push(text::signature_text(s));
            all_texts.push(text::description_text(s));
            all_texts.push(text::examples_text(s));
        }
        let text_refs: Vec<&str> = all_texts.iter().map(|s| s.as_str()).collect();
        let embeddings =
            cached_provider.embed_batch_cached_paced(&text_refs, chunk_size, inter_batch_delay)?;
        cached_provider.flush();

        // Distribute back into the three sub-indexes in stride 3.
        let mut signature_index = SubIndex::new();
        let mut semantic_index = SubIndex::new();
        let mut example_index = SubIndex::new();
        let mut tag_map: HashMap<String, Vec<StageId>> = HashMap::new();

        for (i, s) in active.iter().enumerate() {
            signature_index.add(s.id.clone(), embeddings[i * 3].clone());
            semantic_index.add(s.id.clone(), embeddings[i * 3 + 1].clone());
            example_index.add(s.id.clone(), embeddings[i * 3 + 2].clone());
            for tag in &s.tags {
                tag_map.entry(tag.clone()).or_default().push(s.id.clone());
            }
        }

        Ok(Self {
            provider: Box::new(cached_provider),
            signature_index,
            semantic_index,
            example_index,
            config,
            tag_map,
        })
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
        let mut tag_map: HashMap<String, Vec<StageId>> = HashMap::new();

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

            for tag in &stage.tags {
                tag_map
                    .entry(tag.clone())
                    .or_default()
                    .push(stage.id.clone());
            }
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
            tag_map,
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

        for tag in &stage.tags {
            self.tag_map
                .entry(tag.clone())
                .or_default()
                .push(stage.id.clone());
        }

        Ok(())
    }

    /// Remove a stage from all three indexes.
    pub fn remove_stage(&mut self, stage_id: &StageId) {
        self.signature_index.remove(stage_id);
        self.semantic_index.remove(stage_id);
        self.example_index.remove(stage_id);

        for ids in self.tag_map.values_mut() {
            ids.retain(|id| id != stage_id);
        }
        self.tag_map.retain(|_, ids| !ids.is_empty());
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
        self.search_filtered(query, top_k, None)
    }

    /// Like `search`, but restricts candidates to stages carrying `tag` (exact match).
    /// Passing `tag: None` is equivalent to `search`.
    pub fn search_filtered(
        &self,
        query: &str,
        top_k: usize,
        tag: Option<&str>,
    ) -> Result<Vec<SearchResult>, EmbeddingError> {
        let query_emb = self.provider.embed(query)?;
        let fetch_k = top_k * 2;

        let sig_results = self.signature_index.search(&query_emb, fetch_k);
        let sem_results = self.semantic_index.search(&query_emb, fetch_k);
        let ex_results = self.example_index.search(&query_emb, fetch_k);

        // Optional tag allow-list for filtering
        let allowed: Option<std::collections::BTreeSet<&str>> = tag.map(|t| {
            self.tag_map
                .get(t)
                .map(|ids| ids.iter().map(|id| id.0.as_str()).collect())
                .unwrap_or_default()
        });

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
            .filter(|(id, _)| {
                allowed
                    .as_ref()
                    .map(|a| a.contains(id.as_str()))
                    .unwrap_or(true)
            })
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

    /// Return all stage IDs that carry `tag` (exact match).
    pub fn search_by_tag(&self, tag: &str) -> Vec<StageId> {
        self.tag_map.get(tag).cloned().unwrap_or_default()
    }

    /// Return the set of all known tags across indexed stages.
    pub fn all_tags(&self) -> Vec<String> {
        let mut tags: Vec<String> = self.tag_map.keys().cloned().collect();
        tags.sort();
        tags
    }

    /// Check whether a candidate description is a near-duplicate of an existing stage.
    ///
    /// Returns `Some((stage_id, similarity))` if any existing stage's semantic embedding
    /// exceeds `threshold` (default 0.92). Returns `None` if the description is novel enough.
    pub fn check_duplicate_before_insert(
        &self,
        description: &str,
        threshold: f32,
    ) -> Result<Option<(StageId, f32)>, EmbeddingError> {
        let emb = self.provider.embed(description)?;
        let results = self.semantic_index.search(&emb, 1);
        if let Some(top) = results.first() {
            if top.score >= threshold {
                return Ok(Some((top.stage_id.clone(), top.score)));
            }
        }
        Ok(None)
    }

    /// Scan all active stages for near-duplicate pairs.
    ///
    /// Returns pairs `(id_a, id_b, similarity)` where semantic similarity >= `threshold`.
    /// Each pair appears only once (id_a < id_b lexicographically).
    pub fn find_near_duplicates(&self, threshold: f32) -> Vec<(StageId, StageId, f32)> {
        use search::cosine_similarity;

        let entries = self.semantic_index.entries().to_vec();
        let mut pairs: Vec<(StageId, StageId, f32)> = Vec::new();

        for i in 0..entries.len() {
            for j in (i + 1)..entries.len() {
                let sim = cosine_similarity(&entries[i].embedding, &entries[j].embedding);
                if sim >= threshold {
                    let (a, b) = if entries[i].stage_id.0 < entries[j].stage_id.0 {
                        (entries[i].stage_id.clone(), entries[j].stage_id.clone())
                    } else {
                        (entries[j].stage_id.clone(), entries[i].stage_id.clone())
                    };
                    pairs.push((a, b, sim));
                }
            }
        }

        // Sort by similarity descending
        pairs.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
        pairs
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
            signature_id: None,
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
            implementation_code: None,
            implementation_language: None,
            ui_style: None,
            tags: vec![],
            aliases: vec![],
            name: None,
            properties: Vec::new(),
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

    #[test]
    fn search_by_tag_returns_matching_stages() {
        let mut s1 = make_stage("s1", "http get request", NType::Text, NType::Text);
        s1.tags = vec!["network".into(), "io".into()];
        let mut s2 = make_stage("s2", "text length", NType::Text, NType::Number);
        s2.tags = vec!["text".into(), "pure".into()];

        let stages = vec![s1, s2];
        let index = SemanticIndex::from_stages(
            stages,
            Box::new(MockEmbeddingProvider::new(32)),
            IndexConfig::default(),
        )
        .unwrap();

        let network_ids = index.search_by_tag("network");
        assert_eq!(network_ids.len(), 1);
        assert_eq!(network_ids[0], StageId("s1".into()));

        let pure_ids = index.search_by_tag("pure");
        assert_eq!(pure_ids.len(), 1);
        assert_eq!(pure_ids[0], StageId("s2".into()));

        let missing = index.search_by_tag("nonexistent");
        assert!(missing.is_empty());
    }

    #[test]
    fn all_tags_returns_sorted_set() {
        let mut s1 = make_stage("s1", "a", NType::Text, NType::Text);
        s1.tags = vec!["zebra".into(), "apple".into()];
        let index = SemanticIndex::from_stages(
            vec![s1],
            Box::new(MockEmbeddingProvider::new(32)),
            IndexConfig::default(),
        )
        .unwrap();
        let tags = index.all_tags();
        assert_eq!(tags, vec!["apple", "zebra"]);
    }

    #[test]
    fn search_filtered_restricts_to_tag() {
        let mut s1 = make_stage("s1", "http get request", NType::Text, NType::Text);
        s1.tags = vec!["network".into()];
        let s2 = make_stage("s2", "sort list", NType::Text, NType::Text);

        let stages = vec![s1, s2];
        let index = SemanticIndex::from_stages(
            stages,
            Box::new(MockEmbeddingProvider::new(32)),
            IndexConfig::default(),
        )
        .unwrap();

        let filtered = index
            .search_filtered("anything", 10, Some("network"))
            .unwrap();
        assert!(filtered.iter().all(|r| r.stage_id == StageId("s1".into())));

        let all = index.search_filtered("anything", 10, None).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn remove_stage_cleans_tag_map() {
        let mut s1 = make_stage("s1", "a", NType::Text, NType::Text);
        s1.tags = vec!["mytag".into()];
        let mut index = SemanticIndex::from_stages(
            vec![s1],
            Box::new(MockEmbeddingProvider::new(32)),
            IndexConfig::default(),
        )
        .unwrap();
        assert_eq!(index.search_by_tag("mytag").len(), 1);
        index.remove_stage(&StageId("s1".into()));
        assert!(index.search_by_tag("mytag").is_empty());
        assert!(index.all_tags().is_empty());
    }
}
