# Changelog

Notable changes to Noether. Follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions are SemVer-flavoured per [STABILITY.md](STABILITY.md).

## Unreleased

### Changed — `noether run` memoizes Pure stages by default (M3 `memoize_pure`)

The `PureStageCache` has existed in the engine since earlier hardening but `cmd_run` was calling the non-cached `run_composition` variant, so the cache only fired when callers opted in manually. Now `cmd_run` builds a `PureStageCache::from_store(store)` and passes it to `run_composition_with_cache` on every invocation. Within a single run, a repeated `(stage_id, input)` pair on a Pure-tagged stage hits the cache instead of re-executing.

The cache is in-memory, single-run, never persisted. Non-Pure stages are unaffected: the cache rejects them at `get`/`put` time via the pre-populated `pure_ids` set.

- **Opt out:** `NOETHER_NO_MEMOIZE=1` (or `=true`) — matches the shape of `NOETHER_NO_OPTIMIZE` for benchmarks and bug repros where every dispatch must go through the executor.
- **ACLI envelope:** the success response gains a `memoize: { enabled, hits, misses }` field when at least one hit fired or the opt-out was set. A run with no hits and memoization on leaves the envelope alone to avoid clutter.

This is the M3 `memoize_pure` deliverable, landed as a thin executor-level wiring change rather than a new AST pass. No new cache logic — the existing `PureStageCache` was already well-tested in `crates/noether-engine/src/executor/pure_cache.rs`.

### Added — `canonical_structural` optimizer pass (M3 second slice)

Lifts the M1 canonical-form structural rewrites (flatten nested `Sequential`, collapse singleton `Sequential`, fuse adjacent `Retry`) from hash-time into the execution pipeline. Today `canonicalise` shapes the form we hash; this pass makes the **executor** see the same canonical form, removing pointless wrapper nodes from plans and traces.

Pass list in `cmd_run` now runs `CanonicalStructural` first, then `DeadBranchElimination` — order chosen so dead-branch can fold `Branch` nodes that were hidden inside a collapsible singleton Sequential wrapper.

The rewrites delegate to the existing `canonicalise` function, whose semantics are locked by the M1 law tests. Seven new unit tests cover each rule individually plus the "idempotent after one pass" property that the fixpoint runner relies on.

### Added — graph optimizer framework + `dead_branch` pass (M3 first slice)

A structural AST rewrite layer between type-check and plan generation:

```text
parse → resolve → check_graph → [optimize] → plan → execute
```

Optimizer passes are **semantics-preserving** — they reshape the tree, never the leaf stage identities. `composition_id` is computed on the pre-resolution canonical form, so it stays stable across optimization regardless of what passes run.

- `noether_engine::optimizer::OptimizerPass` trait: `name()` + `rewrite(node) -> (node, changed)`.
- `noether_engine::optimizer::optimize(node, passes, max_iterations)` — fixpoint runner. Returns an `OptimizerReport` listing which passes fired and whether the iteration cap was hit (guards against oscillating passes).
- Default iteration cap: `DEFAULT_MAX_ITERATIONS = 16`.

**First pass:** `dead_branch::DeadBranchElimination`. When a `Branch`'s `predicate` is a `Const(Bool)` node, fold the `Branch` to the selected arm and recurse into it. Non-constant predicates and non-bool constants are left alone. Common on agent-generated graphs where the LLM emits a defensive `Branch(Const(true), real, fallback)` shape; folding lets the planner skip wiring the dead arm entirely.

Other optimizer passes listed in the M3 milestone (`fuse_pure_sequential`, `hoist_invariant`, `memoize_pure`) land as separate PRs — the framework makes each of them a ~300-LOC increment.

### Changed — `noether run` optimizes graphs by default

`cmd_run` now invokes the optimizer between type-check and plan. Set `NOETHER_NO_OPTIMIZE=1` (or `NOETHER_NO_OPTIMIZE=true`) to disable — intended for trace debugging and bug repros where the literal graph must reach the executor unchanged.

The dry-run ACLI envelope gains an `optimizer` field reporting `passes_applied`, `iterations`, and `hit_cap` so operators can see what the optimizer did without re-running with the env var set.

### Added — filesystem-scoped effects (M3.x, [#39](https://github.com/alpibrusl/noether/issues/39) follow-up)

Two new variants on `noether_core::effects::Effect`:

- `Effect::FsRead { path: PathBuf }` — stage reads a specific host path.
- `Effect::FsWrite { path: PathBuf }` — stage writes to a specific host path.

