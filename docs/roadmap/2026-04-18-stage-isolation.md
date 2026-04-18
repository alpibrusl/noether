# Stage execution isolation (M2.4 → v0.7 → v0.8)

**Status:** Draft · 2026-04-18
**Target:** v0.7.0 (Phase 1) and v0.8.0 (Phase 2)

---

## Why

`SECURITY.md` is clear: the Nix-pinned runtime is a **reproducibility**
boundary, not an **isolation** boundary. A user-authored Python stage
has host-user privileges — it can `rm -rf ~/.ssh`, call `curl
attacker.example`, and read `/etc/passwd`. Reproducibility gives you
"same inputs → same outputs". Isolation would give you "the stage
can't reach outside the declared work area."

The ask from alpibrusl:

> tasks can run safely in isolation without affecting other parts of
> the system and being as light as possible.

## What's "light" actually mean

A stage execution budget in Noether is dominated by the runtime
bring-up cost, not the sandbox wrapper:

| Cost layer | Typical time |
|------------|-------------:|
| Nix runtime cold-start | 100–500 ms |
| Nix runtime warm-start (cached) | 10–40 ms |
| bubblewrap wrap overhead | 30–80 ms |
| Native namespace + Landlock + seccomp | 3–8 ms |
| WASM module instantiation | < 1 ms |

Any sandbox whose overhead is below the Nix warm-start cost is
"free" in practice — you don't notice it unless you benchmark.

## Threat model

What a malicious or buggy stage might try, and what each layer
prevents:

| Attack | Host-user execution | Namespaces only | + Landlock | + Seccomp-bpf |
|--------|--------------------|-----------------|------------|---------------|
| Read `~/.ssh/*` | allowed | blocked (rootfs pivot) | blocked | blocked |
| Write outside `/work` | allowed | blocked (ro bind) | blocked | blocked |
| `curl` to arbitrary URL | allowed | blocked (no netns) | blocked | blocked |
| `ptrace` the parent | allowed | allowed | allowed | **blocked** |
| Load BPF program | allowed | allowed | allowed | **blocked** |
| `kexec_load` a new kernel | allowed | allowed | allowed | **blocked** |
| Subvert `/nix/store` | allowed | blocked (ro) | blocked | blocked |
| Fork-bomb DOS | allowed | bounded by PID namespace | bounded | bounded |
| Exhaust memory | allowed | allowed | allowed | bounded via cgroup |

The layering is belt-and-braces: Landlock catches filesystem escapes
that a mount-namespace bug would let through; seccomp catches
syscall-based escapes that filesystem controls can't see.

## Design — Phase 1 (v0.7)

Ship **bubblewrap (bwrap) as the default isolation backend.** Not
because it's the lightest — the native namespaces+Landlock+seccomp
path is 10× lighter on startup — but because:

1. **Policy correctness is the hard part, not the mechanism.** If we
   get the allowlist wrong with a proven tool (bwrap — used by
   Flatpak in production for years), we find out fast. If we roll
   our own namespace syscalls in the first cut, debugging is harder.
2. **Startup cost is dwarfed by Nix.** Adding 50 ms to a 200 ms Nix
   bring-up is 25% — noticeable but not disqualifying. After we
   validate the policy surface, Phase 2 swaps mechanism for ~5 ms.
3. **Supply chain is simple.** `apt install bubblewrap`, `brew
   install bubblewrap`, or `nix profile install bubblewrap`. The
   binary is 60 KB. No dynamic linking surprises.

### `IsolationPolicy`

Derived from the stage's declared `EffectSet`:

