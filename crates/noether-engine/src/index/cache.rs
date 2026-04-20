#![warn(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

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

    /// Embed many texts at once, calling `inner.embed_batch` on cache
    /// misses. Cache hits are served from memory. Misses are sent in chunks
    /// of `chunk_size` to keep individual requests under typical provider
    /// payload limits.
    ///
    /// Two robustness properties matter when a remote provider is rate-limited:
    ///
    /// - **Progressive caching.** Each successful batch is committed to the
    ///   in-memory cache *and* flushed to disk immediately. If the next
    ///   batch trips a 429, the function still returns Err — but the partial
    ///   work done so far is durable, so the next process restart picks up
    ///   exactly where the crash left off.
    /// - **Inter-batch pacing.** Between batch calls we sleep
    ///   `inter_batch_delay`. With Mistral's free-tier 1 req/s ceiling, a
    ///   ~1100 ms sleep keeps us comfortably under the limit; paid tiers
    ///   can pass `Duration::ZERO` to skip pacing.
    ///
    /// Order of results matches order of `texts`.
    pub fn embed_batch_cached_paced(
        &mut self,
        texts: &[&str],
        chunk_size: usize,
        inter_batch_delay: std::time::Duration,
    ) -> Result<Vec<Embedding>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let hashes: Vec<String> = texts.iter().map(|t| Self::text_hash(t)).collect();
        let mut miss_indices: Vec<usize> = Vec::new();
        let mut miss_texts: Vec<&str> = Vec::new();
        for (i, h) in hashes.iter().enumerate() {
            if !self.cache.contains_key(h) {
                miss_indices.push(i);
                miss_texts.push(texts[i]);
            }
        }

        if !miss_texts.is_empty() {
            let chunk = chunk_size.max(1);
            let mut consumed = 0usize;
            for (b, slice) in miss_texts.chunks(chunk).enumerate() {
                if b > 0 && !inter_batch_delay.is_zero() {
                    std::thread::sleep(inter_batch_delay);
                }
                let part = self.inner.embed_batch(slice)?;
                // A well-behaved provider returns exactly one embedding per
                // input text. A misbehaving one (truncated response, rate-
                // limit short-read, etc.) would desync `consumed` against
                // `miss_indices` and leave some cache slots unfilled — the
                // final `cache.get(h).expect(..)` used to panic here. Bail
                // as a typed provider error instead.
                if part.len() != slice.len() {
                    return Err(EmbeddingError::Provider(format!(
                        "embed_batch returned {} embeddings for {} inputs",
                        part.len(),
                        slice.len()
                    )));
                }
                for (j, emb) in part.into_iter().enumerate() {
                    let idx = miss_indices[consumed + j];
                    self.cache.insert(hashes[idx].clone(), emb);
                }
                consumed += slice.len();
                self.dirty = true;
                self.flush();
            }
        }

        let mut out = Vec::with_capacity(hashes.len());
        for h in &hashes {
            match self.cache.get(h).cloned() {
                Some(e) => out.push(e),
                None => {
                    return Err(EmbeddingError::Provider(
                        "embedding cache missing an entry after batch fill; provider or cache \
                         layer returned inconsistent results"
                            .to_string(),
                    ));
                }
            }
        }
        Ok(out)
    }

    /// Backward-compatible wrapper: no inter-batch sleep, single final flush.
    /// Prefer `embed_batch_cached_paced` for rate-limited providers.
    pub fn embed_batch_cached(
        &mut self,
        texts: &[&str],
        chunk_size: usize,
    ) -> Result<Vec<Embedding>, EmbeddingError> {
        self.embed_batch_cached_paced(texts, chunk_size, std::time::Duration::ZERO)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Provider that returns fewer embeddings than requested on the first
    /// batch — simulates a misbehaving remote that truncates responses.
    struct ShortBatchProvider;

    impl EmbeddingProvider for ShortBatchProvider {
        fn dimensions(&self) -> usize {
            4
        }
        fn embed(&self, _text: &str) -> Result<Embedding, EmbeddingError> {
            Ok(vec![0.0; 4])
        }
        fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Embedding>, EmbeddingError> {
            // Deliberately drop the last entry.
            Ok(texts
                .iter()
                .take(texts.len().saturating_sub(1))
                .map(|_| vec![0.0; 4])
                .collect())
        }
    }

    #[test]
    fn short_batch_becomes_provider_error_not_panic() {
        let tmp = std::env::temp_dir().join("noether-cache-short-batch-test.json");
        let _ = std::fs::remove_file(&tmp);
        let mut cp = CachedEmbeddingProvider::new(Box::new(ShortBatchProvider), tmp);
        let texts = ["a", "b", "c"];
        let r = cp.embed_batch_cached(&texts, 8);
        assert!(
            matches!(r, Err(EmbeddingError::Provider(ref m)) if m.contains("embed_batch returned")),
            "expected Provider error, got: {r:?}"
        );
    }
}
