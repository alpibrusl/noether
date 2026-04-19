//! Stage execution isolation.
//!
//! Wraps subprocess execution in a sandbox that restricts what the
//! stage can read, write, and call. Closes the gap documented in
//! `SECURITY.md`: a user-authored Python stage has host-user
//! privileges by default; with isolation it runs in a bounded
//! filesystem + network namespace.
//!
//! Phase 1 (v0.7) backends:
//!
//! - [`IsolationBackend::None`] — legacy pass-through. Emits a
//!   warning unless the user opts in with
//!   `--unsafe-no-isolation` / `NOETHER_ISOLATION=none`.
//! - [`IsolationBackend::Bwrap`] — bubblewrap wrapper. Linux-only.
//!   Requires the `bwrap` binary in `PATH`.
//!
//! Phase 2 (v0.8) will add `IsolationBackend::Native` — direct
//! `unshare(2)` + Landlock + seccomp syscalls, no external binary.
//! See `docs/roadmap/2026-04-18-stage-isolation.md`.
//!
//! ## Policy derivation
//!
//! An [`IsolationPolicy`] is derived from a stage's declared
//! `EffectSet`: stages without `Effect::Network` get a fresh empty
//! network namespace; all stages get read-only `/nix/store` and a
//! per-invocation `/work` tmpdir. Host capabilities are dropped;
//! the host's HOME, SSH keys, and credentials files are unreachable.

use noether_core::effects::{Effect, EffectSet};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Which isolation backend to use for a stage execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IsolationBackend {
    /// No isolation — legacy behaviour. A malicious stage can read
    /// host files, call out to the network, write to the user's
    /// home directory. Noether emits a warning the first time this
    /// backend is used unless `--unsafe-no-isolation` is set.
    None,
    /// Wrap the stage subprocess in `bwrap`. Requires the
    /// bubblewrap binary in `PATH`. Linux-only.
    Bwrap { bwrap_path: PathBuf },
}

impl IsolationBackend {
    /// Resolve `"auto"`: pick the best backend available on this
    /// host. On Linux with `bwrap` on `PATH`, that's
    /// [`IsolationBackend::Bwrap`]. Elsewhere, falls back to
    /// [`IsolationBackend::None`] with the returned warning string
    /// so the caller can surface it.
    pub fn auto() -> (Self, Option<String>) {
        if let Some(path) = find_bwrap() {
            return (IsolationBackend::Bwrap { bwrap_path: path }, None);
        }
        (
            IsolationBackend::None,
            Some(
                "isolation backend 'auto' could not find bubblewrap \
                 (bwrap) on PATH; stage execution runs with full host-user \
                 privileges. Install bubblewrap (apt/brew/nix) to enable \
                 sandboxing, or pass --unsafe-no-isolation to silence \
                 this warning."
                    .into(),
            ),
        )
    }

    /// Parse the `--isolate` / `NOETHER_ISOLATION` argument.
    pub fn from_flag(flag: &str) -> Result<(Self, Option<String>), IsolationError> {
        match flag {
            "auto" => Ok(Self::auto()),
            "bwrap" => match find_bwrap() {
                Some(path) => Ok((IsolationBackend::Bwrap { bwrap_path: path }, None)),
                None => Err(IsolationError::BackendUnavailable {
                    backend: "bwrap".into(),
                    reason: "binary not found in PATH".into(),
                }),
            },
            "none" => Ok((IsolationBackend::None, None)),
            other => Err(IsolationError::UnknownBackend { name: other.into() }),
        }
    }

    pub fn is_effective(&self) -> bool {
        !matches!(self, IsolationBackend::None)
    }
}

/// Error from the isolation layer itself — policy misconfiguration,
/// backend unavailable, bwrap spawn failure. Stage-body errors come
/// back as the usual `ExecutionError` on the inner command.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum IsolationError {
    #[error("isolation backend '{name}' is not recognised; expected one of: auto, bwrap, none")]
    UnknownBackend { name: String },

    #[error("isolation backend '{backend}' is unavailable: {reason}")]
    BackendUnavailable { backend: String, reason: String },

    #[error("failed to create work directory: {path} ({reason})")]
    WorkDirCreate { path: String, reason: String },
}

