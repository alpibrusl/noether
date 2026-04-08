# Changelog

All notable changes to Noether are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Noether uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Added
- `noether-cloud` GitHub Actions CI workflow (planned)

---

## [0.5.0] — 2026-04-08

### Added
- **Phase 8 — Runtime budget enforcement**: `BudgetedExecutor<E>` wraps any executor with atomic cost accounting (`Arc<AtomicU64>`). Pre-execution cost reservation aborts the run with `ExecutionError::BudgetExceeded` if the budget would be exceeded. `noether run --budget-cents <n>` enables it from the CLI. `CompositionResult::spent_cents` reports actual spend.
- **Phase 7 — Cloud Registry hardening**: `DELETE /stages/:id` endpoint on `noether-registry` (protected by API key). `RemoteStageStore` now uses offset-based pagination for bulk refresh, `get_live` for explicit on-demand fetches, and `remove` sends a DELETE request before evicting from the local cache. The `noether-scheduler` can now use a remote registry via `registry_url` / `registry_api_key` config fields.
- **Phase 6 — NixExecutor hardening**: `NixConfig` struct (`timeout_secs`, `max_output_bytes`, `max_stderr_bytes`). Wall-clock timeout via `mpsc::channel` + `kill -9`. `classify_error` distinguishes Nix infrastructure failures from user code errors. `NixExecutor::warmup()` pre-fetches the Python 3 runtime in a background thread.
- **Phase 5 — Effects v2**: `EffectKind` enum, `EffectPolicy` (allow-list), `infer_effects` (graph traversal), `check_effects` (pre-flight enforcement). `noether run --allow-effects <comma-separated>` flag. Remote stages implicitly carry `Network` + `Fallible` effects; unknown stages carry `Unknown`. `--dry-run` output now includes inferred effects.

### Changed
- `noether run --dry-run` output includes `"effects"` and (when budget set) `"expected_cost_cents"`
- `noether run` live output includes `"effects"` and (when budget set) `"spent_cents"`
- `ExecutionError` extended with `TimedOut { stage_id, timeout_secs }` and `BudgetExceeded { spent_cents, budget_cents }`
- Stdlib count: 76 stages (up from 50 at 0.1.0)

---

## [0.1.0] — 2026-04-07

### Added
- **Phase 0** — Type system (`NType`), structural subtyping, stage schema, Ed25519 signing, SHA-256 content addressing
- **Phase 1** — `StageStore` trait + `MemoryStore`, 50 stdlib stages, lifecycle validation
- **Phase 2** — Lagrange composition graph, type checker, `ExecutionPlan`, `run_composition`, traces
- **Phase 3** — Composition Agent, semantic three-index search, `VertexAiLlmProvider`, `noether compose`
- **Phase 4** — `noether build` with `--serve :PORT` browser dashboard, `--dry-run`, store dedup
- ACLI-compliant CLI with structured JSON output for all commands
- `noether store dedup` — detect functionally duplicate stages
- Branch protection and CI workflows on `main`
- MkDocs documentation site
- `noether-research/` design documents: NoetherReact, WASM target, Cloud Registry
