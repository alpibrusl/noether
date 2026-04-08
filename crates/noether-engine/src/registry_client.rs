//! HTTP client for a hosted `noether-registry` (noether-cloud).
//!
//! `RemoteStageStore` implements the sync [`StageStore`] trait by calling the
//! registry REST API with `reqwest::blocking`.  Because `StageStore::get` and
//! `list` must return borrowed references, all active stages are prefetched
//! into a local [`MemoryStore`] at construction time.  Writes go to both the
//! remote registry AND the local cache so the borrows remain valid.
//!
//! # Usage
//!
//! ```no_run
//! use noether_engine::registry_client::RemoteStageStore;
//!
//! // Explicit URL:
//! let store = RemoteStageStore::connect("http://localhost:3000", None).unwrap();
//!
//! // From environment (NOETHER_REGISTRY + optional NOETHER_API_KEY):
//! if let Some(Ok(store)) = RemoteStageStore::from_env() {
//!     println!("Connected to remote registry");
//! }
//! ```

use noether_core::stage::{Stage, StageId, StageLifecycle};
use noether_store::{MemoryStore, StageStore, StoreError, StoreStats};
use serde_json::Value;
use std::collections::BTreeMap;

// ── ACLI response parsing ───────────────────────────────────────────────────

fn extract_result(resp: reqwest::blocking::Response) -> Result<Value, StoreError> {
    let status = resp.status();
    let body: Value = resp.json().map_err(|e| StoreError::IoError {
        message: format!("failed to parse registry response: {e}"),
    })?;

    if body["ok"].as_bool().unwrap_or(false) {
        return Ok(body["result"].clone());
    }

    let code = body["error"]["code"].as_str().unwrap_or("UNKNOWN");
    let msg = body["error"]["message"].as_str().unwrap_or("unknown error");

    if status == 404 || code == "NOT_FOUND" {
        Err(StoreError::IoError {
            message: format!("NOT_FOUND: {msg}"),
        })
    } else {
        Err(StoreError::IoError {
            message: format!("{code}: {msg}"),
        })
    }
}

// ── RemoteStageStore ────────────────────────────────────────────────────────

/// A `StageStore` backed by a remote `noether-registry` over HTTP.
///
/// All stages are prefetched into a local `MemoryStore` on construction,
/// making subsequent reads (get, list) instant and allocation-free.
pub struct RemoteStageStore {
    client: reqwest::blocking::Client,
    base_url: String,
    api_key: Option<String>,
    cache: MemoryStore,
}

