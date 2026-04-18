# Noether Stability Contract

**Effective from:** v1.0.0 (target: 2027-04)
**Draft status:** This document is the proposal under review. It becomes
the contract at the 1.0 release.

This document is what Noether promises — and what it does **not** promise
— across the 1.x line. If the code diverges from this document, the code
is wrong, not the document.

---

## The three-tier contract

Noether identifies stages by **two** hashes, not one, and the contract is
different for each.

### Stage signature ID — stable across 1.x

The **signature ID** (`SignatureId`) is the hex-encoded SHA-256 of the
JCS-canonicalised JSON of `{name, input, output, effects}`. Two stages
with the same values on those four fields have identical signature IDs,
independent of implementation language, source code, or registry
location. **Name is part of the signature** — a rename produces a new
signature ID.

**Promise.** A signature ID resolved from an Active stdlib stage in
v1.0.0 resolves to a stage with identical input/output/effects in every
v1.x release. Same inputs produce same-shaped outputs. The effect set
does not grow (a `Pure` stage in 1.0 stays `Pure` in 1.9).

**Not promised.** Byte-for-byte output equality for stages marked
`NonDeterministic` or `Llm`. Performance. Cost.

### Stage implementation ID — may change on bugfixes

The **implementation ID** (`ImplementationId`, also called `StageId`
for historical reasons — they are type-aliased today) is the
hex-encoded SHA-256 of JCS(`{name, input, output, effects,
implementation_hash}`). The hash **nests** the signature ID: changing
any signature-level field *or* the implementation hash changes the
implementation ID; changing only the implementation hash changes the
implementation ID but leaves the signature ID stable.

**Promise.** When a bugfix changes an implementation ID without changing
the signature ID, graphs that reference the stage by signature keep
working. Graphs with `pinning: "both"` are unaffected — they keep
running the old implementation until the user re-pins.

**Not promised.** That any specific implementation ID remains available
forever. Implementations of `Deprecated` stages may be removed 6 months
after the deprecation announcement.

### Composition ID — stable under canonicalisation

The **composition ID** is the SHA-256 of the canonical form of the
composition graph (see `docs/architecture/semantics.md`). Two graphs that
canonicalise to identical trees have identical composition IDs.

**Promise.** The canonicalisation rules listed in `semantics.md` are
frozen at 1.0 and may only grow additive in 1.x. A graph's composition
ID computed in 1.0 is the same when recomputed in 1.9.

**Not promised.** Stability across major versions. 2.0 may add rules
that cause semantically-equivalent graphs to hash differently than they
did under 1.x.

---

## Operator semantics — frozen at 1.0

Every composition operator documented in `docs/architecture/semantics.md`
is frozen at its 1.0 meaning:

- `Sequential`, `Parallel`, `Fanout`, `Branch`, `Merge`, `Retry`, `Const`,
  `Let`, `Stage`, `RemoteStage`

**Promise.** Denotational meaning as written in semantics.md will not
change in 1.x. Property laws `L1–L13` pass in every 1.x release.

**Not promised.** New operators added in 1.x. Those are additive — they
don't change what existing operators do.

---

## Stdlib stage semantics — frozen at 1.0

Every stage present in the stdlib at 1.0 has its semantics frozen. A
stage whose behaviour needs to change ships as a **new stage with a new
name**, and the old stage is deprecated per the deprecation policy
below.

**Promise.** `csv_parse_rows` in 1.0 and `csv_parse_rows` in 1.9 accept
the same inputs and produce the same outputs. Same effects. Same
properties (the properties list may grow, but existing entries cannot
be removed).

**Not promised.** Implementation-level performance, memory footprint, or
dependency graph. Those may change freely so long as the declared
contract holds.

---

## Stage properties wire format — additive kinds

Every stored Stage may carry a `properties` array. Each entry is a
structured object tagged by `kind`:

```json
"properties": [
  { "kind": "set_member", "field": "output.severity",
    "set": ["CRITICAL", "HIGH", "WARNING"] },
  { "kind": "range", "field": "output.soc_percent",
    "min": 0.0, "max": 100.0 }
]
```

**Kinds frozen at 1.0.** `"set_member"` and `"range"` carry the
meanings documented in `crates/noether-core/src/stage/property.rs`.

**Promise.** Existing `kind` strings, their required fields, and their
evaluation semantics are frozen across 1.x. New `kind` variants may
land (additive); a 1.0 reader loads them as `Property::Unknown` and
skips them in aggregation rather than erroring. Readers MUST NOT treat
an unknown kind as "property holds".

