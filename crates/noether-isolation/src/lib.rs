//! Stage execution isolation — the sandbox primitive extracted from
//! [`noether_engine::executor::isolation`] for consumers that want
//! isolation without pulling in the composition engine.
//!
//! The `noether-engine` crate re-exports this module verbatim, so
//! existing callers see no API change. Downstream consumers
//! ([`agentspec`](https://github.com/alpibrusl/agentspec), the
//! standalone `noether-sandbox` binary) depend on this crate
//! directly.
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
//! `EffectSet`. Phase 1 surfaces exactly one axis from the effect
//! vocabulary — `Effect::Network` toggles whether the sandbox
//! inherits the host's network namespace. Every other effect
//! variant (`Pure`, `Fallible`, `Llm`, `NonDeterministic`, `Process`,
//! `Cost`, `Unknown`) produces the same baseline policy: RO
//! `/nix/store` bind, a sandbox-private `/work` tmpfs,
//! `--cap-drop ALL`, UID/GID mapped to nobody, `--clearenv` with a
//! short allowlist.
//!
//! ### TLS trust store — dual path
//!
//! When `network=true`, the sandbox binds `/etc/ssl/certs`
//! (via `--ro-bind-try`) for non-Nix-aware clients that expect the
//! system trust store (curl, openssl). Nix-built code uses
//! `NIX_SSL_CERT_FILE` / `SSL_CERT_FILE` (both in the env
//! allowlist) pointing into `/nix/store`, which is always bound.
//! So TLS works whether the stage resolves certs through the
//! filesystem path or the env-pointer path; NixOS hosts without
//! `/etc/ssl/certs` fall through to the env path automatically.
//!
//! ### Filesystem effects — not yet expressible
//!
//! The v0.6 `Effect` enum has no `FsRead(path)` / `FsWrite(path)`
//! variants, so there is no way for a stage to declare "I need to
//! read `/etc/ssl` but nothing else." The sandbox compensates by
//! allowing *nothing* outside `/nix/store`, the executor's cache
//! dir, and the nix binary. That is the strictest sane default —
//! but it means stages that legitimately need a specific host path
//! cannot run under isolation today. Planned for v0.8: extend
//! `Effect` with `FsRead` / `FsWrite` path variants, then expand
//! `from_effects` to translate them into bind mounts. Tracked in
//! `docs/roadmap/2026-04-18-stage-isolation.md`.

use noether_core::effects::{Effect, EffectSet};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

/// Which isolation backend to use for a stage execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

/// Error from the isolation layer itself — policy misconfiguration
/// or backend unavailable. Stage-body errors come back as the usual
/// execution error on the inner command.
#[derive(Debug, Clone, PartialEq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IsolationError {
    #[error("isolation backend '{name}' is not recognised; expected one of: auto, bwrap, none")]
    UnknownBackend { name: String },

    #[error("isolation backend '{backend}' is unavailable: {reason}")]
    BackendUnavailable { backend: String, reason: String },
}

/// A single read-only bind mount. Named-struct rather than a tuple
/// so the JSON wire format stays readable for non-Rust consumers:
/// `{"host": "/nix/store", "sandbox": "/nix/store"}` instead of the
/// earlier `["/nix/store", "/nix/store"]`. The latter was terser but
/// gave external language bindings no schema hint about which path
/// was which.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoBind {
    /// Host-side path. Must exist; `bwrap` will fail otherwise.
    pub host: PathBuf,
    /// Path inside the sandbox where the host dir/file appears.
    pub sandbox: PathBuf,
}

impl RoBind {
    pub fn new(host: impl Into<PathBuf>, sandbox: impl Into<PathBuf>) -> Self {
        Self {
            host: host.into(),
            sandbox: sandbox.into(),
        }
    }
}

impl From<(PathBuf, PathBuf)> for RoBind {
    fn from((host, sandbox): (PathBuf, PathBuf)) -> Self {
        Self { host, sandbox }
    }
}

