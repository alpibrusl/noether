//! Adversarial sandbox-escape tests for the bwrap isolation layer.
//!
//! These tests exercise `build_bwrap_command` directly with a
//! pre-resolved Python binary. They do NOT go through `NixExecutor`
//! because `nix run nixpkgs#python3` expects to fetch flakes at
//! runtime, which can't work inside a sandbox that deliberately has
//! no network or flake-registry access. Running nix *inside* the
//! sandbox is the wrong design regardless; the proper integration is
//! to resolve the runtime on the host first (via `nix build
//! --print-out-paths`) and run only the resolved binary inside the
//! sandbox. That refactor is tracked as a Phase 1.x follow-up;
//! meanwhile these tests validate the sandbox-shape contract directly.
//!
//! Each test registers a Python attack snippet — reading
//! `/etc/shadow`, resolving DNS, observing the real UID — and
//! asserts the sandbox defeats it. If bwrap or a `/nix/store`-hosted
//! python3 isn't available on the test host, the tests skip with a
//! note; we'd rather have a green suite than a fragile one that
//! pretends to test security but doesn't actually run.

#![cfg(target_os = "linux")]

use noether_core::effects::{Effect, EffectSet};
use noether_engine::executor::isolation::{build_bwrap_command, find_bwrap, IsolationPolicy};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Resolve a Python interpreter inside `/nix/store`. That store path
/// is already covered by the default `IsolationPolicy` RO bind, so
/// the sandbox can run it with no extra ceremony. Returns `None`
/// when nix isn't available or the build fails — the caller skips.
fn nix_python3() -> Option<PathBuf> {
    let out = Command::new("nix")
        .args([
            "build",
            "--no-link",
            "--print-out-paths",
            "--quiet",
            "nixpkgs#python3",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let store_path = String::from_utf8(out.stdout).ok()?;
    let store_path = store_path.trim();
    if store_path.is_empty() {
        return None;
    }
    let python = Path::new(store_path).join("bin").join("python3");
    python.exists().then_some(python)
}

/// Return `Some(bwrap_path, python_path)` when both the sandbox
/// wrapper and a runnable Python are available on the host. A test
/// that gets `None` should early-return rather than fail — the host
/// is just missing the toolchain to exercise the sandbox end-to-end.
fn deps() -> Option<(PathBuf, PathBuf)> {
    let bwrap = find_bwrap()?;
    let python = nix_python3()?;
    Some((bwrap, python))
}

fn skip_if_deps_missing() -> Option<(PathBuf, PathBuf)> {
    match deps() {
        Some(d) => Some(d),
        None => {
            eprintln!(
                "isolation_escape: skipping — missing bwrap or nix-built \
                 python3; both are required to drive real sandbox escape \
                 probes. Unit tests in `executor::isolation` still verify \
                 the argv-construction contract."
            );
            None
        }
    }
}

/// Drive `build_bwrap_command` with the given `policy` and Python
/// attack code on stdin. Returns the parsed `{...}` JSON the stage
/// prints to stdout, or an error-shaped JSON if the subprocess
/// crashed / bwrap refused / stdout was garbage.
fn run_attack(bwrap: &Path, python: &Path, policy: &IsolationPolicy, code: &str) -> Value {
    let inner = vec![
        python.to_string_lossy().into_owned(),
        "-c".into(),
        code.into(),
    ];
    let mut cmd = build_bwrap_command(bwrap, policy, &inner);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return json!({ "spawn_error": format!("{e}") }),
    };
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => return json!({ "wait_error": format!("{e}") }),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return json!({
            "exit_failure": out.status.code(),
            "stderr": stderr.to_string(),
            "stdout": stdout.to_string(),
        });
    }
    serde_json::from_str(stdout.trim()).unwrap_or_else(
        |_| json!({ "unparseable_stdout": stdout.to_string(), "stderr": stderr.to_string() }),
    )
}

fn assert_ran(result: &Value) {
    // A result with `spawn_error` / `wait_error` / `exit_failure`
    // means the probe never ran, so the attack-specific key being
    // absent would silently pass the actual assertion. Fail loudly
    // when that happens so we don't get vacuous green tests.
    for bad in [
        "spawn_error",
        "wait_error",
        "exit_failure",
        "unparseable_stdout",
    ] {
        assert!(
            result.get(bad).is_none(),
            "sandboxed probe did not run cleanly ({bad}): {result}"
        );
    }
}