**Properties are not part of the content hash** — a stage's
`signature_id` and `id` are determined by `(name, input, output,
effects[, implementation_hash])` only. Adding or tightening a
property changes the stage's declared guarantees but not its
identity. Existing entries cannot be removed or weakened within 1.x.

**Field paths.** The `field` string is a dot-separated path rooted at
`input` or `output`. Those two roots are frozen at 1.0.

---

## Graph node `pinning` — frozen variants

Every `CompositionNode::Stage` carries an optional `pinning` enum that
determines how the node's `id` field resolves in the store. At 1.0 the
variants are:

- `"signature"` (default, omitted in JSON) — `id` is a `SignatureId`;
  the resolver returns the current Active implementation.
- `"both"` — `id` is an `ImplementationId`; the resolver requires an
  exact match and refuses to fall back to a different implementation.

**Promise.** The two variant strings and their wire-level meaning are
frozen at 1.0. New variants may be added in 1.x (additive); existing
ones cannot change. Omitting `pinning` continues to mean `"signature"`
across 1.x.

**Not promised.** That legacy pre-M2 graphs with a bare `"id"` field
(pre-v0.6.0) load identically under every 1.x release — they may be
accepted with a deprecation warning today and removed later in a major.

---

## Graph JSON schema — additive-only within 1.x

The composition-graph JSON format (`CompositionNode` tag = "op" union)
can grow new fields and new operator variants in minor releases, but
**existing fields cannot be removed or repurposed** within 1.x.

**Promise.** A graph JSON that loads under 1.0 loads under 1.9. Unknown
fields on operators are ignored by older versions — so 1.0 can forward-
load a 1.5-created graph as long as it uses 1.0 operators.

**Not promised.** Graphs that use operators introduced in 1.5 will not
load under 1.0. Forward compatibility is bounded by which operators the
reader knows.

---

## Registry API — stable across 1.x

The noether-cloud registry HTTP surface:

- `POST /stages`
- `GET /stages`, `GET /stages/:id`
- `PATCH /stages/:id/lifecycle`
- `DELETE /stages/:id` — soft-delete (Tombstone) by admin
- `GET /stages/search`
- `POST /compositions/run`
- `GET /health`

**Promise.** Endpoints keep their HTTP verb, path, and response schema
(additive-only) across 1.x. Clients written against 1.0 keep working
against a 1.9 registry.

**Not promised.** New endpoints added in 1.x. Deprecated fields may be
marked deprecated but remain in responses until 2.0.

---

## Deprecation policy

A stage, operator, or API endpoint may be **deprecated** at any point in
1.x. Deprecation means:

1. Deprecated-badge on the documentation.
2. Successor pointer (what to use instead).
3. Runtime warning the first time it's invoked in a session.
4. **No removal inside 1.x.** Deprecated items keep executing until 2.0.

Deprecation notice minimums:

- Stages: 6 months before removal is proposed (removal lands in a later
  major).
- Operators: 12 months.
- Registry endpoints: 12 months.

---

## CLI surface — stable command names and flags

`noether stage`, `noether run`, `noether compose`, `noether trace`,
`noether store` subcommands and their documented flags are stable across
1.x. New flags may be added (additive). Existing flags keep their meaning
and their default value.

**Not promised.** Unrouted/undocumented flags, internal-use flags, or
anything in `noether research` / `noether internal` namespaces.

---

## Minimum supported Rust version (MSRV)

**Promise.** 1.x releases compile with Rust stable `1.83` or newer. We
pin the MSRV in `rust-toolchain.toml` and gate CI on it. Bumping the
MSRV within 1.x requires a 6-month notice in the changelog.

**Not promised.** Compilation with nightly-only features, stable
versions older than the pinned MSRV, or non-standard targets (WASM
beyond the listed `wasm32-unknown-unknown` browser target).

---

## Public Rust crate surface

**Promise.** The following crate names publish to crates.io and ship
`pub` symbols covered by the contract above:

- `noether-core` — types, effects, stages, hashes, stdlib loader.
- `noether-store` — `StageStore` trait + Memory / JsonFile impls.
- `noether-engine` — composition engine, checker, planner, runner,
  canonicalisation, semantic index.
- `noether-cli` — `noether` binary.
- `noether-scheduler` — `noether-scheduler` binary.

**Not promised.** `noether-grid-*` crates (experimental;
`publish = false` today). Test crates, `xtask`-style tooling crates,
and any `pub` item gated on `#[doc(hidden)]` or living in modules
named `internal` / `experimental`.

---

## On-disk formats

**Promise.** The following file formats are stable across 1.x —
additive changes only, never field removal or repurposing:

