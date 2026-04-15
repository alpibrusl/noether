# Generalising grid beyond LLM capacity

**Status:** Design note. Not built. Architectural cleanup proposal for
after v0.4.0 ships the LLM-focused MVP.

## The layering smell

Noether is a general structural-typed composition platform. `Effect` is
an eight-variant enum — `Pure`, `Network`, `Llm { model }`, `Fallible`,
`NonDeterministic`, `Cost { cents }`, `Process`, `Unknown` — and
composition stages declare any subset.

Grid, as built (phases 1–5, shipping as v0.4.0), pools **LLM capacity
specifically**:

| Concept | Where LLM is hardcoded |
|---|---|
| Worker capability ad | `LlmCapability { provider, model, auth_via, budget_*, rate_limit_rpm }` in `noether-grid-protocol` |
| Worker discovery | `probe_subscription_clis()` iterates `specs::{CLAUDE, GEMINI, CURSOR, OPENCODE}` |
| Router match | `worker_has_model` / `pick_worker_for` key on `Effect::Llm { model }` exactly |
| Splitter gate | `has_llm_effect(stage)` decides which nodes to rewrite as `RemoteStage` |
| Cost model | `cents_spent_total` assumes metered LLM APIs |

None of this is forced by grid's *shape*. The actual grid shape is:

> A stage declares an effect the broker can't satisfy locally. The
> broker finds a worker that can, rewrites the node as `RemoteStage`
> pointing at that worker, and lets the existing noether-engine runtime
> execute the rewritten graph.

That's general. "LLM subscription" is one instance of "capability the
broker can't satisfy locally" — and the most obvious one for the 2026
market, which is why we implemented it first. But database connections
on a specific VPC, GPU time on a workstation with a CUDA runtime,
scraper rotation through a residential proxy pool, a paid data feed
with per-request billing — all are the same routing problem with
different capability descriptions.

## The proposed shape

### Generic capability profile

Move the LLM-specific types out of the protocol core. A worker
advertises a `Capabilities` map, keyed by profile name:

```json
{
  "worker_id": "pipa-1217319",
  "url": "http://pipa.corp:8089",
  "capabilities": {
    "llm": [
      { "provider": "anthropic-cli", "model": "claude-desktop",
        "auth_via": "cli", "budget_remaining_cents": 2000 }
    ],
    "gpu": [
      { "device": "cuda:0", "memory_mb": 24000,
        "budget_remaining_seconds": 3600 }
    ]
  },
  "noether_version": "0.5.0"
}
```

Each profile is a Rust module that registers:

- A **capability descriptor** type (serialised into the JSON above).
- An **effect matcher**: given an `Effect` and a capability descriptor,
  does this worker cover that effect?
- Optional **probe functions** for the worker to self-discover what
  it has (the LLM profile's `probe_subscription_clis()` becomes one
  such probe; a GPU profile might shell out to `nvidia-smi`).

### Generic splitter

`split_graph` no longer asks "is this an LLM effect?" — it asks "is
there an effect on this stage the local executor can't satisfy that a
registered profile's matcher can?". If yes, rewrite as `RemoteStage`
and let `pick_worker_for` delegate to the responsible profile's
matcher.

`required_llm_models` becomes `required_capabilities`, returning a
`Vec<(profile_name, descriptor_slug)>` which the router uses as the
capability demand vector.

### Profile registration

Grid-core ships without any profiles. The binary links whichever set
the operator enables:

```bash
cargo build --bin noether-grid-broker --features "grid-profile-llm,grid-profile-gpu"
```

The `llm` profile is what we have today, moved behind the profile
trait. Everything else is additive — a new profile adds a crate, the
binary opts in, nothing existing changes.

### Cost story per profile

Each profile owns its own accounting. The shared `/metrics` endpoint
exposes whatever each profile decides is meaningful:

