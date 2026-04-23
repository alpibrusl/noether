# Noether

Typed, content-addressed composition for agent-built pipelines.

A **stage** is an immutable unit of computation addressed by
`SHA-256(signature + implementation)`. Stages compose into graphs that
the type checker verifies before execution. Effects (`Pure`, `Network`,
`Llm`, `Cost`, `Process`, `FsRead`, `FsWrite`, …) are declared in the
signature and enforced pre-flight by policy.

[![Crates.io](https://img.shields.io/crates/v/noether-cli.svg)](https://crates.io/crates/noether-cli)
[![License](https://img.shields.io/badge/license-EUPL--1.2-orange.svg)](./LICENSE)

## Status

One active maintainer, pre-1.0 (current tag: **v0.8.0**). Breaking
changes are possible between minor versions per
[`STABILITY.md`](./STABILITY.md).

- Trust model: [`SECURITY.md`](./SECURITY.md)
- Release notes: [`CHANGELOG.md`](./CHANGELOG.md)
- What ships vs. what's planned: [`docs/roadmap.md`](./docs/roadmap.md)
- Agent-facing entry point: [`AGENTS.md`](./AGENTS.md)

## Install

```bash
cargo install noether-cli
```

Prebuilt binaries: [GitHub Releases](https://github.com/alpibrusl/noether/releases/latest).

Optional runtime dependencies:

- **Nix** — hermetic runtime for Python / JavaScript / Bash stages.
  Rust-native stdlib stages run without it.
- **bubblewrap** — the v0.7+ isolation backend. `--isolate=auto` falls
  back to unsandboxed with a warning; pass `--require-isolation` to
  make that a hard error in CI.

## Usage

```bash
noether stage list              # browse the 85-stage stdlib
noether stage search "parse"    # semantic search across stages
noether compose "parse CSV and count rows"   # LLM-assisted graph synthesis
noether run graph.json          # type-check, plan, execute
noether trace <composition_id>  # replay a past run
```

## What is a stage

```
stage : { input: T } → { output: U }   // structural type signature
      + EffectSet                       // declared effects
      + Option<Implementation>          // Rust fn, Python, JavaScript, or Bash
identity = SHA-256(canonical_json(signature + implementation_hash))
```

Two stages with the same hash are provably the same computation —
across machines, across repositories, forever. The type checker uses
structural subtyping: `Record { a, b, c }` is a subtype of
`Record { a, b }`. Parametric (`<T>`), row (`{ …, ...R }`), and
refinement (`Number | Range(0..=100)`) types are supported as of v0.8.

Checks are on graph **topology** — the checker does not verify that a
stage's implementation honours its declared signature. A Python stage
claiming `Text → Number` can return a string and you find out at run
time. Refinement predicates are enforced at stage boundaries
automatically by `ValidatingExecutor` (merged on `main`; ships in the
next tag; opt-out with `NOETHER_NO_REFINEMENT_CHECK=1`).

## Crate layout

```
crates/
├── noether-core          # Type system, stage schema, hashing, stdlib
├── noether-store         # Immutable store (MemoryStore, JsonFileStore)
├── noether-engine        # Graph checker, planner, executor, optimizer
├── noether-isolation     # Bubblewrap sandbox (standalone crate)
├── noether-sandbox       # Thin CLI wrapper around noether-isolation
├── noether-scheduler     # Cron runner for scheduled compositions
├── noether-cli           # The `noether` command
└── noether-grid-*        # Distributed execution (broker + worker + protocol)
```

## When Noether is *not* the right tool

- **Request/response with SLAs, autoscaling, sticky sessions** — use a
  regular service framework. Noether runs graphs and returns.
- **Hardened sandbox for hostile multi-tenant code** — the bwrap layer
  is sized for "LLM-generated stages I haven't audited", not
  "adversaries targeting a shared kernel." See
  [`SECURITY.md`](./SECURITY.md).
- **Airflow / Prefect / Dagster territory** — those are mature for
  scheduled DAG ops with UI, lineage, and alerting. Noether has none
  of that surface.
- **One-shot scripts** — the content-addressing work pays off on
  run N+1. If there is no run 2, a plain script is simpler.
- **Non-JSON data** — the type system is structural over JSON.
  Streaming video, arbitrary binary, or live-network protocols are
  doable, but you'll fight the model.

## License

EUPL-1.2 — see [`LICENSE`](./LICENSE).
