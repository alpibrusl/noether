//! `noether-sandbox` — run an arbitrary command inside the noether
//! bubblewrap sandbox, driven by an `IsolationPolicy` read from stdin.
//!
//! ## Why this exists
//!
//! The Rust [`noether_isolation`] crate is the library consumers of
//! noether reach for. Non-Rust callers (Python — agentspec, Node,
//! Go, shell scripts) don't want to embed a Rust toolchain; they want
//! a single binary they can `exec` with a policy on stdin.
//!
//! This binary is ~30 LOC of glue: parse JSON, hand it to
//! [`noether_isolation::build_bwrap_command`], spawn the result,
//! propagate the child's exit code.
//!
//! ## Protocol
//!
//! stdin: one JSON object matching [`IsolationPolicy`]. Example:
//!
//! ```json
//! {
//!   "ro_binds": [["/nix/store", "/nix/store"]],
//!   "network": true,
//!   "env_allowlist": ["PATH", "LANG", "RUST_LOG"]
//! }
//! ```
//!
//! argv: `noether-sandbox [--isolate=auto|bwrap|none] -- <cmd> [args...]`.
//! The first arg after `--` is the binary to run inside the sandbox;
//! the rest are its arguments.
//!
//! ## Behaviour
//!
//! - `--isolate=auto` (default) / `NOETHER_ISOLATION=auto` — use bwrap
//!   if available; fall back to running the command unsandboxed with
//!   a warning on stderr.
//! - `--isolate=bwrap` — require bwrap; exit 2 with an error if
//!   unavailable.
//! - `--isolate=none` — run the command directly. For parity with
//!   noether's CLI; a standalone `noether-sandbox` caller that
//!   selects `none` is making an explicit choice.
//!
//! Exit code mirrors the child's. `127` for "binary not found in
//! sandbox" (a bwrap misconfiguration); `2` for argument errors;
//! `128 + signum` when the child dies from a signal (Unix convention
//! so shell automation can detect SIGTERM / SIGKILL / SIGSEGV).

use noether_isolation::{IsolationBackend, IsolationPolicy};
use std::fs;
use std::io::{self, Read};
use std::process::{exit, Command, Stdio};

/// Upper bound on the policy JSON we'll buffer from stdin. Policies
/// are small — a few hundred bytes for a typical pure-effect case,
/// a few KB if a caller enumerates many `ro_binds`. Capping at 1 MiB
/// prevents an accidental `cat huge.bin | noether-sandbox` from
/// eating all memory. Not a security boundary (the caller already
/// has execve rights on this binary); just a sanity guardrail.
const STDIN_POLICY_MAX_BYTES: u64 = 1 << 20;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let parsed = match parse_args(&args) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("noether-sandbox: {msg}");
            eprintln!(
                "usage: noether-sandbox [--isolate=auto|bwrap|none] \
                 [--policy-file <path>] [--require-isolation] -- <cmd> [args...]"
            );
            exit(2);
        }
    };

    // Load the policy. Precedence: --policy-file > stdin > default.
    // --policy-file leaves stdin free to pass through to the child
    // (addresses the "child reading stdin sees EOF" foot-gun flagged
    // in the #37 review).
    let policy: IsolationPolicy = match parsed.policy_source {
        PolicySource::File(path) => match fs::read_to_string(&path) {
            Ok(s) if s.trim().is_empty() => default_policy(),
            Ok(s) => match serde_json::from_str(&s) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("noether-sandbox: invalid policy JSON in {path}: {e}");
                    exit(2);
                }
            },
            Err(e) => {
                eprintln!("noether-sandbox: failed to read policy file {path}: {e}");
                exit(2);
            }
        },
        PolicySource::Stdin => {
            let mut buf = String::new();
            let mut reader = io::stdin().lock().take(STDIN_POLICY_MAX_BYTES + 1);
            if let Err(e) = reader.read_to_string(&mut buf) {
                eprintln!("noether-sandbox: failed to read policy from stdin: {e}");
                exit(2);
            }
            if buf.len() as u64 > STDIN_POLICY_MAX_BYTES {
                eprintln!(
                    "noether-sandbox: policy JSON on stdin exceeds {STDIN_POLICY_MAX_BYTES} \
                     bytes — pass --policy-file for larger policies or trim the input"
                );
                exit(2);
            }
            if buf.trim().is_empty() {
                default_policy()
            } else {
                match serde_json::from_str(&buf) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("noether-sandbox: invalid policy JSON on stdin: {e}");
                        exit(2);
                    }
                }
            }
        }
    };

    let (backend, warning) = match IsolationBackend::from_flag(&parsed.isolate_flag) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("noether-sandbox: {e}");
            exit(2);
        }
    };

    // Fail-closed gate: with `--require-isolation` or
    // `NOETHER_REQUIRE_ISOLATION=1`, a resolved `None` backend is a
    // hard error, not a silent fall-through. Mirrors the
    // `noether run --require-isolation` shape so CI can impose one
    // consistent policy across both entry points.
    if parsed.require_isolation && matches!(backend, IsolationBackend::None) {
        let reason = warning
            .as_deref()
            .unwrap_or("--isolate=none explicitly selected while --require-isolation is in effect");
        eprintln!("noether-sandbox: refusing to run without isolation: {reason}");
        exit(2);
    }

    if let Some(msg) = warning {
        eprintln!("noether-sandbox: warning: {msg}");
    }

    let mut cmd = match &backend {
        IsolationBackend::Bwrap { bwrap_path } => {
            noether_isolation::build_bwrap_command(bwrap_path, &policy, &parsed.inner_cmd)
        }
        IsolationBackend::None => {
            let mut c = Command::new(&parsed.inner_cmd[0]);
            c.args(&parsed.inner_cmd[1..]);
            c
        }
    };
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    match cmd.status() {
        Ok(status) => exit(exit_code_from_status(&status)),
        Err(e) => {
            eprintln!("noether-sandbox: failed to spawn command: {e}");
            exit(127);
        }
    }
}

