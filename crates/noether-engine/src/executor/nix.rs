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
//! ## Timeout
//!
//! Every execution is bounded by [`NixConfig::timeout_secs`] (default 30 s).
//! When the child process exceeds the limit it is sent SIGKILL and the call
//! returns [`ExecutionError::TimedOut`].
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
use std::sync::mpsc;
use std::time::Duration;

// ── Configuration ────────────────────────────────────────────────────────────

/// Tunable knobs for the [`NixExecutor`].
#[derive(Debug, Clone)]
pub struct NixConfig {
    /// Wall-clock timeout for a single stage execution in seconds.
    /// The child process is killed with SIGKILL when exceeded.
    /// Default: 30 s.
    pub timeout_secs: u64,
    /// Maximum number of bytes read from a stage's stdout before truncation.
    /// Prevents runaway allocations from stages that produce huge outputs.
    /// Default: 10 MiB.
    pub max_output_bytes: usize,
    /// Maximum number of bytes captured from stderr (for error messages).
    /// Default: 64 KiB.
    pub max_stderr_bytes: usize,
}

impl Default for NixConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 30,
            max_output_bytes: 10 * 1024 * 1024,
            max_stderr_bytes: 64 * 1024,
        }
    }
}

// ── Internal stage storage ───────────────────────────────────────────────────

/// Maps stage IDs to their implementation (source code + language tag).
#[derive(Clone)]
struct StageImpl {
    code: String,
    language: String,
}

// ── NixExecutor ──────────────────────────────────────────────────────────────

/// Executor that runs synthesized stages through Nix-managed language runtimes.
///
/// When `nix` is available, each stage is executed inside a hermetically isolated
/// subprocess (e.g. `nix run nixpkgs#python3 -- stage.py`).  The Nix binary cache
/// ensures the runtime is downloaded once and then reused forever from the store.
///
/// ## Resource limits
///
/// - **Timeout**: configured via [`NixConfig::timeout_secs`] (default 30 s).
///   The child is sent SIGKILL when the limit is exceeded.
/// - **Output cap**: configured via [`NixConfig::max_output_bytes`] (default 10 MiB).
pub struct NixExecutor {
    nix_bin: PathBuf,
    cache_dir: PathBuf,
    config: NixConfig,
    implementations: HashMap<String, StageImpl>,
}

impl NixExecutor {
    /// Probe the system for a usable `nix` binary.
    /// Returns the path if found, or `None` if Nix is not installed.
    pub fn find_nix() -> Option<PathBuf> {
        // Determinate Systems installer puts nix here:
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
        Self::from_store_with_config(store, NixConfig::default())
    }