```rust
pub struct IsolationPolicy {
    /// Read-only bind mounts (host_path, sandbox_path). Always
    /// includes `/nix/store` read-only so Nix runtimes resolve.
    pub ro_binds: Vec<(PathBuf, PathBuf)>,
    /// Read-write bind mount for the stage's working directory.
    pub work_bind: (PathBuf, PathBuf),
    /// Inherit the host network namespace (`true`) or create a
    /// fresh empty one (`false`). Defaults to `false`; `true` only
    /// when the stage declares `Effect::Network`.
    pub network: bool,
    /// Extra environment variables to pass through. Everything else
    /// is dropped.
    pub env_allowlist: Vec<String>,
    /// Capabilities to drop. Default: all.
    pub capability_drop: CapabilityDrop,
}

pub enum CapabilityDrop {
    All,       // drop every capability
    Keep(Vec<Capability>),  // explicit allowlist
}

impl IsolationPolicy {
    pub fn from_effects(effects: &EffectSet) -> Self {
        Self {
            ro_binds: vec![
                ("/nix/store".into(), "/nix/store".into()),
            ],
            work_bind: (make_per_run_tmpdir(), "/work".into()),
            network: effects.contains(&Effect::Network),
            env_allowlist: vec![
                "PATH".into(), "HOME".into(), "NIX_PATH".into(),
                "NOETHER_LOG_LEVEL".into(),
            ],
            capability_drop: CapabilityDrop::All,
        }
    }
}
```

### `IsolatedNixExecutor`

Decorator around `NixExecutor`:

```rust
pub struct IsolatedNixExecutor {
    inner: NixExecutor,
    backend: IsolationBackend,
}

pub enum IsolationBackend {
    None,          // legacy pass-through, emits a warning
    Bwrap(PathBuf),     // path to bubblewrap binary
    // v0.8 adds: Native (namespaces + landlock + seccomp)
}

impl StageExecutor for IsolatedNixExecutor {
    fn execute(&self, stage_id: &StageId, input: &Value)
        -> Result<Value, ExecutionError>
    {
        let stage = self.inner.resolve_stage(stage_id)?;
        let policy = IsolationPolicy::from_effects(&stage.signature.effects);
        match &self.backend {
            IsolationBackend::None => {
                warn_once_about_disabled_isolation();
                self.inner.execute(stage_id, input)
            }
            IsolationBackend::Bwrap(path) => {
                run_under_bwrap(path, &policy, &self.inner, stage_id, input)
            }
        }
    }
}
```

### CLI surface

```
noether run --isolate=<auto|bwrap|none> graph.json
```

- `auto` (default from v0.8 onward): pick the best available backend.
  In v0.7 `auto` means "bwrap if available, else warn and run `none`".
- `bwrap`: require bubblewrap; exit 1 if not in `PATH`.
- `none`: explicitly opt-out (current behavior). Emit a loud warning
  unless `--unsafe-no-isolation` is also passed — force the user to
  be explicit about downgrading.

Env-var fallback: `NOETHER_ISOLATION=<auto|bwrap|none>`.

### What Phase 1 does NOT do

- Native namespace path (deferred to Phase 2).
- Landlock (deferred to Phase 2).
- Seccomp (deferred to Phase 2; bwrap does provide a default
  seccomp profile, which is a start).
- cgroup-based resource limits (memory caps, fork-bomb protection).
  Tracked as a follow-up; not in the initial 2-week scope.
- macOS and Windows. bwrap is Linux-only. On other platforms
  `--isolate=auto` falls back to `none` with a platform-warning.
  Native sandbox primitives (`sandbox-exec` on macOS, AppContainer
  on Windows) are explicit v0.9+ work.

## Design — Phase 2 (v0.8)

Swap bwrap for **native Linux namespaces + Landlock + seccomp**,
callable from Rust with no external binary:

- Crates: `landlock` (0.4+), `seccompiler` (for syscall filtering),
  either direct `unshare` syscalls or the `caps`+`nix` crate combo.
- Same `IsolationPolicy` surface — the `IsolationBackend::Native`
  variant replaces bwrap invocation with a `fork` + `unshare` +
  `pivot_root` + `landlock_restrict_self` + `seccomp_load` +
  `execve` sequence.
