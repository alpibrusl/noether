use super::{LlmConfig, LlmError, LlmProvider, Message, Role};
use crate::index::embedding::{Embedding, EmbeddingError, EmbeddingProvider};
use serde_json::json;

/// Configuration for Vertex AI.
#[derive(Debug, Clone)]
pub struct VertexAiConfig {
    pub project: String,
    pub location: String,
    pub token: String,
}

impl VertexAiConfig {
    /// Load from environment variables, with ADC fallback for the token.
    ///
    /// Token resolution order:
    ///   1. `VERTEX_AI_TOKEN` env var (explicit, for CI / service accounts)
    ///   2. Application Default Credentials (`~/.config/gcloud/application_default_credentials.json`)
    ///   3. Error — no token available
    pub fn from_env() -> Result<Self, String> {
        let project = std::env::var("VERTEX_AI_PROJECT").unwrap_or_else(|_| "a2p-common".into());
        // Default to europe-west4: works for both Gemini and Mistral, lower latency from EU.
        // Gemini also works on "global"; Mistral requires a regional endpoint.
        let location = std::env::var("VERTEX_AI_LOCATION").unwrap_or_else(|_| "europe-west4".into());

        let token = if let Ok(t) = std::env::var("VERTEX_AI_TOKEN") {
            t
        } else {
            refresh_adc_token().map_err(|e| {
                format!(
                    "no auth token: VERTEX_AI_TOKEN not set and ADC refresh failed ({e}). \
                     Run `gcloud auth application-default login` or set VERTEX_AI_TOKEN."
                )
            })?
        };

        Ok(Self {
            project,
            location,
            token,
        })
    }
}

/// Refresh an OAuth2 access token from Application Default Credentials.
/// Reads `~/.config/gcloud/application_default_credentials.json`.
fn refresh_adc_token() -> Result<String, String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let adc_path = std::path::PathBuf::from(home)
        .join(".config")
        .join("gcloud")
        .join("application_default_credentials.json");

    let content = std::fs::read_to_string(&adc_path)
        .map_err(|e| format!("cannot read {}: {e}", adc_path.display()))?;

    let creds: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("ADC JSON parse error: {e}"))?;

    let client_id = creds["client_id"]
        .as_str()
        .ok_or("missing client_id in ADC")?;
    let client_secret = creds["client_secret"]
        .as_str()
        .ok_or("missing client_secret in ADC")?;
    let refresh_token = creds["refresh_token"]
        .as_str()
        .ok_or("missing refresh_token in ADC")?;

    let client = reqwest::blocking::Client::new();
    let resp = client
        .post("https://oauth2.googleapis.com/token")
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token"),
        ])
        .send()
        .map_err(|e| format!("token refresh HTTP error: {e}"))?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .map_err(|e| format!("token refresh parse error: {e}"))?;

    if !status.is_success() {
        return Err(format!(
            "token refresh failed (HTTP {status}): {}",
            body.get("error_description")
                .or(body.get("error"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown error")
        ));
    }

    body["access_token"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "token refresh response has no access_token".into())
}

/// Vertex AI LLM provider for Gemini models.
/// Uses the global endpoint: https://aiplatform.googleapis.com/v1/...
pub struct VertexAiLlmProvider {
    config: VertexAiConfig,
    client: reqwest::blocking::Client,
}

impl VertexAiLlmProvider {
    pub fn new(config: VertexAiConfig) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("failed to build reqwest client");
        Self { config, client }
    }

    fn base_url(&self) -> String {
        if self.config.location == "global" {
            "https://aiplatform.googleapis.com/v1".into()
        } else {
            format!(
                "https://{}-aiplatform.googleapis.com/v1",
                self.config.location
            )
        }
    }
}

impl LlmProvider for VertexAiLlmProvider {
    fn complete(&self, messages: &[Message], config: &LlmConfig) -> Result<String, LlmError> {
        let url = format!(
            "{base}/projects/{project}/locations/{location}/publishers/google/models/{model}:generateContent",
            base = self.base_url(),
            project = self.config.project,
            location = self.config.location,
            model = config.model,
        );

        // Convert messages to Gemini format
        let system_instruction: Option<String> = messages
            .iter()
            .find(|m| matches!(m.role, Role::System))
            .map(|m| m.content.clone());

        let contents: Vec<serde_json::Value> = messages
            .iter()
            .filter(|m| !matches!(m.role, Role::System))
            .map(|m| {
                let role = match m.role {
                    Role::User => "user",
                    Role::Assistant => "model",
                    Role::System => unreachable!(),
                };
                json!({
                    "role": role,
                    "parts": [{"text": m.content}]
                })
            })
            .collect();

        let mut body = json!({
            "contents": contents,
            "generationConfig": {
                "maxOutputTokens": config.max_tokens,
                "temperature": config.temperature,
            }
        });

        if let Some(sys) = system_instruction {
            body["systemInstruction"] = json!({
                "parts": [{"text": sys}]
            });
        }

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.config.token)
            .json(&body)
            .send()
            .map_err(|e| LlmError::Http(e.to_string()))?;