`EffectKind::FsRead` / `EffectKind::FsWrite` mirror the variants; `Effect::kind()` and `EffectKind::fmt` know about them. CLI `--allow-effects` accepts `fs-read` / `fs-write` tokens.

### Changed — `IsolationPolicy::from_effects` now drives bind mounts from effects

The function now scans the `EffectSet` for path-bearing filesystem effects:

- `FsRead(p)` appends `RoBind { host: p, sandbox: p }` to `ro_binds`.
- `FsWrite(p)` appends `RwBind { host: p, sandbox: p }` to `rw_binds`.

`/nix/store` is still unconditionally bound read-only (Nix-pinned runtimes need it). Multiple effects of the same variant produce multiple binds — declaring `FsRead(/etc)` and `FsRead(/usr/share)` yields two `--ro-bind` entries. The mount-order contract from [#39](https://github.com/alpibrusl/noether/pull/47) (rw → ro → work_host) still holds when binds are effect-driven.

### Closes the gap #39 flagged

When `#39` landed, `from_effects` produced empty `rw_binds` — the `EffectSet` vocabulary simply had no way to express "stage writes to /tmp/out". Consumers (agentspec's `filesystem: scoped`, agent-coding runtimes) had to construct `IsolationPolicy` by hand. With this milestone, a stage can declare its filesystem surface in the signature and `from_effects` does the right thing without caller intervention.

This is a deliberate trust-widening surface on the effect side. Binding `/home/user` RW still grants broad host access — the rustdoc on the new variants keeps the same framing as `RwBind`: the crate cannot validate whether a declared path is sensible to share; that's a caller-authored policy decision.

### Back-compat

- Existing stages that don't declare `FsRead` / `FsWrite` are bit-identical on the wire. Their `StageId` is unchanged.
- Adding a new filesystem effect to an existing stage changes that stage's `StageId` (as it should — the behaviour just changed).
- Wire format: `{"effect": "FsRead", "path": "/etc"}` matches the existing `#[serde(tag = "effect")]` shape the other variants use. Non-Rust consumers (the Python bindings agentspec will grow against `noether-sandbox`) get a uniform schema.

## 0.7.3 — 2026-04-20

Release-pipeline repair. **No source changes in this version — it exists to re-publish crates and ship a new set of release tarballs through the fixed workflow.**

### Fixed — release-workflow drift ([#51](https://github.com/alpibrusl/noether/issues/51), [#52](https://github.com/alpibrusl/noether/pull/52))

The `publish-crates` job has been silently failing since v0.7.1 because `noether-engine` depends on `noether-isolation` (introduced in v0.7.0) but the publish chain never included it. Every release cut since shipped green Build jobs, red Publish job — tags landed on GitHub while crates.io stayed behind at 0.7.0 for `noether-engine` / `noether-cli` / `noether-scheduler`, with `noether-isolation` and `noether-sandbox` never published at all.

The v0.7.3 workflow:

- Publishes in the correct dep order: `core → isolation → store → engine → cli → scheduler → sandbox`.
- Ships a `noether-sandbox-<version>-<linux-target>.tar.gz` tarball on the GitHub release for each Linux target. Built on every target as a compile-check; only packaged on Linux because bubblewrap is Linux-only.

### Downstream impact

If you were pinned via `cargo install noether-cli` or `cargo install noether-scheduler` before v0.7.3, you were stuck on 0.7.0 and missing everything from v0.7.1 (`noether-isolation` crate, `noether-sandbox` binary) and v0.7.2 (`rw_binds`, executor panic-to-error conversions, tutorial pages, coverage CI). A `cargo install --force` after the v0.7.3 publish lifts you to the full current state.

## 0.7.2 — 2026-04-20

Maintenance release — one small feature, hardening, docs audit, CI coverage.

### Added — `IsolationPolicy.rw_binds` ([#39](https://github.com/alpibrusl/noether/issues/39))

Optional `Vec<RwBind>` on `IsolationPolicy`, mirroring `ro_binds`. Consumers with a richer filesystem trust model (agentspec's `filesystem: scoped`, the "agent operates on my `~/projects/foo` RW" pattern) can now declare read-write bind mounts without routing through `work_host` — which is reserved for the single sandbox scratch dir.

- New `RwBind { host, sandbox }` struct — same shape and `From<(PathBuf, PathBuf)>` convenience as `RoBind`.
- `rw_binds: Vec<RwBind>` field on `IsolationPolicy`, `#[serde(default)]` + `skip_serializing_if = "Vec::is_empty"`. Policies on the wire that predate 0.7.2 deserialise to an empty vec; the field doesn't emit when unused.
- `build_bwrap_command` emits `--bind <host> <sandbox>` per entry, in a documented order: **`rw_binds` → `ro_binds` → `work_host`.** RW first lets a narrower RO entry shadow a broader RW parent (the `workdir RW, .ssh RO` case); `work_host` renders last so its `/work` mapping wins.
- `from_effects` does **not** produce `rw_binds`. The `EffectSet` vocabulary has no `FsWrite(path)` variant to drive it, so any `RwBind` is a caller-authored trust decision. The `RwBind` rustdoc spells this out — the crate can't validate whether binding `/home/user` RW is sensible; that responsibility lives with the caller.

agentspec's `TrustSpec.filesystem: scoped` mode can now delegate to `noether-sandbox` via a policy carrying explicit `rw_binds` — see [agentspec #22](https://github.com/alpibrusl/agentspec/pull/22) for the integration path.

### Changed — CLI-reachable `unwrap()` / `expect()` in executor + index converted to `Result` ([#42](https://github.com/alpibrusl/noether/issues/42))

Thread-join panics in `Parallel` and `Let` branches no longer propagate as process-level panics; they surface as typed `ExecutionError::StageFailed` with synthetic `parallel:<name>` / `let:<name>` stage ids so the ACLI envelope shape stays structured. The `CachedEmbeddingProvider` short-read panic was hardened into a typed `EmbeddingError::Provider` with an upstream length check that catches the real failure mode before the in-memory cache lookup. `NixExecutor::extract_pip_requirements` lost its `strip_prefix(...).unwrap()` via an `if let Some(...) = ... else { continue }` rewrite.

Seven in-scope modules now carry `#![warn(clippy::unwrap_used)]` with the standard `#[cfg_attr(test, allow(...))]` pairing, preventing regression on newly-added panics. An audit table — one row per `unwrap`/`expect` call site in the in-scope files — lives at `docs/engineering/unwrap-audit-issue-42.md`, distinguishing converted vs invariant-safe. Out-of-scope hot paths (`executor/runtime.rs`, `executor/budget.rs`, `executor/stages/*`, `planner.rs`, `checker.rs`, grid/scheduler/cli crates) are flagged for a follow-up.

### Changed — `noether stage verify` flag-name drift cleaned up

Earlier release notes and three agent playbooks referred to `--with-properties` / `--signatures-only` flags that never landed. The real v0.7.0+ CLI uses `--signatures` (restricts to signature checks) and `--properties` (restricts to property checks); invoking `stage verify` with no flag runs both. The docs, CHANGELOG, roadmap, and `Stage::check_properties` rustdoc now match the shipped CLI.

No code change — the drift was docs-only. Called out here so readers of the v0.7.0 entry don't trip over the old wording.

### Docs — mkdocs audit + human tutorial section ([#41](https://github.com/alpibrusl/noether/pull/41))

Systematic pass over `docs/` to catch content that had drifted against v0.7.x state. The `docs/index.md` trust-model callout, `nix-execution.md` reproducibility-vs-isolation admonition, and `stage-identity.md` `canonical_id`-removal phrasing all got corrected. Added a milestones table to `roadmap.md` (M1 / M2 / M2.4 / M2.5 / M2.x / M3) alongside the existing phase table, and 0.7.0 + 0.7.1 entries to `docs/changelog.md` with a pointer at root CHANGELOG.md as authoritative.

New three-page human tutorial section: `concepts.md` (5-minute mental model — stage identity, structural types, effects, composition, reproducibility vs isolation), `llm-compose.md` (end-to-end `noether compose` workflow), `when-things-go-wrong.md` (exit-code contract, isolation failures, diagnosis recipes). The existing `citecheck` walkthrough gains a front-of-page warning admonition flagging that the body uses CLI shapes (`noether lint`, `--stage`, `noether skill`, pre-Lagrange graph format) that never landed — rewrite deferred.

### CI — coverage reporting via cargo-llvm-cov ([#43](https://github.com/alpibrusl/noether/issues/43))

New `coverage` job in CI runs `cargo-llvm-cov` on `noether-core`, `noether-engine --lib`, and `noether-store`, uploads to Codecov. `codecov.yml` at repo root defines thresholds: 80% blocking on the three stable crates, 60% informational (non-blocking) on `noether-grid-broker` / `noether-grid-worker` / `noether-scheduler` to avoid red-walling the baseline against known-empty data.

**Operators:** add `CODECOV_TOKEN` as a repo secret before relying on Codecov dashboards; `fail_ci_if_error: false` is set so missing token silently no-ops the upload step rather than red-lining CI.

## 0.7.1 — 2026-04-19

Small release: extract the isolation primitive into its own crate and ship a standalone sandbox binary for non-Rust consumers.

### Added — `noether-isolation` crate

All the sandbox-policy types from v0.7.0 live in a new `noether-isolation` crate instead of buried inside `noether-engine`:

- `IsolationBackend::{None, Bwrap{bwrap_path}}` + `auto()` + `from_flag()`
- `IsolationError::{UnknownBackend, BackendUnavailable}`
- `IsolationPolicy` (now `Serialize + Deserialize` for cross-process use)
- `IsolationPolicy::from_effects(&EffectSet)` / `IsolationPolicy::with_work_host(PathBuf)`
- `build_bwrap_command(bwrap, policy, inner_cmd) -> Command`
- `find_bwrap()` with trusted-path-first discovery
- Constants: `NOBODY_UID`, `NOBODY_GID`, `TRUSTED_BWRAP_PATHS`

Dependency footprint: `noether-core` (for `Effect` / `EffectSet`), `serde`, `thiserror`, `tracing`. Downstream consumers that want the sandbox primitive without the full composition engine now depend on this crate directly.

`noether-engine::executor::isolation` is a thin re-export — existing callers see no API change.

### Added — `noether-sandbox` binary

Thin glue binary (~300 LOC including parser tests) for non-Rust callers:

```bash
echo '{"ro_binds":[{"host":"/nix/store","sandbox":"/nix/store"}], "network":true, "env_allowlist":["PATH","LANG"]}' \
  | noether-sandbox -- claude-code -p "hello"
```

- Reads an `IsolationPolicy` as JSON on stdin or from `--policy-file <path>` (file variant leaves stdin free for the child). Empty stdin → default pure-effect policy.
- `--isolate=auto|bwrap|none` flag mirrors the `noether run` CLI; also reads `NOETHER_ISOLATION` env.
- `--require-isolation` / `NOETHER_REQUIRE_ISOLATION=1` turns `auto → none` fallback into a hard exit (parity with `noether run --require-isolation`).
- Exit code: child's exit for normal termination; `128 + signum` for signal-death (bash/zsh convention so automation can detect SIGTERM/SIGKILL/etc.); `2` for argument or policy errors; `127` for spawn failure.
- 1 MiB cap on stdin policy size — use `--policy-file` for larger policies.
- stdin (when not consumed for the policy) / stdout / stderr pass through to the sandboxed child.

Intended for Python / Node / Go / shell callers (notably agentspec — tracked in [#36](https://github.com/alpibrusl/noether/issues/36)) that want to delegate to noether's sandbox without embedding a Rust toolchain.

### Changed — `IsolationPolicy` is now Serde-enabled

Wire format:

```json
{
  "ro_binds": [{"host": "/nix/store", "sandbox": "/nix/store"}],
  "network": false,
  "env_allowlist": ["PATH", "HOME", "USER", "LANG", "LC_ALL", "LC_CTYPE", "NIX_PATH", "NIX_SSL_CERT_FILE", "SSL_CERT_FILE", "NOETHER_LOG_LEVEL", "RUST_LOG"]
}
```

`ro_binds` entries are `{host, sandbox}` records (not tuples) so language bindings can map them to native record types. `work_host` is omitted when unset (sandbox-private tmpfs at `/work` — the default). Round-trip pinned by a test.

## 0.7.0 — 2026-04-19

M2 close-out: property DSL reaches parity with the "what does this stage guarantee?" use case, the resolver runs at every graph-ingest entry point, the store enforces its Active-per-signature invariant, and stage subprocesses now execute inside a real sandbox by default. The v1.x stability contract ([STABILITY.md](STABILITY.md)) applies from this release.

### Added — sandbox isolation (#34)

`noether run --isolate=auto` (the default from v0.7) wraps every stage subprocess in [bubblewrap](https://github.com/containers/bubblewrap) when available. The sandbox:

- Runs as UID/GID 65534 (`nobody`), independent of the invoking user's real identity.
- Unshares user / pid / mount / uts / ipc / cgroup namespaces; unshares the network namespace unless the stage declares `Effect::Network`.
- Binds `/nix/store` read-only + `cache_dir` read-only; everywhere else on the host filesystem is invisible.
- Uses a sandbox-private tmpfs at `/work` — no host-side workdir to predict, race, or clean up.
- Drops all Linux capabilities (`--cap-drop ALL`).
- Starts a fresh session (`--new-session`) so a stage can't signal the parent shell.
- Binds `/etc/resolv.conf`, `/etc/hosts`, `/etc/nsswitch.conf`, `/etc/ssl/certs` read-only **only when** `Effect::Network` is declared, so DNS and TLS work for opt-in stages.
- Resolves `bwrap` from a fixed list of root-owned paths (`/run/current-system/sw/bin`, `/nix/var/nix/profiles/default/bin`, `/usr/bin`, `/usr/local/bin`) before falling back to `$PATH`; the fallback emits a one-shot warning so operators notice if isolation is trusting an attacker-plantable lookup.

New CLI flags:

- `--isolate=auto|bwrap|none` (default `auto`); also readable from `NOETHER_ISOLATION`.
- `--unsafe-no-isolation` silences the warning when the user deliberately opts out.
- `--require-isolation` (also `NOETHER_REQUIRE_ISOLATION=1`) turns the `auto → none` fallback into a hard exit, for CI and production.

Phase 2 (native `unshare` + Landlock + seccomp, same `IsolationPolicy`, ~10× lower startup cost) is v0.8 work. See [`docs/roadmap/2026-04-18-stage-isolation.md`](docs/roadmap/2026-04-18-stage-isolation.md).

**Caveat**: Isolation requires nix to be installed under `/nix/store` (upstream or Determinate installer). A distro-packaged `/usr/bin/nix` is dynamically linked against host libraries that aren't bound into the sandbox; the executor refuses to run under isolation in that case with a clear message.

Security tests now include a real adversarial suite (`tests/isolation_escape.rs`) that spawns bwrap with Python and verifies `setuid(0)`, `chroot("/")`, opening `/etc/shadow`, reading `~/.ssh/*`, and making DNS calls with network disabled all fail.

### Added — property DSL expansion (#31, #35)

Five new [`Property`](crates/noether-core/src/stage/property.rs) variants on top of the v0.6 `SetMember` / `Range`:

- `FieldLengthEq { left_field, right_field }` — two fields have equal length. Length is UTF-8 code-point count for strings, element count for arrays, key count for objects.
- `FieldLengthMax { subject_field, bound_field }` — subject length ≤ bound length. Useful for `filter`, `take`, `list_dedup`.
- `SubsetOf { subject_field, super_field }` — every element (arrays), (key, value) pair (objects), or contiguous substring (strings) of subject appears in super.
- `Equals { left_field, right_field }` — two fields are JSON-value equal.
- `FieldTypeIn { field, allowed: Vec<JsonKind> }` — the runtime JSON type is one of a typed enum of kinds. Wire format stays snake-case strings; typos fail at deserialization.

`Property::Unknown` is the forward-compat escape for future variants; `Property::shadowed_known_kind()` distinguishes "genuinely unknown kind" (safe to skip) from "typo inside a known kind" (rejected at ingest via the new `ValidationError::ShadowedKnownKind`).

Every stdlib stage carries ≥3 properties. Roadmap: [`docs/roadmap/2026-04-18-property-dsl-expansion.md`](docs/roadmap/2026-04-18-property-dsl-expansion.md).

### Changed — resolver runs at every graph-ingest entry point (#32)

The stage-identity-rewriting pass (`resolve_pinning` + the new `resolve_deprecated_stages` in `noether_engine::lagrange::deprecation`) is now invoked from every place a graph enters the system:

- `noether run`, `noether compose` (both cache-hit and fresh paths), `noether build`, `noether build --target=browser`, `noether serve`
- `noether-scheduler` on each fired job
- `noether-grid-broker::routes` on `POST /jobs`
- `noether-grid-worker::run_graph` on worker dispatch

The `composition_id` is always computed **before** resolution — the M1 / #28 "canonical form is identity" contract. Hashing after resolution would produce unstable IDs across days as the store's Active implementation rotates.

A shared `crates/noether-cli/src/commands/resolver_utils.rs` module collects the stderr-diagnostic version used by the CLI binaries; the broker and worker call the engine-level helpers directly and route diagnostics through `tracing` instead.

The new `DeprecationReport { rewrites, events }` distinguishes routine rewrites from anomalies (`ChainEvent::CycleDetected`, `ChainEvent::MaxHopsExceeded`) explicitly — silent truncation of broken deprecation chains is gone.

### Changed — store enforces ≤1 Active per signature (#33)

`MemoryStore::put` / `MemoryStore::upsert` and the `JsonFileStore` equivalents now auto-deprecate any existing Active stage whose `signature_id` matches an incoming Active. The previous code enforced this only in the `noether stage add` CLI path; direct library `put` could silently violate it. The shared `noether_store::invariant` module centralises the detection, and every auto-deprecation emits a structured `tracing::warn!` with the deprecated and successor IDs so operators see the state change.

### Changed — stage identity is split into `signature_id` + `implementation_id`

Two content-addressed IDs per stage:

- `signature_id` = `SHA-256(name + input + output + effects)` — stable across bugfix-only impl rewrites.
- `implementation_id` (alias: `StageId`) = `SHA-256(signature_id + implementation_hash)` — changes when the code changes.

Composition graphs pin by `signature_id` by default (new `Pinning::Signature` on `CompositionNode::Stage`); add `pinning: "both"` to require an exact implementation match. A bugfix that changes `implementation_hash` changes the `StageId` but not the `signature_id`, so graphs pinned by signature keep working.

Stages on the wire now carry both `id` and `signature_id` fields. `canonical_id` is accepted on deserialization as a deprecated alias for `signature_id` (removal scheduled for 0.7.x) for v0.6.x back-compat.

### Added — properties checked by default in `noether stage verify`

`noether stage verify <id>` now checks both the Ed25519 signature and the declarative properties (against the stage's own `examples`) by default. Passing `--signatures` restricts the run to signature checks; `--properties` restricts to properties. Passing both (or neither) runs both checks, as does the default invocation.

(Early drafts of these release notes referred to `--with-properties` / `--signatures-only`. Those flag names never landed — the shipped CLI uses `--signatures` / `--properties` as described above.)

### Added — [STABILITY.md](STABILITY.md)

Formal 1.x compatibility contract: what fields are stable on the wire, what's additive, what's deprecated, and what can change. Reviewers pinning a composition for 1.x should read this.

### Dependency updates

- `arrow` 54 → 58 (#11)
- `getrandom` 0.2 → 0.4 (#10); WASM target now uses the `wasm_js` feature
- `rand` stays on 0.8 (`ed25519-dalek` 2 pins `rand_core` 0.6; blocks rand 0.10 until ed25519-dalek 3 stable)

### Known limitations / deferred work

- **Filesystem-scoped effects.** `Effect::FsRead(path)` / `Effect::FsWrite(path)` don't exist in the v0.6 vocabulary, so the Phase-1 sandbox defaults to "no `/etc`, no `$HOME`, no arbitrary paths." A stage that legitimately needs a specific host path can't run under isolation today. Extending `Effect` with pathful filesystem variants is Phase-2 / v0.8 work.
- **Phase 2 isolation (native namespaces + Landlock + seccomp)** is v0.8. Phase-1 bwrap ships here.
- **`validate_against_types`** punts on the five new relational property variants — they return `Ok(())` at registration. Structural checks (length-on-numeric, equals-on-disjoint-types) land with M3 refinement types.
- **`"unknown"` composition-id fallbacks** still exist in `run.rs`, `grid-worker`, `scheduler`, `grid-broker`, and the `"embedded"` variant in `build.rs`. Each has different error-surface semantics; a focused follow-up replaces them with loud failure matching the `compose.rs` shape.

### Breaking changes

- `NixExecutor::register` (the unsafe default that stamped `EffectSet::pure()`) is removed. Synthesized-stage registration goes through `NixExecutor::register_with_effects` and `CompositeExecutor::register_synthesized` now takes an `EffectSet` argument. `SynthesisResult` carries the inferred effects forward so the agent → executor handoff is no longer lossy.
- `IsolationPolicy::from_effects` no longer takes a `work_host: PathBuf` argument (the sandbox defaults to a private tmpfs; pass `.with_work_host(...)` to opt back in to a host-visible scratch dir).
- `resolve_deprecated_stages` moved from `noether_cli::commands::resolver_utils` (pub(crate)) to `noether_engine::lagrange::deprecation` (pub). Return type changed from `Vec<(StageId, StageId)>` to `DeprecationReport { rewrites, events }`.
- `Stage.canonical_id` is accepted on the wire but removed from the Rust type — use `signature_id` directly. The JSON alias stays through 0.7.x.

## 0.6.0 — earlier

See the git log: `git log v0.5.0..v0.6.0`. In short: canonical composition form, stage identity split groundwork, property DSL (M1), resolver-normalisation pass.
