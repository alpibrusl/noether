use super::{LlmConfig, LlmError, LlmProvider, Message, Role};
use crate::index::embedding::{Embedding, EmbeddingError, EmbeddingProvider};
use serde_json::json;
use std::sync::Mutex;
use std::time::{Duration, Instant};

// ── Token source ──────────────────────────────────────────────────────────────

/// Cached access token with expiry tracking.
struct CachedToken {
    access_token: String,
    /// Refresh this many seconds before actual expiry to avoid races.
    expires_at: Instant,
}

impl CachedToken {
    fn new(token: String, expires_in_secs: u64) -> Self {
        // Refresh 5 minutes early to avoid using a token that's about to expire.
        let margin = expires_in_secs.saturating_sub(300);
        Self {
            access_token: token,
            expires_at: Instant::now() + Duration::from_secs(margin),
        }
    }

    fn is_valid(&self) -> bool {
        Instant::now() < self.expires_at
    }
}

/// How the provider obtains a Bearer token.
///
/// Resolution order in `VertexAiConfig::from_env()`:
///   1. `VERTEX_AI_TOKEN` env var → `Static` (no refresh, works for 1-hour tokens)
///   2. `GOOGLE_APPLICATION_CREDENTIALS` file / `~/.config/gcloud/application_default_credentials.json`
///      with `type: "authorized_user"` → `RefreshToken` (auto-refreshes every ~55 min)
///   3. GCE/Cloud Run/GKE metadata server → `MetadataServer` (auto-refreshes, zero config)
///   4. `gcloud auth print-access-token` subprocess → `GcloudSubprocess` (local dev fallback)
enum TokenSource {
    /// Explicit static token — no auto-refresh. Fine for short-lived CLI invocations.
    Static(String),
    /// OAuth2 refresh token flow (ADC user credentials or `authorized_user` service files).
    RefreshToken {
        client_id: String,
        client_secret: String,
        refresh_token: String,
        cached: Mutex<Option<CachedToken>>,
    },
    /// GCE instance metadata server — zero-config inside Google Cloud.
    MetadataServer { cached: Mutex<Option<CachedToken>> },
    /// `gcloud auth print-access-token` subprocess — local dev fallback when no ADC file.
    GcloudSubprocess { cached: Mutex<Option<CachedToken>> },
}

impl TokenSource {
    /// Obtain a valid access token, refreshing if necessary.
    fn get_token(&self) -> Result<String, String> {
        match self {
            Self::Static(t) => Ok(t.clone()),

            Self::RefreshToken {
                client_id,
                client_secret,
                refresh_token,
                cached,
            } => {
                let mut guard = cached.lock().unwrap();
                if let Some(ref c) = *guard {
                    if c.is_valid() {
                        return Ok(c.access_token.clone());
                    }
                }
                let (token, expires_in) = oauth2_refresh(client_id, client_secret, refresh_token)?;
                *guard = Some(CachedToken::new(token.clone(), expires_in));
                Ok(token)
            }

            Self::MetadataServer { cached } => {
                let mut guard = cached.lock().unwrap();
                if let Some(ref c) = *guard {
                    if c.is_valid() {
                        return Ok(c.access_token.clone());
                    }
                }
                let (token, expires_in) = metadata_server_token()?;
                *guard = Some(CachedToken::new(token.clone(), expires_in));
                Ok(token)
            }

            Self::GcloudSubprocess { cached } => {
                let mut guard = cached.lock().unwrap();
                if let Some(ref c) = *guard {
                    if c.is_valid() {
                        return Ok(c.access_token.clone());
                    }
                }
                let token = gcloud_print_access_token()?;
                // gcloud tokens last ~1h; cache for 55 minutes.
                *guard = Some(CachedToken::new(token.clone(), 3300));
                Ok(token)
            }
        }
    }
}

// ── VertexAiConfig ────────────────────────────────────────────────────────────

/// Configuration for Vertex AI providers.
pub struct VertexAiConfig {
    pub project: String,
    pub location: String,
    token_source: TokenSource,
}

