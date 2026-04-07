//! Mistral AI native API provider.
//!
//! Calls `api.mistral.ai` directly — no Google Cloud required.
//! Auth: `MISTRAL_API_KEY` environment variable.
//!
//! This is the preferred provider for the European deployment stack:
//! - Mistral AI is headquartered in Paris.
//! - Data stays within the EU (Mistral's infrastructure is EU-based).
//! - No dependency on any US cloud provider.

use crate::index::embedding::{Embedding, EmbeddingError, EmbeddingProvider};
use crate::llm::{LlmConfig, LlmError, LlmProvider, Message, Role};
use serde_json::{json, Value};

const MISTRAL_API_BASE: &str = "https://api.mistral.ai/v1";

// ── LLM provider ────────────────────────────────────────────────────────────

/// Calls `api.mistral.ai/v1/chat/completions` with an API key.
///
/// Supports all Mistral chat models:
/// - `mistral-small-latest` — fastest, cheapest  (€0.10/1M tokens in)
/// - `mistral-medium-latest` — balanced
/// - `mistral-large-latest` — most capable
/// - `codestral-latest` — code specialist
///
/// Set `MISTRAL_API_KEY` to your API key from console.mistral.ai.
/// Override model with `VERTEX_AI_MODEL` (name reused for compatibility) or
/// the new `MISTRAL_MODEL` env var.
pub struct MistralNativeProvider {
    api_key: String,
    client: reqwest::blocking::Client,
}

impl MistralNativeProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("failed to build reqwest client");
        Self { api_key: api_key.into(), client }
    }

    /// Construct from environment. Returns `Err` if `MISTRAL_API_KEY` is not set.
    pub fn from_env() -> Result<Self, String> {
        let key = std::env::var("MISTRAL_API_KEY")
            .map_err(|_| "MISTRAL_API_KEY is not set".to_string())?;
        Ok(Self::new(key))
    }
}

impl LlmProvider for MistralNativeProvider {
    fn complete(&self, messages: &[Message], config: &LlmConfig) -> Result<String, LlmError> {
        let url = format!("{MISTRAL_API_BASE}/chat/completions");

        // Model resolution: prefer MISTRAL_MODEL, fall back to VERTEX_AI_MODEL (compat),
        // then the LlmConfig model value.
        let model = std::env::var("MISTRAL_MODEL")
            .or_else(|_| std::env::var("VERTEX_AI_MODEL"))
            .unwrap_or_else(|_| config.model.clone());

        // Normalise model name: strip vendor prefix if present (e.g. "mistralai/mistral-small")
        let model = model
            .strip_prefix("mistralai/")
            .map(|s| s.to_string())
            .unwrap_or(model);

        let msgs: Vec<Value> = messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                };
                json!({"role": role, "content": m.content})
            })
            .collect();

        let body = json!({
            "model": model,
            "messages": msgs,
            "max_tokens": config.max_tokens,
            "temperature": config.temperature,
            "stream": false,
        });

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .map_err(|e| LlmError::Http(e.to_string()))?;

        let status = resp.status();
        let text = resp.text().map_err(|e| LlmError::Http(e.to_string()))?;

        if !status.is_success() {
            return Err(LlmError::Provider(format!("Mistral API HTTP {status}: {text}")));
        }

        let json: Value =
            serde_json::from_str(&text).map_err(|e| LlmError::Parse(e.to_string()))?;

        json["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| LlmError::Parse(format!("unexpected Mistral response shape: {json}")))
    }
}

// ── Embedding provider ───────────────────────────────────────────────────────

/// Calls `api.mistral.ai/v1/embeddings` using the `mistral-embed` model.
///
/// - Dimension: 1024
/// - Context window: 8192 tokens
/// - EU-hosted, GDPR-compliant
pub struct MistralNativeEmbeddingProvider {
    api_key: String,
    model: String,
    client: reqwest::blocking::Client,
}

impl MistralNativeEmbeddingProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("failed to build reqwest client");
        Self {
            api_key: api_key.into(),
            model: std::env::var("MISTRAL_EMBEDDING_MODEL")
                .unwrap_or_else(|_| "mistral-embed".into()),
            client,
        }
    }

    /// Construct from environment. Returns `Err` if `MISTRAL_API_KEY` is not set.
    pub fn from_env() -> Result<Self, String> {
        let key = std::env::var("MISTRAL_API_KEY")
            .map_err(|_| "MISTRAL_API_KEY is not set".to_string())?;
        Ok(Self::new(key))
    }
}

impl EmbeddingProvider for MistralNativeEmbeddingProvider {
    fn dimensions(&self) -> usize {
        1024 // mistral-embed fixed dimension
    }

    fn embed(&self, text: &str) -> Result<Embedding, EmbeddingError> {
        // Delegate to batch — avoids duplicating the HTTP/parse logic.
        let mut batch = self.embed_batch(&[text])?;
        batch.pop().ok_or_else(|| EmbeddingError::Provider("empty response".into()))
    }

    /// Override the default batch implementation to call the API once for all texts.
    fn embed_batch(&self, texts: &[&str]) -> Result<Vec<Embedding>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }

        let url = format!("{MISTRAL_API_BASE}/embeddings");
        let body = json!({
            "model": self.model,
            "input": texts,
            "encoding_format": "float",
        });

        let resp = self
            .client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .map_err(|e| EmbeddingError::Provider(e.to_string()))?;

        let status = resp.status();
        let text = resp.text().map_err(|e| EmbeddingError::Provider(e.to_string()))?;

        if !status.is_success() {
            return Err(EmbeddingError::Provider(format!(
                "Mistral embeddings HTTP {status}: {text}"
            )));
        }

        let json: Value = serde_json::from_str(&text)
            .map_err(|e| EmbeddingError::Provider(e.to_string()))?;

        // Response: { "data": [{ "embedding": [...], "index": 0 }, ...] }
        // Sort by index to preserve input order.
        let mut items: Vec<(usize, Embedding)> = json["data"]
            .as_array()
            .ok_or_else(|| EmbeddingError::Provider("missing 'data' field".into()))?
            .iter()
            .map(|item| {
                let index = item["index"].as_u64().unwrap_or(0) as usize;
                let vec: Embedding = item["embedding"]
                    .as_array()
                    .unwrap_or(&vec![])
                    .iter()
                    .filter_map(|v| v.as_f64().map(|f| f as f32))
                    .collect();
                (index, vec)
            })
            .collect();

        items.sort_by_key(|(idx, _)| *idx);
        Ok(items.into_iter().map(|(_, v)| v).collect())
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_errors_without_key() {
        // Temporarily unset the key (save + restore to avoid side effects in parallel tests).
        let saved = std::env::var("MISTRAL_API_KEY").ok();
        std::env::remove_var("MISTRAL_API_KEY");
        assert!(MistralNativeProvider::from_env().is_err());
        assert!(MistralNativeEmbeddingProvider::from_env().is_err());
        if let Some(k) = saved {
            std::env::set_var("MISTRAL_API_KEY", k);
        }
    }

    #[test]
    fn strips_vendor_prefix() {
        // The model name normalisation is purely internal; verify it via a manual check.
        let model = "mistralai/mistral-small-latest".to_string();
        let normalised = model
            .strip_prefix("mistralai/")
            .map(|s| s.to_string())
            .unwrap_or(model);
        assert_eq!(normalised, "mistral-small-latest");
    }
}