/// What the sandbox does and doesn't let a stage reach.
///
/// Derived from a stage's `EffectSet` via
/// [`IsolationPolicy::from_effects`]. Callers rarely construct this
/// manually; it's shaped so the stage executor can translate it into
/// backend-specific flags (bwrap args in Phase 1, unshare+landlock+seccomp
/// in Phase 2). Serde-enabled so downstream consumers (e.g. the
/// `noether-sandbox` binary) can exchange policies over IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IsolationPolicy {
    /// Read-only bind mounts. Always includes `/nix/store` so
    /// Nix-pinned runtimes resolve inside the sandbox.
    pub ro_binds: Vec<RoBind>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
            ro_binds: vec![RoBind::new("/nix/store", "/nix/store")],
            work_host: None,
            network: has_network,
            env_allowlist: vec![
                "PATH".into(),
                "HOME".into(),
                "USER".into(),
                "LANG".into(),
                "LC_ALL".into(),
                "LC_CTYPE".into(),
                "NIX_PATH".into(),
                "NIX_SSL_CERT_FILE".into(),
                "SSL_CERT_FILE".into(),
                "NOETHER_LOG_LEVEL".into(),
                "RUST_LOG".into(),
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
pub const NOBODY_UID: u32 = 65534;
pub const NOBODY_GID: u32 = 65534;

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
        // `--share-net` re-enters the host network namespace but the
        // sandbox rootfs is otherwise empty. glibc NSS resolves DNS
        // through `/etc/resolv.conf`, `/etc/nsswitch.conf`, and
        // `/etc/hosts`; without those, even a correctly networked
        // sandbox can't resolve hostnames. `--ro-bind-try` is a
        // no-op when the source is absent (e.g. NixOS systems that
        // route DNS differently), so it's safe to emit regardless.
        //
        // `/etc/ssl/certs` covers non-Nix-aware clients (curl,
        // openssl, etc.) that expect the system trust store.
        // Nix-built code uses `NIX_SSL_CERT_FILE` / `SSL_CERT_FILE`
        // (already in the env allowlist) to point into `/nix/store`,
        // which is bound separately.
        for etc_path in [
            "/etc/resolv.conf",
            "/etc/hosts",
            "/etc/nsswitch.conf",
            "/etc/ssl/certs",
        ] {
            c.arg("--ro-bind-try").arg(etc_path).arg(etc_path);
        }
    }

    for bind in &policy.ro_binds {
        c.arg("--ro-bind").arg(&bind.host).arg(&bind.sandbox);
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

    // Env: `--clearenv` wipes the inner process's inherited env,
    // then `--setenv` repopulates it. Setting `cmd.env(...)` on the
    // outer `Command` would only affect `bwrap` itself, not the
    // inner command — that was the trap the previous design fell
    // into (HOME was set on bwrap but stripped before the stage
    // ran, so `nix` crashed looking for a home directory).
    //
    // HOME / USER are always set to sandbox-consistent values
    // (/work + "nobody" matching the UID mapping). Other allowlist
    // entries inherit their value from the invoking process if set
    // there.
    c.arg("--setenv").arg("HOME").arg("/work");
    c.arg("--setenv").arg("USER").arg("nobody");
    for var in &policy.env_allowlist {
        if var == "HOME" || var == "USER" {
            continue;
        }
        if let Ok(v) = std::env::var(var) {
            c.arg("--setenv").arg(var).arg(v);
        }
    }

    c.arg("--").args(inner_cmd);
    c
}

