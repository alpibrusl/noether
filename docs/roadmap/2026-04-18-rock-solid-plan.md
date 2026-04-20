# Noether 1.0 — Rock-Solid Roadmap

**Status:** Draft · 2026-04-18
**Scope:** 12-month plan, 4 milestones, target end-state = Noether 1.0 with a
written stability contract, property-tested semantics, canonical content
addressing, one curated vertical, one external user.

---

## Premise

Today Noether is v0.4.1. The composition engine works. The type checker
works. `noether compose` works for narrow problems. What's missing before
1.0 is the set of properties that let someone build a multi-year project
on top without being bitten.

"Rock-solid" operationally means five things:

1. **Content addressing is canonical.** Two syntactically different but
   semantically equivalent graphs produce identical composition IDs. Today
   they don't.
2. **Stage semantics are enforceable.** Property predicates are first-class
   alongside examples. Reuse is safe beyond types.
3. **Stability contract is in writing.** Stage signature IDs stable across
   1.x. Implementation fixes don't break pinning. Operator semantics
   frozen. Stdlib semantics frozen.
4. **Stdlib is curated, not accreted.** Admission criteria, deprecation
   path, naming convention. Unix stdlib discipline, not npm sprawl.
5. **One real external user.** Not Alfonso. Published case study.

Everything below is in service of those five.

---

## Milestones

### M1 — Semantics + Canonical Form (Months 0–3, ship as v0.5.0)

**Goal.** Every claim Noether makes about composition is either
property-tested or explicitly marked informal. Equivalent graphs hash
identically.

**Deliverables.**

- `docs/architecture/semantics.md` — two-page formal-ish semantics for
  each composition operator (`Sequential`, `Parallel`, `Fanout`, `Branch`,
  `Retry`, `Const`, `Let`, `Merge`, `Stage`). Input/output relation,
  associativity, identity laws. English + sketched equations. Not
  category-theory notation; prose that an engineer understands.
- `crates/noether-engine/tests/laws.rs` — proptest suite with one test per
  claimed law:
  - `Sequential` associativity: `(a >> b) >> c ≡ a >> (b >> c)`
  - Left/right identity: `id >> f ≡ f ≡ f >> id`
  - `Fanout` = Diagonal ∘ Parallel
  - `Parallel` branch-name permutation invariance
  - `Const` absorbs input
  - `Let` is compositional
  1000 cases per law.
- `crates/noether-engine/src/lagrange/canonical.rs` — canonicalisation
  pass:
  - Flatten nested `Sequential`s into a single N-ary Sequential
  - Sort `Parallel` branches by name
  - Drop identity stages
  - Normalise `Let` binding order
  - Normalise `Merge` source order (alphabetical by subgraph composition ID)
- `compute_composition_id` runs canonicalisation before hashing. This is
  breaking for composition IDs persisted in v0.4.x — that's expected.
- `noether-engine::agent::CompositionAgent` emits canonical form by default.

**Exit criteria.**

- All `laws.rs` proptest cases pass with 1000 iterations each
- Two syntactically different semantically equivalent graphs produce
  identical composition IDs (new integration test)
- v0.5.0 tagged and released with explicit breaking-change note about
  composition ID stability

**Risks.**

- Canonical ordering surfaces existing operator bugs → budget fix time
- Users who pinned v0.4.x composition IDs need a one-time migration →
  ship a `noether trace migrate-ids` script

**Rough size.** ~1500 new LOC + ~300 modified.

---

### M2 — Stability + Versioning + Property Predicates (Months 3–6, ship as v0.6.0)

**Goal.** Stages can evolve without breaking downstream graphs. Stage
semantics are expressible, testable claims — not docstring prose.

**Deliverables.**

- `STABILITY.md` — signed, public contract:
  - Stage **signature IDs** are stable across 1.x minor and patch releases
  - Stage **implementation IDs** may change on bugfixes
  - Composition operator semantics are frozen at 1.0
  - Graph JSON schema is additive-only within 1.x
  - Stdlib stage semantics are frozen; new behaviour ships as a new stage
