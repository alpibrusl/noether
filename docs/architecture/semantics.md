# Composition Semantics

The denotational meaning of each composition operator in the Lagrange
graph language. This document is the **contract**: every property test in
`crates/noether-engine/tests/laws.rs` checks a law stated here, and every
canonicalisation rule in `crates/noether-engine/src/lagrange/canonical.rs`
is justified by one of these equivalences.

When the code and this doc disagree, fix the code. When a new operator
or property ships without a corresponding entry here, the review blocks.

---

## Notation

We write stage/graph execution as a function:

```
⟦g⟧ : Input → Output
```

For composition:

```
⟦f >> g⟧(x) = ⟦g⟧(⟦f⟧(x))
```

Two graphs `g₁` and `g₂` are **semantically equivalent** when
`⟦g₁⟧(x) = ⟦g₂⟧(x)` for all inputs `x` in their common input type.
Canonicalisation rewrites any graph to a representative of its semantic
equivalence class; the composition ID hashes the representative, so
equivalent graphs share an ID.

---

## Operators

### `Stage { id, config }` — reference to a stored stage

```
⟦Stage { id, config }⟧(x) = stage_impl(id)(merge(config, x))
```

When `config` is `None`, the stage receives `x` unchanged. When `config`
is present, `config` fields are merged into `x` before the stage runs —
fields in `x` override fields in `config`.

**Identity.** `Stage { id = "id/any", config = None }` is the identity
for any input type it supports, modulo the stdlib `id` stage being
present. This gives:

```
id >> f ≡ f
f >> id ≡ f
```

### `RemoteStage { url, input, output }` — call a remote Noether

```
⟦RemoteStage { url, input, output }⟧(x) = http_post(url, x)
```

The type checker uses the declared `input` and `output` types; actual
execution dispatches to a remote composition. Equivalence with a local
Stage is *not* enforced: a remote implementation may differ. Two
`RemoteStage`s are equivalent iff their `url`, `input`, and `output` are
identical.

### `Const { value }` — ignore input, emit constant

```
⟦Const { value }⟧(x) = value
```

**Absorption.** Const absorbs any prefix:

```
f >> Const { v } ≡ Const { v }         for any f
```

### `Sequential { stages }` — left-to-right pipeline

```
⟦Sequential { [s₁, s₂, …, sₙ] }⟧(x) = ⟦sₙ⟧(⟦sₙ₋₁⟧(…⟦s₁⟧(x)…))
```

**Associativity.** Nested Sequentials flatten:

```
Sequential [ Sequential [a, b], c ]   ≡  Sequential [a, b, c]
Sequential [ a, Sequential [b, c] ]   ≡  Sequential [a, b, c]
Sequential [ Sequential [a, b], Sequential [c, d] ]  ≡  Sequential [a, b, c, d]
```

**Identity collapse.** A singleton Sequential is the stage itself:

```
Sequential [a]  ≡  a
```

The empty Sequential is rejected at construction time; it has no
well-defined input/output types.

### `Parallel { branches }` — fan-in then per-branch dispatch

```
⟦Parallel { {k₁ → b₁, k₂ → b₂, …} }⟧(x) = { k₁: ⟦b₁⟧(x[k₁] or x),
                                              k₂: ⟦b₂⟧(x[k₂] or x),
                                              … }
```

Each branch receives `x[kᵢ]` when `x` is a Record with a key matching the
branch name; otherwise it receives the full `x`. The output is a Record
keyed by branch names.

**Branch-name commutativity.** Parallel with branches `{a → X, b → Y}`
is semantically equivalent to `{b → Y, a → X}` — the branch map is a
set of name→graph pairs, not an ordered list. The JSON representation is
normalised by sorting keys alphabetically (the `BTreeMap` in
`ast.rs` already enforces this in-memory).

### `Fanout { source, targets }` — same source to many targets