/// Locate the `bwrap` binary.
///
/// Checks a fixed list of trusted system paths first, because they're
/// owned by root on every mainstream Linux distro and therefore can't
/// be planted by a non-privileged attacker. Only if none of those
/// exist does the search fall back to walking `$PATH` — at which
/// point a `tracing::warn!` fires (once per process) so operators can
/// notice that isolation is trusting an attacker-plantable lookup.
///
/// Returns `None` if `bwrap` is not installed anywhere we know to look.
pub fn find_bwrap() -> Option<PathBuf> {
    for trusted in TRUSTED_BWRAP_PATHS {
        let candidate = PathBuf::from(trusted);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    // Fallback: $PATH walk. Operators with a properly-provisioned
    // host should never hit this branch; if they do, either `bwrap`
    // was installed somewhere non-standard or the host's `$PATH` is
    // pointing at attacker-writable directories (user shell rc files,
    // container bind-mount mishaps, etc.).
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join("bwrap");
        if candidate.is_file() {
            if !PATH_FALLBACK_WARNED.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    resolved = %candidate.display(),
                    "bwrap resolved via $PATH — none of the trusted \
                     system paths contained it. If this host's PATH \
                     includes a user-writable directory, isolation can \
                     be trivially bypassed. Install bwrap to /usr/bin \
                     (distro package) or your system Nix profile."
                );
            }
            return Some(candidate);
        }
    }
    None
}

static PATH_FALLBACK_WARNED: AtomicBool = AtomicBool::new(false);

/// Root-owned locations where `bwrap` lives on a correctly-provisioned
/// Linux host. Order matters: NixOS system profile first (nix hosts
/// almost always have this), then the Determinate / single-user nix
/// profile, then distro-packaged `/usr/bin`, then manual installs.
///
/// A non-root attacker can't write to any of these on a standard
/// Linux system, so resolving through them short-circuits the
/// `$PATH` planting vector. Linux-only: bwrap doesn't run on macOS
/// or Windows, and typical macOS install paths (e.g. `/opt/homebrew`)
/// are owned by the installing admin user, not root, so including
/// them here would re-introduce the planting vector we're closing.
pub const TRUSTED_BWRAP_PATHS: &[&str] = &[
    "/run/current-system/sw/bin/bwrap",
    "/nix/var/nix/profiles/default/bin/bwrap",
    "/usr/bin/bwrap",
    "/usr/local/bin/bwrap",
];

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
        let bind = policy
            .ro_binds
            .iter()
            .find(|b| b.sandbox == Path::new("/nix/store"))
            .expect("nix store bind is missing");
        assert_eq!(bind.host, Path::new("/nix/store"));
        assert_eq!(bind.sandbox, Path::new("/nix/store"));
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
        assert!(!argv.contains(&"--share-net".to_string()));
        assert!(argv.contains(&"--dir".to_string()));
        assert!(argv.contains(&"/work".to_string()));
        let dash_dash_idx = argv
            .iter()
            .position(|a| a == "--")
            .expect("missing -- separator");
        assert_eq!(argv[dash_dash_idx + 1], "python3");
    }

    #[test]
    fn bwrap_command_uses_host_bind_when_work_host_set() {
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
    fn trusted_bwrap_paths_are_root_owned_on_linux() {
        for p in TRUSTED_BWRAP_PATHS {
            assert!(
                p.starts_with("/run/") || p.starts_with("/nix/var/") || p.starts_with("/usr/"),
                "TRUSTED_BWRAP_PATHS entry '{p}' is not conventionally \
                 root-owned on Linux; only /run /nix/var /usr prefixes \
                 are permitted"
            );
        }
    }

    #[test]
    fn effectiveness_predicate_matches_variant() {
        assert!(!IsolationBackend::None.is_effective());
        assert!(IsolationBackend::Bwrap {
            bwrap_path: PathBuf::from("/usr/bin/bwrap"),
        }
        .is_effective());
    }

    #[test]
    fn policy_round_trips_through_json() {
        // Policy crosses a process boundary for consumers like the
        // noether-sandbox binary (stdin JSON + argv). Pin the shape so
        // a future field reorder / rename on the wire is deliberate.
        let policy = IsolationPolicy::from_effects(&EffectSet::pure())
            .with_work_host(PathBuf::from("/tmp/work"));
        let json = serde_json::to_string(&policy).unwrap();
        let back: IsolationPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(back.network, policy.network);
        assert_eq!(back.work_host, policy.work_host);
        assert_eq!(back.ro_binds, policy.ro_binds);
        assert_eq!(back.env_allowlist, policy.env_allowlist);
    }
}