- LLM metered (API key): `cents_spent_total` (what we have).
- LLM subscription: `jobs_routed_total{provider}`,
  `capacity_used_ratio{worker, provider}` (the v0.4.1 plan from the
  pilot-4 discussion — subscriptions are flat-rate so cents is
  misleading).
- GPU: `gpu_seconds_used_total{worker, device}`.
- Paid data feed: `api_calls_total{worker, feed}` +
  `cents_spent_total{feed}`.

This also fixes the "cents spent on a Claude subscription is 0,
misleading" awkwardness the caloron agent flagged on pilot-4: the
profile decides what metric makes sense for its capability class;
grid-core stays agnostic.

## What stays exactly the same

- Worker enrolment / heartbeat / drain wire protocol.
- `RemoteStage` node type in the engine.
- Job submission / retry / persistence story.
- The broker's role as a routing plane, not an executor.
- noether-engine's `Effect` enum — this is purely how grid *uses* it,
  not a change to the core.

The changes are localised to the broker's `splitter.rs` + `router.rs`
(effect-kind-generic), the worker's capability probing (profile
plug-ins), and `noether-grid-protocol` (drop hardcoded LLM types).

## When to actually do this

Triggers:

1. **A second profile is concretely needed.** Caloron asking for
   database-connection pooling, or a real user asking for GPU-time
   pooling, would be the forcing function. Until then, generalising
   ahead of the second use case is premature.
2. **The LLM profile's cost model needs enough divergence** (metered
   vs. subscription, rate-limit-aware vs. budget-cap-aware) that the
   single-type `LlmCapability` starts to sprout `Option` fields for
   every profile-shaped axis. Already a bit true today
   (`budget_remaining_cents` is meaningless for OpenCode); not yet
   painful.
3. **Cross-profile graphs** — a stage that needs both LLM *and* GPU
   on the same worker. Today's router is a single-capability match;
   a `Vec<CapabilityDemand>` per stage only makes sense once we
   actually have multiple capability kinds.

None of these are true as of v0.4.0. The LLM-grid pilot just
validated; shipping multi-machine + quota stories first is the better
next move. This doc is the breadcrumb for "the right structural answer
when a second profile shows up."

## Relationship to `llm-here.md`

`docs/research/llm-here.md` is the other consolidation note — unify
the three sibling LLM-detection code paths (caloron `_llm.py`,
agentspec `resolver.py`, grid's `cli_provider.rs`) behind one shared
binary. That's orthogonal to this note:

- `llm-here` is about **deduplication** of one capability's detection
  logic across three projects.
- This note is about **generalisation** of grid to more than one
  capability kind.

Both could land independently. If both land, the LLM profile inside
the generalised grid would shell out to `llm-here` for its probe +
dispatch, and the `llm` profile crate becomes very thin.

## Alternatives considered

- **Keep grid LLM-only; spin up `gpu-grid`, `db-grid` as separate
  binaries.** Rejected: the routing plane is 90% of the work and is
  identical across capability kinds. Duplicating it per kind is the
  same trap `llm-here.md` describes, one layer up.
- **Bake everything into `Effect` with a generic `Remote { profile,
  descriptor }` variant.** Rejected: `Effect` is a type-checker
  concern, not a routing concern. The remote-vs-local decision is
  grid's, not the type system's.
- **Make profiles dynamically loadable (plug-in `.so` / WASM
  modules).** Rejected as overkill. Cargo features + feature-gated
  modules give the same extensibility with a static binary and no
  runtime loader complexity. Revisit only if operators need to
  register profiles without recompiling.

## Meta

This note exists because v0.4.0 shipped a working LLM-grid and the
structural-typing story in noether's README promises more generality
than grid currently demonstrates. The promise isn't broken — grid is
an application *of* noether, not the definition of noether — but the
grid-specific code leaks the LLM assumption far enough into the
protocol that a future "why can't grid pool GPU seats" reader would
have to unwind it. This is the unwind.
