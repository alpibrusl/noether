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

---

## Near-term improvements

These are gaps in the current implementation, not new phases:

| Item | Description |
|---|---|
| `noether compose` + budget | `noether compose` doesn't wrap execution in `BudgetedExecutor` yet |
| `NixExecutor::warmup()` caller | Warmup is implemented but never called at CLI startup |
| `get_live` CLI integration | `RemoteStageStore::get_live` is never called from the CLI |
| Scheduler `registry_url` docs | The scheduler's remote-store config is undocumented outside source code |
| `noether-cloud` CI | No GitHub Actions workflow for the cloud repo yet |

---

## Future directions

These are not scheduled — they are design explorations:

| Idea | Notes |
|---|---|
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