        let status = response.status();
        let text = response.text().map_err(|e| LlmError::Http(e.to_string()))?;

        if !status.is_success() {
            return Err(LlmError::Provider(format!("HTTP {status}: {text}")));
        }

        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| LlmError::Parse(e.to_string()))?;

        // Extract text from Gemini response
        json["candidates"][0]["content"]["parts"][0]["text"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| LlmError::Parse(format!("unexpected response format: {json}")))
    }
}

/// Vertex AI embedding provider.
/// Uses the global endpoint by default.
pub struct VertexAiEmbeddingProvider {
    config: VertexAiConfig,
    model: String,
    dimensions: usize,
    client: reqwest::blocking::Client,
}

impl VertexAiEmbeddingProvider {
    pub fn new(config: VertexAiConfig, model: Option<String>, dimensions: Option<usize>) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("failed to build reqwest client");
        Self {
            config,
            model: model.unwrap_or_else(|| "text-embedding-005".into()),
            dimensions: dimensions.unwrap_or(256),
            client,
        }
    }

    fn base_url(&self) -> String {
        if self.config.location == "global" {
            "https://aiplatform.googleapis.com/v1".into()
        } else {
            format!(
                "https://{}-aiplatform.googleapis.com/v1",
                self.config.location
            )
        }
    }
}

impl EmbeddingProvider for VertexAiEmbeddingProvider {
    fn dimensions(&self) -> usize {
        self.dimensions
    }

    fn embed(&self, text: &str) -> Result<Embedding, EmbeddingError> {
        let url = format!(
            "{base}/projects/{project}/locations/{location}/publishers/google/models/{model}:predict",
            base = self.base_url(),
            project = self.config.project,
            location = self.config.location,
            model = self.model,
        );

        let body = json!({
            "instances": [{"content": text}],
            "parameters": {"outputDimensionality": self.dimensions}
        });

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.config.token)
            .json(&body)
            .send()
            .map_err(|e| EmbeddingError::Provider(e.to_string()))?;

        let status = response.status();
        let text = response
            .text()
            .map_err(|e| EmbeddingError::Provider(e.to_string()))?;

        if !status.is_success() {
            return Err(EmbeddingError::Provider(format!("HTTP {status}: {text}")));
        }

        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| EmbeddingError::Provider(e.to_string()))?;

        let values = json["predictions"][0]["embeddings"]["values"]
            .as_array()
            .ok_or_else(|| EmbeddingError::Provider("unexpected response format".into()))?;

        values
            .iter()
            .map(|v| {
                v.as_f64()
                    .map(|f| f as f32)
                    .ok_or_else(|| EmbeddingError::Provider("non-numeric embedding value".into()))
            })
            .collect()
    }
}

// ── Mistral on Vertex AI ────────────────────────────────────────────────────

/// Vertex AI LLM provider for Mistral models (mistral-small-2503, mistral-medium-3, codestral-2).
///
/// Mistral uses the OpenAI-compatible `rawPredict` endpoint and is only available in
/// `us-central1` and `europe-west4` (not `global`). Models must be enabled from the
/// Model Garden console before use.
///
/// Model name detection: model names containing "mistral" or "codestral" route here.
pub struct MistralLlmProvider {
    config: VertexAiConfig,
    /// Resolved region: defaults to us-central1 if config.location is "global".
    region: String,
    client: reqwest::blocking::Client,
}

impl MistralLlmProvider {
    pub fn new(config: VertexAiConfig) -> Self {
        // Mistral doesn't support "global" — fall back to europe-west4 (enabled by default).
        // us-central1 also works if explicitly set and the model is enabled there.
        let region = if config.location == "global" || config.location.is_empty() {
            "europe-west4".into()
        } else {
            config.location.clone()
        };
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .expect("failed to build reqwest client");
        Self {
            config,
            region,
            client,
        }
    }
}

impl LlmProvider for MistralLlmProvider {
    fn complete(&self, messages: &[Message], config: &LlmConfig) -> Result<String, LlmError> {
        let url = format!(
            "https://{region}-aiplatform.googleapis.com/v1/projects/{project}/locations/{region}/publishers/mistralai/models/{model}:rawPredict",
            region = self.region,
            project = self.config.project,
            model = config.model,
        );

        // OpenAI-compatible message format
        let msgs: Vec<serde_json::Value> = messages
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
            "model": config.model,
            "messages": msgs,
            "max_tokens": config.max_tokens,
            "temperature": config.temperature,
            "stream": false,
        });

        let response = self
            .client
            .post(&url)
            .bearer_auth(&self.config.token)
            .json(&body)
            .send()
            .map_err(|e| LlmError::Http(e.to_string()))?;

        let status = response.status();
        let text = response.text().map_err(|e| LlmError::Http(e.to_string()))?;

        if !status.is_success() {
            return Err(LlmError::Provider(format!("HTTP {status}: {text}")));
        }

        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| LlmError::Parse(e.to_string()))?;

        // OpenAI-compatible response: choices[0].message.content
        json["choices"][0]["message"]["content"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| LlmError::Parse(format!("unexpected Mistral response: {json}")))
    }
}
