# Noether

**Typed, content-addressed pipelines — reproducible by construction, LLM-assisted by option.**

Decompose computation into stages with structural type signatures. The type checker verifies every edge of a composition graph before execution (topology only — it does not prove stage *bodies* correct). Run stages in a Nix-pinned runtime for byte-identical reproduction. Replay any run from its composition hash.

> **Trust model (read first):** the Nix-pinned runtime is a reproducibility boundary, **not** an isolation boundary. Stages run with host-user privileges and can read the filesystem, make network calls, and touch your environment. See [SECURITY.md](./SECURITY.md) before running a stage you did not write.

[![Crates.io](https://img.shields.io/crates/v/noether-cli.svg)](https://crates.io/crates/noether-cli)
[![Docs](https://img.shields.io/badge/docs-noether.alpibru.com-blue.svg)](https://alpibrusl.github.io/noether/)
[![Registry](https://img.shields.io/badge/registry-registry.alpibru.com-green.svg)](https://registry.alpibru.com/docs)
[![License](https://img.shields.io/badge/license-EUPL--1.2-orange.svg)](./LICENSE)

```bash
cargo install noether-cli            # binaries also on GitHub Releases

# Point at the public registry — no credentials needed for read access.
export NOETHER_REGISTRY=https://registry.alpibru.com

noether compose "parse CSV data and count the rows"
# → { "ok": true, "data": { "output": 3.0 } }
```

---

## What it is

A **stage** is an immutable, content-addressed unit of computation with a structural type signature:

```
stage: { input: T } → { output: U }
identity: SHA-256(signature)   ← not a name, not a version, a hash
```

Two stages with the same hash are provably the same computation — across machines, across repos, forever. The **composition engine** type-checks every edge of a graph before executing it, using structural subtyping (`Record { a, b, c }` is a subtype of `Record { a, b }`). This checks graph *topology*; it does not verify that a stage's implementation honours its declared signature (a Python stage claiming `Text → Number` can return a string and you find out at runtime).

Good fit: **typed ETL pipelines, analytics DAGs, data-normalisation across providers, LLM-augmented decisioning, anything where "the same inputs should always produce the same outputs" is a correctness requirement**. Effects are first-class (`Pure`, `Network`, `Llm`, `Cost`, `Process`, etc.) so budget, routing, and policy decisions ride on them.

Noether is **not** a workflow orchestrator, request-response framework, or AI agent runtime. Agents and services use Noether; they are not written in it.

### When Noether is *not* the right tool

- **You need request/response with SLAs, autoscaling, or sticky sessions.** Use a regular service framework (axum, FastAPI, …). Noether doesn't serve traffic; it runs graphs and returns.
- **You want a real sandbox for untrusted code.** The Nix-pinned runtime is a reproducibility boundary, not an isolation boundary. If you need to execute code you didn't write, wrap the executor in bwrap/firejail/nsjail yourself — or wait until Noether ships opt-in isolation.
- **You're scheduling 30 jobs a day across Airflow/Prefect/Dagster-style DAGs with UI ops + lineage + alerting.** Those tools are mature here and Noether has no UI.
- **Your pipeline only runs once.** The content-addressing + verification overhead is there so the *second* run is free. If there is no second run, a plain script is simpler.
- **Your inputs aren't JSON-typable.** Noether's type system is structural over JSON. Streaming video, arbitrary binary, live-network protocols — doable, but you'll fight the model.
- **You need multi-tenant cloud isolation out of the box.** The private `noether-cloud` service has it; the open-source `noether` CLI is single-tenant by design.

### Project status

**One active maintainer, best-effort response times.** See [`SECURITY.md`](./SECURITY.md) for the trust model and [`docs/roadmap.md`](./docs/roadmap.md) for what ships vs. what's planned. Not suitable for deployments that require vendor SLAs.

---

## Install

Two binaries ship from this repo:

- **`noether`** — the main CLI (`stage`, `store`, `run`, `build`, `compose`, `trace`).
- **`noether-scheduler`** — a cron runner that executes Lagrange graphs on a schedule and fires webhooks with the result. Optional; install if you have recurring compositions.

| | |
|---|---|
| **crates.io** | `cargo install noether-cli noether-scheduler` |
| **GitHub Releases** | [Download prebuilt binaries](https://github.com/alpibrusl/noether/releases/latest) — Linux / macOS / Windows, both binaries packaged separately |
| **Source** | `cargo build --release -p noether-cli -p noether-scheduler` |

Nix is optional; it's required only to execute Python / JavaScript / Bash stages in a Nix-pinned runtime (reproducibility, not isolation). Rust-native stdlib stages run without it.

---

## Quickstart

```bash
# Browse the hosted registry — 486 curated stages, no auth needed.
export NOETHER_REGISTRY=https://registry.alpibru.com

noether stage list                            # browse
noether stage search "parse CSV"              # semantic search
noether stage get <prefix>                    # 8-char prefix OK

# Write a graph that uses them.
cat > graph.json <<EOF
{
  "description": "count rows",
  "version": "0.1.0",
  "root": {
    "op": "Sequential",
    "stages": [
      { "op": "Stage", "id": "<csv-parse-prefix>" },
      { "op": "Stage", "id": "<list-length-prefix>" }
    ]
  }
}
EOF

noether run --dry-run graph.json              # type-check only
echo '{"csv": "a,b\n1,2\n3,4"}' | noether run graph.json
```

For the LLM-powered path, Noether picks the first available of:

```bash
# 1. An API key in env (cheapest to script, metered per-call).
export MISTRAL_API_KEY=...   # or VERTEX_AI_PROJECT, OPENAI_API_KEY, ANTHROPIC_API_KEY

# 2. Or a subscription CLI you're already signed into (zero API-key setup,
#    uses your Claude Pro / Gemini Advanced / Cursor seat directly).
export NOETHER_LLM_PROVIDER=claude-cli   # or gemini-cli, cursor-cli, opencode

noether compose "convert text to uppercase and get its length"
```

If you already have `claude` or `gemini` on `$PATH` with an active
session, no extra config is needed — auto-detection picks them up.

---

## Writing a custom stage

Python stages must define a top-level `execute(input)`. The runtime handles stdin / stdout for you — do not read from `sys.stdin` or `print` the result.

```json
{
  "name": "celsius_to_fahrenheit",
  "description": "Convert a Celsius temperature to Fahrenheit",
  "input":  { "Record": [["celsius", "Number"]] },
  "output": { "Record": [["fahrenheit", "Number"]] },
  "effects": ["Pure"],
  "language": "python",
  "implementation": "def execute(input):\n    return {'fahrenheit': input['celsius'] * 9 / 5 + 32}",
  "examples": [
    { "input": {"celsius": 0},   "output": {"fahrenheit": 32} },
    { "input": {"celsius": 100}, "output": {"fahrenheit": 212} }
  ]
}
```

```bash
noether stage add my-stage.json             # adds + auto-activates
noether stage add my-stage.json --draft     # opt out of activation
noether stage sync ./stages/                # bulk-import a directory
```

`stage add` validates the `def execute` contract and auto-deprecates any previous version with the same canonical identity (name + types + effects). Full details: **[Building Custom Stages →](./docs/guides/custom-stages.md)**

---

## Composition graph (Lagrange)

> **Why "Lagrange"?** Noether's theorem connects symmetries to conservation laws via the *Lagrangian*. A Lagrange graph describes a computation; Noether's type system guarantees its correctness — the same relationship as Lagrangian ↔ conservation law.

Nine operators: `Stage` · `Sequential` · `Parallel` · `Branch` · `Fanout` · `Merge` · `Retry` · `Const` · `Let`.

```json
{
  "op": "Let",
  "bindings": {
    "scan": { "op": "Stage", "id": "scan-prefix" },
    "hash": { "op": "Sequential", "stages": [
      { "op": "Stage", "id": "scan-prefix" },
      { "op": "Stage", "id": "hash-prefix" }
    ]}
  },
  "body": { "op": "Stage", "id": "diff-prefix" }
}
```

`Let` solves the canonical scan → hash → diff problem: `diff` needs `state_path` from the original input, which `hash` would otherwise erase. Bindings run concurrently; `body` receives `{...outer fields, binding-name: binding-output}`.

Full operator reference: **[Composition Graphs →](./docs/guides/composition-graphs.md)**

---

## What's new in v0.4

- **`noether-grid`** — distributed execution for composition graphs.
  A broker splits a graph so `Effect::Llm` (or any other effect the
  caller can't satisfy locally) dispatches to a worker that can,
  while pure stages execute locally. Workers advertise whatever LLM
  access they're configured with — API keys, self-hosted models, or
  same-org CLI auth. See
  **[broker README →](./crates/noether-grid-broker/README.md)** and
  **[design →](./docs/research/grid.md)**.
- **Pluggable LLM providers in `noether-engine`** — `NOETHER_LLM_PROVIDER`
  selects between API-key backends (Anthropic, OpenAI, Mistral,
  Vertex) and local CLI backends (`claude-cli`, `gemini-cli`,
  `cursor-cli`, `opencode`) for workstations with an active
  developer session. Auto-detection picks the first available.

## What's new in v0.2

- **`Let` operator** — carry original-input fields through `Sequential` pipelines.
- **`def execute(input)` validated** at `stage add` — clear error instead of cryptic runtime failure.
- **Stage ID prefix resolution in graphs** — the 8-char IDs `stage list` prints work everywhere.
- **Hosted registry** at `registry.alpibru.com` — public read, authed write (Docker Hub model).
- **`stage sync <dir>`** for bulk import.
- **`stage list --signed-by | --lifecycle | --full-ids`**.
- **stdin piping** to `noether run` now works.

Details: **[CHANGELOG →](./docs/changelog.md)**

---

## The hosted registry

`registry.alpibru.com` hosts the Noether stdlib plus ~486 curated community stages. Read access is open; writes require an API key.

```bash
curl https://registry.alpibru.com/health
curl "https://registry.alpibru.com/stages/search?q=validate+schema"

# Point the CLI at it — merges with your local store, local wins on collision.
export NOETHER_REGISTRY=https://registry.alpibru.com
```

Guide: **[Remote Registry →](./docs/guides/remote-registry.md)** — publishing, scheduling, self-hosting.

---

## Architecture (short form)

```
L4 — Agent Interface      ACLI CLI · Composition Agent · Semantic Index
L3 — Composition Engine   Type checker · Planner · Executor · Traces
L2 — Stage Store          Content-addressed registry · Lifecycle · Stdlib
L1 — Execution Layer      Nix-pinned runtimes · Python/JS/Bash (reproducible, not isolated)
```

| Crate | Purpose |
|---|---|
| `noether-core` | Type system, effects, stage schema, Ed25519 signing, stdlib |
| `noether-store` | `StageStore` trait + in-memory / JSON-file implementations |
| `noether-engine` | Lagrange AST, type checker, planner, executor, semantic index, LLM agent |
| `noether-cli` | ACLI-compliant CLI — `stage`, `store`, `run`, `build`, `compose`, `trace` |
| `noether-scheduler` | Cron runner — executes Lagrange graphs on a schedule, fires webhooks on result |

Full walk-through: **[Architecture Overview →](./docs/architecture/overview.md)**

---

## Calling from agents, services, and scripts

Noether is designed to be *called*, not built-into. Any process that can shell out to a CLI or hit an HTTP endpoint can use it — agents, CI jobs, cron, FastAPI services, Python scripts. The composition graph travels as compact JSON; only stages that declare `Effect::Llm` consume tokens. For agent callers specifically, this is a 60–80% token reduction vs. naïve LLM chaining (most plumbing becomes type-checked graph structure instead of prompt text).

```bash
# Structured ACLI output — parseable from any language.
noether compose "extract entities from these documents" --input '...'
# { "ok": true, "command": "compose", "data": {...}, "meta": {"version": "0.4.0"} }
```

---

## Docs · Contributing · License

- **Docs**: <https://alpibrusl.github.io/noether/>
- **Issues & PRs**: [github.com/alpibrusl/noether](https://github.com/alpibrusl/noether)
- **Contributing**: [CONTRIBUTING.md](./CONTRIBUTING.md) — stdlib stages, language runtimes, LLM providers, type-system extensions all welcome
- **License**: [EUPL-1.2](./LICENSE) (copyleft, GPL/LGPL/AGPL/MPL compatible)
