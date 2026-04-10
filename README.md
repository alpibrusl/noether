# Noether

**Verified composition platform for AI agents.**

Named after Emmy Noether: type signature symmetry guarantees composition correctness.

```bash
cargo build --release -p noether-cli
export PATH="$PWD/target/release:$PATH"

noether compose "parse CSV data and count the number of rows"
# → { "ok": true, "data": { "output": 3.0 } }
```

> **[See the demos →](./demo/index.md)** — type safety, parallel execution, stage reuse, and the full agent flow.

---

## What is Noether?

Noether is a platform where AI agents decompose problems into **typed, composable stages** and execute them with reproducibility guarantees.

A **stage** is an immutable, content-addressed unit of computation:

```
stage: { input: T } → { output: U }
identity: SHA-256(signature)   ← not a name, not a version, a hash
```

Two stages with the same hash are provably the same computation — forever, across machines, across repos.

The **composition engine** type-checks stage graphs before executing them. If `stage_a` outputs `Record { url: Text, score: Number }` and `stage_b` expects `Record { url: Text }`, the checker verifies compatibility using structural subtyping — no runtime surprises.

Noether is **not** a workflow orchestrator, AI agent framework, or pipeline runner. It is infrastructure for agents that need to compose and verify computation.

---

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│  L4 — Agent Interface                                    │
│  ACLI-compliant CLI · Composition Agent · Semantic Index │
├─────────────────────────────────────────────────────────┤
│  L3 — Composition Engine                                 │
│  Type checker · DAG planner · Executor · Trace store     │
├─────────────────────────────────────────────────────────┤
│  L2 — Stage Store                                        │
│  Immutable SHA-256 registry · Lifecycle · 80+ stdlib     │
├─────────────────────────────────────────────────────────┤
│  L1 — Execution Layer                                    │
│  Nix hermetic sandboxing · Python/JS/Bash runtimes       │
└─────────────────────────────────────────────────────────┘
```

### Crate structure

| Crate | Purpose |
|---|---|
| `noether-core` | Type system (`NType`), effects, stage schema, stdlib (80+ stages), Ed25519 signing |
| `noether-store` | `StageStore` trait, `MemoryStore`, `JsonFileStore`, lifecycle validation |
| `noether-engine` | Lagrange graph format, type checker, planner, executor, semantic index, LLM agent |
| `noether-cli` | ACLI-compliant CLI — `stage`, `store`, `run`, `build`, `compose`, `trace` |

---

## Quickstart

### Prerequisites
- Rust 1.75+
- Nix (optional, required for Python/JS stage execution)

```bash
git clone https://github.com/alpibrusl/noether
cd noether
cargo build --release -p noether-cli
export PATH="$PWD/target/release:$PATH"
noether version
```

### Run a composition

```bash
# List all stdlib stages
noether stage list

# Search by capability
noether stage search "format HTML report"

# Dry-run a composition graph (type-check only)
noether run --dry-run graph.json

# Execute with input
noether run graph.json --input '{"query": "rust async", "limit": 10}'

# Build a standalone binary
noether build graph.json --output ./my-app

# Run as HTTP microservice
./my-app --serve :8080
```

### LLM-powered composition

```bash
# Set one LLM provider:
export VERTEX_AI_PROJECT=your-project VERTEX_AI_MODEL=gemini-2.5-flash
# or: export OPENAI_API_KEY=sk-...
# or: export ANTHROPIC_API_KEY=sk-ant-...

noether compose "parse CSV data and count the number of rows"
noether compose --dry-run "convert text to uppercase and get its length"
noether compose --verbose "sort a list and take the top 3"  # show reasoning
```

---

## Stage format

Stages are defined as JSON and added to the store:

```json
{
  "name": "my_transform",
  "description": "Transforms a search query into structured results",
  "input":  { "kind": "Record", "value": { "query": {"kind": "Text"}, "limit": {"kind": "Number"} } },
  "output": { "kind": "List",   "value": { "kind": "Record", "value": { "title": {"kind": "Text"}, "url": {"kind": "Text"} } } },
  "language": "python",
  "examples": [
    { "input": {"query": "rust async", "limit": 5}, "output": [] }
  ],
  "implementation": "def execute(input_value):\n    ..."
}
```

```bash
noether stage add my-stage.json
# → { "id": "a4f9bc3e..." }   ← SHA-256 of the signature
```

The ID never changes unless the type signature or implementation changes.

---

## Composition graph (Lagrange format)

> **Why "Lagrange"?** The project is named after Emmy Noether, whose theorem
> connects symmetries to conservation laws via the *Lagrangian* (named after
> Joseph-Louis Lagrange). A Lagrange graph is what you write down to describe
> a computation; Noether's type system guarantees its correctness — the same
> relationship as Lagrangian ↔ conservation law.

```json
{
  "description": "Research digest pipeline",
  "version": "0.1.0",
  "root": {
    "op": "Sequential",
    "stages": [
      {
        "op": "Parallel",
        "branches": {
          "results": { "op": "Stage", "id": "a4f9bc3e..." },
          "topic":   { "op": "Const", "value": "rust async" }
        }
      },
      { "op": "Stage", "id": "b7d2e1a4..." }
    ]
  }
}
```

**Operators:** `Stage` · `Sequential` · `Parallel` · `Branch` · `Fanout` · `Merge` · `Retry` · `Const`

The type checker validates every edge before execution.

---

## Built binaries

`noether build` compiles a composition graph into a self-contained binary with all custom stages bundled:

```bash
noether build graph.json --output ./fleet-briefing