/// What the sandbox does and doesn't let a stage reach.
///
/// Derived from a stage's `EffectSet` via
/// [`IsolationPolicy::from_effects`]. Callers rarely construct this
/// manually; it's shaped so the stage executor can translate it into
/// backend-specific flags (bwrap args in Phase 1, unshare+landlock+seccomp
/// in Phase 2).
#[derive(Debug, Clone)]
pub struct IsolationPolicy {
    /// Read-only bind mounts: `(host_path, sandbox_path)`. Always
    /// includes `/nix/store` so Nix-pinned runtimes resolve inside
    /// the sandbox.
    pub ro_binds: Vec<(PathBuf, PathBuf)>,
    /// Scratch directory strategy for `/work` inside the sandbox.
    ///
    /// - `None` (recommended, and the default from [`Self::from_effects`])
    ///   → `bwrap` creates `/work` as a sandbox-private tmpfs via
    ///   `--dir /work`. No host-side path exists; cleanup happens
    ///   automatically when the sandbox exits; a malicious host user
    ///   can't race to write predicatable filenames into the work
    ///   dir before the stage runs.
    /// - `Some(host)` → `--bind <host> /work`. Host dir must exist
    ///   and be writable by the sandbox's effective UID (65534 by
    ///   default). Only for callers that need to inspect the work
    ///   dir after execution — e.g., an integration test.
    pub work_host: Option<PathBuf>,
    /// Inherit the host's network namespace (`true`) or unshare into
    /// a fresh empty one (`false`). Only `true` when the stage has
    /// `Effect::Network`.
    pub network: bool,
    /// Environment variables to pass through to the sandboxed
    /// process. Everything else in the parent environment is
    /// cleared.
    pub env_allowlist: Vec<String>,
}

impl IsolationPolicy {
    /// Build the policy for a stage with the given effects.
    ///
    /// Defaults to a sandbox-private `/work` (tmpfs, no host-side
    /// state). Callers that need a host-visible work dir can swap in
    /// [`Self::with_work_host`].
    pub fn from_effects(effects: &EffectSet) -> Self {
        let has_network = effects.iter().any(|e| matches!(e, Effect::Network));
        Self {
            ro_binds: vec![(PathBuf::from("/nix/store"), PathBuf::from("/nix/store"))],
            work_host: None,
            network: has_network,
            env_allowlist: vec![
                "PATH".into(),
                "HOME".into(),
                "USER".into(),
                "LANG".into(),
                "NIX_PATH".into(),
                "NIX_SSL_CERT_FILE".into(),
                "SSL_CERT_FILE".into(),
                "NOETHER_LOG_LEVEL".into(),
            ],
        }
    }

    /// Override the sandbox's `/work` to bind a caller-provided host
    /// directory. The directory must already exist and be writable by
    /// the sandbox effective UID (65534). Consumers mostly leave the
    /// default (tmpfs).
    pub fn with_work_host(mut self, host: PathBuf) -> Self {
        self.work_host = Some(host);
        self
    }
}

/// Conventional "nobody" UID/GID on Linux. bwrap maps the invoking
/// user to this identity inside the sandbox so the stage cannot
/// observe the real UID of the caller.
pub(crate) const NOBODY_UID: u32 = 65534;
pub(crate) const NOBODY_GID: u32 = 65534;