impl RemoteStageStore {
    /// Connect to a registry at `base_url` and prefetch all active stages.
    ///
    /// `api_key` is sent as `X-API-Key` header; pass `None` if the registry
    /// runs without auth (local dev with `NOETHER_API_KEY=""`).
    pub fn connect(base_url: &str, api_key: Option<&str>) -> Result<Self, StoreError> {
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .user_agent(concat!("noether-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| StoreError::IoError {
                message: e.to_string(),
            })?;

        let mut store = Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.map(String::from),
            cache: MemoryStore::new(),
        };
        store.refresh()?;
        Ok(store)
    }

    /// Build a store from the `NOETHER_REGISTRY` environment variable.
    /// Also reads `NOETHER_API_KEY` if set.
    /// Returns `None` if `NOETHER_REGISTRY` is not set.
    pub fn from_env() -> Option<Result<Self, StoreError>> {
        let url = std::env::var("NOETHER_REGISTRY").ok()?;
        let key = std::env::var("NOETHER_API_KEY").ok();
        Some(Self::connect(&url, key.as_deref()))
    }

    /// Re-fetch all stages from the registry using offset pagination and
    /// rebuild the local cache. Each page is 200 stages; iteration stops when
    /// the server returns an empty page or `offset >= total`.
    ///
    /// Call this if you know the registry was mutated externally.
    pub fn refresh(&mut self) -> Result<usize, StoreError> {
        const PAGE: usize = 200;
        let mut offset = 0usize;
        let mut new_cache = MemoryStore::new();

        loop {
            let path = format!("/stages?lifecycle=active&limit={PAGE}&offset={offset}");
            let resp = self
                .get_req(&path)
                .send()
                .map_err(|e| StoreError::IoError {
                    message: format!("registry unreachable at {}: {e}", self.base_url),
                })?;

            let result = extract_result(resp)?;
            let page: Vec<Stage> =
                serde_json::from_value(result["stages"].clone()).map_err(|e| {
                    StoreError::IoError {
                        message: format!("malformed /stages response: {e}"),
                    }
                })?;

            let total = result["total"].as_u64().unwrap_or(0) as usize;
            let fetched = page.len();
            for stage in page {
                new_cache.upsert(stage).ok();
            }

            offset += fetched;
            if fetched == 0 || offset >= total {
                break;
            }
        }

        let count = new_cache.list(None).len();
        self.cache = new_cache;
        Ok(count)
    }

    /// The base URL this store is connected to.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Fetch a single stage directly from the registry, bypassing the cache.
    /// On success the stage is inserted into the local cache so subsequent
    /// `get()` calls will find it without another HTTP round-trip.
    ///
    /// Returns `Ok(None)` when the server returns 404.
    pub fn get_live(&mut self, id: &StageId) -> Result<Option<&Stage>, StoreError> {
        let resp = self
            .get_req(&format!("/stages/{}", id.0))
            .send()
            .map_err(|e| StoreError::IoError {
                message: e.to_string(),
            })?;

        match extract_result(resp) {
            Ok(body) => {
                let stage: Stage =
                    serde_json::from_value(body).map_err(|e| StoreError::IoError {
                        message: format!("malformed /stages/:id response: {e}"),
                    })?;
                self.cache.upsert(stage).ok();
                self.cache.get(id)
            }
            Err(StoreError::IoError { message }) if message.contains("NOT_FOUND") => Ok(None),
            Err(e) => Err(e),
        }
    }

    // ── internal request helpers ────────────────────────────────────────────

    fn get_req(&self, path: &str) -> reqwest::blocking::RequestBuilder {
        self.with_auth(self.client.get(format!("{}{path}", self.base_url)))
    }

    fn post_req(&self, path: &str) -> reqwest::blocking::RequestBuilder {
        self.with_auth(self.client.post(format!("{}{path}", self.base_url)))
    }

    fn patch_req(&self, path: &str) -> reqwest::blocking::RequestBuilder {
        self.with_auth(self.client.patch(format!("{}{path}", self.base_url)))
    }

    fn delete_req(&self, path: &str) -> reqwest::blocking::RequestBuilder {
        self.with_auth(self.client.delete(format!("{}{path}", self.base_url)))
    }

    fn with_auth(&self, b: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
        match &self.api_key {
            Some(k) => b.header("X-API-Key", k),
            None => b,
        }
    }
}

// ── StageStore impl ─────────────────────────────────────────────────────────

impl StageStore for RemoteStageStore {
    fn put(&mut self, stage: Stage) -> Result<StageId, StoreError> {
        let resp =
            self.post_req("/stages")
                .json(&stage)
                .send()
                .map_err(|e| StoreError::IoError {
                    message: e.to_string(),
                })?;

        let result = match extract_result(resp) {
            Ok(r) => r,
            Err(e) => {
                // Registry returns VALIDATION_FAILED if it detects AlreadyExists
                if e.to_string().contains("ALREADY_EXISTS") || e.to_string().contains("already") {
                    self.cache.upsert(stage.clone()).ok();
                    return Err(StoreError::AlreadyExists(stage.id));
                }
                return Err(e);
            }
        };

        let id = StageId(result["id"].as_str().unwrap_or(&stage.id.0).to_string());
        let is_new = result["is_new"].as_bool().unwrap_or(true);

        // Always cache, even if not new.
        self.cache.upsert(stage).ok();

        if !is_new {
            return Err(StoreError::AlreadyExists(id));
        }
        Ok(id)
    }

