# Roadmap

Current implementation status and future directions for Noether.

---

## Implemented phases

| Phase | Name | Status | Key deliverables |
|---|---|---|---|
| 0 | Foundation | ✅ Done | `NType` structural type system, SHA-256 content addressing, Ed25519 signing, stage schema |
| 1 | Store + Stdlib | ✅ Done | `StageStore` trait, `MemoryStore` / `JsonFileStore`, 76-stage stdlib, lifecycle validation |
| 2 | Composition Engine | ✅ Done | Lagrange graph format, type checker, `ExecutionPlan`, `run_composition`, structured traces |
| 3 | Agent Interface | ✅ Done | Composition Agent, three-index semantic search, `VertexAiLlmProvider`, `noether compose` |
| 4 | Hardening | ✅ Done | `noether build` + `--target browser`, store dedup, `noether build --serve :PORT` dashboard |
| 5 | Effects v2 | ✅ Done | `EffectKind`, `EffectPolicy`, effect inference (`infer_effects`), `--allow-effects` CLI flag |
| 6 | NixExecutor hardening | ✅ Done | `NixConfig` (timeout, output limits), error classification, `NixExecutor::warmup()` |
| 7 | Cloud Registry hardening | ✅ Done | `DELETE /stages/:id`, paginated refresh, on-demand `get_live`, scheduler remote-store support |
| 8 | Runtime budget enforcement | ✅ Done | `BudgetedExecutor`, `Arc<AtomicU64>` cost tracking, `--budget-cents`, `BudgetExceeded` error |
| 9 | Grid — subscription pooling | ✅ Done (v0.4.0) | `noether-grid-broker` + `noether-grid-worker`, graph splitting on `Effect::Llm`, four subscription-CLI providers, retry-with-exclusion, optional postgres persistence, Prometheus metrics, per-agent quotas |

---

## Milestones (post-phase 9)

Noether shifted from sequential "phase" numbering to milestone tracking with the v0.5 release. Milestones correspond to the [Rock-Solid Plan](roadmap/2026-04-18-rock-solid-plan.md).