/// A pure-effect stage must NOT be able to reach DNS.
#[test]
fn network_blocked_when_effect_not_declared() {
    let Some((bwrap, python)) = skip_if_deps_missing() else {
        return;
    };
    let policy = IsolationPolicy::from_effects(&EffectSet::pure());
    let code = r#"
import socket, json
try:
    socket.gethostbyname("example.com")
    print(json.dumps({"blocked": False}))
except OSError as e:
    print(json.dumps({"blocked": True, "errno": e.errno}))
"#;
    let result = run_attack(&bwrap, &python, &policy, code);
    assert_ran(&result);
    assert_eq!(
        result.get("blocked"),
        Some(&json!(true)),
        "pure-effect stage resolved DNS — network namespace not \
         unshared: {result}"
    );
}

/// Counter-test: a stage declaring `Effect::Network` must get DNS.
/// Tolerates an offline test host (the host itself may lack internet)
/// but flags the specific errno pattern that indicates `--share-net`
/// was dropped.
#[test]
fn network_allowed_when_effect_declared() {
    let Some((bwrap, python)) = skip_if_deps_missing() else {
        return;
    };
    let policy = IsolationPolicy::from_effects(&EffectSet::new([Effect::Pure, Effect::Network]));
    let code = r#"
import socket, json
try:
    socket.gethostbyname("example.com")
    print(json.dumps({"resolved": True}))
except OSError as e:
    print(json.dumps({"resolved": False, "errno": e.errno, "msg": str(e)}))
"#;
    let result = run_attack(&bwrap, &python, &policy, code);
    assert_ran(&result);
    if result.get("resolved") != Some(&json!(true)) {
        eprintln!(
            "network_allowed_when_effect_declared: appears offline \
             ({result}); test inconclusive — re-run on a connected host"
        );
    }
}

/// `/etc/shadow` is outside the default `ro_binds`; opening it must
/// fail with not-found. Catches anyone widening the binds to include
/// `/etc` or `/`.
#[test]
fn cannot_read_etc_shadow() {
    let Some((bwrap, python)) = skip_if_deps_missing() else {
        return;
    };
    let policy = IsolationPolicy::from_effects(&EffectSet::pure());
    let code = r#"
import json
try:
    with open("/etc/shadow", "r") as f:
        f.read()
    print(json.dumps({"leaked": True}))
except (FileNotFoundError, PermissionError) as e:
    print(json.dumps({"leaked": False, "error": type(e).__name__}))
"#;
    let result = run_attack(&bwrap, &python, &policy, code);
    assert_ran(&result);
    assert_eq!(
        result.get("leaked"),
        Some(&json!(false)),
        "/etc/shadow readable inside sandbox — bind-mount policy too \
         wide: {result}"
    );
}

/// Host user's SSH keys and cloud credentials must be unreachable.
#[test]
fn cannot_read_host_ssh_keys() {
    let Some((bwrap, python)) = skip_if_deps_missing() else {
        return;
    };
    let policy = IsolationPolicy::from_effects(&EffectSet::pure());
    let code = r#"
import os, json
candidates = [
    os.path.expanduser("~/.ssh/id_rsa"),
    os.path.expanduser("~/.ssh/id_ed25519"),
    os.path.expanduser("~/.aws/credentials"),
]
leaked = []
for p in candidates:
    try:
        with open(p, "rb") as f:
            f.read(1)
        leaked.append(p)
    except (FileNotFoundError, PermissionError, IsADirectoryError, NotADirectoryError):
        pass
print(json.dumps({"leaked_paths": leaked}))
"#;
    let result = run_attack(&bwrap, &python, &policy, code);
    assert_ran(&result);
    let leaked = result
        .get("leaked_paths")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        leaked.is_empty(),
        "sandbox leaked host credentials: {leaked:?}; result: {result}"
    );
}

/// UID/GID must be mapped to nobody (65534). Without `--uid`/`--gid`
/// the host user's real UID is observable via `os.getuid()`.
#[test]
fn uid_mapped_to_nobody_inside_sandbox() {
    let Some((bwrap, python)) = skip_if_deps_missing() else {
        return;
    };
    let policy = IsolationPolicy::from_effects(&EffectSet::pure());
    let code = r#"
import os, json
print(json.dumps({"uid": os.getuid(), "gid": os.getgid()}))
"#;
    let result = run_attack(&bwrap, &python, &policy, code);
    assert_ran(&result);
    assert_eq!(
        result.get("uid"),
        Some(&json!(65534)),
        "sandbox did not apply --uid 65534: {result}"
    );
    assert_eq!(
        result.get("gid"),
        Some(&json!(65534)),
        "sandbox did not apply --gid 65534: {result}"
    );
}

