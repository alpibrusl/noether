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
//! sandbox" (a bwrap misconfiguration); `2` for argument errors.

use noether_isolation::{IsolationBackend, IsolationPolicy};
use std::io::{self, Read};
use std::process::{exit, Command, Stdio};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (isolate_flag, inner_cmd) = match parse_args(&args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("noether-sandbox: {msg}");
            eprintln!("usage: noether-sandbox [--isolate=auto|bwrap|none] -- <cmd> [args...]");
            exit(2);
        }
    };

    // Read the policy from stdin. Empty stdin → use the default
    // pure-effect policy (no network, /nix/store RO bind, tmpfs
    // /work) rather than erroring; matches the "sensible defaults"
    // story of `IsolationPolicy::from_effects(&EffectSet::pure())`.
    let mut buf = String::new();
    if let Err(e) = io::stdin().read_to_string(&mut buf) {
        eprintln!("noether-sandbox: failed to read policy from stdin: {e}");
        exit(2);
    }
    let policy: IsolationPolicy = if buf.trim().is_empty() {
        IsolationPolicy::from_effects(&noether_core::effects::EffectSet::pure())
    } else {
        match serde_json::from_str(&buf) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("noether-sandbox: invalid policy JSON on stdin: {e}");
                exit(2);
            }
        }
    };

    let (backend, warning) = match IsolationBackend::from_flag(&isolate_flag) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("noether-sandbox: {e}");
            exit(2);
        }
    };
    if let Some(msg) = warning {
        eprintln!("noether-sandbox: warning: {msg}");
    }

    let mut cmd = match &backend {
        IsolationBackend::Bwrap { bwrap_path } => {
            noether_isolation::build_bwrap_command(bwrap_path, &policy, &inner_cmd)
        }
        IsolationBackend::None => {
            let mut c = Command::new(&inner_cmd[0]);
            c.args(&inner_cmd[1..]);
            c
        }
    };
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    match cmd.status() {
        Ok(status) => exit(status.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("noether-sandbox: failed to spawn command: {e}");
            exit(127);
        }
    }
}

/// Parse argv into `(isolate_flag, inner_cmd)`.
///
/// Recognises `--isolate=<value>` / `--isolate <value>` (or
/// `NOETHER_ISOLATION` env var) before `--`. Everything after `--`
/// becomes the inner command. Missing `--` separator, empty inner
/// command, or an unknown flag before `--` is an error.
fn parse_args(args: &[String]) -> Result<(String, Vec<String>), String> {
    let mut isolate = std::env::var("NOETHER_ISOLATION").unwrap_or_else(|_| "auto".into());
    let mut i = 1; // skip argv[0]
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            let inner = args[i + 1..].to_vec();
            if inner.is_empty() {
                return Err("empty inner command after `--`".into());
            }
            return Ok((isolate, inner));
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

    #[test]
    fn parses_default_flag_when_absent() {
        // SAFETY: single-threaded unit test; no other test reads this
        // var concurrently. The env override is restored after.
        let prev = std::env::var_os("NOETHER_ISOLATION");
        unsafe {
            std::env::remove_var("NOETHER_ISOLATION");
        }
        let (flag, inner) = parse_args(&a(&["noether-sandbox", "--", "echo", "hi"])).unwrap();
        assert_eq!(flag, "auto");
        assert_eq!(inner, vec!["echo".to_string(), "hi".to_string()]);
        if let Some(v) = prev {
            unsafe {
                std::env::set_var("NOETHER_ISOLATION", v);
            }
        }
    }

    #[test]
    fn parses_isolate_equals() {
        let (flag, inner) =
            parse_args(&a(&["noether-sandbox", "--isolate=bwrap", "--", "echo"])).unwrap();
        assert_eq!(flag, "bwrap");
        assert_eq!(inner, vec!["echo".to_string()]);
    }

    #[test]
    fn parses_isolate_space() {
        let (flag, _) =
            parse_args(&a(&["noether-sandbox", "--isolate", "none", "--", "echo"])).unwrap();
        assert_eq!(flag, "none");
    }

    #[test]
    fn rejects_missing_separator() {
        assert!(parse_args(&a(&["noether-sandbox", "echo"])).is_err());
    }

    #[test]
    fn rejects_empty_inner() {
        assert!(parse_args(&a(&["noether-sandbox", "--"])).is_err());
    }

    #[test]
    fn rejects_unknown_flag() {
        assert!(parse_args(&a(&["noether-sandbox", "--unknown", "--", "echo"])).is_err());
    }
}
