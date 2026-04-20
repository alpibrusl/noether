#![warn(clippy::unwrap_used)]
#![cfg_attr(test, allow(clippy::unwrap_used))]

//! Nix-based executor for synthesized stages.
//!
//! Runs stage implementations as subprocesses using `nix run nixpkgs#<runtime>`,
//! giving a reproducible, Nix-pinned runtime for Python/JavaScript/Bash code
//! without requiring any ambient language runtime on the host.
//!
//! **This is a reproducibility boundary, not an isolation boundary.** Stages
//! run with the privileges of the host user — they can read/write the
//! filesystem, make arbitrary network calls, and read environment variables.
//! Do not execute untrusted stages on a host with credentials you are not
//! willing to risk. See SECURITY.md for the full trust model.
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
    /// Isolation backend to wrap each stage subprocess in. When set
    /// to [`super::isolation::IsolationBackend::None`] (the default
    /// for back-compat), stages run with full host-user privileges
    /// — see SECURITY.md. Set via
    /// [`NixConfig::with_isolation`] or the CLI `--isolate` flag.
    pub isolation: super::isolation::IsolationBackend,
}

impl Default for NixConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 30,
            max_output_bytes: 10 * 1024 * 1024,
            max_stderr_bytes: 64 * 1024,
            isolation: super::isolation::IsolationBackend::None,
        }
    }
}

impl NixConfig {
    /// Set the isolation backend. Returns `self` for chaining.
    pub fn with_isolation(mut self, backend: super::isolation::IsolationBackend) -> Self {
        self.isolation = backend;
        self
    }
}

// ── Internal stage storage ───────────────────────────────────────────────────

/// Maps stage IDs to their implementation (source code + language tag +
/// declared effects — needed so the isolation layer can derive a
/// policy).
#[derive(Clone)]
struct StageImpl {
    code: String,
    language: String,
    effects: noether_core::effects::EffectSet,
}

// ── NixExecutor ──────────────────────────────────────────────────────────────