```
⟦Fanout { source, targets = [t₁, …, tₙ] }⟧(x) =
  let s = ⟦source⟧(x) in
  [⟦t₁⟧(s), ⟦t₂⟧(s), …, ⟦tₙ⟧(s)]
```

**Target order matters.** Unlike `Parallel`, Fanout's output is a list
indexed by position; swapping targets changes the output. Canonicalisation
does NOT reorder `Fanout` targets.

**Equivalence with Parallel.** `Fanout { source, [t₁, t₂] }` is
semantically equivalent to `source >> Parallel { { "0": t₁, "1": t₂ } }`
only up to output shape (list vs record). Not an identity; don't rewrite.

### `Branch { predicate, if_true, if_false }` — conditional routing

```
⟦Branch { p, t, f }⟧(x) =
  if ⟦p⟧(x) then ⟦t⟧(x) else ⟦f⟧(x)
```

The predicate stage must produce a Bool; this is a type-checker
requirement, not a runtime one.

**Dead-branch elimination.** When the predicate is `Const { true }`:

```
Branch { Const true, t, f }  ≡  t
Branch { Const false, t, f }  ≡  f
```

Canonicalisation applies this rewrite in M3 (the optimizer milestone),
not in M1 — M1 treats Branch as opaque.

### `Merge { sources, target }` — fan-in to one target

```
⟦Merge { [s₁, …, sₙ], target }⟧(x) =
  let outputs = [⟦s₁⟧(x), …, ⟦sₙ⟧(x)] in
  ⟦target⟧(outputs)
```

The target receives a list of source outputs in source-order.

**Source ordering.** Source order is semantically significant — the list
passed to `target` is ordered by the input list. Canonicalisation does
NOT reorder sources. If the target is commutative (e.g., `sum`), the
*user* should write a Parallel-plus-reduce pattern instead.

### `Retry { stage, max_attempts, delay_ms }` — retry on failure

```
⟦Retry { s, n, d }⟧(x) =
  try ⟦s⟧(x)
  catch error-that-is-retryable:
    sleep d ms
    retry up to n-1 more times
```

**Fixed-point when max_attempts = 1.** `Retry { s, 1, _ }` is
semantically equivalent to `s`. Canonicalisation applies this.

**Nested retry collapse.** Two nested `Retry`s with the same `delay_ms`
multiply their attempt counts:

```
Retry { Retry { s, n, d }, m, d }  ≡  Retry { s, n·m, d }
```

Canonicalisation applies this only when `delay_ms` matches (different
delays produce observably different timing and are NOT equivalent).

### `Let { bindings, body }` — let-bindings with shared outer input

```
⟦Let { {k₁ → b₁, …}, body }⟧(x) =
  let bs = { k₁: ⟦b₁⟧(x), k₂: ⟦b₂⟧(x), … } in
  ⟦body⟧({ ...x, ...bs })
```

All bindings receive the *outer* input `x` — there is no inter-binding
dependency ordering. The body receives a Record that is `x` extended
with the binding outputs; binding names that collide with fields in `x`
shadow them.

**Binding name commutativity.** Like Parallel, the binding map is a set
of name→graph pairs; ordering is not semantic. BTreeMap enforces
alphabetical key order in the serialised form.

**Empty-binding collapse.** `Let { {}, body }` is semantically equivalent
to `body` when body doesn't reference any shadowed fields. Canonicalisation
applies this.

---

## Canonical form

A graph is in **canonical form** when all of the following hold:

1. **No nested Sequentials.** Every `Sequential { stages }` has no child
   that is itself `Sequential`. Flattened in left-to-right order.
2. **No singleton Sequentials.** `Sequential { [s] }` is rewritten to
   `s`.
3. **Parallel branches sorted alphabetically** by name (BTreeMap already
   guarantees this in serialisation; we verify on deserialisation).
4. **Let bindings sorted alphabetically** by name (same).
5. **Retry collapse.** `Retry { _, 1, _ }` → inner stage. `Retry { Retry
   { s, n, d }, m, d }` → `Retry { s, n·m, d }`.
