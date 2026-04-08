use noether_core::stage::StageId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompositionTrace {
    pub composition_id: String,
    pub started_at: String,
    pub duration_ms: u64,
    pub status: TraceStatus,
    pub stages: Vec<StageTrace>,
    /// Capability violations detected during pre-flight (informational; executions are blocked before this).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security_events: Vec<SecurityEvent>,
    /// Effect warnings produced by the type checker.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// A security event recorded when a capability policy blocks a stage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecurityEvent {
    pub stage_id: StageId,
    pub capability: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum TraceStatus {
    Ok,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageTrace {
    pub stage_id: StageId,
    pub step_index: usize,
    pub status: StageStatus,
    pub duration_ms: u64,
    pub input_hash: Option<String>,
    pub output_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StageStatus {
    Ok,
    Failed { code: String, message: String },
    Skipped { reason: String },
}

/// In-memory trace store.
#[derive(Debug, Default)]
pub struct MemoryTraceStore {
    traces: HashMap<String, CompositionTrace>,
}

impl MemoryTraceStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&mut self, trace: CompositionTrace) -> String {
        let id = trace.composition_id.clone();
        self.traces.insert(id.clone(), trace);
        id
    }

    pub fn get(&self, composition_id: &str) -> Option<&CompositionTrace> {
        self.traces.get(composition_id)
    }

    pub fn list(&self) -> Vec<&CompositionTrace> {
        self.traces.values().collect()
    }
}

/// File-backed trace store. Persists to JSON on every put.
#[cfg(feature = "native")]
pub struct JsonFileTraceStore {
    path: std::path::PathBuf,
    traces: HashMap<String, CompositionTrace>,
}

#[cfg(feature = "native")]
#[derive(Serialize, Deserialize)]
struct TraceFile {
    traces: Vec<CompositionTrace>,
}

#[cfg(feature = "native")]
impl JsonFileTraceStore {
    pub fn open(path: impl Into<std::path::PathBuf>) -> Result<Self, String> {
        let path = path.into();
        let traces = if path.exists() {
            let content = std::fs::read_to_string(&path).map_err(|e| format!("read error: {e}"))?;
            let file: TraceFile =
                serde_json::from_str(&content).map_err(|e| format!("parse error: {e}"))?;
            file.traces
                .into_iter()
                .map(|t| (t.composition_id.clone(), t))
                .collect()
        } else {
            HashMap::new()
        };
        Ok(Self { path, traces })
    }

    pub fn put(&mut self, trace: CompositionTrace) -> String {
        let id = trace.composition_id.clone();
        self.traces.insert(id.clone(), trace);
        let _ = self.save();
        id
    }

    pub fn get(&self, composition_id: &str) -> Option<&CompositionTrace> {
        self.traces.get(composition_id)
    }

    pub fn list(&self) -> Vec<&CompositionTrace> {
        self.traces.values().collect()
    }

    fn save(&self) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("mkdir error: {e}"))?;
        }
        let file = TraceFile {
            traces: self.traces.values().cloned().collect(),
        };
        let json = serde_json::to_string_pretty(&file).map_err(|e| format!("json error: {e}"))?;
        std::fs::write(&self.path, json).map_err(|e| format!("write error: {e}"))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_trace() -> CompositionTrace {
        CompositionTrace {
            composition_id: "abc123".into(),
            started_at: "2026-04-05T10:00:00Z".into(),
            duration_ms: 100,
            status: TraceStatus::Ok,
            stages: vec![StageTrace {
                stage_id: StageId("stage1".into()),
                step_index: 0,
                status: StageStatus::Ok,
                duration_ms: 50,
                input_hash: Some("inhash".into()),
                output_hash: Some("outhash".into()),
            }],
            security_events: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn trace_store_put_get() {
        let mut store = MemoryTraceStore::new();
        let trace = sample_trace();
        store.put(trace.clone());
        let retrieved = store.get("abc123").unwrap();
        assert_eq!(retrieved, &trace);
    }

    #[test]
    fn trace_serde_round_trip() {
        let trace = sample_trace();
        let json = serde_json::to_string(&trace).unwrap();
        let parsed: CompositionTrace = serde_json::from_str(&json).unwrap();
        assert_eq!(trace, parsed);
    }
}