    /// Like [`from_store`] but with a custom [`NixConfig`].
    pub fn from_store_with_config(
        store: &dyn noether_store::StageStore,
        config: NixConfig,
    ) -> Option<Self> {
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
            config,
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

    /// Pre-fetch the Python 3 runtime into the Nix store in a background thread.
    ///
    /// The first time any Python stage runs, Nix may take several seconds to
    /// download and verify the runtime closure.  Calling `warmup()` at startup
    /// overlaps that latency with application boot time.
    ///
    /// The returned `JoinHandle` can be ignored — any error is logged to stderr
    /// but does not affect correctness; the runtime will still be fetched on first
    /// actual use.
    pub fn warmup(&self) -> std::thread::JoinHandle<()> {
        let nix_bin = self.nix_bin.clone();
        std::thread::spawn(move || {
            // `nix build` with `--dry-run` is enough to populate the binary cache
            // without running any user code.
            let status = Command::new(&nix_bin)
                .args([
                    "build",
                    "--no-link",
                    "--quiet",
                    "--no-write-lock-file",
                    "nixpkgs#python3",
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            match status {
                Ok(s) if s.success() => {
                    eprintln!("[noether] nix warmup: python3 runtime cached");
                }
                Ok(s) => {
                    eprintln!("[noether] nix warmup: exited with {s} (non-fatal)");
                }
                Err(e) => {
                    eprintln!("[noether] nix warmup: failed to spawn ({e}) (non-fatal)");
                }
            }
        })
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

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

    /// Run the stage script via Nix with JSON on stdin, enforcing timeout and
    /// output-size limits.
    fn run_script(
        &self,
        stage_id: &StageId,
        script: &Path,
        language: &str,
        input: &Value,
    ) -> Result<Value, ExecutionError> {
        let input_json = serde_json::to_string(input).unwrap_or_default();

        let code = self
            .implementations
            .get(&stage_id.0)
            .map(|i| i.code.as_str())
            .unwrap_or("");

        let (nix_subcommand, args) = self.build_nix_command(language, script, code);

        // __direct__ means run the binary directly (venv Python), not via nix
        let mut child = if nix_subcommand == "__direct__" {
            Command::new(&args[0])
                .args(&args[1..])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        } else {
            Command::new(&self.nix_bin)
                .arg(&nix_subcommand)
                .args(["--no-write-lock-file", "--quiet"])
                .args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        }
        .map_err(|e| ExecutionError::StageFailed {
            stage_id: stage_id.clone(),
            message: format!("failed to spawn process: {e}"),
        })?;

        // Write stdin in a background thread so we don't deadlock when the
        // child's stdin pipe fills before we start reading stdout.
        if let Some(mut stdin) = child.stdin.take() {
            let bytes = input_json.into_bytes();
            std::thread::spawn(move || {
                let _ = stdin.write_all(&bytes);
            });
        }

        // Collect output with a wall-clock timeout.
        let pid = child.id();
        let timeout = Duration::from_secs(self.config.timeout_secs);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(child.wait_with_output());
        });

        let out = match rx.recv_timeout(timeout) {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                return Err(ExecutionError::StageFailed {
                    stage_id: stage_id.clone(),
                    message: format!("nix process error: {e}"),
                });
            }
            Err(_elapsed) => {
                // Best-effort kill — process may already have exited.
                let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
                return Err(ExecutionError::TimedOut {
                    stage_id: stage_id.clone(),
                    timeout_secs: self.config.timeout_secs,
                });
            }
        };

        // Truncate stderr to avoid huge allocations from noisy runtimes.
        let stderr_raw = &out.stderr[..out.stderr.len().min(self.config.max_stderr_bytes)];
        let stderr = String::from_utf8_lossy(stderr_raw);

        if !out.status.success() {
            return Err(ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: Self::classify_error(&stderr, out.status.code()),
            });
        }

        // Truncate stdout to the configured limit.
        let stdout_raw = &out.stdout[..out.stdout.len().min(self.config.max_output_bytes)];
        let stdout = String::from_utf8_lossy(stdout_raw);

        if stdout_raw.len() == self.config.max_output_bytes && !out.stdout.is_empty() {
            return Err(ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: format!(
                    "stage output exceeded {} bytes limit",
                    self.config.max_output_bytes
                ),
            });
        }