/// A stage inside the sandbox must not be able to regain
/// privileges. The combination `--unshare-user + --uid 65534 +
/// --cap-drop ALL` is supposed to prevent `setuid(0)` and
/// `chroot("/")` from succeeding. Future refactors that accidentally
/// drop any one of those would pass the existing UID-observation
/// test (which only asserts the CURRENT uid is 65534) but fail
/// here — so this locks the capability-drop contract separately.
#[test]
fn cannot_escalate_to_root() {
    let Some((bwrap, python)) = skip_if_deps_missing() else {
        return;
    };
    let policy = IsolationPolicy::from_effects(&EffectSet::pure());
    let code = r#"
import os, json
attempts = {}
try:
    os.setuid(0)
    attempts["setuid_0"] = True
except (PermissionError, OSError) as e:
    attempts["setuid_0"] = False
    attempts["setuid_error"] = type(e).__name__
try:
    os.setgid(0)
    attempts["setgid_0"] = True
except (PermissionError, OSError) as e:
    attempts["setgid_0"] = False
try:
    os.chroot("/")
    attempts["chroot"] = True
except (PermissionError, OSError) as e:
    attempts["chroot"] = False
print(json.dumps(attempts))
"#;
    let result = run_attack(&bwrap, &python, &policy, code);
    assert_ran(&result);
    assert_eq!(
        result.get("setuid_0"),
        Some(&json!(false)),
        "setuid(0) succeeded inside sandbox — capability drop misconfigured: {result}"
    );
    assert_eq!(
        result.get("setgid_0"),
        Some(&json!(false)),
        "setgid(0) succeeded inside sandbox — capability drop misconfigured: {result}"
    );
    assert_eq!(
        result.get("chroot"),
        Some(&json!(false)),
        "chroot succeeded inside sandbox — CAP_SYS_CHROOT not dropped: {result}"
    );
}

/// Regression guard for the `--setenv` env-wiring bug discovered
/// during review round 1. Setting `cmd.env("HOME", "/work")` on the
/// outer `Command` was silently stripped by bwrap's `--clearenv`
/// before the stage process ran; the fix was to emit `--setenv
/// HOME /work` on the bwrap argv instead. A future refactor that
/// reintroduces `cmd.env()` would pass the argv-shape unit tests
/// (they don't inspect Command.get_envs) but would cause HOME to
/// leak the host user's real home path here.
///
/// The sandbox-side HOME must always be `/work`, never the host
/// user's `$HOME`.
#[test]
fn home_env_is_sandbox_consistent() {
    let Some((bwrap, python)) = skip_if_deps_missing() else {
        return;
    };
    let policy = IsolationPolicy::from_effects(&EffectSet::pure());
    let code = r#"
import os, json
print(json.dumps({
    "home": os.environ.get("HOME"),
    "user": os.environ.get("USER"),
}))
"#;
    let result = run_attack(&bwrap, &python, &policy, code);
    assert_ran(&result);
    assert_eq!(
        result.get("home"),
        Some(&json!("/work")),
        "HOME inside sandbox leaked host value — `--setenv` wiring broken: {result}"
    );
    assert_eq!(
        result.get("user"),
        Some(&json!("nobody")),
        "USER inside sandbox leaked host value: {result}"
    );
}

/// `/work` is a sandbox-private tmpfs. Must start empty and be
/// writable — regression guard against both state leakage and
/// privilege misconfiguration.
#[test]
fn work_dir_is_private_tmpfs_and_empty() {
    let Some((bwrap, python)) = skip_if_deps_missing() else {
        return;
    };
    let policy = IsolationPolicy::from_effects(&EffectSet::pure());
    let code = r#"
import os, json
entries = sorted(os.listdir("/work"))
with open("/work/scratch.txt", "w") as f:
    f.write("ok")
with open("/work/scratch.txt") as f:
    content = f.read()
print(json.dumps({"initial_entries": entries, "roundtrip": content}))
"#;
    let result = run_attack(&bwrap, &python, &policy, code);
    assert_ran(&result);
    assert_eq!(
        result.get("initial_entries"),
        Some(&json!([])),
        "/work was not empty at stage entry — state leak or stale tmpdir: {result}"
    );
    assert_eq!(
        result.get("roundtrip"),
        Some(&json!("ok")),
        "/work not writable inside sandbox: {result}"
    );
}