| Milestone | Name | Status | Shipped as | Key deliverables |
|---|---|---|---|---|
| M1 | Semantics + Canonical Form | ✅ Done | v0.5.0 | `canonicalise` for every composition op, pre-resolution `composition_id` contract, `laws.rs` property tests |
| M2 | Stability + Versioning + Property Predicates | ✅ Done | v0.6.0 + v0.7.0 | Stage identity split (`signature_id` + `implementation_id`), graph-level pinning, declarative properties DSL (7 kinds), resolver pass, `stage verify` checks signatures + properties by default, STABILITY.md, store ≤1-Active-per-signature invariant |
| M2.4 | Stage execution isolation — Phase 1 | ✅ Done | v0.7.0 | Bubblewrap sandbox by default, UID mapping to `nobody`, sandbox-private `/work` tmpfs, trusted `bwrap` path discovery, `--require-isolation` CI gate, DNS/TLS binds when network declared, adversarial escape-test suite |
| M2.5 | Property DSL expansion | ✅ Done | v0.7.0 | `FieldLengthEq` / `FieldLengthMax` / `SubsetOf` / `Equals` / `FieldTypeIn`, typed `JsonKind` enum, shadowed-kind ingest rejection |
| M2.x | `noether-isolation` crate extraction | ✅ Done | v0.7.1 | Standalone crate + `noether-sandbox` binary for non-Rust consumers (agentspec, future Python/Node/Go bindings) |
| M3 | Optimizer + Richer Types | ⏳ In progress | targeting v0.8.0 | Graph optimizer: framework + `dead_branch` + `canonical_structural` + `memoize_pure` ✅ landed; `fuse_pure_sequential` and `hoist_invariant` remain (planner/executor work, not AST). Parametric polymorphism: unification module ✅ + `NType::Var` ✅ + `check_graph` threading ✅ + generic stdlib stages ✅. Row polymorphism: `NType::RecordWith` + Record↔RecordWith unification + `mark_done` stdlib stage ✅ (RecordWith↔RecordWith deferred). Refinement types still pending. |
| M3.x | Filesystem-scoped effects | ✅ Done | unreleased (next tag) | `Effect::FsRead(path)` / `FsWrite(path)` variants wired through `IsolationPolicy::from_effects` so path-scoped binds fall out of the signature — closes the gap [#39](https://github.com/alpibrusl/noether/issues/39) flagged around `from_effects` being unable to drive `rw_binds` |
| M4 | Stdlib Curation + Vertical Depth + 1.0 | ⏳ Planned | targeting 1.0.0 | Stdlib audit, vertical depth in a chosen domain, freeze |
| Phase 2 isolation | Native namespaces + Landlock + seccomp | ⏳ Planned | targeting v0.8.0 | Replace bwrap subprocess with direct `unshare` + Landlock + seccomp; same `IsolationPolicy` surface, ~10× lower startup |

---

## Near-term improvements

Smaller tech-debt items tracked outside the milestone cadence:

| Item | Description |
|---|---|
| `noether compose` + budget | `noether compose` doesn't wrap execution in `BudgetedExecutor` yet |
| `NixExecutor::warmup()` caller | Warmup is implemented but never called at CLI startup |
| `get_live` CLI integration | `RemoteStageStore::get_live` is never called from the CLI |
| Scheduler `registry_url` docs | The scheduler's remote-store config is undocumented outside source code |
| Registry unconditional LLM-provider init | `routes::compositions::run` constructs a `reqwest::blocking::Client` even when no LLM env is set, which blocks HTTP-level integration testing from `#[tokio::test]` in noether-cloud. Make provider construction lazy or env-gated |
| `validate_against_types` for relational property variants | Structural checks (length-on-numeric, equals-on-disjoint-types) currently punt at registration; land naturally with M3 refinement types |

---

## Future directions

These are not scheduled — they are design explorations:

| Idea | Notes |
|---|---|
| **Grid — capability generalisation** | Lift grid routing beyond `Effect::Llm` to any capability kind (GPU time, DB connections, scraper rotation). See [research](research/grid-capabilities.md). |
| **`llm-here`** | Unify caloron's `_llm.py`, agentspec's resolver, and grid's `cli_provider.rs` behind one shared tool. See [research](research/llm-here.md). |
| **NoetherReact** | Content-addressed UI components as stages; `UI = f(stage_graph(state))`. See [research](research/noether-react.md). |
| **WASM stdlib** | Compile Pure Rust stdlib stages to WASM for zero-latency in-browser execution. See [research](research/wasm-target.md). |
| **Multi-tenant stores** | Separate stage namespaces per agent / team |
| **Pure-stage caching** | Automatic output memoisation for `Pure`-annotated stages |
| **Remote gRPC executor** | High-throughput data routing via gRPC + Apache Arrow for stream stages |
| **Effect pollution warnings** | Detect `NonDeterministic >> Pure >> db_write` chains at type-check time |
| **Automatic parallelisation** | Identify independent `Pure` subgraphs and execute them concurrently without agent input |

---

## Design philosophy (stable)

> Every symmetry in the composition algebra corresponds to a conservation law in execution.
> Identical inputs + identical pipeline spec = identical outputs. Always.

| Principle | Implementation |
|---|---|
| Content-addressed identity | Every stage is identified by `SHA-256(impl + signature)`, never by mutable name |
| Structural typing | Two types are compatible if their structure matches — no nominal coordination needed |
| Reproducibility | Nix hermetic sandboxing guarantees same outputs from same inputs across machines |
| Effects as first-class | Effects declared in signature; `EffectPolicy` enforces allowed kinds pre-flight |
| Immutability | Stages never change; new versions create new identities |