impl VertexAiConfig {
    /// Load from environment variables.
    ///
    /// Token resolution order:
    ///   1. `VERTEX_AI_TOKEN` — explicit static token
    ///   2. `GOOGLE_APPLICATION_CREDENTIALS` file (authorized_user or service account key)
    ///   3. `~/.config/gcloud/application_default_credentials.json` (ADC)
    ///   4. GCE/Cloud Run metadata server (http://metadata.google.internal/...)
    ///   5. `gcloud auth print-access-token` subprocess
    pub fn from_env() -> Result<Self, String> {
        let project = std::env::var("VERTEX_AI_PROJECT")
            .or_else(|_| std::env::var("GOOGLE_CLOUD_PROJECT"))
            .map_err(|_| {
                "Vertex AI project not configured. Set VERTEX_AI_PROJECT \
                 (or GOOGLE_CLOUD_PROJECT) to your GCP project ID."
                    .to_string()
            })?;
        let location = std::env::var("VERTEX_AI_LOCATION")
            .or_else(|_| std::env::var("GOOGLE_CLOUD_LOCATION"))
            .unwrap_or_else(|_| "europe-west1".into());

        let token_source = resolve_token_source()?;
        Ok(Self {
            project,
            location,
            token_source,
        })
    }

    /// Get a valid access token, auto-refreshing if the current one has expired.
    pub fn get_token(&self) -> Result<String, String> {
        self.token_source.get_token()
    }
}

// Manual Clone: we need to clone config for the providers, but Mutex isn't Clone.
// We just start with a fresh empty cache in the clone.
impl Clone for VertexAiConfig {
    fn clone(&self) -> Self {
        let token_source = match &self.token_source {
            TokenSource::Static(t) => TokenSource::Static(t.clone()),
            TokenSource::RefreshToken {
                client_id,
                client_secret,
                refresh_token,
                ..
            } => TokenSource::RefreshToken {
                client_id: client_id.clone(),
                client_secret: client_secret.clone(),
                refresh_token: refresh_token.clone(),
                cached: Mutex::new(None),
            },
            TokenSource::MetadataServer { .. } => TokenSource::MetadataServer {
                cached: Mutex::new(None),
            },
            TokenSource::GcloudSubprocess { .. } => TokenSource::GcloudSubprocess {
                cached: Mutex::new(None),
            },
        };
        Self {
            project: self.project.clone(),
            location: self.location.clone(),
            token_source,
        }
    }
}

impl std::fmt::Debug for VertexAiConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let source = match &self.token_source {
            TokenSource::Static(_) => "static",
            TokenSource::RefreshToken { .. } => "refresh_token",
            TokenSource::MetadataServer { .. } => "metadata_server",
            TokenSource::GcloudSubprocess { .. } => "gcloud_subprocess",
        };
        f.debug_struct("VertexAiConfig")
            .field("project", &self.project)
            .field("location", &self.location)
            .field("token_source", &source)
            .finish()
    }
}

// ── Token resolution ──────────────────────────────────────────────────────────

fn resolve_token_source() -> Result<TokenSource, String> {
    // 1. Explicit static token
    if let Ok(t) = std::env::var("VERTEX_AI_TOKEN") {
        return Ok(TokenSource::Static(t));
    }

    // 2. GOOGLE_APPLICATION_CREDENTIALS file
    if let Ok(path) = std::env::var("GOOGLE_APPLICATION_CREDENTIALS") {
        if let Ok(source) = load_credentials_file(&path) {
            return Ok(source);
        }
    }

    // 3. ADC file (~/.config/gcloud/application_default_credentials.json)
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let adc_path =
        std::path::PathBuf::from(&home).join(".config/gcloud/application_default_credentials.json");
    if adc_path.exists() {
        if let Ok(source) = load_credentials_file(adc_path.to_str().unwrap_or("")) {
            return Ok(source);
        }
    }

    // 4. GCE / Cloud Run / GKE metadata server
    if metadata_server_available() {
        return Ok(TokenSource::MetadataServer {
            cached: Mutex::new(None),
        });
    }

    // 5. gcloud subprocess (local dev fallback)
    if gcloud_available() {
        return Ok(TokenSource::GcloudSubprocess {
            cached: Mutex::new(None),
        });
    }

    Err("No Google credentials found. Options:\n\
         • Run `gcloud auth application-default login`\n\
         • Set VERTEX_AI_TOKEN to an access token\n\
         • Set GOOGLE_APPLICATION_CREDENTIALS to a service account key file\n\
         • Run on GCE/Cloud Run/GKE (metadata server)"
        .into())
}

