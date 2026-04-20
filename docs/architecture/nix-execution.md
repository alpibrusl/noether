# Nix Execution Layer

Nix provides a reproducible, pinned runtime for Python, JavaScript, and bash
stages. It is Noether's L1 — the layer below the stage store.

!!! info "Reproducibility and isolation are separate boundaries (v0.7+)"
    Nix pins the runtime's binaries and libraries so you always get
    the same Python, NumPy, everything — that's the **reproducibility**
    boundary. The **isolation** boundary is separate: from v0.7,
    `noether run --isolate=auto` (the default) wraps every stage
    subprocess in bubblewrap with UID mapped to `nobody`, `--cap-drop
    ALL`, sandbox-private `/work` tmpfs, and network unshared unless
    the stage declares `Effect::Network`. See
    [`noether-isolation`](https://github.com/alpibrusl/noether/tree/main/crates/noether-isolation)
    for the crate, [`SECURITY.md`](https://github.com/alpibrusl/noether/blob/main/SECURITY.md)
    for the threat model, and
    [`docs/roadmap/2026-04-18-stage-isolation.md`](../roadmap/2026-04-18-stage-isolation.md)
    for Phase-2 (native namespaces + Landlock + seccomp, v0.8).

    Opt out with `--isolate=none --unsafe-no-isolation`. In CI, pass
    `--require-isolation` (or `NOETHER_REQUIRE_ISOLATION=1`) to turn
    the `auto → none` fallback into a hard error when bwrap is missing.

    Distro-packaged `nix` at `/usr/bin/nix` can't run under isolation
    (dynamically linked against host libs that aren't bound) — the
    executor refuses cleanly. Install via Determinate / upstream so
    `nix` lives in `/nix/store`.

---

## Why Nix

A stage is identified by its content hash. For that guarantee to mean
anything, the runtime must also be reproducible: the same Python stage on
two machines must produce the same output from the same input.

Nix gives us that via:

- **Content-addressed derivations** — every package is identified by the
  hash of its build recipe. Two Nix derivations with the same hash produce
  bit-for-bit identical outputs.
- **No shared mutable state** — packages live in `/nix/store/<hash>-<name>`,
  isolated from system libraries.
- **Hermetic builds** — Nix evaluation is hermetic at *build time*: network
  access is blocked, all inputs are declared explicitly. (This is distinct
  from the subprocess's network access at *run time*, which is unrestricted.)

---

## How `NixExecutor` works

```rust
// crates/noether-engine/src/executor/nix.rs
pub struct NixConfig {
    pub timeout_secs: u64,       // wall-clock limit; process killed with SIGKILL on expiry
    pub max_output_bytes: usize, // stdout truncated to this length
    pub max_stderr_bytes: usize, // stderr truncated to this length
}

pub struct NixExecutor {
    store: Box<dyn StageStore>,
    nix_bin: PathBuf,
    config: NixConfig,
}
```

Execution flow per stage invocation:

1. Look up stage implementation (Python / Bash code string) in the store
2. Write to a temp file in `/tmp/noether-<uuid>/`
3. Build a minimal Nix shell (`nix-shell --pure`) with the required runtime packages
4. Spawn the process; pass `input.json` on stdin
5. A background thread waits for the child; the main thread calls `recv_timeout(config.timeout_secs)`
6. On timeout: `kill -9 <pid>`, return `ExecutionError::TimedOut`
7. On success: parse stdout as JSON; classify stderr for useful error messages

The Python wrapper receives the stage input as JSON on stdin and must write its
output as JSON to stdout. Stderr is captured and used for error classification.

---

## Error classification

`NixExecutor` inspects stderr to distinguish infrastructure failures from user code failures:

| Stderr pattern | Classification |
|---|---|
| `nix-daemon` / `nix daemon` | Nix infrastructure error — daemon not running |
| `flake.nix` | Nix infrastructure error — flake configuration |
| `No space left` | Disk space exhausted |
| `command not found` | Missing binary in environment |
| *(anything else)* | User code error — stage implementation bug |

This classification is included in the `ExecutionError::StageFailed` message so
agents and users can distinguish "Nix is broken" from "the Python code crashed".

---

## Warmup

Cold Nix environments fetch Python/Node from `cache.nixos.org`, which takes 1-3 s.
`NixExecutor::warmup()` pre-fetches the Python 3 runtime in a background thread at
startup, so the first real stage invocation sees a warm cache:

```rust
NixExecutor::warmup(); // call once at CLI startup; returns immediately
```

The warmup runs a no-op `nix-shell` in the background and does not block the CLI.

---

## Stage implementation languages

| Language | Executor | Isolation | Startup (warm) |
|---|---|---|---|
| Rust (inline) | `InlineExecutor` | In-process | ~0 ms |
| Python | `NixExecutor` | Nix sandbox | ~200 ms |
| JavaScript | `NixExecutor` | Nix sandbox | ~150 ms |
| Bash | `NixExecutor` | Nix sandbox | ~50 ms |

The stdlib uses `InlineExecutor` for all Pure Rust stages (zero overhead).
`NixExecutor` is used for stages that need Python libraries (numpy, pandas, etc.).

---

## Binary cache

Nix packages are fetched from `cache.nixos.org` on first use and cached in
`/nix/store`. Subsequent runs of the same stage use the cache — startup overhead
drops from ~2 s (cold fetch) to ~200 ms (warm).

In CI and production, a team can run a private Nix binary cache to share built
derivations across machines.

---

## Running the tests

```bash
# Unit tests (no Nix required)
cargo test -p noether-engine

# Integration tests (require Nix in PATH, warm cache recommended)
cargo test -p noether-engine -- --ignored
```

Integration tests are marked `#[ignore]` because they require downloading the
Python 3 runtime on first run (~2 s cold) and would time out in CI.

---

## Phase history

| Phase | Nix feature | Status |
|---|---|---|
| 2 | `NixExecutor` — spawn subprocess, pass JSON over stdio | ✅ Done |
| 3 | `InlineExecutor` for Pure Rust stages (zero overhead) | ✅ Done |
| 6 | `NixConfig` (timeout, output limits), error classification, `warmup()` | ✅ Done |