- `StageId` split into two fields in the serialised Stage format:
  ```json
  { "signature_id": "sha256-of-canonical-signature",
    "implementation_id": "sha256-of-impl-source" }
  ```
  Backwards-compat: until v0.7.0, legacy single-field `"id"` still accepted.
- Graph `pinning` field (per-node, optional):
  - `"pinning": "signature"` (default) — latest implementation for this signature
  - `"pinning": "both"` — bit-reproducible, refuses implementation drift
- `properties` array on stage specs — minimal DSL:
  - Set membership: `output.severity in {"CRITICAL","HIGH","WARNING"}`
  - Arithmetic: `output.soc_percent >= 0 and output.soc_percent <= 100`
  - Universally-quantified implications over examples:
    `for all examples, if input.battery.soc_percent is null then output is null`
  - No higher-order quantification, no type-class predicates in M2
- `noether stage verify` — checks the Ed25519 signature and the
  declared properties against the stage's examples by default;
  `--signatures` / `--properties` restrict to one side
- Registry stores both IDs; clients resolve `signature → latest
  implementation` by default

**Exit criteria.**

- A deliberately buggy stage fix that changes implementation hash but not
  signature hash leaves all existing graphs operational (integration test)
- Every stdlib stage ships with ≥3 properties
- `STABILITY.md` committed, linked from README
- v0.6.0 released

**Risks.**

- Property DSL scope creep — keep the M2 version tiny, push
  higher-order features to post-1.0
- Splitting StageId breaks external tooling using the registry API → one
  minor version backward-compat window

**Rough size.** ~2000 new LOC + ~500 modified.

---

### M3 — Optimizer + Richer Types (Months 6–9, ship as v0.7.0)

**Goal.** Composition is not just correct but measurably faster than the
equivalent hand-written code. Types are expressive enough that common
patterns don't need `Any → Any` escape hatches.

**Deliverables.**

- `crates/noether-engine/src/optimizer/`:
  - `fuse_pure_sequential.rs` — fold N consecutive Pure stages into one
    compiled Rust closure
  - `hoist_invariant.rs` — move stages whose output doesn't depend on the
    flowing input out of hot loops
  - `dead_branch.rs` — compile-time elimination of `Branch` whose
    predicate is Const
  - `memoize_pure.rs` — opt-in per-composition memoisation for Pure stages
- Type system additions (each independently small):
  - Parametric polymorphism on stage signatures (`sort<T: Orderable>` —
    one stage instead of one per T)
  - Row polymorphism on records (`{name: Text, ...}` matches any record
    with a `name` field)
  - Refinement types with runtime check (`Number where x >= 0`)
- Benchmarks in `crates/noether-engine/benches/`:
  - 5-stage Pure pipeline — naive vs fused
  - 10-vehicle fleet summary — unoptimised vs optimised
  - Expect 2–5× speedups on realistic graphs
- `noether run --no-optimize` escape hatch so debugging stays possible
- Property-based comparison in CI: `optimized(graph).run(input) ==
  unoptimized(graph).run(input)` for 1000 random inputs

**Exit criteria.**

- Benchmark suite shows ≥2× speedup on at least three canonical graph
  shapes
- Optimizer preserves composition IDs (optimised and unoptimised graphs
  hash identically)
- At least one stdlib stage uses parametric polymorphism in its signature
- v0.7.0 released

**Risks.**

- Type system additions interact with canonical form from M1 — re-verify
  `laws.rs` passes after every type change
- Optimizer bugs are subtle and silent — property-based equivalence
  testing is the only defense

**Rough size.** ~3500 new LOC.

---

### M4 — Stdlib Curation + Vertical Depth + 1.0 (Months 9–12, ship as 1.0.0)

**Goal.** One vertical has dense, curated stdlib coverage. One real
external user runs real workloads. Stability contract enforced.

**Deliverables.**