/// Build a `bwrap` invocation that runs `cmd` inside a sandbox.
///
/// Returns a `Command` ready to spawn — the caller keeps ownership
/// of stdin/stdout/stderr piping and waits on the child. The
/// `work_host` path must exist; `bwrap` will fail otherwise.
///
/// Flags used (see bubblewrap(1)):
///
/// - `--unshare-all` — fresh user, pid, uts, ipc, mount, cgroup
///   namespaces. Network namespace is unshared too, unless the
///   policy re-shares via `--share-net` (see below).
/// - `--uid 65534 --gid 65534` — map the invoking user to
///   `nobody/nogroup` inside the sandbox. Without this, the stage
///   would observe the host user's real UID (informational leak,
///   and potentially exploitable when combined with filesystem
///   bind-mount misconfiguration).
/// - `--die-with-parent` — if the parent dies, so does the sandbox.
/// - `--proc /proc`, `--dev /dev` — standard Linux mounts.
/// - `--ro-bind <host> <sandbox>` — read-only mounts from the
///   policy's `ro_binds`. Always includes `/nix/store`.
/// - `--bind <work_host> /work` — writable scratch.
/// - `--chdir /work` — subprocess starts in the scratch dir.
/// - `--clearenv` — wipe the environment; the executor re-adds the
///   allowlisted variables via `.env(...)`.
/// - `--share-net` — only when `policy.network` is true.
/// - `--cap-drop ALL` — drop every capability inside the sandbox.
pub fn build_bwrap_command(
    bwrap: &Path,
    policy: &IsolationPolicy,
    inner_cmd: &[String],
) -> Command {
    let mut c = Command::new(bwrap);
    c.arg("--unshare-all")
        .arg("--die-with-parent")
        .arg("--new-session")
        .arg("--uid")
        .arg(NOBODY_UID.to_string())
        .arg("--gid")
        .arg(NOBODY_GID.to_string())
        .arg("--proc")
        .arg("/proc")
        .arg("--dev")
        .arg("/dev")
        .arg("--tmpfs")
        .arg("/tmp")
        .arg("--clearenv")
        .arg("--cap-drop")
        .arg("ALL");

    if policy.network {
        c.arg("--share-net");
    }

    for (host, sandbox) in &policy.ro_binds {
        c.arg("--ro-bind").arg(host).arg(sandbox);
    }

    match &policy.work_host {
        Some(host) => {
            c.arg("--bind").arg(host).arg("/work");
        }
        None => {
            // Sandbox-private tmpfs at /work. No host-side path,
            // so nothing to clean up and nothing for a host-side
            // attacker to race into before the sandbox starts.
            c.arg("--dir").arg("/work");
        }
    }
    c.arg("--chdir").arg("/work");

    c.arg("--").args(inner_cmd);
    c
}

