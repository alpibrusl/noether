# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Status

Noether has completed **Phases 0–9** per `docs/roadmap.md`, covering
foundation, stdlib, composition engine, agent interface, hardening,
effects v2, NixExecutor hardening, cloud-registry hardening, runtime
budget enforcement, and distributed execution (`noether-grid-*`).
Treat that file as the source of truth — this summary is prone to
drifting, `docs/roadmap.md` isn't.

## Build & Test Commands

```bash
cargo build                # build all crates
cargo test                 # run all tests (unit + integration + proptest)
cargo clippy -- -D warnings  # lint with warnings-as-errors
cargo fmt --check          # verify formatting
cargo fmt                  # auto-format

# Run specific crate tests
cargo test -p noether-core
cargo test -p noether-store

# Run a single test by name
cargo test -p noether-core reflexivity

# CLI binary
cargo run --bin noether -- version
cargo run --bin noether -- introspect
cargo run --bin noether -- stage search "query"  # semantic search across all stages
cargo run --bin noether -- stage list       # list all stdlib stages
cargo run --bin noether -- stage get <hash> # get stage by ID
cargo run --bin noether -- store stats      # store statistics
cargo run --bin noether -- run graph.json            # execute composition graph
cargo run --bin noether -- run --dry-run graph.json  # type-check and plan only
cargo run --bin noether -- stage activate <hash>     # promote Draft → Active
cargo run --bin noether -- trace <composition_id>    # retrieve past trace
cargo run --bin noether -- compose "problem description"      # LLM-powered composition
cargo run --bin noether -- compose --dry-run "problem"        # graph only, no execution
cargo run --bin noether -- compose --model gemini-2.0-flash "problem"

# Remote registry (noether-cloud)
NOETHER_REGISTRY=http://localhost:8080 cargo run --bin noether -- stage list   # list from remote
NOETHER_REGISTRY=http://localhost:8080 cargo run --bin noether -- compose "problem"  # compose with remote stages
cargo run --bin noether -- --registry https://registry.example.com stage search "query"
```

## What Is Noether

An agent-native verified composition platform. Primary users are AI agents (not humans) that decompose problems into typed, composable stages and execute them with reproducibility guarantees. Named after Emmy Noether's theorem: type signature symmetry guarantees composition correctness.

**Noether is not** a workflow orchestrator, pipeline runner, AI agent framework, or package manager.

## Crate Structure

```
crates/
├── noether-core/     # Type system, effects, stage schema, hashing, signing, stdlib
├── noether-store/    # StageStore trait + MemoryStore + lifecycle validation
├── noether-engine/   # Composition engine — graph format, type checker, planner, executor
└── noether-cli/      # ACLI-compliant CLI with stage/store/run/trace commands
```

### noether-core modules
- `types::NType` — structural type enum (Text, Number, Bool, Bytes, Null, List, Map, Record, Union, Stream, Any)
- `types::checker::is_subtype_of` — structural subtyping with width/depth record subtyping
- `effects::{Effect, EffectSet}` — effect declarations (Pure, Network, Llm, Fallible, etc.)
- `stage::StageSignature` — identity-determining fields (input, output, effects, implementation_hash)
- `stage::StageBuilder` — fluent API for constructing stages with `build_stdlib()` and `build_unsigned()`
- `stage::validation` — `infer_type()` / `infer_type_with_hint()` for JSON→NType inference, `validate_stage()` for example validation
- `stdlib::load_stdlib()` — loads all stdlib stages (deterministic IDs, Ed25519-signed)

### stdlib categories
Scalar, Collections, Control, I/O, LLM primitives, Data, Noether internal, Text processing, Process

### noether-store modules
- `StageStore` trait — put/get/contains/list/update_lifecycle/stats
- `MemoryStore` — in-memory HashMap implementation with lifecycle transition validation
- `validate_transition()` — enforces Draft→Active, Active→Deprecated, Active→Tombstone

### noether-engine modules
- `lagrange` — Composition graph AST (`CompositionNode` enum with 7 operators: Stage, Sequential, Parallel, Branch, Fanout, Merge, Retry), JSON parse/serialize via serde `tag = "op"`, SHA-256 composition ID
- `checker::check_graph()` — recursive graph type checker using `is_subtype_of`, validates all edges
- `planner::plan_graph()` — flattens AST to linear `ExecutionPlan` with dependency tracking, parallelization groups, cost estimation
- `executor::StageExecutor` trait — pluggable single-stage execution interface
- `executor::MockExecutor` — returns store example data, configurable overrides
- `executor::runner::run_composition()` — orchestrates plan execution, data routing, retry, trace collection
- `trace` — `CompositionTrace`, `StageTrace`, `MemoryTraceStore`
- `index` — three-index semantic search (signature, description, example)
  - `EmbeddingProvider` trait + `MockEmbeddingProvider` (hash-based deterministic embeddings)
  - `SemanticIndex::build()` indexes all non-tombstoned stages, `search()` returns weighted fusion of cosine similarity across all 3 indexes (weights: signature 0.3, semantic 0.5, example 0.2)
  - Brute-force cosine similarity (sub-1ms for 50 stages); HNSW can be added later behind same interface
