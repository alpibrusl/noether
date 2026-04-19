# Changelog

Notable changes to Noether. Follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions are SemVer-flavoured per [STABILITY.md](STABILITY.md).

## Unreleased

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

### Added — `noether stage verify --with-properties`

`noether stage verify <id>` now checks both signatures and declarative properties (against the stage's own `examples`) by default. Pass `--signatures-only` for the v0.6 behaviour.

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
