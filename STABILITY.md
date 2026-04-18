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

The **signature ID** is the SHA-256 of the canonical JSON of the
`StageSignature` fields: `name`, `input`, `output`, `effects`. Two stages
with identical signatures have identical signature IDs, independent of
implementation language, source code, or registry location.

**Promise.** A signature ID resolved from an active stdlib stage in
v1.0.0 will resolve to a stage with identical input/output/effects in
every v1.x release. Same inputs produce same-shaped outputs. Effect set
does not grow (i.e. a `Pure` stage in 1.0 stays `Pure` in 1.9).

**Not promised.** Byte-for-byte output equality for stages marked
`NonDeterministic` or `Llm`. Performance. Cost.

### Stage implementation ID — may change on bugfixes

The **implementation ID** is the SHA-256 of the canonical JSON of the
stage body (script source, config, runtime pins). Two stages with
identical implementations but different signatures (e.g. a rename) have
different signature IDs but may have the same implementation ID.

**Promise.** When a bugfix changes an implementation ID without changing
the signature ID, graphs that reference the stage by signature keep
working. Existing pinned graphs that reference the stage by `both`
(signature + implementation) are unaffected — they keep running the old
implementation until the user re-pins.

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

## What the contract does NOT cover

- Source-code stability. Rust crate internal APIs (`pub fn` inside a
  crate that isn't re-exported from the top level) can change freely.
- Wire format of `noether-grid-*`. Experimental; reserved for 2.0.
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

1. Check the CI status of the `stability` workflow on the release tag.
2. Run `noether stability verify` on your graph. It walks every stage
   reference, confirms each signature ID exists in the registry, and
   reports any pinned implementation IDs that have been deprecated.
3. For air-gapped pinning, use `pinning: "both"` on every node. The
   engine then refuses to resolve a different implementation.

---

## What to do if this contract is violated

File an issue at <https://github.com/alpibrusl/noether/issues> with the
label `stability-break`. Those issues are triaged before feature work.
Breaking the contract without a major-version bump is considered a
release-blocking bug.