fn load_credentials_file(path: &str) -> Result<TokenSource, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read credentials file {path}: {e}"))?;
    let creds: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("credentials JSON parse error: {e}"))?;

    match creds["type"].as_str() {
        Some("authorized_user") => Ok(TokenSource::RefreshToken {
            client_id: creds["client_id"]
                .as_str()
                .ok_or("missing client_id")?
                .into(),
            client_secret: creds["client_secret"]
                .as_str()
                .ok_or("missing client_secret")?
                .into(),
            refresh_token: creds["refresh_token"]
                .as_str()
                .ok_or("missing refresh_token")?
                .into(),
            cached: Mutex::new(None),
        }),
        Some("service_account") => {
            // Service accounts on non-GCE machines need JWT → token exchange.
            // We delegate to `gcloud auth print-access-token` which handles this
            // transparently when GOOGLE_APPLICATION_CREDENTIALS is set.
            Ok(TokenSource::GcloudSubprocess {
                cached: Mutex::new(None),
            })
        }
        other => Err(format!(
            "unsupported credentials type: {:?}",
            other.unwrap_or("missing")
        )),
    }
}

/// Exchange a refresh token for an access token via the Google OAuth2 endpoint.
/// Returns `(access_token, expires_in_seconds)`.
fn oauth2_refresh(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<(String, u64), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new());
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

    let token = body["access_token"]
        .as_str()
        .ok_or("token refresh response has no access_token")?
        .to_string();
    let expires_in = body["expires_in"].as_u64().unwrap_or(3600);
    Ok((token, expires_in))
}

/// Fetch a token from the GCE instance metadata server.
/// Returns `(access_token, expires_in_seconds)`.
fn metadata_server_token() -> Result<(String, u64), String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let resp = client
        .get("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token")
        .header("Metadata-Flavor", "Google")
        .send()
        .map_err(|e| format!("metadata server request failed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("metadata server returned HTTP {}", resp.status()));
    }

    let body: serde_json::Value = resp
        .json()
        .map_err(|e| format!("metadata server parse error: {e}"))?;
    let token = body["access_token"]
        .as_str()
        .ok_or("metadata server response has no access_token")?
        .to_string();
    let expires_in = body["expires_in"].as_u64().unwrap_or(3600);
    Ok((token, expires_in))
}

fn metadata_server_available() -> bool {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .unwrap_or_else(|_| reqwest::blocking::Client::new());
    client
        .get("http://metadata.google.internal/")
        .header("Metadata-Flavor", "Google")
        .send()
        .is_ok()
}

fn gcloud_available() -> bool {
    std::process::Command::new("gcloud")
        .arg("version")
        .output()
        .is_ok()
}

fn gcloud_print_access_token() -> Result<String, String> {
    let out = std::process::Command::new("gcloud")
        .args(["auth", "print-access-token"])
        .output()
        .map_err(|e| format!("gcloud subprocess failed: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "gcloud auth print-access-token failed: {stderr}. \
             Run `gcloud auth application-default login` to authenticate."
        ));
    }

    Ok(std::str::from_utf8(&out.stdout)
        .map_err(|e| format!("gcloud output encoding error: {e}"))?
        .trim()
        .to_string())
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

        let token = self
            .config
            .get_token()
            .map_err(|e| LlmError::Provider(format!("auth error: {e}")))?;

        let response = self
            .client
            .post(&url)
            .bearer_auth(&token)
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

        let token = self
            .config
            .get_token()
            .map_err(|e| EmbeddingError::Provider(format!("auth error: {e}")))?;

        let response = self
            .client
            .post(&url)
            .bearer_auth(&token)
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

        let token = self
            .config
            .get_token()
            .map_err(|e| LlmError::Provider(format!("auth error: {e}")))?;

        let response = self
            .client
            .post(&url)
            .bearer_auth(&token)
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