- `llm` — LLM provider abstraction
  - `LlmProvider` trait + `MockLlmProvider` for testing
  - `VertexAiLlmProvider` — calls Vertex AI REST API (Gemini, Claude, Mistral)
  - `VertexAiEmbeddingProvider` — real embeddings via Vertex AI
  - `OpenAiProvider` — calls OpenAI-compatible API (also works with Ollama)
  - `AnthropicProvider` — calls Anthropic messages API
  - Config via env vars: `VERTEX_AI_PROJECT`, `VERTEX_AI_LOCATION`, `VERTEX_AI_TOKEN`, `VERTEX_AI_MODEL`, `OPENAI_API_KEY`, `OPENAI_MODEL`, `OPENAI_API_BASE`, `ANTHROPIC_API_KEY`, `ANTHROPIC_MODEL`
- `agent::CompositionAgent` — translates problem descriptions into composition graphs
  - Searches semantic index for top-20 candidate stages
  - Builds dynamic prompt with candidates + type system + operators
  - Calls LLM, parses Lagrange JSON response, type-checks, retries on failure (up to 3 attempts)

## Architecture (4 Layers)

- **L1 — Nix Execution Layer**: Reproducible Nix-pinned runtime for Python/JS/Bash stages (CAS, binary cache). **Not an isolation boundary** — subprocess inherits host-user privileges. See SECURITY.md.
- **L2 — Stage Store**: Immutable versioned registry of stages identified by SHA-256 content hash (not name). Lifecycle: draft → active → deprecated → tombstone.
- **L3 — Composition Engine**: Type checking, DAG verification, execution planning, structured trace output.
- **L4 — Agent Interface**: ACLI-compliant CLI (the only public API), Composition Agent (LLM-powered), semantic search index.

## Key Design Decisions

- **Structural typing, not nominal**: two types are compatible if their structure matches. `Record { a, b, c }` is subtype of `Record { a, b }` (width subtyping).
- **Content-addressed identity**: stages identified by SHA-256 hash of `StageSignature` canonical JSON. `BTreeMap`/`BTreeSet` used everywhere for deterministic serialization order.
- **Union normalization**: `NType::union()` constructor flattens nested unions, deduplicates, and sorts. This is the only way to create normalized unions.
- **Any is bidirectional escape hatch**: `is_subtype_of(T, Any)` and `is_subtype_of(Any, T)` are both Compatible.
- **Effects reserved in v1**: declared in schema but not enforced until Phase 5.
- **StageSignature vs Stage**: only `StageSignature` fields determine the content hash; metadata (description, examples, cost) does not affect identity.

## Relationship with Caloron

Caloron decides **what** to do (sprint planning, task decomposition). Noether decides **how** to execute it. Caloron calls `noether compose <problem>` and receives structured ACLI results.

## Implementation Phases

| Phase | Focus | Status |
|---|---|---|
| 0 | Foundation — type system, hashing, stage schema | **Done** |
| 1 | Store + Stdlib — 76 stdlib stages, test harness | **Done** |
| 2 | Composition Engine — DAG executor, trace output | **Done** |
| 3 | Agent Interface — Composition Agent, semantic index | **Done** |
| 4 | Hardening — `noether build`, store dedup, browser target | **Done** |
| 5 | Effects v2 — `EffectPolicy`, `infer_effects`, `--allow-effects` | **Done** |
| 6 | NixExecutor hardening — `NixConfig`, error classification, `warmup()` | **Done** |
| 7 | Cloud Registry hardening — `DELETE /stages/:id`, paginated refresh | **Done** |
| 8 | Runtime budget enforcement — `BudgetedExecutor`, `--budget-cents` | **Done** |
| 9 | Grid — subscription pooling — `noether-grid-broker`/`worker` | **Done (v0.4.0)** |

`docs/roadmap.md` is the source of truth; this table will drift otherwise.

## noether-cloud Architecture

noether-cloud (`/home/alpibru/workspace/noether-cloud/`) depends on noether as a library.
Each noether-cloud crate adds HTTP/scheduling/billing infrastructure; all business logic lives in
Noether compositions.

```
noether-cloud/
├── registry/      # Axum HTTP registry (POST /stages, GET /stages/search, POST /compositions/run)
└── scheduler/     # Cron composition runner — reads scheduler.json, fires webhooks on result
```

### Library entry points for noether-cloud
- `noether_engine::checker::check_graph(node, store)` — type-check a graph
- `noether_engine::planner::plan_graph(node, store)` — produce ExecutionPlan
- `noether_engine::executor::runner::run_composition(node, input, executor, id)` — execute
- `noether_engine::index::SemanticIndex::build(store, embedding, config)` — build search index
- `noether_engine::providers::{build_embedding_provider, build_llm_provider}` — env-based provider factory
- `noether_store::StageStore::{get_owned, list_owned}` — owned clones for async contexts

### Registry API routes
| Method | Path | Description |
|---|---|---|
| `POST` | `/stages` | Submit + validate a stage (hash check, signature verify, dedup warn) |
| `GET` | `/stages` | List active stages (paginated) |
| `GET` | `/stages/:id` | Get a stage by ID |
| `PATCH` | `/stages/:id/lifecycle` | Update lifecycle (draft→active→deprecated→tombstone) |
| `GET` | `/stages/search?q=` | Semantic search (three-index cosine similarity) |
| `POST` | `/compositions/run` | Run or dry-run a composition graph |
| `GET` | `/health` | Store stats + index size |

Auth: `X-API-Key` header. Disabled when `NOETHER_API_KEY=""`.

### Scheduler config format (`scheduler.json`)
```json
{
  "store_path": ".noether/registry.json",
  "jobs": [
    {
      "name": "hourly-health",
      "cron": "0 * * * *",
      "graph": "graphs/health-check.json",
      "webhook": "https://hooks.example.com/noether-health"
    }
  ]
}
```
