# NOETHER
## Agent-Native Verified Composition Platform
### Implementation Roadmap — v1.0

> Every symmetry in the composition algebra corresponds to a conservation law in execution. Identical inputs + identical pipeline spec = identical outputs. Always.

*April 2026*

---

## Table of Contents

1. [Vision and Core Philosophy](#1-vision-and-core-philosophy)
2. [Architecture Overview](#2-architecture-overview)
3. [The Type System](#3-the-type-system)
4. [The Stage Store](#4-the-stage-store)
5. [The Composition Engine](#5-the-composition-engine)
6. [The Composition Agent (Internal)](#6-the-composition-agent-internal)
7. [The ACLI Interface](#7-the-acli-interface)
8. [Security Model](#8-security-model)
9. [Integration with External AI Agents](#9-integration-with-external-ai-agents)
10. [Implementation Roadmap](#10-implementation-roadmap)
11. [Technology Stack](#11-technology-stack)
12. [Success Metrics by Phase](#12-success-metrics-by-phase)
13. [Open Questions and Deferred Decisions](#13-open-questions-and-deferred-decisions)

---

## 1. Vision and Core Philosophy

Noether is an agent-native platform for verified, reproducible computation. It is not a pipeline runner, not a workflow orchestrator, and not a low-code tool for humans. Its primary users are AI agents that need to decompose complex problems into smaller verifiable units, find existing solutions in a shared store, compose them into graphs, and execute them with mathematical guarantees of reproducibility.

The platform takes its name from Emmy Noether's theorem: every symmetry implies a conservation law. In Noether's case, the symmetry is the type signature of a stage — if it holds, the composition is guaranteed to be correct. Complexity is structural, not accidental.

> **Design goal:** building complex solutions becomes a problem of designing compositions, not writing code. An agent that understands types can build arbitrarily complex systems from simple verified parts.

### 1.1 What Noether Is NOT

- Not a human-facing CLI tool (though it exposes one for introspection and debugging)
- Not a general-purpose workflow engine (Airflow, Prefect, Temporal)
- Not an AI agent framework (those call Noether as a tool)
- Not a package manager (it uses Nix for that)

### 1.2 Philosophical Foundations

| Principle | Origin | How Noether implements it |
|---|---|---|
| Content-addressed identity | Nix + Unison | Every stage is identified by hash of its content, never by name |
| Pure functional composition | Category theory | Stages are morphisms; composition is verified at the type level |
| Reproducibility as invariant | Nix | Same inputs + same spec = same outputs, always, everywhere |
| Immutability | Unison | Stages never change; new versions create new identities |
| Effects as first-class | Algebraic effects theory | Effects declared in signature; reserved in v1, enforced in v2 |

---

## 2. Architecture Overview

Noether is structured in four layers. Each layer has a single responsibility and communicates with adjacent layers through well-defined contracts.

| Layer | Name | Responsibility |
|---|---|---|
| L1 | Nix Execution Layer | Hermetic sandboxed execution, content-addressed store, binary cache, runtime management |
| L2 | Stage Store | Immutable versioned registry of stages with type signatures and semantic metadata |
| L3 | Composition Engine | Type checking, DAG verification, execution planning, data routing between stages |
| L4 | Agent Interface | ACLI-compliant CLI, Composition Agent, semantic search index |

External agents (Claude, GPT-4, any LLM-based system) interact exclusively with L4. They never need to understand L1, L2, or L3. The ACLI contract is the only public API.

### 2.1 The Relationship with Nix

Nix is not a deployment target — it is the execution model. Specifically:

- **Nix store as CAS** — every stage implementation is stored as a Nix derivation, content-addressed by default. No reimplementation needed.
- **Nix sandbox** — every stage executes in hermetic isolation: no access to filesystem outside declared inputs, no network unless explicitly declared in capabilities.
- **Binary cache** — compiled stages are shared across machines via Nix substituters. An agent on machine B gets the same stage that ran on machine A without recompilation.
- **Nixpkgs as stdlib of runtimes** — Python, Node, Rust, R, WASM runtimes are Nix packages. Noether never manages runtimes — it declares them as Nix dependencies.

What Noether does **not** use from Nix:

- The Nix expression language as a user-facing syntax — Noether generates Nix expressions internally. Agents and developers never write Nix.
- NixOS modules — these are optional and serve as a premium deployment target, not a requirement.

### 2.2 The Relationship with Unison

Noether adopts Unison's core identity model without adopting its runtime:

- **Code is identified by content hash, not by name** — renaming a stage creates no breaking changes. All references to a stage use its hash, never a mutable string name.
- **No broken builds** — if a stage's dependencies are in the store and its hash is known, it always runs. No dependency hell, no version conflicts.

What Noether does **not** use from Unison:

- The Unison runtime or VM — stages are polyglot (Python, Rust, WASM, etc.)
- Algebraic effects system from Unison — Noether designs its own effects model

---

## 3. The Type System

The type system is the central arbitration mechanism of Noether. If two stages' types are compatible, they can be composed. If not, composition is rejected before any execution happens. The type checker is the most important component in the entire system.

> Types in Noether are **structural, not nominal**. Two types are compatible if their structure matches, regardless of name. This allows stages from different authors to compose without coordination.

### 3.1 Type Primitives (v1)

| Type | Description | Example |
|---|---|---|
| `Text` | UTF-8 string | `"hello world"` |
| `Number` | 64-bit float | `3.14` |
| `Bool` | true / false | `true` |
| `Bytes` | Raw binary | image data |
| `Null` | Absence of value | `null` |
| `List<T>` | Homogeneous ordered collection | `List<Number>` |
| `Map<K, V>` | Key-value store | `Map<Text, Number>` |
| `Record { field: T }` | Structural product type | `{ name: Text, age: Number }` |
| `T \| U` | Sum type / union | `Text \| Null` |
| `Stream<T>` | Lazy sequence | `Stream<Record>` |
| `Any` | Escape hatch — disables type checking | `Any` |

### 3.2 Stage Signature

Every stage declares a complete signature:

```
stage {
  id:           <sha256-hash>          // identity = content hash
  input:        Type                   // structural input type
  output:       Type                   // structural output type
  effects:      EffectSet | Unknown    // v1: always Unknown unless stdlib
  capabilities: [network?, fs:read?, fs:write?, gpu?, llm?]
  cost:         { time_ms_p50, tokens_est, memory_mb }
  description:  Text                   // semantic metadata for search
  examples:     List<{input, output}>  // used for embedding generation
}
```

The `effects` field is reserved in v1. All user-created stages default to `Unknown`. Stdlib stages are annotated with correct effects from the start, establishing the pattern for v2.

### 3.3 Composition Operators

| Operator | Syntax | Semantics | Type rule |
|---|---|---|---|
| Sequential | `A >> B` | Output of A feeds input of B | `output(A)` must be subtype of `input(B)` |
| Parallel | `A \|\|\| B` | A and B execute concurrently | inputs are independent; outputs merge into Record |
| Branch | `if(pred, A, B)` | Conditional routing | `output(A)` must equal `output(B)` |
| Fanout | `A >> [B, C, D]` | A's output sent to B, C, D | `input(B) = input(C) = input(D) = output(A)` |
| Merge | `[A, B, C] >> D` | Outputs of A, B, C merged | output types must be compatible with `input(D)` |
| Retry | `retry(A, n)` | Retry A up to n times on failure | same type as A; requires `Fallible` effect in v2 |

### 3.4 Effects System (v1 reserved, v2 enforced)

The following effects are defined in the schema from day one but not enforced in v1:

| Effect | Meaning | v1 behavior | v2 behavior |
|---|---|---|---|
| `Pure` | No side effects, deterministic, cacheable forever | Ignored | Composition Engine auto-caches |
| `Network` | Makes external HTTP calls | Ignored | Requires explicit capability declaration |
| `LLM<model>` | Calls a language model | Ignored | Cost estimation enabled; `NonDeterministic` implied |
| `NonDeterministic` | Same inputs may give different outputs | Ignored | Disables caching; forces explicit acknowledgment |
| `Fallible` | May fail for non-type reasons | Ignored | `retry()` operator requires this; type checker enforces |
| `Cost<$n>` | Estimated monetary cost per call | Stored only | Budget constraints in composition planning |
| `Unknown` | Default for user stages in v1 | No enforcement | Treated conservatively as all effects possible |

---

## 4. The Stage Store

The Stage Store is the immutable registry of all known stages. It is content-addressed: a stage's identity is the SHA-256 hash of its implementation + signature. No two stages share an identity. No stage is ever deleted — only deprecated with a pointer to its successor.

### 4.1 The Bootstrap Problem: Stdlib

The store cannot start empty. Noether ships with a standard library of approximately 50 primitive stages covering the fundamental operations needed to bootstrap composition:

| Category | Stages | Notes |
|---|---|---|
| Scalar | `to_text`, `to_number`, `to_bool`, `parse_json`, `to_json` | All Pure |
| Collections | `map`, `filter`, `reduce`, `sort`, `group_by`, `flatten`, `zip`, `take` | All Pure |
| Control | `branch`, `retry`, `fallback`, `timeout`, `race`, `parallel` | Fallible effects |
| I/O | `read_file`, `write_file`, `http_get`, `http_post`, `http_put`, `stdin_read`, `stdout_write`, `env_get` | Network/FS effects |
| LLM primitives | `llm_complete`, `llm_embed`, `llm_classify`, `llm_extract` | LLM + NonDeterministic |
| Data | `csv_parse`, `csv_write`, `json_schema_validate`, `arrow_from_records`, `records_to_arrow`, `json_merge`, `json_path` | Pure |
| Noether internal | `store_search`, `store_add`, `composition_verify`, `trace_read`, `stage_describe`, `type_check` | Internal effects |
| Text processing | `text_split`, `text_join`, `regex_match`, `regex_replace`, `text_template`, `text_hash` | Pure |

These 50 stages are written by hand, fully annotated with effects, signed by the Noether project key, and represent the grammar from which the Composition Agent can synthesize almost anything else through composition.

### 4.2 Stage Lifecycle

| State | Meaning | Transition |
|---|---|---|
| `draft` | Created by Composition Agent, not yet tested | → `active` (passes test harness) |
| `active` | In the store, available for composition | → `deprecated` (superseded) |
| `deprecated` | Replaced by successor, still functional | `successor_id` field points to replacement |
| `tombstone` | Hash recorded but implementation removed | Rare; only for security reasons |

### 4.3 Stage Creation: Three Paths

#### Path 1 — Synthesis (primary path)

The Composition Agent detects that no existing stage satisfies a required signature. It generates candidate implementations using an LLM, runs them against the test harness, and if they pass, registers them in the store. This is the expected path for 80% of new stages.

#### Path 2 — Human authoring

A developer writes a stage using the Noether SDK, defines its complete signature including effects, and runs the registration command. The test harness validates the implementation before the stage enters the store. Intended for low-level, performance-critical, or security-sensitive stages.

#### Path 3 — Composition promotion

A frequently-used composition of N stages is automatically collapsed into a single atomic stage. This is a performance optimization: the new stage has the same observable signature as the composition but executes with lower overhead. The original composition is preserved for auditability.

### 4.4 Deduplication and Store Health

Before any stage enters the store, the system computes its similarity against existing stages:

- **Semantic similarity check** — if cosine similarity > 0.92 against an existing stage's description embedding, insertion is blocked and the existing stage is returned instead.
- **Type signature as arbiter** — two stages with identical semantics but different type signatures are legitimately different and both are allowed.
- **Periodic retro** — the store runs a canonicalization process that clusters near-duplicate stages, proposes merges, deprecates unused stages (not accessed in N sprints), and suggests more general stages that cover multiple similar cases.

---

## 5. The Composition Engine

The Composition Engine is the core runtime of Noether. It receives a composition graph (a DAG of stage references with composition operators), verifies it, plans its execution, routes data between stages, and returns structured results.

### 5.1 Execution Modes

The engine selects the execution mode for each stage automatically based on the data volume declared in the type signature:

| Mode | Data size | Transport | Use case |
|---|---|---|---|
| Inline (WASM) | < 1 MB | In-process memory | Small transforms, LLM calls, logic stages |
| Process (IPC) | 1 MB – 1 GB | Nix store temp file (content-addressed) | Data processing, file operations |
| Remote (gRPC) | > 1 GB or streaming | Apache Arrow over shared memory | Heavy data, GPU workloads, streaming |

### 5.2 Execution Pipeline

For every composition graph, the engine executes the following steps in order:

1. **Parse** — deserialize the composition graph from the agent's request
2. **Resolve** — look up all stage hashes in the store; fail fast if any are missing
3. **Type check** — verify all composition operators against type signatures; fail with precise error location if mismatch
4. **Plan** — topological sort; assign execution modes; identify parallelizable subgraphs
5. **Execute** — run stages according to plan; collect structured traces
6. **Return** — emit ACLI-compliant structured output with full trace

### 5.3 Observability: The Trace

Every execution produces a structured trace automatically. This trace is the primary output when something fails and is always included in the ACLI response envelope:

```json
{
  "composition_id": "<hash-of-the-graph>",
  "started_at": "2026-04-05T10:00:00Z",
  "duration_ms": 342,
  "stages": [
    {
      "stage_id": "<hash>",
      "status": "ok",
      "duration_ms": 12,
      "input_hash": "<hash>",
      "output_hash": "<hash>"
    },
    {
      "stage_id": "<hash>",
      "status": "failed",
      "error": {
        "code": "TYPE_MISMATCH",
        "expected": "DataFrame",
        "got": "List<Row>",
        "stage_position": 3
      }
    }
  ]
}
```

---

## 6. The Composition Agent (Internal)

The Composition Agent is an LLM-powered agent that lives inside Noether. Its sole responsibility is to translate a problem description into a valid composition graph. It is not a general-purpose agent — it reasons exclusively about types, stages, and compositions. External agents call Noether without needing to understand its internals.

> The Composition Agent is an implementation detail of Noether. External agents see a black box: problem in, verified composition out. The intelligence is encapsulated.

### 6.1 Why Not Fine-tuning

The Composition Agent is not fine-tuned. The Stage Store is dynamic — it grows with every sprint. A fine-tuned model cannot know stages added after its training cutoff. Fine-tuning teaches reasoning style; it cannot encode a growing catalogue. RAG over the semantic index solves the knowledge problem; a well-designed prompt solves the reasoning problem.

### 6.2 Two-Phase Search

#### Phase 1 — Vector Search (< 5ms)

Before involving any LLM, the agent runs a pure vector similarity search over three pre-computed indexes in memory:

- **Signature index** — embeddings of type signatures (`input → output`)
- **Semantic index** — embeddings of natural language stage descriptions
- **Example index** — embeddings of input/output example pairs

The search returns the top-20 candidates. No LLM is involved. This phase is sub-5ms regardless of store size.

#### Phase 2 — LLM Re-ranking (~200ms)

The LLM receives only the top-20 candidates with their full signatures, the problem description, and the available input types. It decides:

- Does one candidate solve the problem directly? → return that stage
- Can 2–3 candidates be composed to solve it? → return the composition
- Is a new stage needed? → describe its signature precisely for synthesis

The LLM never sees the full store. It reasons over a small, pre-filtered candidate set. This is what makes the system scalable.

### 6.3 Composition Agent System Prompt Structure

The system prompt is assembled dynamically for each request:

| Section | Content | Size |
|---|---|---|
| Role | You are the Composition Agent of Noether... | ~200 tokens |
| Type system | The complete type grammar with examples | ~500 tokens |
| Composition operators | All operators with type rules | ~300 tokens |
| Top-20 candidates | Stage signatures + descriptions from vector search | ~1000 tokens |
| Problem context | The input problem + available types | ~200 tokens |
| Output contract | JSON schema of the expected response | ~200 tokens |

### 6.4 Stage Synthesis

When the agent determines that no existing stage satisfies the required signature, it initiates synthesis:

1. The agent generates a precise stage specification: name, description, input type, output type, effects (`Unknown` for synthesized), and implementation language.
2. An LLM generates the implementation code.
3. The test harness runs the implementation against generated input/output examples derived from the type signature.
4. If tests pass, the stage is registered in the store with `status: active`.
5. If tests fail, the agent retries synthesis up to 3 times before escalating to the caller.

---

## 7. The ACLI Interface

Noether's public surface is a CLI built on the [ACLI specification](https://alpibrusl.github.io/acli). ACLI enables agent-driven tool discovery at runtime: an agent runs `noether --help` and learns the complete capability surface without any pre-loaded schema. The tool is self-documenting.

### 7.1 Primary Commands

| Command | Description | Primary user |
|---|---|---|
| `noether compose <problem>` | Translate a natural language problem into a composition graph and execute it | AI agents |
| `noether run <graph.json>` | Execute a pre-defined composition graph directly | AI agents |
| `noether stage search <query>` | Search the store by semantic query | AI agents + developers |
| `noether stage add <spec.json>` | Register a new stage from a specification file | Developers |
| `noether store retro` | Run the store canonicalization process | Automated / scheduled |
| `noether introspect` | Return full command tree as JSON (ACLI standard) | AI agents |
| `noether trace <composition_id>` | Retrieve the execution trace for a past composition | AI agents + developers |

### 7.2 Output Contracts

All commands return ACLI-compliant structured JSON. Every response envelope includes:

- **`status`** — `ok | error | partial`
- **`data`** — the primary result payload
- **`trace`** — execution trace (always present on `compose`/`run`)
- **`hints`** — actionable suggestions if status is `error` (ACLI standard)
- **`cost`** — actual cost incurred (tokens, time, money estimate)

### 7.3 Dry-run Support

Every mutating command supports `--dry-run`. This allows an external agent to verify what Noether would do before committing. The dry-run response includes the full composition graph, type check results, estimated cost, and any warnings — but executes nothing. Critical for agents that need to reason about cost before calling expensive stages.

---

## 8. Security Model

Noether executes arbitrary code from the store. Security is a first-class design constraint, not an afterthought. Three layers compose to give defense-in-depth.

### Layer 1 — Nix Sandbox

Every stage executes inside Nix's hermetic build sandbox. By default a stage has no access to the filesystem outside its declared inputs, no network access, no access to environment variables, and no access to other processes. Violations cause immediate execution failure.

### Layer 2 — Capability Declarations

The stage signature includes a `capabilities` field that explicitly enumerates what the stage needs beyond sandbox defaults: `network`, `fs:read`, `fs:write`, `gpu`, `llm`. The Composition Engine inspects this field before execution. A stage that tries to use network without declaring it in capabilities will fail at the sandbox level and the discrepancy is recorded in the trace as a security event.

### Layer 3 — Store Signing

Every stage in the store carries a cryptographic signature (Ed25519). The Noether stdlib is signed by the Noether project key. Stages authored by external developers are signed by their author key. Stages synthesized by the Composition Agent are signed by the agent's ephemeral key for that session. The Composition Engine refuses to execute stages with invalid or missing signatures.

### What Noether Does Not Guarantee

Noether does not attempt to solve the full problem of adversarial stage injection. A stage that correctly declares its capabilities and passes signing verification can still produce undesirable outputs. This is the responsibility of the external agent that decides whether to trust a particular stage source.

---

## 9. Integration with External AI Agents

Noether is designed as a tool that AI agents call, not as an agent itself. The boundary is strict: Noether handles computation (how) and agents handle reasoning (what and why). This separation must be maintained as the ecosystem evolves.

| Concern | External Agent | Noether |
|---|---|---|
| What to do | ✓ Goal decomposition, task planning | ✗ |
| How to execute it | ✗ | ✓ Composition, type checking, execution |
| Agent orchestration | ✓ Multi-step coordination | ✗ |
| Computation verification | ✗ | ✓ Type guarantees, reproducibility |
| LLM reasoning | ✓ High-level planning and strategy | ✓ Composition Agent (scoped to types/stages only) |
| Human interface | ✓ User-facing output, decisions | ✗ (agents only) |

> **The clean boundary:** An external agent tells Noether WHAT problem needs solving. Noether tells the agent HOW it was solved and whether it was correct. Neither crosses into the other's domain.

From any agent's perspective, Noether is an ACLI tool. The agent calls `noether compose <problem>` and receives a structured result. It does not need to know about stages, type signatures, or Nix.

---

## 10. Implementation Roadmap

### Phase Overview

| Phase | Name | Duration | Outcome |
|---|---|---|---|
| 0 | Foundation | 3 weeks | Nix integration, type system core, stage schema |
| 1 | Store + Stdlib | 4 weeks | Stage Store with 50 stdlib stages, test harness |
| 2 | Composition Engine | 4 weeks | Type checker, DAG executor, trace output |
| 3 | Agent Interface | 3 weeks | Composition Agent, semantic index, ACLI CLI |
| 4 | Hardening | 3 weeks | Security model, deduplication, store retro |
| 5 | Effects (v2) | 4 weeks | Effect inference, enforcement, optimization |

**Total: ~21 weeks**

---

### Phase 0 — Foundation (Weeks 1–3)

**Goal:** establish the non-negotiable invariants of the system. Nothing else can be built correctly without this.

#### 0.1 Repository structure

- **`noether-core`** — Rust crate: type system, hashing, stage schema
- **`noether-store`** — Rust crate: CAS implementation over Nix store
- **`noether-engine`** — Rust crate: composition engine (empty at this phase)
- **`noether-cli`** — Rust binary: ACLI-compliant CLI (skeleton at this phase)
- **`noether-sdk-python`** — Python package: stage authoring SDK

#### 0.2 Type system implementation

- Implement all v1 primitive types as Rust enums
- Implement structural subtyping: `Record { a: T, b: U }` is a subtype of `Record { a: T }`
- Implement type unification for composition operator verification
- Implement the `EffectSet` type with `Unknown` as default
- Write the complete type checker: given two types, returns `Compatible | Incompatible(reason)`
- Property-based tests covering all type combinations

#### 0.3 Stage schema and identity

- Define the `Stage` struct in Rust with all signature fields
- Implement content hashing: SHA-256 of canonical JSON serialization of `(input_type, output_type, effects, implementation_hash)`
- Implement stage signing and verification
- Define the three store states: `draft`, `active`, `deprecated`

#### 0.4 Nix integration

- Write the Nix expression generator: given a Stage, produce a valid derivation
- Implement the store wrapper: read/write stages as Nix derivations
- Verify hermetic sandbox enforcement for Python and Node runtimes
- Set up binary cache configuration for development environment

---

### Phase 1 — Store and Stdlib (Weeks 4–7)

**Goal:** a populated, queryable store. The system can answer "do you have a stage that does X?"

#### 1.1 Stage Store implementation

- Implement the full stage lifecycle: `draft → active → deprecated → tombstone`
- Implement the successor pointer graph for deprecation chains
- Implement the test harness: given a stage and its signature, generate test cases from examples and run them
- Implement the registration flow for all three authoring paths (synthesis, human, promotion)

#### 1.2 Stdlib — 50 stages

- **Scalar** (5): `to_text`, `to_number`, `to_bool`, `parse_json`, `to_json`
- **Collections** (8): `map`, `filter`, `reduce`, `sort`, `group_by`, `flatten`, `zip`, `take`
- **Control** (6): `branch`, `retry`, `fallback`, `timeout`, `race`, `parallel`
- **I/O** (8): `read_file`, `write_file`, `http_get`, `http_post`, `http_put`, `stdin_read`, `stdout_write`, `env_get`
- **LLM primitives** (4): `llm_complete`, `llm_embed`, `llm_classify`, `llm_extract`
- **Data** (7): `csv_parse`, `csv_write`, `json_schema_validate`, `arrow_from_records`, `records_to_arrow`, `json_merge`, `json_path`
- **Noether internal** (6): `store_search`, `store_add`, `composition_verify`, `trace_read`, `stage_describe`, `type_check`
- **Text processing** (6): `text_split`, `text_join`, `regex_match`, `regex_replace`, `text_template`, `text_hash`

All stdlib stages: fully effect-annotated, signed by Noether project key, tested with minimum 5 example pairs each.

#### 1.3 Store CLI commands

- `noether stage add <spec.json>` — register a stage
- `noether stage get <hash>` — retrieve a stage by identity
- `noether stage list` — list all active stages with signatures
- `noether store stats` — count, size, effect distribution

---

### Phase 2 — Composition Engine (Weeks 8–11)

**Goal:** given a valid composition graph, execute it correctly and produce a structured trace.

#### 2.1 Composition graph format (Lagrange)

- Define the Lagrange format: JSON representation of composition graphs using the five operators
- Implement the parser: Lagrange JSON → internal AST
- Implement the serializer: internal AST → Lagrange JSON
- Define the ACLI output contract for composition results

#### 2.2 Type checker for graphs

- Implement graph-level type checking: walk the DAG, verify every edge using the stage type checker
- Produce precise error messages: *"Stage 3 (hash: abc...) expects DataFrame but stage 2 produces List\<Row\>. Consider adding a `records_to_dataframe` stage."*
- Implement subgraph extraction: identify which subgraphs are independently parallelizable

#### 2.3 Execution planner

- Topological sort of the DAG
- Execution mode assignment (inline/process/remote) based on type volume annotations
- Parallelization plan: identify independent branches
- Cost estimation: sum of stage cost estimates

#### 2.4 Executor

- Implement the inline executor (WASM runtime for small stages)
- Implement the process executor (Nix sandbox, IPC via store temp files)
- Implement the remote executor (gRPC + Arrow, stub for v1)
- Implement the trace collector: structured JSON trace for every execution
- Implement the retry executor: wraps any stage with configurable retry policy

#### 2.5 Commands

- `noether run <graph.json>` — execute a composition graph
- `noether run --dry-run <graph.json>` — verify and plan without executing
- `noether trace <composition_id>` — retrieve past trace

---

### Phase 3 — Agent Interface (Weeks 12–14)

**Goal:** an external agent can call `noether compose "problem description"` and receive a working result.

#### 3.1 Semantic index

- Generate embeddings for all 50 stdlib stages at store registration time
- Implement the three-index structure in memory (signature, semantic, example)
- Implement cosine similarity search with top-K results
- Implement index update on every `store.add` event
- Benchmark: sub-5ms for top-20 search on a 1000-stage store

#### 3.2 Composition Agent

- Implement the dynamic system prompt assembler: role + type system + operators + top-20 candidates + problem context + output contract
- Implement the three-decision output parser: direct stage / composition / synthesis request
- Implement the synthesis loop: spec → LLM code generation → test harness → store registration (up to 3 retries)
- Implement the composition validator: verify that the agent's proposed graph type-checks before returning it

#### 3.3 ACLI completion

- `noether compose <problem>` — full compose flow
- `noether introspect` — full command tree as JSON
- `noether stage search <query>` — semantic search
- `noether version` — semver with JSON output
- Complete `.cli/` folder with README, examples, JSON schemas
- All commands implement `--dry-run` and `--output json`
- Semantic exit codes (0–9) per ACLI spec

---

### Phase 4 — Hardening (Weeks 15–17)

**Goal:** the system is ready for agent use in production. Security model complete, store stays healthy.

#### 4.1 Security model

- Harden Nix sandbox configuration: verify network isolation, filesystem isolation
- Implement capability declaration enforcement: stages that attempt undeclared capabilities fail with security event in trace
- Implement the signing pipeline for all three stage creation paths
- Implement the signature verification step in the Composition Engine pre-execution check
- Security audit of the IPC data routing between stages

#### 4.2 Deduplication

- Implement pre-insertion similarity check against the semantic index
- Implement the 0.92 cosine similarity threshold with configurable override
- Implement the type-signature-as-arbiter rule: same semantics but different types → both allowed
- Add deduplication report to `noether store stats`

#### 4.3 Store retro

- Implement the clustering algorithm: DBSCAN or k-means over the semantic index
- Implement the retro report: clusters with >3 similar stages, unused stages, merge candidates
- Implement the deprecation workflow triggered from retro
- `noether store retro --dry-run` — show report without applying changes
- `noether store retro --apply` — apply deprecations and merges

---

### Phase 5 — Effects v2 (Weeks 18–21)

**Goal:** the type checker enforces effects. The Composition Engine uses effects to optimize execution.

#### 5.1 Effect inference

- For synthesized stages: implement LLM-assisted effect inference from implementation code
- For human-authored stages: enforce effect declaration in the SDK
- Implement the `Unknown → inferred_effects` migration for existing store entries

#### 5.2 Effect enforcement in type checker

- `NonDeterministic >> Pure >> store_to_db` — detect and warn on effect pollution
- `Fallible` stages without `retry()` wrapper — warn on unhandled failures
- `Cost<$n>` accumulation — block compositions that exceed declared budget constraints

#### 5.3 Engine optimizations enabled by effects

- `Pure` stages with deterministic inputs — cache output forever
- Parallelization of independent `Pure` subgraphs — automatic, no agent input needed
- LLM stage deduplication — same prompt + same model → cache hit (`NonDeterministic` with explicit override)

---

## 11. Technology Stack

| Component | Technology | Rationale |
|---|---|---|
| Core engine | Rust | Performance, memory safety, no runtime overhead |
| Type checker | Rust | Must be fast; called on every composition |
| Stage Store | Rust + Nix store | CAS is provided by Nix; wrapper in Rust |
| Inline executor | WASM (wasmtime) | Sub-millisecond cold start, hermetic by design |
| Process executor | Nix sandbox | Hermetic, reproducible, uses Nixpkgs runtimes |
| Remote executor | gRPC + Apache Arrow | Standard for high-throughput data between services |
| Semantic index | In-memory + HNSW | Sub-5ms search; HNSW for approximate nearest neighbor |
| Embeddings | text-embedding-3-small or local model | Balance cost vs. quality; swappable |
| Composition Agent LLM | claude-sonnet-4 (configurable) | Abstracted behind interface; swappable |
| CLI framework | ACLI spec + clap (Rust) | ACLI compliance; clap for argument parsing |
| Stage authoring SDK | Python (primary), Rust (secondary) | Python for LLM-generated stages; Rust for stdlib |
| Signing | Ed25519 | Fast, small signatures, widely supported |

---

## 12. Success Metrics by Phase

| Phase | Metric | Target |
|---|---|---|
| 0 | Type checker correctness | 100% on property-based test suite (1000+ cases) |
| 1 | Stdlib coverage | 50 stages, all passing test harness |
| 2 | Composition execution | p95 latency < 500ms for graphs of 10 inline stages |
| 3 | Semantic search | Top-20 recall > 80% on held-out query set; p95 < 5ms |
| 3 | Agent compose success rate | 80% of well-formed problems produce valid composition on first attempt |
| 4 | Store health | < 5% near-duplicate rate after retro |
| 4 | Security sandbox | 0 capability violations in test suite |
| 5 | Cache hit rate | Pure stage cache hit > 60% in typical sprint workload |
| 5 | Effect inference accuracy | > 90% agreement with human-annotated ground truth |

---

## 13. Open Questions and Deferred Decisions

| Question | Impact | Deferred to |
|---|---|---|
| How does Noether handle secret management (API keys in stages)? | Security | Phase 4 |
| Should the semantic index be persistent or always rebuilt from store? | Performance on restart | Phase 3 |
| What is the maximum composition graph size before performance degrades? | Scalability limits | Phase 2 benchmarks |
| Should Noether support multi-tenant stores (different agents with different stage sets)? | Architecture | Post-Phase 5 |
| How does the remote gRPC executor handle stage failure mid-stream? | Reliability | Phase 2 |
| Should composition promotion (Path 3) be automatic or require agent approval? | Store quality | Phase 4 |

---

*Noether — Build systems from verified parts, not from code.*
