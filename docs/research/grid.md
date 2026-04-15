# noether-grid (research)

**Status:** Research. Branch-only — not merged to `main`, not published, not
supported. This page describes the design; the prototype lives on the
`research/grid` branch.

## Problem

Companies pay for per-seat LLM subscriptions (Claude, Cursor, GPT,
Copilot, ...) and under-use most of them. A typical engineer uses 30% of
their monthly quota; the rest is pre-paid capacity that no one else can
reach.

`noether-grid` is a broker that pools idle LLM capacity *inside one
company* so any agent on the network can route through any employee's
seat. It addresses waste, not cross-company arbitrage — the latter is a
ToS-violation minefield (see the [parent discussion](#why-intra-company)).

## Why intra-company only

Every major LLM provider's ToS explicitly forbids sharing per-seat
credentials across users. Pooling within one company — where the seat
owners and the consuming agents are employees of the same legal entity —
is the narrow band where pooling is commercially defensible. Anything
broader is a non-starter until we have commercial API agreements, at
which point it becomes OpenRouter-class infrastructure and `noether-grid`
is not the right layer.

## Architecture

```
┌────────────────────────────────────────────────────────────────┐
│  Agent (caloron, dev-laptop, whatever)                          │
│  POST http://broker.corp:8080/jobs  { graph, input }            │
└────────────────────────┬───────────────────────────────────────┘
                         │
                         ▼
┌────────────────────────────────────────────────────────────────┐
│  noether-grid-broker                                            │
│  - Worker registry   (heartbeat-driven, TTL-expired)            │
│  - Capability index  (which worker has which Llm{provider})     │
│  - Job queue         (FIFO per capability class)                │
│  - Cost ledger       (per-worker monthly budget, decremented)   │
└────────┬─────────────────────────────┬─────────────────────────┘
         │                             │
         ▼                             ▼
┌────────────────────┐   ┌──────────────────────────────────────┐
│ Worker: Alice's MBP │   │ Worker: Bob's workstation           │
│ advertises Llm:     │   │ advertises Llm:                     │
│   claude-opus-4     │   │   gpt-4-turbo                       │
│   cursor            │   │   claude-haiku                      │
│ POST /execute → runs│   │ POST /execute → runs via noether    │
│   via noether-engine│   │   engine                            │
└────────────────────┘   └──────────────────────────────────────┘
```

## Trust model

Assumes **trusted private network**:

- No TLS between broker/workers (corp VPN boundary).
- Workers authenticate to the broker via a shared `NOETHER_GRID_SECRET`.
- Agents authenticate to the broker via per-agent API keys (reuses the
  `NOETHER_API_KEY` convention).
- Prompt data is in-flight only — broker holds it long enough to dispatch
  and relay the result, then drops it. No prompt persistence.

For internet-reachable deployments we'd need TLS everywhere plus a
re-think of the prompt-visibility story. That's out of scope for v0.

## What this reuses from existing noether

| Existing primitive | Role |
|---|---|
| `noether_engine::executor::runner::run_composition` | Worker's `/execute` impl |
| `noether_engine::lagrange::{parse_graph, CompositionNode}` | JSON wire format |
| `noether_engine::checker::infer_effects` | Broker's routing decision |
| `RemoteStage` AST node | Phase-2 graph splitting |
| `noether_core::capability::Capability` | Advertisement format |
| `CompositionResult::spent_cents` | Cost-ledger updates |
| Shared `NOETHER_REGISTRY` | All workers resolve same stage IDs |

## What's genuinely new (per-crate)

| Crate | Purpose | Rough LOC |
|---|---|---|
| `noether-grid-protocol` | Shared serde types | ~200 |
| `noether-grid-broker` | HTTP service, registry, queue | ~1000 |
| `noether-grid-worker` | Client binary wrapping `noether serve` | ~300 |
| Integration tests | 2 workers + broker, dispatch, heartbeat-death | ~400 |
| CLI `--grid` flag on `noether run` / `compose` | ~50 |

~2000 LOC total for an MVP, shared between three crates.

## Capability advertisement

```jsonc
{
  "worker_id": "alice-macbook",
  "url": "http://alice-macbook.corp.internal:8080",
  "capabilities": [
    {
      "kind": "llm",
      "provider": "anthropic",
      "model": "claude-opus-4-6",
      "auth_via": "cli",
      "budget_monthly_cents": 20000,
      "budget_remaining_cents": 14200,
      "rate_limit_rpm": 60
    }
  ],
  "noether_version": "0.3.2",
  "heartbeat_interval_secs": 10
}
```

## Routing algorithm

### Phase 1 — single-worker-per-graph

1. `infer_effects` on the incoming graph.
2. Filter workers whose advertised capabilities cover every `Llm{model}`
   in the graph.
3. Pick the match with highest `budget_remaining_cents`, LRU as tiebreak.
4. `POST` the graph + input to that worker's `/execute`.
5. On success, subtract reported `spent_cents` from the worker's ledger.
6. On worker timeout / network failure, requeue with a different worker.

Wastes capacity when a graph needs Claude for one stage and GPT for
another, but correct and simple. Covers caloron-style graphs that
consistently use one provider.

### Phase 2 — graph splitting

Broker walks the AST, finds each `Stage` with an `Effect::Llm{model}`,
picks a worker per node, rewrites the node as
`RemoteStage { url, input, output }`, then runs the rewritten graph from
the broker with `run_composition` + the native `RemoteStage` executor.

## HTTP surface

### Broker

| Method | Path | Body | Purpose |
|---|---|---|---|
| `POST` | `/workers` | `WorkerAdvertisement` | Enrol (`X-Grid-Secret`) |
| `POST` | `/workers/{id}/heartbeat` | `Heartbeat` | Liveness + capacity |
| `DELETE` | `/workers/{id}` | — | Graceful drain |
| `POST` | `/jobs` | `JobSpec` | Submit a graph (`X-API-Key`) |
| `GET` | `/jobs/{id}` | — | Poll status + result |
| `GET` | `/workers` | — | Observability |
| `GET` | `/health` | — | Liveness |

### Worker

| Method | Path | Body | Purpose |
|---|---|---|---|
| `POST` | `/execute` | `ExecuteRequest` | Run graph, return trace |
| `GET` | `/health` | — | Liveness + load |

## Phasing

**Phase 1 (MVP, ~1 week):** scaffolding + single-worker dispatch + basic
heartbeat. Exit criteria: 2 workers, 1 broker, 10 jobs round-trip with
zero failed dispatches. No cost accounting.

**Phase 2 (~2 weeks):** cost ledger, per-agent quotas, Prometheus
metrics, graph splitting for heterogeneous `Effect::Llm` graphs.

**Phase 3 (~2 weeks):** postgres state, worker-death retry, systemd / k8s
manifests, registry-UI dashboard page.

## Open design questions

1. **Worker self-reports its own budget.** For API-key providers, usage
   is queryable. For CLI-based providers (Claude Pro, Cursor), the
   worker has to estimate from observed history. Phase 1 uses
   honour-system config-declared caps. Good enough for intra-company.
2. **Race on the same worker.** Broker serialises `POST /jobs` worker
   selection; worker's `/execute` is concurrency-safe (just
   `noether serve` internals). Each job runs in its own thread.
3. **LLM CLI logged out.** First failure → capability marked
   "degraded" for 60s → excluded from routing. Next heartbeat reports
   the cap missing → auto-heals when the employee logs back in.
4. **Caloron integration.** Exactly one env var: set
   `NOETHER_GRID_BROKER=http://broker.corp:8080` in caloron's scheduler
   config; LLM stages route through the broker. No graph changes.

## Why this is not in `main`

- Scope: ~2000 LOC, three new crates, a workspace reshape.
- Commercial ambiguity: even the intra-company framing is a ToS
  gray area until we write it down explicitly with legal.
- API stability: the protocol types will churn before we've run a
  pilot; no point stabilising them in `main` until we know the shape.

The branch exists so the design is checked in, the prototype can evolve
without blocking main, and we can pilot with caloron internally before
deciding whether to promote.
