use super::embedding::Embedding;
use noether_core::stage::StageId;

/// A single entry in a sub-index.
#[derive(Debug, Clone)]
pub struct IndexEntry {
    pub stage_id: StageId,
    pub embedding: Embedding,
}

/// One of the three sub-indexes (signature, semantic, or example).
#[derive(Debug, Clone, Default)]
pub struct SubIndex {
    entries: Vec<IndexEntry>,
}

/// A search result from a single sub-index.
#[derive(Debug, Clone)]
pub struct SubSearchResult {
    pub stage_id: StageId,
    pub score: f32,
}

impl SubIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, stage_id: StageId, embedding: Embedding) {
        self.entries.push(IndexEntry {
            stage_id,
            embedding,
        });
    }

    pub fn remove(&mut self, stage_id: &StageId) {
        self.entries.retain(|e| &e.stage_id != stage_id);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Read-only access to all entries (used by near-duplicate scanning).
    pub fn entries(&self) -> &[IndexEntry] {
        &self.entries
    }

    /// Brute-force search: compute cosine similarity against all entries,
    /// return top-k results sorted by descending score.
    pub fn search(&self, query: &Embedding, top_k: usize) -> Vec<SubSearchResult> {
        let mut scored: Vec<SubSearchResult> = self
            .entries
            .iter()
            .map(|entry| SubSearchResult {
                stage_id: entry.stage_id.clone(),
                score: cosine_similarity(query, &entry.embedding),
            })
            .collect();

        // Sort descending by score
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(top_k);
        scored
    }
}

/// Cosine similarity between two vectors. Returns value in [-1, 1].
/// For normalized vectors, this reduces to dot product.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical_vectors() {
        let v = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_vectors() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn cosine_opposite_vectors() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn subindex_search_returns_top_k() {
        let mut idx = SubIndex::new();
        for i in 0..10 {
            let mut emb = vec![0.0; 4];
            emb[i % 4] = 1.0;
            idx.add(StageId(format!("s{i}")), emb);
        }
        let query = vec![1.0, 0.0, 0.0, 0.0];
        let results = idx.search(&query, 3);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn subindex_search_sorted_by_score() {
        let mut idx = SubIndex::new();
        idx.add(StageId("a".into()), vec![1.0, 0.0]);
        idx.add(StageId("b".into()), vec![0.5, 0.5]);
        idx.add(StageId("c".into()), vec![0.0, 1.0]);
        let query = vec![1.0, 0.0];
        let results = idx.search(&query, 3);
        assert!(results[0].score >= results[1].score);
        assert!(results[1].score >= results[2].score);
    }

    #[test]
    fn subindex_empty_returns_empty() {
        let idx = SubIndex::new();
        let results = idx.search(&vec![1.0, 0.0], 5);
        assert!(results.is_empty());
    }

    #[test]
    fn subindex_remove() {
        let mut idx = SubIndex::new();
        idx.add(StageId("a".into()), vec![1.0, 0.0]);
        idx.add(StageId("b".into()), vec![0.0, 1.0]);
        assert_eq!(idx.len(), 2);
        idx.remove(&StageId("a".into()));
        assert_eq!(idx.len(), 1);
    }
}