- Startup drops from ~50 ms (bwrap) to ~5 ms (direct syscalls).
- v0.8 makes `Native` the `auto` default on Linux kernels ≥ 5.13.

Zero user-facing changes beyond faster execution. `--isolate=bwrap`
stays as a compatibility flag.

## Design — Phase 3 (v0.9+ or later)

macOS (`sandbox-exec`) and Windows (`AppContainer`) backends, picked
automatically via `IsolationBackend::auto_for_platform()`. Each has
different primitives but the same `IsolationPolicy` abstraction.

## Relationship to WASM executor (Path B)

Stage isolation (this doc) and the WASM executor (see
`2026-04-18-property-dsl-expansion.md` references) are **orthogonal**.

- Isolation wraps `NixExecutor` — stages still run as Nix-pinned OS
  processes, the sandbox just restricts what they can touch.
- WASM would be a different `StageExecutor` where stages are
  in-process WASM modules with capability-gated host functions.
  Inherently isolated by the WASM model.

Both can coexist. A stage author picks the executor at stage-add
time; the `--isolate` flag only affects the Nix path. WASM stages
always run in the wasmtime sandbox regardless of the `--isolate`
flag.

## Grid-mode compatibility

The isolation wrapper is per-executor, so every grid hop that runs
Nix stages — local broker execution and remote worker execution —
gets isolated independently. No grid-protocol changes needed for
Phase 1.

Phase 2's native-namespaces path has the same story: each hop runs
its local NixExecutor wrapped in `IsolationBackend::Native`. Workers
advertise isolation capability alongside their LLM models so the
broker can refuse to dispatch to an un-isolated worker if the caller
requested isolation. (Small protocol addition; not in the Phase 1
scope.)

## STABILITY.md additions

Phase 1 ships with these promises added to `STABILITY.md`:

1. `--isolate` and `NOETHER_ISOLATION` are stable CLI/env surface.
   Variant names (`auto`, `bwrap`, `none`, eventually `native`) are
   frozen.
2. `IsolationPolicy::from_effects` derivation is stable — a stage
   with a given `EffectSet` gets the same sandbox shape across 1.x.
3. The set of env vars passed through to the sandboxed process is
   additive-only within 1.x. Removing an allowlisted env is a
   breaking change.

## Test strategy

1. **Negative tests** — each a synthesized stage that *tries* to
   escape, paired with an assertion that it fails:
   - Filesystem read outside `/work` → `ExecutionError` or empty result.
   - Filesystem write outside `/work` → `ExecutionError`.
   - Network call when `Effect::Network` not declared → `ExecutionError`.
   - `ptrace(2)` on the parent → blocked by seccomp (Phase 2+).
2. **Positive tests** — legitimate stages continue to work:
   - Nix stdlib stage (e.g. `text_length`) runs under `--isolate=bwrap`.
   - Network stage with `Effect::Network` declared succeeds at HTTP.
   - Filesystem stage with declared `FsRead` on `/data` succeeds.
3. **Regression test** — graph of 5 stages runs end-to-end with
   `--isolate=bwrap`, benchmark output captured.

## Open decisions before writing code

1. **Feature flag or always-on?** I lean *always compile in*, default
   `--isolate=auto`. On systems without bwrap, `auto` falls back to
   `none` with a warning. This keeps one binary on crates.io.
2. **Capability drop default: all or minimal?** I lean *all*.
   `Effect::Network` doesn't need any capability today (it's about
   netns, not `CAP_NET_RAW`). Any stage that needs a capability
   should have to declare it explicitly; none in the current stdlib
   does.
3. **Work-dir lifecycle.** Per-invocation tmpdir (cleaned on exit) or
   per-composition (reused across stages)? I lean *per-invocation*
   with an escape hatch for stages that need to pass files between
   each other via the host filesystem. Most cross-stage data flows
   through JSON over the stage pipeline anyway.
