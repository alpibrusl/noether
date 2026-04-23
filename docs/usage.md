# Usage

The CLI surface is four verbs — `stage`, `compose`, `run`, `trace` —
plus a handful of helpers. Every command emits [ACLI](https://acli.dev)-
shaped JSON (`{ ok: bool, command, result | error, meta }`) so downstream
agents can parse results without brittle stdout scraping.

## Browse the stdlib

```bash
noether stage list                    # all 85 stages
noether stage list --tag text         # filter by tag
noether stage get <id-or-prefix>      # 8-char prefix is enough for stdlib
noether stage search "parse CSV"      # semantic search (three-index fusion)
```

`stage search` runs a hash-based mock embedding locally. Point at a
real embedding provider (Mistral, OpenAI, Vertex) via env vars to get
better-quality results — see [Install → LLM providers](install.md#llm-providers-for-noether-compose).

## Compose a graph from a problem description

```bash
# Uses the first LLM provider configured in your env.
noether compose "parse CSV data and count the rows"

# Don't execute — just show the graph the agent produced.
noether compose --dry-run "extract email domains from a list of addresses"

# Bypass the composition cache to force a fresh LLM call.
noether compose --force "…"

# See the full reasoning — search candidates, LLM prompt, each attempt.
noether compose --verbose "…"
```

Without an LLM key, `compose` runs against the mock provider and
produces placeholder graphs — fine for smoke tests, not for real use.

## Run a graph

```bash
noether run graph.json                      # execute end-to-end
noether run --dry-run graph.json            # type-check + plan only
noether run --input '{"x": 1}' graph.json   # pass runtime input
```

Policy flags:

```bash
# Block stages that declare effect kinds you didn't allow.
noether run --allow-effects pure,fallible graph.json

# Block stages that need capabilities you didn't grant.
noether run --allow-capabilities network,fs-read graph.json

# Reject graphs whose estimated cost exceeds N cents.
noether run --budget-cents 50 graph.json

# Isolation knobs (non-Rust stages only).
noether run --isolate=auto graph.json              # bwrap if present, warn-fallback otherwise
noether run --isolate=bwrap graph.json             # require bwrap; hard error if missing
noether run --isolate=none graph.json              # disable sandbox (warns)
noether run --require-isolation graph.json         # make `auto`'s fallback a hard error (CI)
```

Refinement-predicate enforcement runs automatically when the graph
uses refined types (merged on main; ships in the next tag). Disable
with `NOETHER_NO_REFINEMENT_CHECK=1` only for debugging.

## Replay a past run

Every run writes a `CompositionTrace` to the local trace store:

```bash
noether run graph.json
# → { "ok": true, "result": { "composition_id": "abc123…", "output": … } }

noether trace abc123
# → full trace: per-stage inputs, outputs, timing, any errors
```

The `composition_id` is the SHA-256 of the **pre-resolution** canonical
graph — so the same source graph produces the same id on every run,
regardless of which concrete implementation each signature-pinned node
resolved to. That's what makes replay useful across implementation
rotations.

## Remote registry

Point at a noether-cloud registry to pull stages over HTTP:

```bash
export NOETHER_REGISTRY=https://registry.alpibru.com
noether stage list            # now queries the remote
noether stage search "…"      # semantic search over the remote index
```

Every stage / store command honours `NOETHER_REGISTRY`. `noether run`
and `noether compose` snapshot what the remote reports and execute
locally — there's no "remote execution" mode in the CLI.

## Build a graph into a binary

```bash
noether build graph.json                          # native binary at ./noether-app
noether build graph.json --target browser         # HTML + WASM bundle
noether build graph.json --serve :8080            # native build + serve as HTTP API
```

The built binary accepts runtime input on stdin and prints ACLI JSON on
stdout — same contract as `noether run`. The browser target produces a
self-contained HTML page that runs the pure subset of the graph in the
user's tab.

## Introspect

For agents wiring this up programmatically:

```bash
noether introspect           # full command tree as JSON (ACLI standard)
noether agent-docs           # list of intent-keyed agent playbooks
noether agent-docs compose-a-graph    # a specific playbook
noether agent-docs --search sandbox   # search playbooks by keyword
```

## The scheduler

The `noether-scheduler` binary runs a cron of Lagrange graphs and fires
webhooks with the result. Configure via `scheduler.json`:

```json
{
  "store_path": ".noether/registry.json",
  "jobs": [
    {
      "name": "hourly-health-check",
      "cron": "0 * * * *",
      "graph": "graphs/health-check.json",
      "webhook": "https://hooks.example.com/noether-health"
    }
  ]
}
```

```bash
noether-scheduler --config scheduler.json
```

Stateless per run — the graph's composition id + trace go to the
webhook; restart-safety is your responsibility.
