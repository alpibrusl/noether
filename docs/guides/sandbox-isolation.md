# Sandbox & Isolation

From v0.7 onwards, every Python / JavaScript / Bash stage subprocess runs inside a bubblewrap sandbox by default. This page covers the `--isolate` flag, what the sandbox actually guarantees, the caveats that bite in practice, and the Phase 2 roadmap.

## The one-liner

```bash
noether run graph.json              # --isolate=auto: bwrap if available, else unsandboxed with a warning
noether run graph.json --isolate=bwrap          # require bwrap; hard error if missing
noether run graph.json --isolate=none --unsafe-no-isolation  # explicit opt-out, silences the warning
noether run graph.json --require-isolation      # auto-to-none fallback becomes a hard error
```

`--isolate=auto` is the default. In CI or any environment where running a stage unsandboxed is never the right answer, set `NOETHER_REQUIRE_ISOLATION=1` (or pass `--require-isolation`).

## What the sandbox does

When `--isolate=bwrap` (or auto with bwrap found), the stage subprocess runs with:

- `--unshare-all` — fresh user / pid / uts / ipc / mount / cgroup / network namespaces.
- `--uid 65534 --gid 65534` — mapped to the conventional `nobody/nogroup` identity. The stage cannot observe the host user's real UID.
- `--die-with-parent` — if the engine dies, the sandbox dies with it.
- `--proc /proc`, `--dev /dev`, `--tmpfs /tmp` — minimal rootfs skeleton.
- `--ro-bind /nix/store /nix/store` — Nix-pinned runtimes resolve inside the sandbox.
- `--bind <work_host> /work` *or* `--dir /work` — a writable scratch. The default is a sandbox-private tmpfs; callers who need host visibility opt in via `IsolationPolicy::with_work_host`.
- `--clearenv` — the environment is wiped, then the executor re-adds the allowlisted variables.
- `--cap-drop ALL` — every capability dropped inside the sandbox.
- `--share-net` — only when the stage declares `Effect::Network`. When network is shared, `/etc/resolv.conf`, `/etc/hosts`, `/etc/nsswitch.conf`, and `/etc/ssl/certs` are `--ro-bind-try`'d so DNS and TLS still resolve.

Two binaries expose the sandbox:

- **`noether run`** — embeds the engine and applies the policy derived from each stage's `EffectSet`.
- **`noether-sandbox`** — standalone binary (v0.7.1+). Reads an `IsolationPolicy` as JSON on stdin (or `--policy-file <path>`), runs the argv after `--` inside the sandbox. For Python / Node / Go / shell callers that want to delegate without embedding the crate.

## What the sandbox does NOT guarantee

- **It is Phase 1, not Phase 2.** The v0.7.x sandbox relies on bubblewrap as a subprocess wrapper. That's enough for LLM-synthesized stages you haven't audited and for "I don't want a runaway test writing to my home directory" — not enough for genuinely hostile code targeting shared-kernel multi-tenant hosts. The threat model is spelled out in [SECURITY.md](https://github.com/alpibrusl/noether/blob/main/SECURITY.md).
- **It is Linux-only.** bwrap doesn't exist on macOS or Windows. The `noether-sandbox` binary compiles on those platforms (so cross-platform CI catches breakage) but fails at run time with `IsolationError::BackendUnavailable`. macOS / Windows release tarballs don't ship a sandbox binary.
- **Nix must be on `/nix/store`.** Distro-packaged `nix` (e.g. the Debian `nix-bin` package installing `/usr/bin/nix`) links to shared libraries outside `/nix/store` that the sandbox can't bind. The sandbox fails with a clear message pointing at the Determinate / upstream installer (which places `nix` under `/nix/store`).
- **Reproducibility is not isolation.** Two separate boundaries: Nix pins the runtime (same Python, same NumPy, same everything → same output), bubblewrap bounds the stage's view of the host (fs, net, env). They're orthogonal.

## Caller-managed filesystem trust

Two fields on `IsolationPolicy` let callers widen the default posture:

- **`ro_binds: Vec<RoBind>`** — always includes `/nix/store`. `IsolationPolicy::from_effects` adds one per `Effect::FsRead(path)` declared on the stage.
- **`rw_binds: Vec<RwBind>`** — empty by default. `from_effects` adds one per `Effect::FsWrite(path)`.

Mount order is **`rw_binds → ro_binds → work_host`**. RW first lets a narrower RO shadow a broader RW parent:

```
rw_binds: [{ host: "/home/user/project", sandbox: "/home/user/project" }]
ro_binds: [{ host: "/home/user/project/.ssh", sandbox: "/home/user/project/.ssh" }]
```

The whole project is writable, but `.ssh` inside it is RO — bwrap applies binds in argv order and the later one wins for overlapping subpaths.

`rw_binds` is a deliberate trust widening. The crate cannot validate whether binding `/home/user` RW is sensible — that's a policy decision the caller is making. The rustdoc on `RwBind` says this in plain language.

For how filesystem effects drive this automatically, see [guides/filesystem-effects](./filesystem-effects.md).

## Common failure modes

| Message on stderr | Cause | Remedy |
|---|---|---|
| `bubblewrap (bwrap) not found on PATH` with `--isolate=bwrap` | bwrap isn't installed | `apt install bubblewrap` / `brew install bubblewrap` / `nix profile install nixpkgs#bubblewrap` |
| `bwrap resolved via $PATH` (warning) | bwrap found outside a trusted system path | Install to `/usr/bin` or a root-owned Nix profile. PATH-planting risk |
| `nix is installed at /usr/bin/nix (outside /nix/store)` | Distro-packaged Nix | Install via the Determinate or upstream installer; or run with `--isolate=none` |
| `refusing to run without isolation` | `--require-isolation` / `NOETHER_REQUIRE_ISOLATION=1` set, bwrap unavailable | Install bwrap or drop the flag |
| Network declared but DNS silently fails inside the sandbox | `/etc/resolv.conf` / `/etc/hosts` / `/etc/nsswitch.conf` missing on host | The sandbox does `--ro-bind-try`; if the host file is absent it's a no-op. Create the missing file(s) — a one-line `/etc/resolv.conf` with `nameserver 1.1.1.1` is enough for a smoke test |

`noether run` exits with code **1** for all of the above (parse / resolution / preflight). Full exit-code map lives in [tutorial/when-things-go-wrong](../tutorial/when-things-go-wrong.md).

## Phase 2 roadmap

**v0.7.x is Phase 1** — bubblewrap as a subprocess wrapper. Target for **v0.8** is Phase 2: replace the bwrap subprocess with in-process `unshare` + Landlock (for filesystem scoping) + seccomp (for syscall filtering). The `IsolationPolicy` surface stays — the policy you build today survives the switch. Expected win: ~10× lower startup overhead (no subprocess fork per stage) and finer-grained syscall control.

Tracked on the [roadmap](../roadmap.md) under `Phase 2 isolation`. Issue [#44](https://github.com/alpibrusl/noether/issues/44) covers the compliance-matrix / threat-model-docs piece that lands alongside it.

## See also

- [SECURITY.md](https://github.com/alpibrusl/noether/blob/main/SECURITY.md) — full threat model
- [guides/filesystem-effects](./filesystem-effects.md) — path-scoped `Effect::FsRead` / `FsWrite` driving the policy
- [tutorial/when-things-go-wrong](../tutorial/when-things-go-wrong.md) — human-readable failure narrative
- [agents/debug-a-failed-graph](../agents/debug-a-failed-graph.md) — the same terrain in machine-parseable form