6. **Empty Let collapse.** `Let { {}, body }` → `body`. With no bindings
   there are no shadowed fields, so the rewrite is always safe — the
   "body must be independent of shadowed fields" qualifier of the general
   semantics is vacuous.

The canonical form is what the composition ID hashes. Two graphs with
the same canonical form are semantically equivalent; two graphs with
different canonical forms *may* still be semantically equivalent (e.g.,
M3 will add more rewrites), but we accept that false-negative as the
cost of keeping M1's canonicalisation fast and bug-unlikely.

**What canonicalisation does NOT do in M1:**

- Stage-level identity detection ("is this stage definitionally
  identity?") — needs stage metadata, deferred to M2.
- Dead-branch elimination with Const predicate — deferred to M3.
- Parallel/Fanout cross-conversion — not equivalent (output shape).
- Cross-node fusion — optimizer territory, deferred to M3.
- Type-aware rewrites — canonicalisation is syntactic in M1.

---

## Equivalence claims testable in `laws.rs`

### Tested in M1

Every item below has at least one proptest in `laws.rs`:

| Law | Statement |
|-----|-----------|
| `L1` Sequential associativity | `flatten(Sequential[Sequential[a,b], c]) == Sequential[a,b,c]` |
| `L4` Sequential singleton | `canonicalise(Sequential[a]) == a` |
| `L5` Sequential nested flatten | any nesting of Sequentials produces a flat equivalent |
| `L6` Parallel permutation | permuting branch-key insertion order yields an equal composition ID (JSON-level test — BTreeMap in-memory already enforces this; the test catches serialisation drift) |
| `L7` Let binding permutation | permuting binding-key insertion order yields an equal composition ID (same rationale as L6) |
| `L9` Retry 1-attempt collapse | `canonicalise(Retry{s, 1, _}) == s` |
| `L10` Retry multiplication | `canonicalise(Retry{Retry{s, n, d}, m, d}) == Retry{s, n·m, d}` |
| `L11` Empty Let collapse | `canonicalise(Let{{}, body}) == body` |
| `L12` Canonicalisation is idempotent | `canonicalise(canonicalise(g)) == canonicalise(g)` |
| `L13` Composition ID stability | `compute_composition_id(g) == compute_composition_id(permuted_but_equivalent(g))` |

### Deferred — claims stand but tests ship in a later milestone

These laws are true by the semantics above; proptests land alongside the
canonical-form rules that realise them.

| Law | Statement | Ships with |
|-----|-----------|------------|
| `L2` Sequential left identity | `canonicalise(Sequential[id_stage, f]) ≡ f` | M2 (stage-level identity detection needs stage metadata) |
| `L3` Sequential right identity | `canonicalise(Sequential[f, id_stage]) ≡ f` | M2 (same) |
| `L8` Const absorption | `canonicalise(Sequential[f, Const{v}]) ≡ Const{v}` | M2 (rewrite is semantics-preserving but deferred to M2 optimiser) |

---

## Notes on future milestones

- **M2 adds property predicates to stages** — not new operators. Semantics
  above stand.
- **M3 adds type-system additions** (parametric polymorphism, row
  polymorphism, refinement types) — may require new equivalences, e.g.,
  `sort<Number>` and `sort<Text>` canonicalise to the same
  signature-polymorphic stage. Those rewrites join canonical.rs in M3.
- **M3 optimizer rewrites** (Pure-fusion, invariant hoisting) are
  semantically preserving but not part of canonical form — they happen on
  an already-canonical graph and produce an optimised execution plan.
  Composition ID is computed from the canonical form *before*
  optimisation, so optimiser work doesn't change the ID.

---

## Why this matters

Without a written semantics doc, every canonicalisation rule and every
property test is a judgement call. With one, they are either consistent
with this doc or they're bugs. That's the entire value of doing this
work before implementing the laws — the contract exists first, the code
proves it later.