- **JsonFileStore** (`.noether/registry.json`) — the list-of-stages
  JSON that `noether stage add` writes. Loader must accept prior 1.x
  outputs.
- **Lagrange graph** (`*.json`, typically `graph.json`) — the
  `CompositionGraph` JSON that `noether run` consumes. Additive fields
  allowed on `CompositionNode` variants; readers ignore unknown ones.
- **Composition trace** (written by `noether run --trace` / read by
  `noether trace`) — the `CompositionTrace` JSON.
- **Ed25519 stage signatures** — hex-encoded 64-byte Ed25519 signature
  over the stage id bytes. Verification key is hex-encoded 32-byte
  Ed25519 public key. Format frozen; new signing schemes (if any) ship
  alongside the existing one, not in place of it.

**Not promised.** Internal binary formats like cached build artefacts
in `target/` or the semantic index on-disk cache. Those regenerate
from scratch on version mismatch.

---

## Environment variable contract

Environment variables that are part of the public surface:

- `NOETHER_REGISTRY` — registry URL (CLI).
- `NOETHER_API_KEY` / `NOETHER_API_KEYS` — registry authentication.
- `NOETHER_LLM_PROVIDER` — explicit LLM provider selection.
- `VERTEX_AI_*`, `OPENAI_*`, `ANTHROPIC_*`, `MISTRAL_*` — provider-
  specific credentials and overrides.

**Promise.** Variable names and their documented effect are stable
across 1.x. Additional variables may be added (additive).

**Not promised.** Non-documented variables (typically `NOETHER_DEBUG_*`,
`NOETHER_TEST_*`) are implementation details and may change.

---

## What the contract does NOT cover

- Source-code stability. Rust crate internal APIs (`pub fn` inside a
  crate that isn't re-exported from the top level) can change freely.
- Wire format of `noether-grid-*`. Experimental; the crates are
  `publish = false` today and their ship posture for 1.0 is an open
  decision tracked in `docs/roadmap/2026-04-18-rock-solid-plan.md`.
- WASM target. Experimental; may be removed if unused by 1.0.
- Performance SLAs. Noether is a single-maintainer project; we don't
  commit to microbenchmarks across releases.
- Cost SLAs. `cost_estimate_cents` on stages is advisory, not contractual.

---

## CI enforcement

A 1.0 release blocks merges that violate this contract:

- `scripts/check_breaking_change.sh` diffs stdlib stage signatures
  against the last tagged release. Any signature hash delta on a
  non-deprecated stage fails CI.
- Graph-JSON schema diffs are checked against an additive-only rule set.
- `STABILITY.md` itself is checksummed; edits to normative sections
  require a major-version bump.

CI scripting lands with M4 (the 1.0 milestone). Until 1.0, this document
is informative.

---

## Versioning

Noether follows SemVer **over the stability contract above**, not over
every public Rust API. Interpret the numbers like this:

- **Patch** (1.0.x) — bugfixes. Implementation IDs of existing stdlib
  stages may change. Signature IDs do not. No new operators, no new
  CLI flags.
- **Minor** (1.x.0) — new additive functionality: new stdlib stages,
  new operators, new CLI flags, new registry endpoints. Nothing in the
  "Promise" sections above changes.
- **Major** (2.0.0) — anything in the "Promise" sections may change.
  Migration guide ships with the release.

---

## How to verify a given release upholds this contract

1. CI enforcement (`scripts/check_breaking_change.sh`) lands with M4
   (the 1.0 milestone). Until then, this document is informative.
2. Property-level regressions: run `noether stage verify --properties`
   against the stdlib. Every stage ships with properties **where they
   are naturally expressible in the DSL**, and each declared property
   must hold for every declared example. The earlier "≥3 per stage"
   target is **not** enforced: the v0.6 DSL (SetMember, Range) only
   expresses numeric bounds and enumerated sets, which don't fit most
   stages that transform data structurally (e.g. `text_reverse`,
   `filter`, `map`). DSL expansion to cross-field equality and
   input-dependent ranges lands in a follow-up milestone
   (`docs/roadmap/2026-04-18-property-dsl-expansion.md`).
3. For air-gapped bit-exact pinning, set `pinning: "both"` on every
   `Stage` node in the graph and include the `implementation_id`. The
   resolver refuses to substitute any other implementation.

---

## What to do if this contract is violated

File an issue at <https://github.com/alpibrusl/noether/issues> with the
label `stability-break`. Those issues are triaged before feature work.
Breaking the contract without a major-version bump is considered a
release-blocking bug.
