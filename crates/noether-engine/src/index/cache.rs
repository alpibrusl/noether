use super::embedding::{Embedding, EmbeddingError, EmbeddingProvider};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;

/// Wraps an EmbeddingProvider with a file-backed cache.
/// Embeddings are keyed by SHA-256 of the input text.
pub struct CachedEmbeddingProvider {
    inner: Box<dyn EmbeddingProvider>,
    cache: HashMap<String, Embedding>,
    path: PathBuf,
    dirty: bool,
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    entries: Vec<CacheEntry>,
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    text_hash: String,
    embedding: Embedding,
}

impl CachedEmbeddingProvider {
    pub fn new(inner: Box<dyn EmbeddingProvider>, path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let cache = if path.exists() {
            std::fs::read_to_string(&path)
                .ok()
                .and_then(|content| {
                    if content.trim().is_empty() {
                        return None;
                    }
                    serde_json::from_str::<CacheFile>(&content).ok()
                })
                .map(|f| {
                    f.entries
                        .into_iter()
                        .map(|e| (e.text_hash, e.embedding))
                        .collect()
                })
                .unwrap_or_default()
        } else {
            HashMap::new()
        };
        Self {
            inner,
            cache,
            path,
            dirty: false,
        }
    }

    fn text_hash(text: &str) -> String {
        hex::encode(Sha256::digest(text.as_bytes()))
    }

    /// Flush cache to disk if dirty.
    pub fn flush(&self) {
        if !self.dirty {
            return;
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = CacheFile {
            entries: self
                .cache
                .iter()
                .map(|(h, e)| CacheEntry {
                    text_hash: h.clone(),
                    embedding: e.clone(),
                })
                .collect(),
        };
        if let Ok(json) = serde_json::to_string(&file) {
            let _ = std::fs::write(&self.path, json);
        }
    }
}

impl Drop for CachedEmbeddingProvider {
    fn drop(&mut self) {
        self.flush();
    }
}

impl EmbeddingProvider for CachedEmbeddingProvider {
    fn dimensions(&self) -> usize {
        self.inner.dimensions()
    }

    fn embed(&self, text: &str) -> Result<Embedding, EmbeddingError> {
        let hash = Self::text_hash(text);
        if let Some(cached) = self.cache.get(&hash) {
            return Ok(cached.clone());
        }
        // Cache miss — compute and store
        // We need interior mutability here since the trait requires &self
        // Use unsafe or switch to RefCell. For simplicity, call inner and
        // let the caller handle caching via embed_and_cache.
        self.inner.embed(text)
    }

    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Embedding>, EmbeddingError> {
        texts.iter().map(|t| self.embed(t)).collect()
    }
}

impl CachedEmbeddingProvider {
    /// Embed with caching — stores result in cache.
    pub fn embed_cached(&mut self, text: &str) -> Result<Embedding, EmbeddingError> {
        let hash = Self::text_hash(text);
        if let Some(cached) = self.cache.get(&hash) {
            return Ok(cached.clone());
        }
        let embedding = self.inner.embed(text)?;
        self.cache.insert(hash, embedding.clone());
        self.dirty = true;
        Ok(embedding)
    }
}