        serde_json::from_str(stdout.trim()).map_err(|e| ExecutionError::StageFailed {
            stage_id: stage_id.clone(),
            message: format!("failed to parse stage output as JSON: {e} (got: {stdout:?})"),
        })
    }

    /// Classify a non-zero exit into a human-readable message, distinguishing
    /// Nix infrastructure errors from user code errors.
    fn classify_error(stderr: &str, exit_code: Option<i32>) -> String {
        // Nix daemon / networking errors.
        if stderr.contains("cannot connect to nix daemon")
            || stderr.contains("Cannot connect to the Nix daemon")
        {
            return "nix daemon is not running — start it with `sudo systemctl start nix-daemon` \
                    or `nix daemon`"
                .to_string();
        }
        if stderr.contains("error: flake") || stderr.contains("error: getting flake") {
            return format!(
                "nix flake error (check network / nixpkgs access): {}",
                first_line(stderr)
            );
        }
        if stderr.contains("error: downloading") || stderr.contains("error: fetching") {
            return format!(
                "nix failed to fetch runtime package (check network): {}",
                first_line(stderr)
            );
        }
        if stderr.contains("out of disk space") || stderr.contains("No space left on device") {
            return "nix store out of disk space — run `nix-collect-garbage -d` to free space"
                .to_string();
        }
        if stderr.contains("nix: command not found") || stderr.contains("No such file") {
            return "nix binary not found — is Nix installed?".to_string();
        }
        // User code errors (exit 1 from the stage wrapper).
        let code_str = exit_code
            .map(|c| format!(" (exit {c})"))
            .unwrap_or_default();
        if stderr.trim().is_empty() {
            format!("stage exited without output{code_str}")
        } else {
            format!("stage error{code_str}: {stderr}")
        }
    }

    /// Build the nix subcommand + argument list for running a stage script.
    ///
    /// - Python with no third-party imports: `nix run nixpkgs#python3 -- script.py`
    /// - Python with third-party imports:    `nix shell nixpkgs#python3Packages.X ... --command python3 script.py`
    /// - JS/Bash: `nix run nixpkgs#<runtime> -- script`
    fn build_nix_command(
        &self,
        language: &str,
        script: &Path,
        code: &str,
    ) -> (String, Vec<String>) {
        let script_path = script.to_str().unwrap_or("/dev/null").to_string();

        match language {
            "python" | "python3" | "" => {
                // If the code has `# requires:` with pip packages, use a venv
                // with system Python instead of Nix (Nix's python3Packages
                // don't reliably work with `nix shell`).
                if let Some(reqs) = Self::extract_pip_requirements(code) {
                    let venv_hash = {
                        use sha2::{Digest, Sha256};
                        let h = Sha256::digest(reqs.as_bytes());
                        hex::encode(&h[..8])
                    };
                    let venv_dir = self.cache_dir.join(format!("venv-{venv_hash}"));
                    let venv_str = venv_dir.to_string_lossy().to_string();
                    let python = venv_dir.join("bin").join("python3");
                    let python_str = python.to_string_lossy().to_string();

                    // Create venv + install deps if not cached
                    if !python.exists() {
                        let setup = std::process::Command::new("python3")
                            .args(["-m", "venv", &venv_str])
                            .output();
                        if let Ok(out) = setup {
                            if out.status.success() {
                                let pip = venv_dir.join("bin").join("pip");
                                let pkgs: Vec<&str> = reqs.split(", ").collect();
                                let mut pip_args =
                                    vec!["install", "--quiet", "--disable-pip-version-check"];
                                pip_args.extend(pkgs);
                                let _ = std::process::Command::new(pip.to_string_lossy().as_ref())
                                    .args(&pip_args)
                                    .output();
                            }
                        }
                    }

                    // Run with the venv Python directly (no nix)
                    return ("__direct__".to_string(), vec![python_str, script_path]);
                }

                let extra_pkgs = Self::detect_python_packages(code);
                if extra_pkgs.is_empty() {
                    (
                        "run".to_string(),
                        vec!["nixpkgs#python3".into(), "--".into(), script_path],
                    )
                } else {
                    let mut args: Vec<String> = extra_pkgs
                        .iter()
                        .map(|pkg| format!("nixpkgs#python3Packages.{pkg}"))
                        .collect();
                    args.extend_from_slice(&["--command".into(), "python3".into(), script_path]);
                    ("shell".to_string(), args)
                }
            }
            "javascript" | "js" => (
                "run".to_string(),
                vec!["nixpkgs#nodejs".into(), "--".into(), script_path],
            ),
            _ => (
                "run".to_string(),
                vec!["nixpkgs#bash".into(), "--".into(), script_path],
            ),
        }
    }

    /// Extract pip requirements from `# requires: pkg1==ver, pkg2==ver` comments.
    fn extract_pip_requirements(code: &str) -> Option<String> {
        for line in code.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("# requires:") {
                let reqs = trimmed.strip_prefix("# requires:").unwrap().trim();
                if !reqs.is_empty() {
                    return Some(reqs.to_string());
                }
            }
        }
        None
    }

    /// Scan Python source for `import X` / `from X import` statements and return
    /// the Nix package names for any recognised third-party libraries.
    fn detect_python_packages(code: &str) -> Vec<&'static str> {
        // Map of Python import name → nixpkgs python3Packages attribute name.
        const KNOWN: &[(&str, &str)] = &[
            ("requests", "requests"),
            ("httpx", "httpx"),
            ("aiohttp", "aiohttp"),
            ("bs4", "beautifulsoup4"),
            ("lxml", "lxml"),
            ("pandas", "pandas"),
            ("numpy", "numpy"),
            ("scipy", "scipy"),
            ("sklearn", "scikit-learn"),
            ("PIL", "Pillow"),
            ("cv2", "opencv4"),
            ("yaml", "pyyaml"),
            ("toml", "toml"),
            ("dateutil", "python-dateutil"),
            ("pytz", "pytz"),
            ("boto3", "boto3"),
            ("psycopg2", "psycopg2"),
            ("pymongo", "pymongo"),
            ("redis", "redis"),
            ("celery", "celery"),
            ("fastapi", "fastapi"),
            ("pydantic", "pydantic"),
            ("cryptography", "cryptography"),
            ("jwt", "pyjwt"),
            ("paramiko", "paramiko"),
            ("dotenv", "python-dotenv"),
            ("joblib", "joblib"),
            ("torch", "pytorch"),
            ("transformers", "transformers"),
            ("datasets", "datasets"),
            ("pyarrow", "pyarrow"),
        ];

        let mut found: Vec<&'static str> = Vec::new();
        for (import_name, nix_name) in KNOWN {
            let patterns = [
                format!("import {import_name}"),
                format!("import {import_name} "),
                format!("from {import_name} "),
                format!("from {import_name}."),
            ];
            if patterns.iter().any(|p| code.contains(p.as_str())) {
                found.push(nix_name);
            }
        }
        found
    }

    // ── Language wrappers ────────────────────────────────────────────────────

    fn wrap_python(user_code: &str) -> String {
        // Skip pip install — dependencies are handled by the venv executor
        // (build_nix_command creates a venv with pip packages pre-installed)
        // or by Nix packages (for known imports like numpy, pandas, etc.).
        let pip_install = String::new();

        format!(
            r#"import sys, json as _json
{pip_install}
# ---- user implementation ----
{user_code}
# ---- end implementation ----

if __name__ == '__main__':
    try:
        _raw = _json.loads(sys.stdin.read())
        # If the runtime passed input as a JSON-encoded string, decode it once more.
        # This happens when input arrives as null or a bare string from the CLI.
        if isinstance(_raw, str):
            try:
                _raw = _json.loads(_raw)
            except Exception:
                pass
        _output = execute(_raw if _raw is not None else {{}})
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

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Return the first non-empty line of a multi-line string, trimmed.
fn first_line(s: &str) -> &str {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or(s)
}

// ── StageExecutor impl ────────────────────────────────────────────────────────

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

    #[allow(dead_code)] // only used by the ignored integration tests
    fn make_executor() -> NixExecutor {
        let nix_bin = NixExecutor::find_nix().unwrap_or_else(|| PathBuf::from("/usr/bin/nix"));
        let cache_dir = std::env::temp_dir().join("noether-test-impl-cache");
        let _ = std::fs::create_dir_all(&cache_dir);
        NixExecutor {
            nix_bin,
            cache_dir,
            config: NixConfig::default(),
            implementations: HashMap::new(),
        }
    }

    #[test]
    fn detect_python_packages_requests() {
        let code = "import requests\ndef execute(v):\n    return requests.get(v).json()";
        let pkgs = NixExecutor::detect_python_packages(code);
        assert!(
            pkgs.contains(&"requests"),
            "expected 'requests' in {pkgs:?}"
        );
    }

    #[test]
    fn detect_python_packages_stdlib_only() {
        let code = "import urllib.request, json\ndef execute(v):\n    return json.loads(v)";
        let pkgs = NixExecutor::detect_python_packages(code);
        assert!(
            pkgs.is_empty(),
            "stdlib imports should not trigger packages: {pkgs:?}"
        );
    }

    #[test]
    fn detect_python_packages_multiple() {
        let code = "import pandas\nimport numpy as np\nfrom bs4 import BeautifulSoup\ndef execute(v): pass";
        let pkgs = NixExecutor::detect_python_packages(code);
        assert!(pkgs.contains(&"pandas"));
        assert!(pkgs.contains(&"numpy"));
        assert!(pkgs.contains(&"beautifulsoup4"));
    }

    fn test_executor() -> NixExecutor {
        NixExecutor {
            nix_bin: PathBuf::from("/usr/bin/nix"),
            cache_dir: PathBuf::from("/tmp/noether-test-cache"),
            config: NixConfig::default(),
            implementations: HashMap::new(),
        }
    }

    #[test]
    fn build_nix_command_no_packages() {
        let exec = test_executor();
        let (sub, args) = exec.build_nix_command("python", Path::new("/tmp/x.py"), "import json");
        assert_eq!(sub, "run");
        assert!(args.iter().any(|a| a.contains("python3")));
        assert!(!args.iter().any(|a| a.contains("shell")));
    }

    #[test]
    fn build_nix_command_with_requests() {
        let exec = test_executor();
        let (sub, args) =
            exec.build_nix_command("python", Path::new("/tmp/x.py"), "import requests");
        assert_eq!(sub, "shell");
        assert!(args.iter().any(|a| a.contains("python3Packages.requests")));
        assert!(args.iter().any(|a| a == "--command"));
        // Must NOT include bare nixpkgs#python3 — it conflicts with python3Packages.*
        assert!(
            !args.iter().any(|a| a == "nixpkgs#python3"),
            "bare python3 conflicts: {args:?}"
        );
    }

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

    #[test]
    fn classify_error_daemon_not_running() {
        let msg = NixExecutor::classify_error("error: cannot connect to nix daemon", Some(1));
        assert!(msg.contains("nix daemon is not running"), "got: {msg}");
    }

    #[test]
    fn classify_error_user_code_exit1() {
        let msg = NixExecutor::classify_error("ValueError: invalid input", Some(1));
        assert!(msg.contains("ValueError"), "got: {msg}");
        assert!(msg.contains("exit 1"), "got: {msg}");
    }

    #[test]
    fn classify_error_disk_full() {
        let msg = NixExecutor::classify_error("No space left on device", Some(1));
        assert!(msg.contains("disk space"), "got: {msg}");
    }

    #[test]
    fn classify_error_empty_stderr() {
        let msg = NixExecutor::classify_error("", Some(137));
        assert!(msg.contains("exit 137"), "got: {msg}");
    }

    #[test]
    fn nix_config_defaults() {
        let cfg = NixConfig::default();
        assert_eq!(cfg.timeout_secs, 30);
        assert_eq!(cfg.max_output_bytes, 10 * 1024 * 1024);
        assert_eq!(cfg.max_stderr_bytes, 64 * 1024);
    }

    #[test]
    fn first_line_extracts_correctly() {
        assert_eq!(first_line("  \nfoo\nbar"), "foo");
        assert_eq!(first_line("single"), "single");
        assert_eq!(first_line(""), "");
    }

    /// Integration test — runs when nix is available (skips gracefully if not).
    /// Requires a warm Nix binary cache; run with `cargo test -- --ignored` to include.
    #[test]
    #[ignore = "requires nix + warm binary cache; run manually with `cargo test -- --ignored`"]
    fn nix_python_identity_stage() {
        let nix_bin = match NixExecutor::find_nix() {
            Some(p) => p,
            None => {
                eprintln!("nix not found, skipping");
                return;
            }
        };

        let cache_dir = std::env::temp_dir().join("noether-nix-integ");
        let _ = std::fs::create_dir_all(&cache_dir);

        let code = "def execute(x):\n    return x";
        let executor = NixExecutor {
            nix_bin,
            cache_dir,
            config: NixConfig::default(),
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

        let id = StageId("test_identity".into());
        let result = executor.execute(&id, &serde_json::json!({"hello": "world"}));
        assert_eq!(result.unwrap(), serde_json::json!({"hello": "world"}));
    }

    /// Verify that a stage that hangs returns TimedOut, not a hang.
    /// Requires nix + warm binary cache; run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "requires nix + warm binary cache; run manually with `cargo test -- --ignored`"]
    fn nix_timeout_kills_hanging_stage() {
        let nix_bin = match NixExecutor::find_nix() {
            Some(p) => p,
            None => {
                eprintln!("nix not found, skipping timeout test");
                return;
            }
        };

        let cache_dir = std::env::temp_dir().join("noether-nix-timeout");
        let _ = std::fs::create_dir_all(&cache_dir);

        let code = "import time\ndef execute(x):\n    time.sleep(9999)\n    return x";
        let executor = NixExecutor {
            nix_bin,
            cache_dir,
            config: NixConfig {
                timeout_secs: 2,
                ..NixConfig::default()
            },
            implementations: {
                let mut m = HashMap::new();
                let id = StageId("hanging".into());
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

        let id = StageId("hanging".into());
        let result = executor.execute(&id, &serde_json::json!(null));
        assert!(
            matches!(
                result,
                Err(ExecutionError::TimedOut {
                    timeout_secs: 2,
                    ..
                })
            ),
            "expected TimedOut, got: {result:?}"
        );
    }
}
