# `llm-here` — one shared tool for "which LLM is reachable from this host"

**Status:** Design note. Not built. Consolidation proposal for when the
triplicated LLM-provider-detection code across three sibling projects
becomes a drag on maintenance.

## The problem: three implementations of the same answer

As of `research/grid` phase 5, three separate codebases implement
"detect available LLM tooling on this host, decide which to use, run a
prompt through it":

| Lives in | Language | File | Scope |
|---|---|---|---|
| caloron-noether | Python | `stages/phases/_llm.py` | 4 CLIs + 3 APIs, concatenated into stage impls |
| agentspec | Python | `src/agentspec/resolver/resolver.py` | 7 runtimes + Vertex routing |
| noether-grid | Rust | `crates/noether-engine/src/llm/cli_provider.rs` | 4 CLIs (phase 5), ported from caloron's lessons |

All three answer variations of:

- *"Is `claude` / `gemini` / `cursor-agent` / `opencode` on PATH?"*
- *"Do I have `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` / …?"*
- *"If both, which do I prefer?"*
- *"How do I invoke the chosen one with a prompt and a timeout?"*
- *"How do I avoid stalling inside a sandboxed `$HOME`?"*

They've already drifted. caloron discovered the 25 s-under-Nix-30 s
timeout cap before noether-grid did; noether-grid picked it up in
phase 5d as a backport. agentspec knows about Vertex routing; the
other two don't. The pattern will repeat.

## The proposed tool

A single-purpose binary **`llm-here`** that both Python and Rust call
as a subprocess. Reads / writes JSON on stdout/stdin — language-agnostic.

### Commands

```bash
# 1. Detect: what's reachable on this host?
llm-here detect

# stdout (JSON):
# {
#   "providers": [
#     { "id": "claude-cli",   "kind": "cli",  "binary": "/usr/local/bin/claude",
#       "provider": "anthropic", "model_default": "claude-desktop" },
#     { "id": "openai-api",   "kind": "api",  "env": "OPENAI_API_KEY",
#       "provider": "openai",    "model_default": "gpt-4o" },
#     ...
#   ]
# }

# 2. Dispatch: run a prompt through a named provider.
llm-here run --provider claude-cli --timeout 25 <<EOF
Tell me a joke about Rust.
EOF

# stdout (JSON):
# { "ok": true, "text": "...", "provider_used": "claude-cli",
#   "duration_ms": 1834, "error": null }

# 3. Auto-chain: try providers in fallback order, return first success.
llm-here run --auto  <<EOF
...
EOF
# → same JSON shape, "provider_used" tells you which one won.
```

### What it knows

Single source of truth for:

- **Binary names** per CLI (`claude`, `gemini`, `cursor-agent`, `opencode`).
- **Argv templates** per CLI (flag order, system-prompt flag, dangerous-permissions flag).
- **Endpoint URLs** per API provider (Anthropic, OpenAI, Gemini,
  Mistral, Vertex).
- **Env-var names** for API keys (`ANTHROPIC_API_KEY`, etc.).
- **Sandbox detection:** honours `NOETHER_LLM_SKIP_CLI` /
  `CALORON_LLM_SKIP_CLI` / `AGENTSPEC_LLM_SKIP_CLI` (matches any of
  the three so callers can keep their existing env conventions).
- **Timeout policy:** defaults to 25 s; callers override with `--timeout`.
- **Fallback order:** the caloron-settled chain (Claude CLI > Gemini
  CLI > Cursor CLI > OpenCode > Anthropic API > OpenAI API > Gemini
  API > mock).

### What it deliberately doesn't do

- **No state.** Each invocation is independent. No caching of previous
  responses, no conversation history, no session tokens.
- **No cost accounting.** Callers (broker, agentspec runner, caloron
  phases) own their own cost ledger; `llm-here` just runs prompts.
- **No streaming.** Single prompt → single completion. Streaming
  belongs in whatever consumes `llm-here`'s output, not in `llm-here`.
- **No agent-loop semantics.** No tool-use, no multi-turn history.
  Anyone who wants that either layers it on top or stays on their own
  direct integration.

## How each project would use it

### caloron-noether

`stages/phases/_llm.py` shrinks from 243 lines to ~30:

```python
import json, subprocess

def call_llm(prompt: str, timeout: int = 120) -> str | None:
    try:
        r = subprocess.run(
            ["llm-here", "run", "--auto", "--timeout", str(timeout)],
            input=prompt, capture_output=True, text=True, timeout=timeout + 5,
        )
    except (subprocess.TimeoutExpired, FileNotFoundError):
        return None
    if r.returncode != 0:
        return None
    try:
        out = json.loads(r.stdout)
    except json.JSONDecodeError:
        return None
    return out.get("text") if out.get("ok") else None
```