/// Locate the `bwrap` binary. Returns `None` if it's not on `PATH`.
pub fn find_bwrap() -> Option<PathBuf> {
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join("bwrap");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use noether_core::effects::{Effect, EffectSet};

    #[test]
    fn from_flag_parses_known_values() {
        assert!(matches!(
            IsolationBackend::from_flag("none").unwrap().0,
            IsolationBackend::None
        ));
        assert!(IsolationBackend::from_flag("unknown").is_err());
    }

    #[test]
    fn policy_without_network_effect_isolates_network() {
        let effects = EffectSet::pure();
        let policy = IsolationPolicy::from_effects(&effects);
        assert!(!policy.network);
    }

    #[test]
    fn policy_with_network_effect_shares_network() {
        let effects = EffectSet::new([Effect::Pure, Effect::Network]);
        let policy = IsolationPolicy::from_effects(&effects);
        assert!(policy.network);
    }

    #[test]
    fn policy_defaults_to_sandbox_private_work() {
        // New default after the v0.7 hardening: no host-side workdir.
        let policy = IsolationPolicy::from_effects(&EffectSet::pure());
        assert!(
            policy.work_host.is_none(),
            "from_effects must default to sandbox-private /work; \
             callers asking for host-visible scratch must opt in via \
             .with_work_host(...)"
        );
    }

    #[test]
    fn policy_always_binds_nix_store() {
        let policy = IsolationPolicy::from_effects(&EffectSet::pure());
        let (host, sandbox) = policy
            .ro_binds
            .iter()
            .find(|(_, s)| s == Path::new("/nix/store"))
            .expect("nix store bind is missing");
        assert_eq!(host, Path::new("/nix/store"));
        assert_eq!(sandbox, Path::new("/nix/store"));
    }

    #[test]
    fn bwrap_command_includes_core_flags() {
        let policy = IsolationPolicy::from_effects(&EffectSet::pure());
        let cmd = build_bwrap_command(
            Path::new("/usr/bin/bwrap"),
            &policy,
            &["python3".into(), "script.py".into()],
        );
        let argv: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into()).collect();

        assert!(argv.contains(&"--unshare-all".to_string()));
        assert!(argv.contains(&"--clearenv".to_string()));
        assert!(argv.contains(&"--cap-drop".to_string()));
        assert!(argv.contains(&"ALL".to_string()));
        assert!(argv.contains(&"--die-with-parent".to_string()));
        // No --share-net when no Network effect.
        assert!(!argv.contains(&"--share-net".to_string()));
        // Default workdir is sandbox-private tmpfs, not a host bind.
        assert!(argv.contains(&"--dir".to_string()));
        assert!(argv.contains(&"/work".to_string()));
        // Inner command appended after --.
        let dash_dash_idx = argv
            .iter()
            .position(|a| a == "--")
            .expect("missing -- separator");
        assert_eq!(argv[dash_dash_idx + 1], "python3");
    }

    #[test]
    fn bwrap_command_uses_host_bind_when_work_host_set() {
        // Integration tests and debugging tools can still opt into a
        // host-visible work dir via `with_work_host`.
        let policy = IsolationPolicy::from_effects(&EffectSet::pure())
            .with_work_host(PathBuf::from("/tmp/inspect-me"));
        let cmd = build_bwrap_command(Path::new("/usr/bin/bwrap"), &policy, &["python3".into()]);
        let argv: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into()).collect();
        let bind_pos = argv
            .iter()
            .position(|a| a == "--bind")
            .expect("--bind missing");
        assert_eq!(argv[bind_pos + 1], "/tmp/inspect-me");
        assert_eq!(argv[bind_pos + 2], "/work");
    }

    #[test]
    fn bwrap_command_adds_share_net_for_network_effect() {
        let policy =
            IsolationPolicy::from_effects(&EffectSet::new([Effect::Pure, Effect::Network]));
        let cmd = build_bwrap_command(
            Path::new("/usr/bin/bwrap"),
            &policy,
            &["curl".into(), "https://example.com".into()],
        );
        let argv: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into()).collect();
        assert!(argv.contains(&"--share-net".to_string()));
    }

    #[test]
    fn bwrap_command_maps_to_nobody_uid_and_gid() {
        // Regression guard: the sandbox must not surface the invoking
        // user's real UID. Without `--uid 65534 --gid 65534` a stage
        // can call `os.getuid()` / `id` and observe the host user —
        // that's both an info leak and a stepping stone when combined
        // with any bind-mount misconfiguration.
        let policy = IsolationPolicy::from_effects(&EffectSet::pure());
        let cmd = build_bwrap_command(Path::new("/usr/bin/bwrap"), &policy, &["python3".into()]);
        let argv: Vec<String> = cmd.get_args().map(|a| a.to_string_lossy().into()).collect();

        let uid_pos = argv
            .iter()
            .position(|a| a == "--uid")
            .expect("--uid missing");
        assert_eq!(argv[uid_pos + 1], "65534");
        let gid_pos = argv
            .iter()
            .position(|a| a == "--gid")
            .expect("--gid missing");
        assert_eq!(argv[gid_pos + 1], "65534");
    }

    #[test]
    fn effectiveness_predicate_matches_variant() {
        assert!(!IsolationBackend::None.is_effective());
        assert!(IsolationBackend::Bwrap {
            bwrap_path: PathBuf::from("/usr/bin/bwrap"),
        }
        .is_effective());
    }
}
