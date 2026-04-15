# noether-grid

**Pool the LLM subscriptions your company already pays for.**

Most companies have two LLM problems at once:

- **Some seats sit idle.** A senior engineer's $200/mo Claude Team seat used 30% of its quota last month. Multiply by 50 employees, you've lit ~$70k/year on fire.
- **Other people are token-starved.** The data team can't run their analyst without paying out of pocket; the AI ops engineer is rate-limited mid-incident.

`noether-grid` lets the second group reach the first group's idle capacity, on your private network, with content-addressed routing and per-agent quotas.

```
┌────────────────────────────────────────────────────────────┐
│ Anyone's agent             POST http://broker.corp/jobs    │
│ (sprint loop, scraper,                                      │
│  STORM-style writer, …)                                     │
└────────────────────────┬───────────────────────────────────┘
                         ▼
┌────────────────────────────────────────────────────────────┐
│ noether-grid-broker                                         │
│   • Stage catalogue + Lagrange graph splitter               │
│   • Picks worker per LLM call (most remaining budget wins)  │
│   • Per-agent quotas, cost ledger, retry on worker death    │
│   • Live dashboard + Prometheus /metrics                    │
└────────┬─────────────────────────────┬─────────────────────┘
         ▼                             ▼
┌────────────────────┐   ┌─────────────────────────────────┐
│ Alice's MBP         │   │ Bob's workstation               │
│ Claude Team seat    │   │ GPT-4 + Vertex                  │
│ noether-grid-worker │   │ noether-grid-worker             │
└────────────────────┘   └─────────────────────────────────┘
```

---

## Status

**Research.** Branch-only on `research/grid` — not on crates.io, not in `noether` main. The implementation is complete enough for a real intra-company pilot; the design doc at [`docs/research/grid.md`](../../docs/research/grid.md) explains why we haven't promoted it yet.

| | |
|---|---|
| Worker enrolment + heartbeat + capability advertisement | ✅ |
| Graph splitting (LLM stages → worker, rest local) | ✅ |
| Per-agent quotas (`X-API-Key` → monthly cents) | ✅ |
| Cost ledger (per-worker, per-call) | ✅ |
| Worker-death retry (3 attempts, exclude failed worker) | ✅ |
| Live HTML dashboard | ✅ |
| Prometheus `/metrics` | ✅ |
| systemd units (broker + worker) | ✅ |
| Postgres broker state | ⏳ deferred — in-memory single-instance for now |

---

## Deploying across your company — roles and minimum install

The grid has three roles. Each role runs exactly one binary and needs
only the things listed below — nothing else.

```
          ┌─────────────────────────────────────────────────┐
          │ Broker node                                      │
          │ one per company, reachable from every worker     │
          │                                                  │
          │ binary:   noether-grid-broker  (~15 MB, Rust)    │
          │ deps:     none at runtime                        │
          │ inbound:  :8088  from workers + agent submitters │
          │ outbound: none  (workers call back with results) │
          │ state:    in-memory (default) / postgres (opt)   │
          │ host:     any always-on Linux/macOS box          │
          └─────────────────────────────────────────────────┘

          ┌─────────────────────────────────────────────────┐
          │ Worker node                                      │
          │ one per machine whose LLM seat you want pooled   │
          │                                                  │
          │ binary:   noether-grid-worker  (~15 MB, Rust)    │
          │ deps:     whatever LLM tooling already lives on  │
          │           the machine (Claude CLI, API key in    │
          │           env, Cursor install, etc.)             │
          │ inbound:  :8089  from broker only                │
          │ outbound: to the broker + to LLM providers       │
          │ state:    none (stateless subprocess wrapper)    │
          │ host:     any machine with an LLM login —        │
          │           dev laptop, shared GPU box, CI runner  │
          └─────────────────────────────────────────────────┘

          ┌─────────────────────────────────────────────────┐
          │ Agent submitter                                  │
          │ anywhere an agent calls noether                  │
          │                                                  │
          │ binary:   noether CLI  or  noether-scheduler     │
          │ deps:     none                                   │
          │ config:   NOETHER_GRID_BROKER=http://... or      │
          │           `grid_broker:` in scheduler.json       │
          └─────────────────────────────────────────────────┘
```

### Broker node — one per company

