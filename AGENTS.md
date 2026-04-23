# AGENTS.md

Dense, machine-readable map of Noether for AI agents. Humans reading this for the first time: start at [`README.md`](README.md) — it's narrative-shaped. This file is indexable fragments optimised for token-efficient agent consumption.

## What Noether is, in one paragraph

Content-addressed, typed, composable stages (functions) wired into verified pipelines. Every stage has: a structural type signature (`input → output`), a declared effect set, examples, a SHA-256 content hash as its identity. Graphs over stages (`Sequential`, `Parallel`, `Branch`, `Fanout`, `Merge`, `Retry`, `Let`) are type-checked before execution and hash-identified so the same source graph produces the same `composition_id` forever. Stage execution is sandboxed by default (`bwrap` subprocess) from v0.7. Built for AI agents to decompose problems into verified compositions; humans use it too.

## Layered reference

- [`STABILITY.md`](STABILITY.md) — v1.x wire-format contract. What's frozen, what's additive, what's deprecated. Authoritative on what your code can rely on.
- [`SECURITY.md`](SECURITY.md) — trust model. Isolation semantics, capability boundaries, what the sandbox does and doesn't defend against.
- [`CHANGELOG.md`](CHANGELOG.md) — per-release delta. Start here when checking "what changed between vX and vY."
- [`docs/roadmap.md`](docs/roadmap.md) — what ships vs. what's planned.
- [`docs/agents/`](docs/agents/) — intent-keyed playbooks. Each answers one concrete "how do I…?" question.

## Agent entry points

Use these instead of grepping the codebase:

| Intent | Tool |
| --- | --- |
| List available playbooks | `noether agent-docs` |
| Read a playbook | `noether agent-docs <key>` |
| Search playbooks by keyword | `noether agent-docs --search <term>` |
| Full command tree as JSON (ACLI) | `noether introspect` |
| Search stages by intent | `noether stage search "<problem description>"` |
| List all registered stages | `noether stage list` |
| Get a specific stage | `noether stage get <id-or-prefix>` |
| Type-check a graph without executing | `noether run --dry-run <graph.json>` |
| Produce a graph from a problem description | `noether compose "<problem>"` |

## Playbook index

Each playbook is a fixed-shape fragment (`Intent / Preconditions / Steps / Output shape / Failure modes / Verification`). Load on demand — you do not need to read them linearly.

- [`compose-a-graph`](docs/agents/compose-a-graph.md) — translate a problem description into a valid composition graph using the Composition Agent.
- [`find-an-existing-stage`](docs/agents/find-an-existing-stage.md) — search the stdlib + registry for stages matching a type signature or intent.
- [`synthesize-a-new-stage`](docs/agents/synthesize-a-new-stage.md) — author a stage when nothing in the catalogue fits; sign, validate, register.
- [`express-a-property`](docs/agents/express-a-property.md) — add declarative property claims to a stage using the DSL.
- [`debug-a-failed-graph`](docs/agents/debug-a-failed-graph.md) — interpret type errors, effect violations, capability errors, resolver warnings, and runtime traces.

## Type system at a glance

`NType` is structural. Subtyping is structural. Relevant variants (see `crates/noether-core/src/types/`):

- `Text`, `Number`, `Bool`, `Null`, `Bytes`, `VNode`, `Any`
- `List(T)`, `Map<K, V>`
- `Record { field: T, ... }` — width + depth subtyping (`Record { a, b, c }` is subtype of `Record { a, b }`).
- `Union(variants)` — flattened, deduped, sorted by constructor (`union()` is the only normalising path).
- `Stream<T>` — unbounded, typed channel.

`Any` is a bidirectional escape hatch: `is_subtype_of(T, Any)` AND `is_subtype_of(Any, T)` both hold.

## Effect system at a glance

`EffectSet` is a set of `Effect` variants (`crates/noether-core/src/effects/effect.rs`):

- `Pure` — no side effects. Default when nothing declared.
- `Fallible` — can return an error the caller must handle.
- `Network` — makes outbound network calls. Sandbox uses this to toggle `--share-net`.
- `Llm { model }` — invokes an LLM. `model` string is a hint; policy checks use `EffectKind::Llm`.
- `NonDeterministic` — same input may produce different output.
- `Process` — spawns, signals, or waits on OS processes.
- `Cost { cents }` — declared monetary cost. `--budget-cents` uses this.
- `Unknown` — effect-inference LLM couldn't classify; treated conservatively.

## Wire formats

- **Stage spec**: see `.cli/schemas/stage-spec.json` (JSON Schema).
- **Composition graph (Lagrange)**: see `crates/noether-engine/src/lagrange/ast.rs` — `CompositionNode` enum, tagged on `"op"`.
- **Isolation policy**: see `crates/noether-isolation/src/lib.rs` — `IsolationPolicy` struct, serde-enabled for cross-process use.
- **ACLI response envelope**: see `acli` crate. Every CLI output is `{ ok: bool, command: str, result?|error: ..., meta: {version, duration_ms} }`.

## Non-goals (stop here, do not try these)

- Noether is **not** a workflow orchestrator, pipeline runner, AI agent framework, or package manager. Don't try to use it to schedule recurring jobs (use `noether-scheduler`), glue LLM prompts together (use an agent framework), or distribute binaries (use a package registry).
- Stages are content-addressed. Mutation is modelled via new stages + deprecation chains, not in-place edits. Don't try to "update" an existing stage; put a new one with the same `signature_id` and the store auto-deprecates the old.
- Graph identity is pre-resolution (M1 contract). Never hash a graph after `resolve_pinning` — the id drifts when the Active implementation rotates.

## Versioning

- Current: **v0.7.1** (M2 closed + `noether-isolation` extracted).
- STABILITY.md applies from 0.7.0.
- Breaking changes go in major/minor bumps with a CHANGELOG entry marked `Breaking`.
- 1.0 target: M4 completion (see `docs/roadmap/2026-04-18-rock-solid-plan.md`).