# CLI mode
./fleet-briefing --input '{"fleet_name": "Nordic GmbH", "routes": [...]}'

# HTTP microservice (browser dashboard included)
./fleet-briefing --serve :8080
# GET  /        → browser dashboard (auto-populated with example input)
# POST /        → execute graph, returns HTML or JSON
# GET  /health  → liveness check
```

---

## Type system

Noether uses **structural subtyping** — no class hierarchy, no nominal types:

```
NType := Text | Number | Bool | Bytes | Null
       | List<T>
       | Record { field: T, ... }    -- width subtyping
       | Union<T1 | T2 | ...>        -- normalized, deduplicated
       | Stream<T>
       | Any                         -- bidirectional escape hatch
```

`Record { name: Text, score: Number, url: Text }` is a subtype of `Record { name: Text }` — width subtyping means a stage that needs a subset of fields always accepts a richer record.

---

## Store & lifecycle

Stages follow a lifecycle: `Draft → Active → Deprecated → Tombstone`

```bash
noether store stats        # store statistics
noether store health       # audit: signatures, missing examples, orphans
noether store dedup        # find near-duplicate stages (cosine similarity)
noether store dedup --apply  # deprecate confirmed duplicates (with successor pointer)
noether stage activate <id>  # promote Draft → Active
```

---

## Semantic search

Every stage is indexed across three vectors (signature, description, examples). Search uses cosine similarity with weighted fusion:

```bash
noether stage search "convert temperature units"
noether stage search "parse and validate JSON schema"
```

The same index powers `noether compose` — the LLM agent receives the top-20 candidates and constructs a composition graph.

---

## Persistent state (KV store)

Stages that need to persist state across runs use the built-in KV store (SQLite, `~/.noether/kv.db`):

```python
def execute(input_value):
    import sqlite3, pathlib
    db = sqlite3.connect(str(pathlib.Path.home() / '.noether' / 'kv.db'))
    db.execute('CREATE TABLE IF NOT EXISTS kv (namespace TEXT, key TEXT, value TEXT, PRIMARY KEY(namespace,key))')
    # read/write state across invocations
```

Or via the stdlib stages: `kv_get`, `kv_set`, `kv_delete`, `kv_exists`, `kv_list`.

---

## Stdlib (80+ stages)

| Category | Stages |
|---|---|
| Scalar | `parse_number`, `parse_bool`, `to_string`, `parse_json`, `to_json` |
| Collections | `list_map`, `list_filter`, `list_reduce`, `list_sort`, `list_take`, `list_flatten`, `zip`, `group_by` |
| Control | `identity`, `const`, `branch_if`, `retry`, `validate_schema`, `coerce_type` |
| I/O | `http_get`, `http_post`, `read_file`, `write_file`, `kv_get`, `kv_set`, `kv_delete`, `kv_exists` |
| LLM | `llm_complete`, `llm_classify`, `llm_extract`, `llm_embed` |
| Data | `json_merge`, `json_path`, `csv_parse`, `csv_format`, `json_schema_validate`, `diff_objects`, `template_render` |
| Text | `regex_match`, `regex_replace`, `text_split`, `text_join`, `text_trim`, `text_contains` |
| Noether | `stage_get`, `stage_search`, `compose_graph`, `kv_list`, `list_length`, `format_trace` |

---

## Relationship with AI agents

Noether is designed to be called **by** agents, not to contain them:

```bash
# An AI agent calls Noether to execute a sub-problem
noether compose "extract key entities from these documents" --input '...'

# The agent receives structured ACLI output and continues
{
  "ok": true,
  "command": "noether",
  "data": { "output": [...] },
  "meta": { "version": "0.1.0" }
}
```

**Token efficiency:** the composition graph travels as compact JSON (not prompt text). Only the final LLM stages consume tokens. Benchmarks show 60-80% token reduction vs. naïve chaining.

---

## Roadmap

| Phase | Status |
|---|---|
| 0 — Foundation (type system, hashing, stage schema) | ✅ Done |
| 1 — Store + Stdlib (80+ stages, test harness) | ✅ Done |
| 2 — Composition Engine (DAG executor, traces) | ✅ Done |
| 3 — Agent Interface (Composition Agent, semantic index) | ✅ Done |
| 4 — Hardening (signatures, dedup, store health) | ✅ Done |
| 5 — Effects v2 (inference & enforcement, `--allow-effects`) | ✅ Done |
| 6 — NixExecutor hardening (timeout, error classification, warmup) | ✅ Done |
| 7 — Cloud Registry hardening (DELETE, paginated refresh, scheduler) | ✅ Done |
| 8 — Runtime budget enforcement (`--budget-cents`, `BudgetedExecutor`) | ✅ Done |

See [roadmap.md](./docs/roadmap.md) for near-term improvements and future directions.

---

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md).

Areas where contributions are especially welcome:
- New stdlib stages (any domain)
- Language runtimes beyond Python (JS, Ruby, Go)
- LLM provider integrations (OpenAI, Anthropic, Mistral, Vertex AI supported; Ollama via OpenAI-compatible API)
- Type system extensions (generic types, row polymorphism)

---

## License

European Union Public Licence v1.2 (EUPL-1.2) — see [LICENSE](./LICENSE).

The EUPL is a copyleft licence compatible with GPL, LGPL, AGPL, MPL, EUPL, and others.
It was designed specifically for public sector and open-source software within the EU.