/// Executor that runs synthesized stages through Nix-managed language runtimes.
///
/// When `nix` is available, each stage is executed as a subprocess with a
/// Nix-pinned runtime (e.g. `nix run nixpkgs#python3 -- stage.py`). The Nix
/// binary cache ensures the runtime is downloaded once and then reused from
/// the store. **This gives reproducibility, not isolation**: the subprocess
/// inherits the host user's privileges, filesystem, and network. See module
/// docs and SECURITY.md for the full trust model.
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

        // Walk $PATH directly rather than spawning `which`. Avoids a
        // subprocess + the risk that `which` is missing or shadowed on
        // minimal systems (e.g. some container base images).
        let path_env = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path_env) {
            let candidate = dir.join("nix");
            if candidate.is_file() {
                return Some(candidate);
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
                        effects: stage.signature.effects.clone(),
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

    /// Clone the current config (minus the implementations map) for
    /// callers that want to rebuild with different knobs.
    pub fn config_snapshot(&self) -> NixConfig {
        self.config.clone()
    }

    /// Rebuild a NixExecutor with a replacement config, preserving
    /// its registered implementations. Returns `Some(..)` or `None`
    /// when reconstruction fails — today it can't fail, but the
    /// Option keeps the API forward-compatible.
    pub fn rebuild_with_config(mut self, config: NixConfig) -> Option<Self> {
        self.config = config;
        Some(self)
    }

    /// Register a stage with explicit declared effects. Used by tests
    /// and by callers that want to drive the isolation policy without
    /// going through the full StageStore.
    pub fn register_with_effects(
        &mut self,
        stage_id: &StageId,
        code: &str,
        language: &str,
        effects: noether_core::effects::EffectSet,
    ) {
        self.implementations.insert(
            stage_id.0.clone(),
            StageImpl {
                code: code.into(),
                language: language.into(),
                effects,
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

        // Build the full argv — either raw (no isolation) or wrapped in
        // `bwrap` when an isolation backend is configured. The wrapped
        // command spawns bwrap which execs the inner command inside a
        // fresh sandbox.
        let raw_argv: Vec<String> = if nix_subcommand == "__direct__" {
            args.clone()
        } else {
            let mut v = vec![self.nix_bin.display().to_string(), nix_subcommand.clone()];
            v.push("--no-write-lock-file".into());
            v.push("--quiet".into());
            v.extend(args.iter().cloned());
            v
        };

        let mut spawn = match &self.config.isolation {
            super::isolation::IsolationBackend::None => {
                // No sandbox — legacy behaviour.
                let mut cmd = Command::new(&raw_argv[0]);
                cmd.args(&raw_argv[1..]);
                cmd
            }
            super::isolation::IsolationBackend::Bwrap { bwrap_path } => {
                // /work is a sandbox-private tmpfs (set by
                // `IsolationPolicy::from_effects` default) — no host-side
                // tmpdir to manage, no cleanup, no race.
                let mut policy = super::isolation::IsolationPolicy::from_effects(
                    self.implementations
                        .get(&stage_id.0)
                        .map(|i| &i.effects)
                        .unwrap_or(&noether_core::effects::EffectSet::pure()),
                );
                // Expose the stage-script cache (where this invocation's
                // wrapped `.py` / `.sh` / `.js` file lives). Scoped to
                // `cache_dir` so the sandbox sees noether's own
                // workspace and nothing else from the host user's home.
                policy.ro_binds.push(noether_isolation::RoBind::new(
                    self.cache_dir.to_path_buf(),
                    self.cache_dir.to_path_buf(),
                ));
                // Nix binary visibility inside the sandbox has three cases:
                //
                // 1. `nix_bin` is under `/nix/store` — covered by the
                //    default `/nix/store` bind. Nothing to add.
                // 2. `nix_bin` is under `cache_dir` — covered by the
                //    `cache_dir` bind above. Nothing to add.
                // 3. `nix_bin` is a distro-packaged install (e.g.
                //    `/usr/bin/nix`, `/usr/local/bin/nix`). The
                //    binary is dynamically linked against glibc,
                //    libcrypto, and readline living in `/usr/lib*`.
                //    Binding just the nix executable file would let
                //    the sandbox exec it but immediately fail
                //    resolving `ld-linux-x86-64.so.2` — the kernel
                //    can't find the dynamic loader.
                //
                //    Widening the bind set to include `/usr/lib*`
                //    re-exposes the full suid-binary surface the
                //    hardening closed. Instead: refuse to run, with
                //    a clear message pointing the operator at the
                //    Nix-native install path. The trust model here
                //    is "nix belongs to the same reproducibility
                //    boundary as the stages it dispatches;" a
                //    distro-packaged nix violates that boundary
                //    anyway.
                if !self.nix_bin.starts_with("/nix/store")
                    && !self.nix_bin.starts_with(&self.cache_dir)
                {
                    return Err(ExecutionError::StageFailed {
                        stage_id: stage_id.clone(),
                        message: format!(
                            "stage isolation is enabled but nix is installed at \
                             {} (outside /nix/store). A distro-packaged nix is \
                             dynamically linked against host libraries; binding \
                             those into the sandbox would defeat isolation. \
                             Install nix via the Determinate / upstream \
                             installer (places nix under /nix/store) or pass \
                             --isolate=none to run without the sandbox.",
                            self.nix_bin.display()
                        ),
                    });
                }
                // `build_bwrap_command` emits `--setenv` args for
                // the sandbox's env allowlist (HOME=/work,
                // USER=nobody, + inherited). Nothing else to do here.
                super::isolation::build_bwrap_command(bwrap_path, &policy, &raw_argv)
            }
        };

        let mut child = spawn
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| ExecutionError::StageFailed {
                stage_id: stage_id.clone(),
                message: format!("failed to spawn process: {e}"),
            })?;
        let _ = raw_argv;

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
    ///
    /// Each spec is validated to prevent typosquatting and shell-metacharacter
    /// injection from LLM-authored stages. By default, every package must be
    /// pinned to an exact version (`pkg==1.2.3`). Set
    /// `NOETHER_ALLOW_UNPINNED_PIP=1` to lift the pinning requirement for
    /// local development; the character-set validation always runs.
    ///
    /// Invalid specs are dropped with a warning; if no valid specs remain,
    /// this returns `None` and the caller falls back to the default Nix
    /// runtime (where the missing dependency will surface as an honest
    /// runtime error instead of a silent pip-install of attacker-chosen
    /// names).
    fn extract_pip_requirements(code: &str) -> Option<String> {
        for line in code.lines() {
            let trimmed = line.trim();
            let Some(reqs_raw) = trimmed.strip_prefix("# requires:") else {
                continue;
            };
            let reqs = reqs_raw.trim();
            if reqs.is_empty() {
                continue;
            }
            let valid: Vec<String> = reqs
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .filter(|s| match validate_pip_spec(s) {
                    Ok(()) => true,
                    Err(reason) => {
                        eprintln!(
                            "[noether] rejected `# requires:` entry {s:?} ({reason}); skipping"
                        );
                        false
                    }
                })
                .map(|s| s.to_string())
                .collect();

            if valid.is_empty() {
                eprintln!(
                    "[noether] all `# requires:` entries rejected (raw={reqs:?}); falling back to default Nix runtime"
                );
                return None;
            }
            return Some(valid.join(", "));
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

    #[cfg(test)]
    #[allow(dead_code)]
    fn _expose_extract_future_imports(code: &str) -> (String, String) {
        Self::extract_future_imports(code)
    }

    /// Pull every `from __future__ import ...` line out of `code` and return
    /// `(joined_future_imports, code_without_them)`. The future imports are
    /// returned with trailing newlines so the caller can embed them directly
    /// at the top of the wrapper. Detection is line-based (no AST) — matches
    /// any non-indented line starting with `from __future__ import`.
    fn extract_future_imports(code: &str) -> (String, String) {
        let mut hoisted = String::new();
        let mut remaining = String::new();
        for line in code.lines() {
            let trimmed = line.trim_start();
            if !line.starts_with(' ')
                && !line.starts_with('\t')
                && trimmed.starts_with("from __future__ import")
            {
                hoisted.push_str(line);
                hoisted.push('\n');
            } else {
                remaining.push_str(line);
                remaining.push('\n');
            }
        }
        (hoisted, remaining)
    }

    fn wrap_python(user_code: &str) -> String {
        // Skip pip install — dependencies are handled by the venv executor
        // (build_nix_command creates a venv with pip packages pre-installed)
        // or by Nix packages (for known imports like numpy, pandas, etc.).
        let pip_install = String::new();

        // Hoist any `from __future__ import ...` lines out of user code and
        // emit them as the very first statements of the wrapper. Python
        // requires `__future__` imports to be the first non-comment,
        // non-docstring statement in a module — leaving them embedded in the
        // user-code block (which is line ~17 of the wrapped file) raises
        // `SyntaxError: from __future__ imports must occur at the
        // beginning of the file`.
        let (future_imports, user_code_clean) = Self::extract_future_imports(user_code);

        format!(
            r#"{future_imports}import sys, json as _json
{pip_install}
# ---- user implementation ----
{user_code_clean}
# ---- end implementation ----

if __name__ == '__main__':
    if 'execute' not in dir() or not callable(globals().get('execute')):
        print(
            "Noether stage error: implementation must define a top-level "
            "function `def execute(input): ...` that takes the parsed input dict "
            "and returns the output dict. Do not read from stdin or print to stdout — "
            "the Noether runtime handles I/O for you.",
            file=sys.stderr,
        )
        sys.exit(1)
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

/// Validate a single pip requirement spec from a `# requires:` comment.
///
/// Accepts `pkg==version`. The package name must match PEP 503 normalisation
/// (letters, digits, `_`, `-`, `.`). The version must be a straightforward
/// PEP 440-ish literal (letters, digits, `.`, `+`, `!`, `-`). The pinning
/// requirement (`==`) can be lifted with `NOETHER_ALLOW_UNPINNED_PIP=1`
/// for local dev, but the character-set validation always runs so injected
/// shell metacharacters, quotes, or URL-form specs are rejected.
fn validate_pip_spec(spec: &str) -> Result<(), &'static str> {
    let allow_unpinned = matches!(
        std::env::var("NOETHER_ALLOW_UNPINNED_PIP").as_deref(),
        Ok("1" | "true" | "yes" | "on")
    );

    // Split at the first `==`. If absent, require the opt-in flag.
    let (name, version) = match spec.split_once("==") {
        Some((n, v)) => (n.trim(), Some(v.trim())),
        None => {
            if !allow_unpinned {
                return Err("unpinned; use pkg==version or set NOETHER_ALLOW_UNPINNED_PIP=1");
            }
            (spec.trim(), None)
        }
    };

    if name.is_empty() {
        return Err("empty package name");
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
    {
        return Err("package name contains disallowed characters");
    }
    if let Some(v) = version {
        if v.is_empty() {
            return Err("empty version after `==`");
        }
        if !v
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'!' | b'-'))
        {
            return Err("version contains disallowed characters");
        }
    }
    Ok(())
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
    fn register_with_effects_preserves_network_effect() {
        // Regression guard on the synthesized-stage effects path.
        // Pre-hardening, `register_synthesized` → `NixExecutor::register`
        // dropped the declared effects and stamped `EffectSet::pure()`
        // onto the stored `StageImpl`. A Network-effect stage ended
        // up with a no-network sandbox and failed with DNS errors at
        // runtime. The `register()` shim is gone; this test locks in
        // that `register_with_effects` is the only registration path
        // and that it threads the effects through verbatim.
        use noether_core::effects::{Effect, EffectSet};
        let mut exec = make_executor();
        let id = StageId("sig_network".into());
        let effects = EffectSet::new([Effect::Pure, Effect::Network]);
        exec.register_with_effects(&id, "code", "python", effects.clone());
        let stored = exec
            .implementations
            .get(&id.0)
            .expect("stage should be registered");
        assert_eq!(
            stored.effects, effects,
            "declared effects must survive register_with_effects"
        );
        assert!(
            stored.effects.iter().any(|e| matches!(e, Effect::Network)),
            "Network must be preserved so the sandbox opens the net ns"
        );
    }

    #[test]
    fn validate_pip_spec_accepts_pinned() {
        assert!(validate_pip_spec("pandas==2.0.0").is_ok());
        assert!(validate_pip_spec("scikit-learn==1.5.1").is_ok());
        assert!(validate_pip_spec("urllib3==2.2.3").is_ok());
        assert!(validate_pip_spec("pydantic==2.5.0+cu121").is_ok());
    }

    #[test]
    fn validate_pip_spec_rejects_unpinned_by_default() {
        // Ensure the opt-in flag is not accidentally set in the test env.
        let guard = (std::env::var_os("NOETHER_ALLOW_UNPINNED_PIP"),);
        // SAFETY: single-threaded test — no other test reads this var at the same time.
        unsafe {
            std::env::remove_var("NOETHER_ALLOW_UNPINNED_PIP");
        }
        let result = validate_pip_spec("pandas");
        // Restore prior state before asserting.
        if let (Some(prev),) = guard {
            unsafe {
                std::env::set_var("NOETHER_ALLOW_UNPINNED_PIP", prev);
            }
        }
        assert!(result.is_err(), "bare name must be rejected without opt-in");
    }

    #[test]
    fn validate_pip_spec_rejects_shell_metacharacters() {
        for bad in [
            "pandas; rm -rf /",
            "pandas==$(whoami)",
            "pandas==1.0.0; echo pwned",
            "pandas==`id`",
            "https://evil.example/wheel.whl",
            "git+https://example.com/repo.git",
            "pkg with space==1.0",
            "pkg==1.0 && echo",
        ] {
            assert!(validate_pip_spec(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn validate_pip_spec_rejects_empty() {
        assert!(validate_pip_spec("==1.0").is_err());
        assert!(validate_pip_spec("pkg==").is_err());
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
    fn future_imports_are_hoisted_out_of_user_code() {
        let user = "from __future__ import annotations\nimport json\n\ndef execute(input):\n    return input\n";
        let wrapped = NixExecutor::wrap_python(user);
        // The future import must come BEFORE `import sys, json as _json`.
        let future_pos = wrapped
            .find("from __future__ import annotations")
            .expect("future import should be present in wrapper");
        let stdlib_pos = wrapped
            .find("import sys, json as _json")
            .expect("stdlib imports should be present");
        assert!(
            future_pos < stdlib_pos,
            "future import must precede stdlib imports in wrapped output"
        );
    }

    #[test]
    fn user_code_without_future_imports_is_unchanged() {
        let user = "import json\n\ndef execute(input):\n    return input\n";
        let (hoisted, remaining) = NixExecutor::extract_future_imports(user);
        assert_eq!(hoisted, "");
        assert_eq!(remaining.trim(), user.trim());
    }

    #[test]
    fn nested_future_import_inside_function_is_not_hoisted() {
        // Indented "from __future__" lines (inside a function) are not
        // valid Python anyway, but the hoister must not promote them.
        let user =
            "def execute(input):\n    from __future__ import annotations\n    return input\n";
        let (hoisted, _) = NixExecutor::extract_future_imports(user);
        assert_eq!(hoisted, "");
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
                        effects: noether_core::effects::EffectSet::pure(),
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
                        effects: noether_core::effects::EffectSet::pure(),
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
