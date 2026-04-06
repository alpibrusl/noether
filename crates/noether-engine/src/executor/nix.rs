//! Nix-based executor for synthesized stages.
//!
//! Runs stage implementations as isolated subprocesses using `nix run nixpkgs#<runtime>`,
//! giving us hermetic, reproducible execution without requiring any ambient language runtime.
//!
//! ## Execution protocol
//!
//! - stdin  → JSON-encoded input value followed by a newline
//! - stdout → JSON-encoded output value followed by a newline
//! - stderr → error message (any content is treated as failure)
//! - exit 0 → success; exit non-zero → `ExecutionError::StageFailed`
//!
//! ## Generated wrapper (Python example)
//!
//! ```python
//! import sys, json as _json
//!
//! # ---- user code ----
//! def execute(input_value):
//!     ...
//! # -------------------
//!
//! if __name__ == '__main__':
//!     try:
//!         _output = execute(_json.loads(sys.stdin.read()))
//!         print(_json.dumps(_output))
//!     except Exception as e:
//!         print(str(e), file=sys.stderr)
//!         sys.exit(1)
//! ```

use super::{ExecutionError, StageExecutor};
use noether_core::stage::StageId;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Maps stage IDs to their implementation (source code + language tag).
#[derive(Clone)]
struct StageImpl {
    code: String,
    language: String,
}

/// Executor that runs synthesized stages through Nix-managed language runtimes.
///
/// When `nix` is available, each stage is executed inside a hermetically isolated
/// subprocess (e.g. `nix run nixpkgs#python3 -- stage.py`).  The Nix binary cache
/// ensures the runtime is downloaded once and then reused forever from the store.
pub struct NixExecutor {
    nix_bin: PathBuf,
    cache_dir: PathBuf,
    implementations: HashMap<String, StageImpl>,
}

impl NixExecutor {
    /// Probe the system for a usable `nix` binary.
    /// Returns the path if found, or `None` if Nix is not installed.
    pub fn find_nix() -> Option<PathBuf> {
        // Deterministic installer puts nix here:
        let determinate = PathBuf::from("/nix/var/nix/profiles/default/bin/nix");
        if determinate.exists() {
            return Some(determinate);
        }
        // Fallback: check PATH
        if let Ok(output) = Command::new("which").arg("nix").output() {
            let p = std::str::from_utf8(&output.stdout)
                .unwrap_or("")
                .trim()
                .to_string();
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
        None
    }

    /// Build an executor that can run synthesized stages found in `store`.
    ///
    /// Returns `None` when `nix` is not available — callers should fall back to
    /// `InlineExecutor` exclusively in that case.
    pub fn from_store(store: &dyn noether_store::StageStore) -> Option<Self> {
        let nix_bin = Self::find_nix()?;

        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let cache_dir = PathBuf::from(home).join(".noether").join("impl_cache");
        let _ = std::fs::create_dir_all(&cache_dir);

        let mut implementations = HashMap::new();
        for stage in store.list(None) {
            if let (Some(code), Some(lang)) =
                (&stage.implementation_code, &stage.implementation_language)
            {
                implementations.insert(
                    stage.id.0.clone(),
                    StageImpl {
                        code: code.clone(),
                        language: lang.clone(),
                    },
                );
            }
        }

        Some(Self {
            nix_bin,
            cache_dir,
            implementations,
        })
    }

    /// Register an in-memory synthesized stage without re-querying the store.
    pub fn register(&mut self, stage_id: &StageId, code: &str, language: &str) {
        self.implementations.insert(
            stage_id.0.clone(),
            StageImpl {
                code: code.into(),
                language: language.into(),
            },
        );
    }

    /// True when we have a real implementation for this stage.
    pub fn has_implementation(&self, stage_id: &StageId) -> bool {
        self.implementations.contains_key(&stage_id.0)
    }

    // ── Internal helpers ────────────────────────────────────────────────────

    /// Hash the code string to get a stable cache key.
    fn code_hash(code: &str) -> String {
        hex::encode(Sha256::digest(code.as_bytes()))
    }

    /// Ensure the wrapped script for `impl_hash` exists on disk.
    /// Returns the path to the file.
    fn ensure_script(
        &self,
        impl_hash: &str,
        code: &str,
        language: &str,
    ) -> Result<PathBuf, ExecutionError> {
        let ext = match language {
            "javascript" | "js" => "js",
            "bash" | "sh" => "sh",
            _ => "py",
        };

        let path = self.cache_dir.join(format!("{impl_hash}.{ext}"));
        if path.exists() {
            return Ok(path);
        }

        let wrapped = match language {
            "javascript" | "js" => Self::wrap_javascript(code),
            "bash" | "sh" => Self::wrap_bash(code),
            _ => Self::wrap_python(code),
        };

        std::fs::write(&path, &wrapped).map_err(|e| ExecutionError::StageFailed {
            stage_id: StageId(impl_hash.into()),
            message: format!("failed to write stage script: {e}"),
        })?;

        Ok(path)
    }

    /// Nix package name for the given language.
    fn nixpkgs_runtime(language: &str) -> &'static str {
        match language {
            "javascript" | "js" => "nodejs",
            "bash" | "sh" => "bash",
            _ => "python3",
        }
    }

