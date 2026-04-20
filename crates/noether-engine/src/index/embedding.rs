#![warn(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

use sha2::{Digest, Sha256};

pub type Embedding = Vec<f32>;

#[derive(Debug, thiserror::Error)]
pub enum EmbeddingError {
    #[error("embedding provider error: {0}")]
    Provider(String),
}

/// Trait for generating vector embeddings from text.
pub trait EmbeddingProvider: Send + Sync {
    fn dimensions(&self) -> usize;
    fn embed(&self, text: &str) -> Result<Embedding, EmbeddingError>;

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Embedding>, EmbeddingError> {
        texts.iter().map(|t| self.embed(t)).collect()
    }
}

/// Deterministic mock embedding provider using SHA-256 hashing.
///
/// Produces normalized vectors where identical text always yields identical
/// embeddings. Different text yields uncorrelated embeddings. No semantic
/// similarity — purely structural; use a real provider for semantic quality.
pub struct MockEmbeddingProvider {
    dimensions: usize,
}

impl MockEmbeddingProvider {
    pub fn new(dimensions: usize) -> Self {
        Self { dimensions }
    }
}

impl EmbeddingProvider for MockEmbeddingProvider {
    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, text: &str) -> Result<Embedding, EmbeddingError> {
        // Generate deterministic bytes by iteratively hashing
        let mut bytes = Vec::with_capacity(self.dimensions);
        let mut current = Sha256::digest(text.as_bytes()).to_vec();
        while bytes.len() < self.dimensions {
            for &b in &current {
                if bytes.len() >= self.dimensions {
                    break;
                }
                bytes.push(b);
            }
            // Hash the current hash to get more bytes
            current = Sha256::digest(&current).to_vec();
        }

        let mut vec: Vec<f32> = bytes[..self.dimensions]
            .iter()
            .map(|&b| (b as f32 / 127.5) - 1.0)
            .collect();

        // L2-normalize
        let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for v in &mut vec {
                *v /= norm;
            }
        }

        Ok(vec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_produces_correct_dimensions() {
        let provider = MockEmbeddingProvider::new(64);
        let emb = provider.embed("hello").unwrap();
        assert_eq!(emb.len(), 64);
    }

    #[test]
    fn mock_is_deterministic() {
        let provider = MockEmbeddingProvider::new(32);
        let e1 = provider.embed("hello world").unwrap();
        let e2 = provider.embed("hello world").unwrap();
        assert_eq!(e1, e2);
    }

    #[test]
    fn mock_different_text_different_embedding() {
        let provider = MockEmbeddingProvider::new(32);
        let e1 = provider.embed("hello").unwrap();
        let e2 = provider.embed("world").unwrap();
        assert_ne!(e1, e2);
    }

    #[test]
    fn mock_embeddings_are_normalized() {
        let provider = MockEmbeddingProvider::new(128);
        let emb = provider.embed("test text").unwrap();
        let norm: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm should be ~1.0, got {norm}");
    }

    #[test]
    fn mock_batch_matches_individual() {
        let provider = MockEmbeddingProvider::new(32);
        let batch = provider.embed_batch(&["a", "b", "c"]).unwrap();
        let individual: Vec<Embedding> = ["a", "b", "c"]
            .iter()
            .map(|t| provider.embed(t).unwrap())
            .collect();
        assert_eq!(batch, individual);
    }
}