    fn upsert(&mut self, stage: Stage) -> Result<StageId, StoreError> {
        let id = stage.id.clone();
        match self.put(stage.clone()) {
            Err(StoreError::AlreadyExists(_)) => {
                self.cache.upsert(stage).ok();
                Ok(id)
            }
            other => other,
        }
    }

    /// Removes the stage from the remote registry (DELETE /stages/:id) and
    /// then from the local cache.
    fn remove(&mut self, id: &StageId) -> Result<(), StoreError> {
        let resp = self
            .delete_req(&format!("/stages/{}", id.0))
            .send()
            .map_err(|e| StoreError::IoError {
                message: e.to_string(),
            })?;

        let status_str = extract_result(resp)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        if !status_str.is_empty() && !status_str.contains("NOT_FOUND") {
            return Err(StoreError::IoError {
                message: status_str,
            });
        }
        // Best-effort cache eviction (stage may already be absent from cache).
        let _ = self.cache.remove(id);
        Ok(())
    }

    /// Returns the stage from the local cache.
    ///
    /// For a guaranteed-fresh read use [`RemoteStageStore::get_live`] (which
    /// takes `&mut self` so it can update the cache).
    fn get(&self, id: &StageId) -> Result<Option<&Stage>, StoreError> {
        self.cache.get(id)
    }

    fn contains(&self, id: &StageId) -> bool {
        self.cache.contains(id)
    }

    fn list(&self, lifecycle: Option<&StageLifecycle>) -> Vec<&Stage> {
        self.cache.list(lifecycle)
    }

    fn update_lifecycle(
        &mut self,
        id: &StageId,
        lifecycle: StageLifecycle,
    ) -> Result<(), StoreError> {
        let (lc_str, successor_id) = match &lifecycle {
            StageLifecycle::Draft => ("draft", None),
            StageLifecycle::Active => ("active", None),
            StageLifecycle::Deprecated { successor_id } => {
                ("deprecated", Some(successor_id.0.clone()))
            }
            StageLifecycle::Tombstone => ("tombstone", None),
        };

        let mut body = serde_json::json!({ "lifecycle": lc_str });
        if let Some(succ) = successor_id {
            body["successor_id"] = Value::String(succ);
        }

        let resp = self
            .patch_req(&format!("/stages/{}/lifecycle", id.0))
            .json(&body)
            .send()
            .map_err(|e| StoreError::IoError {
                message: e.to_string(),
            })?;

        extract_result(resp)?;

        // Mirror in local cache so subsequent list()/get() reflect the change.
        self.cache.update_lifecycle(id, lifecycle)
    }

    fn stats(&self) -> StoreStats {
        // Fetch live stats from /health; fall back to cache if unreachable.
        if let Ok(resp) = self.get_req("/health").send() {
            if let Ok(result) = extract_result(resp) {
                if let Some(store_json) = result.get("store") {
                    let total = store_json["total_stages"].as_u64().unwrap_or(0) as usize;
                    let by_lifecycle: BTreeMap<String, usize> = store_json["by_lifecycle"]
                        .as_object()
                        .map(|m| {
                            m.iter()
                                .filter_map(|(k, v)| Some((k.clone(), v.as_u64()? as usize)))
                                .collect()
                        })
                        .unwrap_or_default();
                    let by_effect: BTreeMap<String, usize> = store_json["by_effect"]
                        .as_object()
                        .map(|m| {
                            m.iter()
                                .filter_map(|(k, v)| Some((k.clone(), v.as_u64()? as usize)))
                                .collect()
                        })
                        .unwrap_or_default();
                    return StoreStats {
                        total,
                        by_lifecycle,
                        by_effect,
                    };
                }
            }
        }
        self.cache.stats()
    }
}