fn default_policy() -> IsolationPolicy {
    IsolationPolicy::from_effects(&noether_core::effects::EffectSet::pure())
}

/// Map `ExitStatus` to a shell-compatible exit code.
///
/// - Normal exit → child's exit code.
/// - Killed by signal → `128 + signum` (Unix shell convention; bash
///   and zsh both use this so automation that greps for
///   `$? == 143` on SIGTERM keeps working).
/// - Neither available → `1`.
fn exit_code_from_status(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signum) = status.signal() {
            return 128 + signum;
        }
    }
    1
}

/// Source the policy is read from.
enum PolicySource {
    Stdin,
    File(String),
}

/// Result of parsing argv.
struct ParsedArgs {
    isolate_flag: String,
    inner_cmd: Vec<String>,
    policy_source: PolicySource,
    require_isolation: bool,
}

/// Parse argv into a [`ParsedArgs`].
///
/// Recognised flags (all before the `--` separator):
/// - `--isolate=<value>` / `--isolate <value>` — backend selector.
///   Also read from `NOETHER_ISOLATION` env var.
/// - `--policy-file <path>` — read the policy from a file instead
///   of stdin. Leaves stdin free for the child process.
/// - `--require-isolation` — turn `auto → none` fallback into a
///   hard error. Also read from `NOETHER_REQUIRE_ISOLATION=1` env.
///
/// Everything after `--` is the inner command. Missing separator,
/// empty inner command, or an unknown flag is an error.
fn parse_args(args: &[String]) -> Result<ParsedArgs, String> {
    let mut isolate = std::env::var("NOETHER_ISOLATION").unwrap_or_else(|_| "auto".into());
    let mut policy_source = PolicySource::Stdin;
    let mut require_isolation = std::env::var("NOETHER_REQUIRE_ISOLATION")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false);
    let mut i = 1; // skip argv[0]
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            let inner: Vec<String> = args[i + 1..].to_vec();
            if inner.is_empty() {
                return Err("empty inner command after `--`".into());
            }
            return Ok(ParsedArgs {
                isolate_flag: isolate,
                inner_cmd: inner,
                policy_source,
                require_isolation,
            });
        }
        if let Some(v) = a.strip_prefix("--isolate=") {
            isolate = v.into();
            i += 1;
            continue;
        }
        if a == "--isolate" {
            let v = args
                .get(i + 1)
                .ok_or_else(|| "missing value for --isolate".to_string())?;
            isolate = v.clone();
            i += 2;
            continue;
        }
        if let Some(v) = a.strip_prefix("--policy-file=") {
            policy_source = PolicySource::File(v.into());
            i += 1;
            continue;
        }
        if a == "--policy-file" {
            let v = args
                .get(i + 1)
                .ok_or_else(|| "missing value for --policy-file".to_string())?;
            policy_source = PolicySource::File(v.clone());
            i += 2;
            continue;
        }
        if a == "--require-isolation" {
            require_isolation = true;
            i += 1;
            continue;
        }
        return Err(format!("unknown argument `{a}` before `--`"));
    }
    Err("missing `--` separator; usage: noether-sandbox [flags] -- <cmd> [args...]".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    /// Clear env vars that leak into parse_args defaults. Returns a
    /// guard restoring the previous values on drop. Tests that touch
    /// env vars must use this to avoid ordering dependencies.
    struct EnvGuard {
        isolation: Option<std::ffi::OsString>,
        require: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn new() -> Self {
            // SAFETY: single-threaded test scope. The guard restores
            // whatever was there before this test ran.
            let g = EnvGuard {
                isolation: std::env::var_os("NOETHER_ISOLATION"),
                require: std::env::var_os("NOETHER_REQUIRE_ISOLATION"),
            };
            unsafe {
                std::env::remove_var("NOETHER_ISOLATION");
                std::env::remove_var("NOETHER_REQUIRE_ISOLATION");
            }
            g
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match self.isolation.take() {
                    Some(v) => std::env::set_var("NOETHER_ISOLATION", v),
                    None => std::env::remove_var("NOETHER_ISOLATION"),
                }
                match self.require.take() {
                    Some(v) => std::env::set_var("NOETHER_REQUIRE_ISOLATION", v),
                    None => std::env::remove_var("NOETHER_REQUIRE_ISOLATION"),
                }
            }
        }
    }

    #[test]
    fn parses_default_flag_when_absent() {
        let _g = EnvGuard::new();
        let p = parse_args(&a(&["noether-sandbox", "--", "echo", "hi"])).unwrap();
        assert_eq!(p.isolate_flag, "auto");
        assert_eq!(p.inner_cmd, vec!["echo".to_string(), "hi".to_string()]);
        assert!(matches!(p.policy_source, PolicySource::Stdin));
        assert!(!p.require_isolation);
    }

    #[test]
    fn parses_isolate_equals() {
        let _g = EnvGuard::new();
        let p = parse_args(&a(&["noether-sandbox", "--isolate=bwrap", "--", "echo"])).unwrap();
        assert_eq!(p.isolate_flag, "bwrap");
        assert_eq!(p.inner_cmd, vec!["echo".to_string()]);
    }

    #[test]
    fn parses_isolate_space() {
        let _g = EnvGuard::new();
        let p = parse_args(&a(&["noether-sandbox", "--isolate", "none", "--", "echo"])).unwrap();
        assert_eq!(p.isolate_flag, "none");
    }

    #[test]
    fn parses_policy_file_flag() {
        // `--policy-file <path>` sets the policy source; stdin is
        // left free for the child process. Addresses the #37 review
        // point that a child reading stdin would see EOF when
        // noether-sandbox consumed stdin for the policy.
        let _g = EnvGuard::new();
        let p = parse_args(&a(&[
            "noether-sandbox",
            "--policy-file",
            "/tmp/p.json",
            "--",
            "cat",
        ]))
        .unwrap();
        match p.policy_source {
            PolicySource::File(path) => assert_eq!(path, "/tmp/p.json"),
            PolicySource::Stdin => panic!("expected file source, got stdin"),
        }
    }

    #[test]
    fn parses_policy_file_equals_form() {
        let _g = EnvGuard::new();
        let p = parse_args(&a(&[
            "noether-sandbox",
            "--policy-file=/tmp/p.json",
            "--",
            "cat",
        ]))
        .unwrap();
        assert!(matches!(p.policy_source, PolicySource::File(ref s) if s == "/tmp/p.json"));
    }

    #[test]
    fn parses_require_isolation_flag() {
        let _g = EnvGuard::new();
        let p = parse_args(&a(&[
            "noether-sandbox",
            "--require-isolation",
            "--",
            "echo",
        ]))
        .unwrap();
        assert!(p.require_isolation);
    }

    #[test]
    fn require_isolation_from_env() {
        let _g = EnvGuard::new();
        // SAFETY: single-threaded; EnvGuard will restore on drop.
        unsafe {
            std::env::set_var("NOETHER_REQUIRE_ISOLATION", "1");
        }
        let p = parse_args(&a(&["noether-sandbox", "--", "echo"])).unwrap();
        assert!(p.require_isolation);
    }

    #[test]
    fn rejects_missing_separator() {
        let _g = EnvGuard::new();
        assert!(parse_args(&a(&["noether-sandbox", "echo"])).is_err());
    }

    #[test]
    fn rejects_empty_inner() {
        let _g = EnvGuard::new();
        assert!(parse_args(&a(&["noether-sandbox", "--"])).is_err());
    }

    #[test]
    fn rejects_unknown_flag() {
        let _g = EnvGuard::new();
        assert!(parse_args(&a(&["noether-sandbox", "--unknown", "--", "echo"])).is_err());
    }
}
