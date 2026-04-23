# Noether

Typed, content-addressed composition for agent-built pipelines.

A **stage** is an immutable unit of computation addressed by
`SHA-256(signature + implementation)`. Stages compose into graphs
that the type checker verifies before execution. Effects
(`Pure`, `Network`, `Llm`, `Cost`, `Process`, `FsRead`, `FsWrite`, …)
are declared in the signature and enforced pre-flight by policy.

## Status

One active maintainer, pre-1.0. Current tag: **v0.8.0** (tagged 2026-04-21).

Breaking changes are possible between minor versions per
[STABILITY.md](https://github.com/alpibrusl/noether/blob/main/STABILITY.md).
See [`CHANGELOG.md`](https://github.com/alpibrusl/noether/blob/main/CHANGELOG.md)
for release notes and [`roadmap.md`](roadmap.md) for what ships vs.
what's planned.

## What v0.8 shipped

Closes **M3 — Optimizer + Richer Types**:

- **Parametric polymorphism** — stage signatures can carry `NType::Var("T")`;
  `check_graph` threads substitutions through every edge.
- **Row polymorphism** — `NType::RecordWith { fields, rest }` captures
  unknown extra fields; `{ name, age } >> mark_done` resolves the tail to
  `Record { name, age, done }` instead of silently dropping extras.
- **Refinement types** — `NType::Refined { base, refinement }` attaches
  runtime-checkable predicates (`Range`, `OneOf`, `NonEmpty`).
- **Graph optimizer** — three passes run between type-check and plan:
  `canonical_structural` (flattens nested wrappers), `dead_branch`
  (folds `Branch(Const(bool), …)`), `memoize_pure` (caches Pure-stage
  invocations within a run).
- **Stdlib grew to 85** — added `identity`, `head`, `tail`, `mark_done`,
  `clamp_percent` for the new type machinery.
- **Filesystem-scoped effects** — `Effect::FsRead(path)` /
  `Effect::FsWrite(path)` drive `IsolationPolicy::from_effects`, so
  path-scoped bind mounts fall out of the declared signature.

**On main, ships in the next tag:** runtime enforcement of refinement
predicates at stage boundaries via `ValidatingExecutor`.

## Where to go next

- [Install](install.md) — five minutes from zero to running.
- [Concepts](concepts.md) — stages, types, effects, composition graphs,
  content addressing. The reference you'll come back to.
- [Usage](usage.md) — the CLI surface: `stage`, `compose`, `run`, `trace`.
- [Examples](examples.md) — worked end-to-end runs.

## Not the right tool for

- **Request/response with SLAs, autoscaling, sticky sessions** — use a
  regular service framework. Noether runs graphs and returns.
- **Hardened sandbox for hostile multi-tenant code** — the bwrap layer
  is sized for "LLM-generated stages I haven't audited", not
  "adversaries targeting a shared kernel". See
  [SECURITY.md](https://github.com/alpibrusl/noether/blob/main/SECURITY.md).
- **Airflow / Prefect / Dagster territory** — those are mature for
  scheduled DAGs with UI ops, lineage, alerting. Noether has none of
  that surface.
- **One-shot scripts** — content-addressing pays off on run N+1. If
  there is no run 2, a plain script is simpler.
- **Non-JSON data** — the type system is structural over JSON. Streaming
  video, arbitrary binary, or live-network protocols are doable but
  you'll fight the model.