- `docs/stdlib/curation.md` — public admission + deprecation doctrine:
  - **Admission.** New stage needs: examples ≥ 5, properties ≥ 3, a
    "does this generalise?" review by one maintainer other than the author
  - **Deprecation.** 6-month notice, successor pointer, deprecated badge.
    Deprecated stages keep executing but emit a warning.
  - **Naming.** `{domain}_{verb}_{noun}` — `csv_parse_rows`,
    `geo_haversine_distance`, `etl_batch_upsert`. Agents find stages via
    name-prefix search; chaotic naming kills discoverability.
- **One vertical deeply.** Candidate: typed ETL / analytics pipelines —
  broad addressable market, existing material (noether-telemetry
  templates), and the real gap in the ecosystem. Target: 500 curated
  stages in that vertical.
- **One external reference user.** Begin recruitment at M2; target an
  introduction by M3; a signed case-study ready for the 1.0 announcement.
  Candidates: a small data team at a friendly company, an academic group,
  a solo-open-source project that needs a pipeline.
- **1.0 release**: stability contract active, CI enforces it:
  - `scripts/check_breaking_change.sh` diffs stage signatures against
    the last tagged release; refuses to merge if any stdlib signature
    changed
  - Graph JSON schema diffs are additive-only
- **Long-form post on alpibru.com** — the quarterly content hit from the
  audit recommendation. "Building a telemetry pipeline in Noether" walks
  an end-to-end feature with real numbers.

**Exit criteria.**

- 500 stdlib stages in one vertical, each with examples + properties
- External user in production with a published testimonial
- CI refuses any commit that breaks stability contract
- 1.0.0 tag pushed; release notes signed by maintainer

**Risks.**

- Finding an external user as a solo maintainer is the hardest part of
  the plan — begin outreach at M2, not M4
- 500 stages is ambitious; fall back to 200 core + 300 extensions if
  time-pressed

**Rough size.** ~500 stage JSONs + ~1000 doc LOC + CI plumbing.

---

## Quarterly content cadence

| Month | Post |
|-------|------|
| 2 | "Canonical composition hashing — what changed in 0.5" |
| 4 | "The Noether stability contract — what we're promising for 1.x" |
| 7 | "Pure-stage fusion — why Noether pipelines now run 3× faster" |
| 10 | "Building X with Noether" (external user case study) |
| 12 | 1.0 release post |

---

## Open decisions

These need answers before the milestone that depends on them. None block
M1.

1. **Which vertical for M4?** Typed ETL is the widest market; `electromobility`
   has existing momentum via noether-telemetry. Decide before M3 begins.
2. **Does `noether-grid-*` stay in workspace, move to `noether-research`,
   or ship with 1.0?** Today it's `publish = false` — ambiguous. Decide
   before M2.
3. **Scope of property DSL.** Start minimal in M2; revisit if users demand
   more.
4. **Runtime enforcement of properties, or CI only?** Default CI only;
   `--enforce-properties` runtime flag for paranoid environments.
5. **WASM target — drop or land.** Roadmap mentions it as research. Decide
   by M2: either ship a worked browser example or remove from pitch.

---

## Explicit non-goals

Writing these down so the scope stays honest.

- Dependent types, proof-carrying code, or refinement types beyond
  the simple arithmetic/set predicates in M2
- Distributed execution beyond `noether-grid-*` as an experimental crate
- A general-purpose programming language — Noether is a composition
  language; the actual computation lives in Python/Rust stages
- Compatibility with any specific workflow engine (Airflow, Prefect,
  Dagster) — Noether complements them, doesn't replace them
- A UI — agents and scripts are the intended interface

---

## What happens if we don't do this

The failure mode isn't dramatic. Noether keeps working, keeps shipping
features, and slowly drifts from "a language for agents" into "Alfonso's
workflow engine that other people sometimes use." Stages don't get
properties. IDs drift as implementations change. Stdlib accretes. Every
new adopter writes the same type-checking ad hoc, because they can't trust
the checks the existing checks make. A fork or a competitor with a
narrower scope and a clean contract eventually appears and takes the
mindshare.

The plan above is the minimum set of decisions that prevent that drift
from being invisible. Each milestone leaves Noether more useful as a
tool and harder to replace with a me-too.