    /// Run the stage script via `nix run nixpkgs#<runtime> -- <script>` with JSON on stdin.
    fn run_script(
        &self,
        stage_id: &StageId,
        script: &Path,
        language: &str,
        input: &Value,
    ) -> Result<Value, ExecutionError> {
        let runtime = Self::nixpkgs_runtime(language);
        let input_json = serde_json::to_string(input).unwrap_or_default();

        let mut child = Command::new(&self.nix_bin)
            .args([
                "run",
                "--no-write-lock-file",
                "--quiet",
                &format!("nixpkgs#{runtime}"),
                "--",
                script.to_str().unwrap_or("/dev/null"),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: format!("failed to spawn nix: {e}"),
            })?;

        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(input_json.as_bytes());
        }

        let out = child
            .wait_with_output()
            .map_err(|e| ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: format!("nix process error: {e}"),
            })?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: format!("stage process exited with error: {stderr}"),
            });
        }

        let stdout = String::from_utf8_lossy(&out.stdout);
        serde_json::from_str(stdout.trim()).map_err(|e| ExecutionError::StageFailed {
            stage_id: stage_id.clone(),
            message: format!("failed to parse stage output as JSON: {e} (got: {stdout:?})"),
        })
    }

    // ── Language wrappers ───────────────────────────────────────────────────

    fn wrap_python(user_code: &str) -> String {
        format!(
            r#"import sys, json as _json

# ---- user implementation ----
{user_code}
# ---- end implementation ----

if __name__ == '__main__':
    try:
        _input = _json.loads(sys.stdin.read())
        _output = execute(_input)
        print(_json.dumps(_output))
    except Exception as _e:
        print(str(_e), file=sys.stderr)
        sys.exit(1)
"#
        )
    }

    fn wrap_javascript(user_code: &str) -> String {
        format!(
            r#"const _readline = require('readline');
let _input = '';
process.stdin.on('data', d => _input += d);
process.stdin.on('end', () => {{
    try {{
        // ---- user implementation ----
        {user_code}
        // ---- end implementation ----
        const _result = execute(JSON.parse(_input));
        process.stdout.write(JSON.stringify(_result) + '\n');
    }} catch (e) {{
        process.stderr.write(String(e) + '\n');
        process.exit(1);
    }}
}});
"#
        )
    }

    fn wrap_bash(user_code: &str) -> String {
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
INPUT=$(cat)

# ---- user implementation ----
{user_code}
# ---- end implementation ----

execute "$INPUT"
"#
        )
    }
}

impl StageExecutor for NixExecutor {
    fn execute(&self, stage_id: &StageId, input: &Value) -> Result<Value, ExecutionError> {
        let impl_ = self
            .implementations
            .get(&stage_id.0)
            .ok_or_else(|| ExecutionError::StageNotFound(stage_id.clone()))?;

        let code_hash = Self::code_hash(&impl_.code);
        let script = self.ensure_script(&code_hash, &impl_.code, &impl_.language)?;
        self.run_script(stage_id, &script, &impl_.language, input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_wrapper_contains_boilerplate() {
        let wrapped = NixExecutor::wrap_python("def execute(x):\n    return x + 1");
        assert!(wrapped.contains("sys.stdin.read()"));
        assert!(wrapped.contains("_json.dumps(_output)"));
        assert!(wrapped.contains("def execute(x)"));
    }

    #[test]
    fn code_hash_is_stable() {
        let h1 = NixExecutor::code_hash("hello world");
        let h2 = NixExecutor::code_hash("hello world");
        let h3 = NixExecutor::code_hash("different");
        assert_eq!(h1, h2);
        assert_ne!(h1, h3);
    }

    /// Integration test — only runs when nix is available on the CI/dev machine.
    #[test]
    #[ignore = "requires nix"]
    fn nix_python_identity_stage() {
        let nix_bin = match NixExecutor::find_nix() {
            Some(p) => p,
            None => {
                eprintln!("nix not found, skipping");
                return;
            }
        };

        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let cache_dir = PathBuf::from(home).join(".noether").join("impl_cache");
        let _ = std::fs::create_dir_all(&cache_dir);

        let code = "def execute(x):\n    return x";
        let code_hash = NixExecutor::code_hash(code);

        let executor = NixExecutor {
            nix_bin,
            cache_dir,
            implementations: {
                let mut m = HashMap::new();
                let id = StageId("test_identity".into());
                m.insert(
                    id.0.clone(),
                    StageImpl {
                        code: code.into(),
                        language: "python".into(),
                    },
                );
                m
            },
        };

        let _ = code_hash;
        let id = StageId("test_identity".into());
        let result = executor.execute(&id, &serde_json::json!({"hello": "world"}));
        assert_eq!(result.unwrap(), serde_json::json!({"hello": "world"}));
    }
}
