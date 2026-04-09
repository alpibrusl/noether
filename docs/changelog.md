# Changelog

All notable changes to Noether are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.0.0/).
Noether uses [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [Unreleased]

### Added
- `noether-cloud` GitHub Actions CI workflow (planned)

---

## [0.6.0] — 2026-04-09

### Added
- **Canonical stage identity** (`canonical_id`): SHA-256 of name + input + output + effects. Enables automatic versioning — `noether stage add` auto-deprecates the previous version when a stage with the same canonical_id is re-registered.
- **`noether stage activate` command**: Promotes Draft stages to Active lifecycle. Supports ID prefix matching.
- **OpenAI LLM + embedding provider**: Set `OPENAI_API_KEY` to use GPT models. Also works with Ollama via `OPENAI_API_BASE`.
- **Anthropic LLM provider**: Set `ANTHROPIC_API_KEY` to use Claude models.
- **Simplified type syntax** (`normalize_type`): Stage spec files now accept `"Text"` instead of `{"kind":"Text"}`, and `{"Record":[["field","Text"]]}` instead of the verbose canonical format.
- **Stage spec tags and aliases**: `noether stage add` now reads `tags` and `aliases` from simple spec format.
- **Deprecated stage resolution**: `noether run` transparently follows Deprecated → successor_id chains. Graphs referencing deprecated stages no longer fail.
- **370 stage specs** across 50 open-source libraries (in noether-cloud/stages/).
- **Capability benchmark** with 4 scenarios: type safety, parallel execution, reusability, token analysis.

### Changed
- `noether store dedup --apply` now uses `Deprecated{successor_id}` instead of `Tombstone`, preserving forward pointers for existing graphs.
- `noether stage list` now defaults to Active lifecycle filter (was showing all including tombstoned).
- Provider auto-detection priority: Mistral → OpenAI → Anthropic → Vertex AI → Mock.

### Fixed
- Stage spec `tags` and `aliases` were silently ignored in simple format — now parsed correctly.
- `.cli/schemas/stage-spec.json` updated to document both simple and full spec formats.

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