Whoever your "DevOps" is (even if that's you on a free-tier VM) runs one
broker, typically on a stable server reachable from every worker.

```bash
# On the broker host
cargo install --git https://github.com/alpibrusl/noether \
    --branch research/grid noether-grid-broker
# → installed at ~/.cargo/bin/noether-grid-broker

# Optional config in /etc/noether/grid-broker.env:
#   NOETHER_GRID_BIND=0.0.0.0:8088
#   NOETHER_GRID_SECRET=<random secret shared with workers>
#   NOETHER_GRID_QUOTAS_FILE=/etc/noether/quotas.json
#   NOETHER_STORE_PATH=/var/lib/noether/store.json

# Run as a systemd service (unit file in infra/)
sudo cp infra/noether-grid-broker.service /etc/systemd/system/
sudo systemctl enable --now noether-grid-broker
```

Networking: open `:8088` on the broker host to the subnet that contains
workers and agent submitters. Nothing else.

### Worker node — one per machine with an LLM seat

The worker is installed on any machine whose LLM credentials you want in
the pool. Two flavours, depending on where the auth lives:

**Headless worker (server with API keys in env):**

```bash
cargo install --git https://github.com/alpibrusl/noether \
    --branch research/grid noether-grid-worker

# /etc/noether/grid-worker.env — API keys + declared monthly caps:
#   NOETHER_GRID_BROKER=http://broker.corp:8088
#   NOETHER_GRID_SECRET=<same secret as broker>
#   ANTHROPIC_API_KEY=sk-ant-...
#   NOETHER_GRID_ANTHROPIC_BUDGET_CENTS=20000
#   OPENAI_API_KEY=sk-...
#   NOETHER_GRID_OPENAI_BUDGET_CENTS=10000

sudo cp infra/noether-grid-worker.service /etc/systemd/system/
sudo systemctl enable --now noether-grid-worker
```

Networking: worker needs outbound to the broker and to the LLM providers;
inbound on `:8089` from the broker. Typically no inbound from anywhere
else — the dispatch flow is broker → worker, not client → worker.

**Developer laptop worker (CLI auth — Claude Desktop, Cursor, Copilot):**

Phase-5-in-progress. Currently the worker still requires env-var API keys
for any seat it advertises; CLI-based subscriptions (`~/.config/anthropic`,
`~/.cursor`, `~/.config/gh/copilot`) are detected manually by setting
the relevant env vars yourself. Auto-discovery of logged-in CLIs is the
next item on the branch backlog — see `docs/research/grid.md`.

Until that lands, on a developer laptop you'd typically point the worker
at whatever ambient API key the user has:

```bash
# Add to ~/.profile or ~/.zshrc
export NOETHER_GRID_BROKER=http://broker.corp:8088
export NOETHER_GRID_ANTHROPIC_BUDGET_CENTS=20000
# ANTHROPIC_API_KEY already set from your normal noether usage

# Run in the background (or as a user-level systemd service):
nohup noether-grid-worker &
```

### Agent submitter — anywhere an agent runs

Whatever was running `noether run` / `noether compose` / `noether-scheduler`
against a local store continues to work. To route through the broker,
one env var (or one scheduler config line) flips the behaviour:

```bash
# One-off graph
NOETHER_GRID_BROKER=http://broker.corp:8088 noether run graph.json

# Scheduled graphs — edit scheduler.json:
{
  "store_path": ".noether/store.json",
  "grid_broker": "http://broker.corp:8088",
  "jobs": [ ... ]
}
```

No changes to graphs. No changes to stage specs. Nothing else in the
agent's setup differs between "local mode" and "grid mode".

### Sizing — how many of each role

| Role | Count | Notes |
|---|---|---|
| Broker | **1** per company | Multi-instance with leader election is phase-4 deferred; one is plenty for a pilot. |
| Worker | **1 per LLM-seat-holder** | Each developer laptop, shared GPU host, build bot, CI runner with an LLM key. Deploy gradually — 2 workers is enough to test; pool grows as you enrol more. |
| Agent submitter | **unbounded** | Every existing `noether`/`noether-scheduler` invocation becomes a submitter by setting the env var. No enrolment, no heartbeat, no lifecycle — stateless clients of the broker. |

---

## Five-minute install (LAN, single-host demo)

```bash
# Build (from a fresh clone of the noether research/grid branch)
cargo build --release \
  -p noether-grid-broker \
  -p noether-grid-worker

# Terminal 1 — broker
./target/release/noether-grid-broker
# → listening on 0.0.0.0:8088
# → dashboard at http://localhost:8088

# Terminal 2 — worker (advertises whatever LLM env vars you have set)
ANTHROPIC_API_KEY=sk-ant-... \
NOETHER_GRID_ANTHROPIC_BUDGET_CENTS=20000 \
NOETHER_GRID_BROKER=http://localhost:8088 \
./target/release/noether-grid-worker

# Terminal 3 — submit a job
curl -X POST http://localhost:8088/jobs \
  -H 'Content-Type: application/json' \
  -d '{
        "graph": {"description": "test", "version": "0.1.0",
                  "root": {"op": "Const", "value": "hello"}},
        "input": null
      }'
# → {"job_id": "...", "status": "queued"}
```

Visit `http://localhost:8088` to watch the worker enrol and the job complete.

---

## Caloron-style integration (zero graph changes)

If you're already running `noether-scheduler` on a recurring graph (sprint loop, ingest pipeline, hourly digest), enabling grid mode is one config line:

```diff
  // scheduler.json
  {
    "store_path": ".noether/store.json",
+   "grid_broker": "http://broker.corp.internal:8088",
    "jobs": [
      { "name": "sprint-tick", "cron": "* * * * *",
        "graph": "compositions/sprint_tick.json",
        "input": { "sprint_id": "sprint-1" }
      }
    ]
  }
```

The scheduler submits each tick to the broker. The broker walks the graph, finds the LLM stages, dispatches them to whichever worker has Claude/GPT/Cursor capacity, runs the rest locally, and returns the trace via the same webhook your scheduler already calls.

---

## Cost model

Quick estimate of what's reclaimable, per company:

```
Yearly spend on idle seats =
    seats × monthly_cost × (1 − utilisation) × 12
```

| Company shape | Yearly idle spend |
|---|---|
| 10 engineers, $20 Cursor + $20 Copilot, 50% utilisation | **$2,400/yr** |
| 50 engineers, $50 Claude Team avg, 35% utilisation | **$19,500/yr** |
| 200 engineers, $80/mo mixed avg, 40% utilisation | **$115,200/yr** |

The grid doesn't capture all of that — workers can't transfer credentials, only route through a colleague's logged-in CLI — but realistically you reclaim 60–80% of the idle budget by routing token-starved workloads through the under-used seats. So in the 50-engineer case, **~$12k/yr saved**, against ~10 hours of one-time setup on a single broker host.

Cost on the LLM-provider side: zero. Per-seat ToS allow the seat-holder's machine to make calls on behalf of internal workloads (it's the seat-holder's machine doing the work). What the ToS forbid — and what `noether-grid` deliberately doesn't enable — is sharing credentials cross-company or impersonating other users' identity to the provider.

---

## Configuration reference

### Broker

| Env / flag | Default | Purpose |
|---|---|---|
| `NOETHER_GRID_BIND` | `0.0.0.0:8088` | bind address |
| `NOETHER_GRID_SECRET` | empty (no auth) | shared secret workers present on enrolment |
| `NOETHER_STORE_PATH` | `.noether/store.json` | local stage store seeded into broker catalogue |
| `NOETHER_GRID_QUOTAS_FILE` | unset | path to JSON `{"<api-key>": <monthly-cents>}` |
| `RUST_LOG` | `noether_grid_broker=info` | structured logging level |

### Worker

| Env / flag | Default | Purpose |
|---|---|---|
| `NOETHER_GRID_BROKER` | required | broker URL (e.g. `http://broker.corp:8088`) |
| `NOETHER_GRID_WORKER_BIND` | `0.0.0.0:8089` | local listen address |
| `NOETHER_GRID_WORKER_URL` | derived from hostname | URL the broker dispatches to |
| `NOETHER_GRID_SECRET` | empty | shared secret matching the broker |
| `MISTRAL_API_KEY` / `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` / `VERTEX_AI_PROJECT` | unset | LLM credentials. Each present key advertises a capability. |
| `NOETHER_GRID_<provider>_BUDGET_CENTS` | `0` | declared monthly cap per provider |
| `NOETHER_STORE_PATH` | `.noether/store.json` | local stage store the worker resolves stages from |

systemd units in `infra/`. Quota file format:

```json
{
  "key-eng-team-sprint-loop": 50000,
  "key-data-team-analyst": 20000,
  "key-research-storm": 100000
}
```

---

## What it is not

- **Not a cross-company marketplace.** Pooling per-seat LLM subscriptions across legal entities violates every major provider's ToS (Claude, OpenAI, Cursor, Copilot are all per-user/per-team). Don't.
- **Not a workload-orchestration platform.** It's a routing layer for the LLM-call subset of a larger composition. Stage scheduling, cron, retries-of-the-graph stay with `noether-scheduler` or whatever you use today.
- **Not a billing system.** The cost ledger tracks intra-company spend so you can refuse runaway agents and report monthly burn — it doesn't issue invoices.

---

## Roadmap

- **Phase 4 (deferred):** postgres broker state, dashboard panels in the noether-registry UI, multi-instance broker with leader election.
- **Phase 5 (research):** record-and-replay LLM responses for cacheable Pure-effect prompts (cuts spend further).
- **Phase 6 (open question):** LDAP / OAuth-backed agent identity instead of static API keys.

See [`docs/research/grid.md`](../../docs/research/grid.md) for the full design discussion, alternatives we ruled out, and the open design questions.

---

## License

Same as `noether` — EUPL-1.2.
