# Changelog

All notable changes to Noether are documented here.

Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Noether
uses [Semantic Versioning](https://semver.org/).

> **Pre-0.2 internal numbering.** Before reaching crates.io, Noether used
> a sequential `0.1.0 … 0.6.0` internal numbering tracked in this file.
> The first public release to crates.io is **0.2.1**; public numbering
> resets there. The pre-public entries are kept below under
> [Pre-release history](#pre-release-history) for reference — don't map
> them to crates.io versions.

> **Authoritative source.** The root-level
> [`CHANGELOG.md`](https://github.com/alpibrusl/noether/blob/main/CHANGELOG.md)
> is the canonical changelog from v0.7.0 forward. This doc summarises
> the same entries for MkDocs navigation; the full detail + rationale
> lives at the root.

---

## [0.7.1] — 2026-04-19

Small release: extract isolation into its own crate + ship a standalone sandbox binary for non-Rust consumers.

- **New crate `noether-isolation`** — extracted from `noether-engine::executor::isolation`. Small dependency footprint (`noether-core` + `serde` + `thiserror` + `tracing`). `IsolationPolicy` is now Serde-enabled with a round-trip test pinning the wire format. `noether-engine` re-exports from this crate so existing callers see no API change.
- **New binary `noether-sandbox`** — ~300 LOC glue. Reads an `IsolationPolicy` on stdin (or `--policy-file <path>`), takes argv after `--`, runs the inner command inside bubblewrap. Flags: `--isolate=auto|bwrap|none`, `--require-isolation` (mirrors the engine-side env var), 1 MiB stdin cap, `128 + signum` exit codes for signal deaths. Intended for Python/Node/Go/shell callers (notably agentspec) that want the sandbox primitive without a Rust toolchain.
- **`ro_binds` wire format** switched from tuple `[[host, sandbox]]` to named-struct `[{"host": ..., "sandbox": ...}]` before external consumers pinned the shape.

## [0.7.0] — 2026-04-19

M2 close-out: property DSL parity with the "what does this stage guarantee?" use case, resolver at every graph-ingest entry point, store enforces its Active-per-signature invariant, stage subprocesses execute inside a real sandbox by default. The v1.x stability contract ([STABILITY.md](https://github.com/alpibrusl/noether/blob/main/STABILITY.md)) applies from this release.

### Added — sandbox isolation

`noether run --isolate=auto` (default from v0.7) wraps every stage subprocess in bubblewrap: UID mapped to `nobody` (65534), unshared user/pid/mount/uts/ipc/cgroup namespaces, `/nix/store` RO, sandbox-private `/work` tmpfs, `--cap-drop ALL`, `--new-session`, env cleared to a short allowlist, network namespace unshared unless `Effect::Network` is declared. When network is on, `/etc/resolv.conf`, `/etc/hosts`, `/etc/nsswitch.conf`, `/etc/ssl/certs` bind read-only so DNS and TLS actually work. `bwrap` resolved from trusted system paths first (`/run/current-system/sw/bin`, `/nix/var/nix/profiles/default/bin`, `/usr/bin`, `/usr/local/bin`) before falling back to `$PATH`.

CLI flags: `--isolate=auto|bwrap|none` (+ `NOETHER_ISOLATION`), `--unsafe-no-isolation`, `--require-isolation` (+ `NOETHER_REQUIRE_ISOLATION=1` for CI fail-closed).

Adversarial escape-test suite (`tests/isolation_escape.rs`) runs real bwrap+python and verifies `setuid(0)`, `chroot("/")`, reading `/etc/shadow`, reading `~/.ssh/*`, and DNS with `network:false` all fail.

**Caveat**: isolation requires nix installed under `/nix/store` (upstream or Determinate). Distro-packaged `/usr/bin/nix` is dynamically linked against host libs that aren't bound; the executor refuses cleanly with a clear message.

Phase 2 (v0.8): native `unshare` + Landlock + seccomp, same `IsolationPolicy`, ~10× lower startup. Roadmap: [`docs/roadmap/2026-04-18-stage-isolation.md`](roadmap/2026-04-18-stage-isolation.md).

### Added — property DSL expansion

Five new `Property` variants on top of v0.6 `SetMember` / `Range`:

- `FieldLengthEq { left_field, right_field }` — equal length. Strings: UTF-8 code-point count. Arrays: element count. Objects: key count.
- `FieldLengthMax { subject_field, bound_field }` — subject length ≤ bound length.
- `SubsetOf { subject_field, super_field }` — element / (key, value) / contiguous substring subset. Three branches per JSON kind, each pinned by a dedicated test.
- `Equals { left_field, right_field }` — JSON-value equality.
- `FieldTypeIn { field, allowed: Vec<JsonKind> }` — runtime JSON type in the allowed set. `JsonKind` is a typed enum; wire format stays snake-case strings.

`Property::Unknown` is the forward-compat escape; `Property::shadowed_known_kind()` distinguishes "genuinely unknown kind" (safe skip) from "typo inside a known kind" (rejected at ingest via `ValidationError::ShadowedKnownKind`). Every stdlib stage carries ≥3 properties.

### Changed — resolver runs at every graph-ingest entry point

`resolve_pinning` + the new `resolve_deprecated_stages` (in `noether_engine::lagrange::deprecation`) now runs from every entry point that ingests a graph: `noether run`, `compose`, `build`, `serve`, the scheduler, the grid broker, and the grid worker. `composition_id` is always computed **before** resolution per the M1/#28 "canonical form is identity" contract.

New `DeprecationReport { rewrites, events }` distinguishes routine rewrites from anomalies (`ChainEvent::CycleDetected`, `MaxHopsExceeded`) explicitly.

### Changed — store enforces ≤1 Active per signature

`MemoryStore::put` / `upsert` and `JsonFileStore` equivalents auto-deprecate any existing Active stage whose `signature_id` matches an incoming Active. Shared `noether_store::invariant` module; every auto-deprecation emits structured `tracing::warn!`.

### Changed — stage identity split

Two content-addressed IDs per stage: `signature_id = SHA-256(name + input + output + effects)` (stable across bugfix-only impl rewrites) and `implementation_id` aka `StageId = SHA-256(signature_id + implementation_hash)`. Graphs pin by `signature_id` by default (`Pinning::Signature`); `Pinning::Both` requires exact impl match.

### Added — properties checked by default in `noether stage verify`

Default now checks both Ed25519 signatures and declarative properties against declared examples. `--signatures` restricts to signatures; `--properties` restricts to properties. Default or both flags → both checks.

(Early drafts of these notes referred to `--with-properties` / `--signatures-only`; those flag names never landed.)

### Added — STABILITY.md

Formal 1.x compatibility contract — what's stable on the wire, what's additive, what's deprecated, what can change.

### Breaking changes

- `NixExecutor::register` (unsafe default) removed. Synthesized-stage registration goes through `NixExecutor::register_with_effects`; `CompositeExecutor::register_synthesized` takes an `EffectSet` argument.
- `IsolationPolicy::from_effects` no longer takes a `work_host: PathBuf` argument (sandbox defaults to a private tmpfs; opt-in via `.with_work_host(...)`).
- `resolve_deprecated_stages` moved from `noether_cli::commands::resolver_utils` to `noether_engine::lagrange::deprecation` (now `pub`). Return type changed to `DeprecationReport`.
- `Stage.canonical_id` accepted on the wire but removed from the Rust type — use `signature_id`. JSON alias stays through 0.7.x.

### Known limitations / deferred

- Filesystem-scoped effect variants (`Effect::FsRead(path)` / `FsWrite(path)`) — v0.8, paired with Phase-2 isolation.
- `validate_against_types` punts on the five new relational property variants. Structural checks land with M3 refinement types.

---

## [0.6.0] — 2026-04-18

**Breaking release.** M1 (canonical composition form) and M2 (stage
identity split, graph-level pinning, declarative properties, resolver
pass, verify CLI) land together. **Composition IDs and Stage IDs change
format** — v0.4.x IDs do not round-trip. Regeneration via `noether
compose` or `noether stage add` is required; registries must be rebuilt.

### Breaking

- **Composition ID format changed.** `compute_composition_id` now hashes
  the pre-resolution canonical form of the graph (flattened Sequentials,
  collapsed singletons, collapsed Retries, etc.) via RFC 8785 JCS, not
  the raw JSON. See [`docs/architecture/semantics.md`](./architecture/semantics.md)
  for the rules and property laws.
- **Stage ID hash now includes `name`.** `compute_stage_id(name, &sig)`
  nests `compute_signature_id`: changing any signature-level field
  (including name) changes the StageId; changing only the
  implementation_hash changes StageId but not SignatureId. Old IDs
  computed from StageSignature alone no longer resolve.
- **`canonical_id` renamed to `signature_id`** at the Rust API and
  JSON field level. The old field name is accepted as a deserialisation
  alias through v0.6.x; the Rust `CanonicalId` / `compute_canonical_id`
  symbols are deprecated and removed in v0.7.0.
- **`CheckPropertiesError`** replaces the old
  `Result<(), Vec<(usize, PropertyViolation)>>` return of
  `Stage::check_properties`. A stage with properties but no examples
  now errors (`NoExamples`) rather than passing vacuously.

### Added

- **Declarative properties** on stage specs
  (`crates/noether-core/src/stage/property.rs`). `Property::SetMember`
  and `Property::Range` variants, plus `Property::Unknown` for forward
  compatibility. `noether stage verify --properties` runs every
  declared property against every example.
- **Per-node graph pinning.** `CompositionNode::Stage` gains
  `pinning: Pinning`:
  - `"signature"` (default, omitted in JSON) — resolves `id` as a
    `SignatureId` to the currently Active implementation.
  - `"both"` — resolves `id` as an `ImplementationId`, bit-exact.
  The resolver falls back from signature → Active implementation
  lookup only when the fallback's lifecycle is Active (deprecated
  stages don't silently run).
- **Resolver pass** (`lagrange::resolve_pinning`). Rewrites a graph
  so every `Stage.id` holds a concrete ImplementationId; downstream
  passes (checker, planner, budget, runner) keep using `store.get`.
  `noether run` calls this between prefix resolution and
  deprecation-chain resolution. Emits a `MultiActiveWarning` when
  more than one Active implementation shares a signature.
- **`noether stage verify`** — signatures-and/or-properties verifier.
  Flags: `--signatures` (Ed25519 only), `--properties` (declarative
  only), neither (both). Reports structured ACLI output; exits 1 and
  emits `acli_error` on failure so agents can't miss violations.
- **`STABILITY.md`** — the 1.x stability contract. Covers signature
  ID, implementation ID, composition ID, operator semantics, stdlib
  freeze, graph JSON schema, registry API, MSRV (1.83 stable),
  public crate surface, on-disk formats, and env-var contract.

### Fixed (from M1 review pass)

- Nested `Retry` collapse is now idempotent at arbitrary depth — the
  combined result re-feeds through the local canonical rewrites.
- `L6`/`L7` proptests (`Parallel` / `Let` permutation invariance)
  now actually test permutation: they build JSON with shuffled key
  order and assert equal composition IDs. The previous BTreeMap-only
  version was tautological.
- Semantics doc now distinguishes laws tested in M1 (`L1, L4-L7,
  L9-L13`) from laws deferred to later milestones (`L2, L3, L8`).

## [0.4.1] — 2026-04-16

DX release: two ergonomic frictions that surfaced while building a
realistic telemetry pipeline (see `/home/alpibru/workspace/noether-telemetry`
for the exercise that drove these) are now fixed. Backward-compatible
for the type system; additive for the graph resolver. **No change to
stage content hashes** — existing stages and compositions keep their
identities.

### Changed

- **Nullable Record fields are now optional.** A field declared
  `T | Null` (or `Null`, or `Any`) in a record type is treated as
  optional — the value may omit the key entirely instead of being
  required to include it with a null. Type-checker rule:
  `is_subtype_of(Record{…}, Record{field: T | Null})` now accepts
  records that don't contain `field`. Non-nullable fields remain
  strictly required. Motivation: config-like stage inputs
  (`threshold_pct`, `peak_power_kwp`) previously had to be carried
  through every upstream stage's output schema, because `T | Null`
  meant "present, possibly null" not "may be absent." The new
  semantics line up with how JSON Schema + TypeScript + pydantic
  treat nullable fields.

### Added

- **Stage lookup by `name` in graph references.** Composition graphs
  can now write `{"op": "Stage", "id": "volvo_map"}` — the graph
  loader resolves the string against the store by name when it
  doesn't match an ID prefix. Active stages win over Draft /
  Deprecated; duplicates across Active lifecycles are still an
  explicit `Ambiguous` error. Previously, graphs had to paste the
  8-char content-hash prefix that `stage add` emitted. Existing
  hex-prefix references continue to work — the new behaviour only
  kicks in when the reference isn't a valid hex prefix of any stored
  stage.
- `Stage.name: Option<String>` field, populated automatically from
  the spec's top-level `name`. Not part of the content hash —
  changing the human-authored name doesn't change the stage's
  identity. Two stages with the same name but different types remain
  distinct identities (the resolver errors on ambiguity rather than
  guessing).
- `StageStore::find_by_name(name)` default method — returns all
  stages with a matching `name` field, across all lifecycles. Built
  on `list()`, so every store impl gets it for free.

### Migration

Nothing breaks. Existing stage specs, composition graphs, and stores
from v0.4.0 load and run identically. Two optional cleanups you can
make to take advantage:

1. Drop the carried-through config fields from intermediate stage
   outputs. A downstream stage that declares `field: Number | Null`
   no longer requires every upstream stage to include that field.
2. Rewrite graph references from hex prefixes to names:
   `{"id": "abc12345"}` → `{"id": "<spec-name>"}`.

---

## [0.4.0] — 2026-04-15

First release with **noether-grid**: a broker + worker pair that pools
LLM capacity across machines. Three new binaries ship alongside the
existing `noether` / `noether-scheduler`:
`noether-grid-broker`, `noether-grid-worker`, and the
`noether-grid-protocol` crate they share.

See `crates/noether-grid-broker/README.md` for the per-role deploy
walkthrough and `docs/research/grid.md` for the design.

### Added
- **`noether-grid-broker`** — pools worker capacity, splits Lagrange
  graphs so `Effect::Llm` stages dispatch to a worker with matching
  capability while pure stages execute locally. Retry-with-exclusion
  on worker failure, optional postgres write-through persistence
  (`--features postgres`), self-contained HTML dashboard at `/`,
  Prometheus metrics at `/metrics`, per-agent quotas via
  `--quotas-file`.
- **`noether-grid-worker`** — enrols with a broker, advertises its
  LLM capabilities, serves `/execute` (full graph) and `/stage/{id}`
  (single-stage, `RemoteStage`-compatible). Auto-discovers four
  subscription CLIs (Claude, Gemini, Cursor Agent, OpenCode) plus
  API-key providers (Anthropic, OpenAI, Mistral, Vertex AI).
- **Subscription-CLI support in `noether-engine`.** New
  `crate::llm::cli_provider` module generalises over Claude Desktop,
  Gemini CLI, Cursor Agent, and OpenCode. Opt in via
  `NOETHER_LLM_PROVIDER={claude-cli,gemini-cli,cursor-cli,opencode}`
  or auto-detect when no API key is set. Suppress via
  `NOETHER_LLM_SKIP_CLI=1` for sandboxed environments.
- **`RemoteStage` error surface** now propagates the remote worker's
  `ok: false, error: <msg>` verbatim instead of masking it as
  "missing data.output field".
- **Three research notes** in `docs/research/`: `grid.md` (design),
  `grid-capabilities.md` (future generalisation beyond LLMs),
  `llm-here.md` (planned consolidation with caloron's `_llm.py` and
  agentspec's resolver).

### Migration

**You only need to migrate if you adopt `noether-grid`.** The
`noether` CLI, stdlib, scheduler, and graph format are unchanged.

If you deploy grid:

1. **Store path must match on broker and all workers.** Both
   `noether-grid-broker` and `noether-grid-worker` default their
   `--store-path` to `$HOME/.noether/store.json` (matching the CLI's
   `noether_dir()`). Previous prerelease builds of grid used a
   CWD-relative `.noether/store.json` default, which silently
   diverged when the broker and worker were launched from different
   directories. If you pinned an earlier grid build and relied on
   that behaviour, set `NOETHER_STORE_PATH` explicitly — or nothing,
   and let the new `$HOME` default apply.

2. **Subscription CLIs are auto-detected by default.** Running grid
   on a machine with `claude` / `gemini` / `cursor-agent` /
   `opencode` on `$PATH` now advertises each as pooled capacity.
   If you want a headless worker that ignores ambient CLI auth
   (e.g. a CI runner, a Nix-sandboxed executor), set
   `NOETHER_LLM_SKIP_CLI=1`.

3. **Bare-string `"llm"` effects now route.** A stage declaring
   `"effects": ["llm"]` parses as `Effect::Llm { model: "unknown" }`
   and dispatches to any worker with any LLM capability. Previous
   behaviour was to refuse routing with `no worker matches ["unknown"]`.
   If you previously worked around this by declaring
   `Effect::Llm { model: "<specific>" }`, nothing changes — exact-model
   match still wins when the model is set.

### Fixed
- `jobs_failed_total` now increments on the splitter-refusal terminal
  path (it previously only counted post-dispatch failures).
- Worker capability probing logs the resolved path per subscription
  CLI at `INFO`, so a silent zero-capabilities advertisement surfaces
  its cause instead of requiring out-of-band debugging.
- Broker logs the resolved store path + stage count at boot and warns
  loudly when the seeded catalogue looks small (<20 extra stages).

### Known caveats (not blockers)
- Cost model today assumes metered APIs — subscription-path jobs
  report `cents_spent_total = 0`. Capacity-based metrics
  (`jobs_routed_total{provider}`, `capacity_used_ratio`) are the
  v0.4.1 plan; see `docs/research/grid-capabilities.md`.
- Cross-machine + multi-seat fan-out is implemented and
  unit-tested, but has not been piloted on production hardware as
  of this release. The MVP pilot was single-broker + single-worker
  on one host.

---

## [0.3.1] — 2026-04-14

Bug-fix release driven by issues caloron-noether hit migrating from v0.2.

### Fixed

- **Python `from __future__ import` no longer breaks the Nix wrapper.**
  Stage implementations starting with `from __future__ import annotations`
  used to land at line ~17 of the synthesized wrapper, which Python rejects
  with `SyntaxError: from __future__ imports must occur at the beginning
  of the file`. The wrapper now hoists every top-level
  `from __future__ import …` line to the very first lines of the wrapped
  module.

- **`noether stage get <prefix>` now resolves prefixes**, the same way
  `stage activate` and graph loaders already do. Previous versions did an
  exact-string lookup and then surfaced a "did you mean" hint that echoed
  the user's input back at them — because the hint also truncated to 16
  characters even when the input was already 16 characters. Both halves
  are fixed: `cmd_get` resolves through `resolve_stage_id`, and the hint
  shows IDs at *prefix length + 8 chars* so collisions become visible.

- **Stage-spec effect parser accepts `Llm`, `Cost`, `Unknown`, plus
  lowercase / snake_case variants.** v0.2 specs that declared
  `"effects": ["Llm"]` were silently dropping that effect with a cryptic
  `Warning: unknown effect 'Llm', ignoring.` log line — the stage would
  then run as if it were Pure. Now decoded correctly. Llm without an
  explicit `model` defaults to `"unknown"`; Cost without `cents` defaults
  to `0`.

### Upgrading from v0.2

The v0.2 → v0.3 transition has two breaking surfaces beyond what the
v0.3.0 release notes covered. Both now have clearer error messages but
existing specs still need a one-time rewrite:

1. **Effect names are now case-tolerant** — `Llm`, `llm`, and any of
   `non-deterministic` / `nondeterministic` / `NonDeterministic` all
   work. If you saw `unknown effect '<X>', ignoring` warnings on v0.3.0,
   re-add this release and the warnings go away.

2. **Type-spec format is `{"Record": [["field", T], …]}`**, not the
   `{"type": "Record", "fields": {…}}` form some v0.2 examples used.
   We don't ship an automatic migration; rewrite by hand. The simplified
   syntax (bare strings like `"Text"`, `"Number"`) works for primitives
   inside Record cells.

If you maintain a downstream stage catalogue and want a one-shot
`noether stage-spec migrate` command, file an issue — happy to add.

---

## [0.3.0] — 2026-04-14

### Added
- **`noether-scheduler` migrated into the public repo.** The cron-based
  composition runner now lives at `crates/noether-scheduler/`, ships a
  binary in every GitHub release, and publishes to crates.io
  (`cargo install noether-scheduler`). Previously lived in the private
  `noether-cloud` workspace.
- **`--config <PATH>` flag** on `noether-scheduler`, alongside the
  existing positional argument. `noether-scheduler --config
  scheduler.json`, `noether-scheduler scheduler.json`, and the bare
  `noether-scheduler` (defaults to `./scheduler.json`) all work.
- **[Dedicated scheduler guide](guides/scheduler.md)** — config schema,
  cron semantics, webhook payload, systemd unit template, Docker recipe,
  troubleshooting.

### Changed
- Workspace version bumped to `0.3.0` to cover path-dep versions on
  `noether-core`, `noether-store`, `noether-engine` (no runtime API
  changed; the bump is for coherent cross-crate publishing).
- Dockerfile in `noether-cloud/infra/` now builds `noether-scheduler`
  from the public noether checkout instead of a local workspace copy.

### Fixed
- Documentation now reflects the actual crates.io publishing flow. No
  more references to a private source.

---

## [0.2.1] — 2026-04-14

### Added
- **crates.io metadata** on every crate (`description`, `license` set to
  `EUPL-1.2`, `repository`, `homepage`, `keywords`, `categories`). The
  0.2.0 publish failed with "missing metadata"; 0.2.1 is functionally
  identical to 0.2.0 but actually installable.
- **Path-dep versions** (`version = "0.2"`) on workspace path
  dependencies so downstream crates resolve correctly once published.

### Notes
- First release actually on crates.io. Use `cargo install noether-cli`
  from this version onward.

---

## [0.2.0] — 2026-04-13

Feedback-driven release. External developer wrote up every friction they
hit building a real pipeline on 0.1; this release addresses it.

### Added — engine
- **`Let` operator** for binding named intermediate results and carrying
  original-input fields through `Sequential` pipelines. Solves the
  canonical `scan → hash → diff` pattern where a later stage needs a
  field an earlier stage erased. Bindings run concurrently against the
  outer input; the body sees a merged record
  `{...outer fields, name → binding output}`.
- **Python `def execute(input)` contract validated at `stage add` time.**
  Specs missing a top-level `execute` are rejected with a clear error
  pointing at the docs instead of the cryptic `'NoneType' object is not
  subscriptable` runtime failure.
- **Stage ID prefix resolution in graphs.** Graph loaders accept the
  8-char IDs `noether stage list` prints; the CLI resolves them to full
  SHA-256s at load time and errors clearly when a prefix is ambiguous.
- **Boot-time curated-stages loader** in `noether-registry`:
  `NOETHER_STAGES_DIR` env → every `*.json` under the directory is
  parsed, signed with the stdlib key, upserted, and marked Active.
  Idempotent on content hash.
- **Progressive embedding cache + inter-batch pacing.** Partial cold-start
  progress survives rate-limit crashes. New env knobs:
  `NOETHER_EMBEDDING_CACHE`, `NOETHER_EMBEDDING_BATCH`,
  `NOETHER_EMBEDDING_DELAY_MS`.

### Added — CLI
- `stage add` **auto-promotes** Draft → Active by default; opt out with
  `--draft`.
- `stage sync <dir>` — bulk-import every `*.json` spec, idempotent on
  hash.
- `stage list` gains `--signed-by stdlib|custom|<keyprefix>`,
  `--lifecycle <state>`, `--full-ids`.
- `noether run` and `noether compose` read JSON from stdin when
  `--input` is absent and stdin is a pipe
  (`echo '{...}' | noether run graph.json`).
- Embedding-provider warnings suppressed on commands that don't actually
  use semantic search (`list`, `get`, `add`, `stats`, ...). Surface them
  via `NOETHER_VERBOSE=1` or on `search`/`compose`.

### Changed — docs
- "Python Stage Contract" is the lead of `guides/custom-stages.md`.
- `guides/composition-graphs.md` corrected to match the real schema
  (`id`/`stages`/`predicate`/`if_true`/`delay_ms`), added the
  stages-vs-branches rationale table, documented Sequential's
  no-projection limitation, added the new `Let` operator section.
- Three overlapping `getting-started/` pages merged into one.
- `guides/remote-registry.md` rewritten to lead with the public registry
  at `registry.alpibru.com` and the Docker-Hub-style auth model
  (anonymous read, authed write).
- Obsolete `guides/stage-store-build.md` (564 lines, duplicate of
  `custom-stages.md`) deleted.

### Fixed
- Python stages that defined `execute` but imported `sys.stdin` at
  module level would sometimes race the wrapper. Wrapper rewrite.
- `cargo publish` previously failed with "missing metadata" (addressed
  in 0.2.1).

### Not a bug
- "stdin dropped under Nix executor" reported in feedback turned out to
  be a CLI UX bug — the Nix executor forwards stdin correctly; the
  CLI just wasn't reading its own pipe. That's now the stdin fallback.

---

## Pre-release history

Internal numbering before crates.io. Kept for reference; do not map to
public versions.

### [0.6.0] — 2026-04-09 *(internal)*

- **Canonical stage identity** (`canonical_id`): SHA-256 of name + input
  + output + effects. Enables automatic versioning — `noether stage add`
  auto-deprecates the previous version when a stage with the same
  canonical_id is re-registered.
- `noether stage activate` promotes Draft stages to Active; supports ID
  prefix matching.
- **OpenAI LLM + embedding provider** (`OPENAI_API_KEY`; Ollama-compatible
  via `OPENAI_API_BASE`). **Anthropic LLM provider** (`ANTHROPIC_API_KEY`).
- **Simplified type syntax** (`normalize_type`): stage spec files accept
  `"Text"` instead of `{"kind":"Text"}`, `{"Record":[["field","Text"]]}`
  instead of the verbose canonical form.
- Stage spec `tags` and `aliases` parsed from simple format.
- **Deprecated stage resolution**: `noether run` transparently follows
  `Deprecated → successor_id` chains.
- **370 stage specs** across 50 open-source libraries (in
  `noether-cloud/stages/`).
- Capability benchmark with 4 scenarios: type safety, parallel execution,
  reusability, token analysis.
- `noether store dedup --apply` uses `Deprecated{successor_id}` instead
  of `Tombstone`, preserving forward pointers.
- `noether stage list` defaults to Active lifecycle filter.

### [0.5.0] — 2026-04-08 *(internal)*

- **Runtime budget enforcement**: `BudgetedExecutor<E>` with atomic cost
  accounting. `noether run --budget-cents <n>`.
  `CompositionResult::spent_cents` reports actual spend.
- **Cloud Registry hardening**: `DELETE /stages/:id`, offset-based
  pagination, `get_live` for on-demand fetches. `noether-scheduler` gains
  `registry_url` / `registry_api_key` config fields.
- **NixExecutor hardening**: configurable timeout, output caps, stderr
  cap. Wall-clock timeout via `mpsc::channel` + `kill -9`.
  `classify_error` distinguishes Nix infra failures from user code
  errors. `NixExecutor::warmup()` pre-fetches the Python 3 runtime.
- **Effects v2**: `EffectKind`, `EffectPolicy`, `infer_effects`,
  `check_effects`, `noether run --allow-effects <...>`. Remote stages
  implicitly carry `Network + Fallible`; unknown stages carry `Unknown`.
- Stdlib count: 76 stages.

### [0.1.0] — 2026-04-07 *(internal)*

- **Phase 0** — Type system (`NType`), structural subtyping, stage
  schema, Ed25519 signing, SHA-256 content addressing.
- **Phase 1** — `StageStore` trait, `MemoryStore`, 50 stdlib stages,
  lifecycle validation.
- **Phase 2** — Lagrange composition graph, type checker,
  `ExecutionPlan`, `run_composition`, traces.
- **Phase 3** — Composition Agent, semantic three-index search,
  `VertexAiLlmProvider`, `noether compose`.
- **Phase 4** — `noether build` with `--serve :PORT` browser dashboard,
  `--dry-run`, store dedup.
- ACLI-compliant CLI with structured JSON output.
- MkDocs documentation site.