Lessons caloron learned stay in `llm-here`. Stages concatenate 30
lines instead of 243. Tests still pass because the contract (prompt
in, text out, or `None`) is unchanged.

### agentspec

Resolver's `PROVIDER_MAP` + `RUNTIME_BINARIES` delegate capability
detection to `llm-here detect`:

```python
def _detect_runtimes() -> dict[str, bool]:
    out = subprocess.check_output(["llm-here", "detect"], text=True)
    return {p["id"]: True for p in json.loads(out)["providers"]}
```

Vertex AI routing (agentspec's unique piece) stays in the resolver —
it's a runtime-selection concern, not a detection concern. `llm-here`
answers the "what's reachable" question; agentspec still owns "what
should this manifest run on".

### noether-grid worker

`probe_subscription_clis()` becomes a single call:

```rust
fn probe_subscription_clis() -> Vec<LlmCapability> {
    let out = Command::new("llm-here").arg("detect").output().ok()?;
    let providers: Providers = serde_json::from_slice(&out.stdout).ok()?;
    providers.into_iter()
        .filter(|p| p.kind == "cli")
        .map(|p| LlmCapability { /* map fields */ })
        .collect()
}
```

And `CliProvider::complete` dispatches by `llm-here run --provider <id>`
instead of open-coding the subprocess. The engine's `LlmProvider` trait
stays; only the concrete impls move out.

## Rough implementation cost

Ballpark:

- New crate: `noether-llm-here` (Rust, static binary, ~500 LOC
  covering all 4 CLIs + 4 API providers + the JSON surface).
- Replace caloron's `_llm.py` body with the shim above (~20 min +
  existing test fixtures keep passing).
- Replace agentspec's detection with `llm-here detect` (~30 min,
  careful about Vertex routing staying put).
- Replace noether-engine's `cli_provider.rs` impls with subprocess
  calls (~40 min; providers.rs auto-detect chain gets simpler, not
  harder).
- Total: **one focused day**, deletes ~1,200 LOC across the three
  repos, eliminates the known drift sources.

## When to actually do this

Not yet. Triggers that would change that:

1. **A fifth project** (probably the next noether-adjacent thing) needs
   the same detection logic. That's the `shame on me` threshold.
2. **A feature crosses sides** — e.g. Vertex routing would be useful in
   caloron's stages — and re-implementing it a second time costs more
   than extraction.
3. **A bug bites twice** — the same drift-induced bug has to be fixed
   once in each repo. Unification would have prevented the second fix.

None of these are true yet. Phase 5d backported caloron's lessons into
Rust and that's bought us parity for now. `llm-here` is the right
structural answer when one of these triggers fires; until then the
triplication is cheaper than the consolidation work.

## Packaging when we do build it

- Ships from the `noether` workspace as a fourth binary alongside
  `noether`, `noether-scheduler`, `noether-grid-broker`,
  `noether-grid-worker`. Crates.io publish is free.
- Binary is small (~5 MB) — both caloron and agentspec can vendor it
  per-machine or pull from the noether GitHub release.
- Versioning: the JSON-wire format is what downstream callers depend
  on. Semver the JSON schema, not the binary. Additive changes
  (new provider in the list) are minor bumps; removing a field is a
  major.
- Feature parity test: the extraction commit includes a cross-repo
  test that sends one prompt through each of `llm-here`, caloron's
  old `_llm.py`, and agentspec's resolver, asserts identical
  stdout contents for a mock provider. Catches the "we broke
  something during consolidation" class of regression.

## Alternatives considered and rejected

- **Rust crate linked from caloron + agentspec via PyO3 bindings.** Ties
  all three projects to the Rust build toolchain. Caloron and agentspec
  benefit from being pure Python for ops reasons. A subprocess boundary
  is looser coupling.
- **Git submodule of a shared Python lib.** Only helps Python callers.
  noether-grid is Rust; the worker would still need its own
  implementation.
- **Move everything into `noether-engine::llm` and have the Python
  projects spawn `noether run ...`.** Wrong layer — `noether-engine`
  is a composition engine, not a generic "call an LLM" tool. Would
  bloat noether-engine's dependency graph with caller-specific
  concerns.

## Meta

This doc exists because I almost rebuilt `_llm.py` in Rust from scratch
before noticing caloron already had it. It's a reminder to look in the
other projects first, and a placeholder the next agent (or future me)
can point at when the drift becomes expensive.
