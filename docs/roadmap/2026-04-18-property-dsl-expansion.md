# Property DSL expansion (M2.5)

**Status:** Draft · 2026-04-18
**Target release:** v0.6.x or v0.7.0 (pre-1.0)

---

## Why this exists

M2 (v0.6.0) shipped a deliberately tiny property DSL:

- `Property::SetMember { field, set }` — JSON-value equality against an
  enumerated list.
- `Property::Range { field, min, max }` — numeric bound.
- `Property::Unknown` — forward-compat catch-all.

The intent was a minimum-viable surface that's trivial to evaluate and
easy to serialize. The M2 exit criterion said *"every stdlib stage
ships with ≥3 properties"*.

Post-M2 we did the survey. Of ~176 stdlib stages:

- **0 stages** support ≥3 meaningful properties under the v0.6 DSL.
- **~45 stages** support 1–2 (bool outputs → `SetMember`; numeric
  bounds → `Range`; HTTP status → `Range 100..=599`).
- **~35 stages** support none — their guarantees are *structural*, not
  numeric or enumerable.

The stages blocked on DSL expression fall into five patterns. The
table below names each pattern, the example stages, and what the DSL
would need.

| Pattern | Example stages | What's needed |
|---------|----------------|---------------|
| Length preservation | `text_reverse`, `text_upper`, `text_lower`, `zip` | Cross-field length equality: `output.length == input.length` |
| Input-dependent range | `filter`, `take`, `list_dedup`, `flatten` | Relative bounds: `output.length ≤ input.items.length` |
| Type-dependent output set | `sort` (union input), `reduce` | Conditional constraints: when input is `List<X>`, output matches pattern |
| Transformation invariant | `group_by` (keys ⊆ distinct input values), `json_merge` | Subset predicates: `keys(output) ⊆ keys(input)` |
| Polymorphic outputs | `parse_json`, `kv_get`, `reduce` | No constraint; deferred to M3 refinement types |

---

## Proposed new variants

In order of utility:

### 1. `FieldLengthEq` / `FieldLengthMax`

```
FieldLengthEq { left_field: String, right_field: String }
FieldLengthMax { subject_field: String, bound_field: String }
```

**Semantics.** `FieldLengthEq` holds iff `len(left) == len(right)`
where `len` is defined as:
- string length (UTF-8 characters) for `NType::Text`
- list length for `NType::List<_>`
- map cardinality for `NType::Map<_, _>`
- record field count for `NType::Record`

`FieldLengthMax` holds iff `len(subject) ≤ len(bound)` — the
input-dependent range case.

**Unlocks.** `text_reverse`, `text_upper`, `text_lower`, `filter`,
`take`, `list_dedup`, `map`.

### 2. `SubsetOf`

```
SubsetOf { subject_field: String, super_field: String }
```

Holds iff every element/key of `subject` appears in `super`. Elements
compared by JSON-value equality.

**Unlocks.** `group_by` (`keys(output) ⊆ keys(input)` after projection),
`sort` (`values(output) == values(input)` via bidirectional subset),
`json_merge`.

### 3. `Equals`

```
Equals { left_field: String, right_field: String }
```

JSON-value equality between two paths. Most useful with the
`implementation_id` for reflexivity (identity stages), and for
`left.body == right.content` kinds of preserve-content claims.

**Unlocks.** `identity`, `noop`, `kv_set` + `kv_get` roundtrip
(composition-level property, actually — separate work).

### 4. `FieldTypeIn`

```
FieldTypeIn { field: String, allowed: Vec<NTypeKind> }
```

The runtime JSON type at `field` is one of the allowed set. Bridges
the gap between the structural type system and runtime-shape checks.
Useful for `parse_json`: "output is one of {Number, Text, Bool, Null,
Record, List}" — which is trivially true but worth pinning.

---

## Scope guardrails

Explicit non-goals (same as M2's):

- No quantifiers (`forall`, `exists`).
- No higher-order predicates (properties that take other properties).
- No temporal predicates (properties that reference prior execution
  state).
- No propositional connectives (AND/OR/NOT). Properties are
  conjunctively checked; if a stage needs a disjunction, the DSL
  author writes the equivalent as two stages or waits for M3
  refinement types.

The four variants above expand coverage from ~45 backfillable stages
to an estimated ~120 (the rest are polymorphic / content-generating
and stay un-annotated through 1.0).

---

## Forward compatibility

Per `STABILITY.md`: unknown property kinds deserialise into
`Property::Unknown` and are skipped in aggregation. v0.6 readers load
v0.7 graphs with the new variants; they just don't evaluate them.

New variants CAN land as a 0.6.x minor release. They do not break
existing graphs, existing stages, or existing IDs — properties are
not part of the content hash.

---

## Migration for the stdlib

Once the new variants ship, the stdlib backfill becomes mechanical:

1. `text_*` (length-preserving): add `FieldLengthEq { left:
   "output", right: "input" }` or `FieldLengthEq { left: "output",
   right: "input.text" }` for record-wrapped inputs.
2. `filter` / `take` / `list_dedup`: add `FieldLengthMax { subject:
   "output", bound: "input.items" }`.
3. `map`: add `FieldLengthEq { left: "output", right: "input.items" }`.
4. `group_by`: add `SubsetOf { subject: "keys(output)", super:
   "values(input.items, input.key)" }`. May need a field-accessor
   mini-grammar for this.

Budget: ~400 LOC for the DSL variants and evaluator; ~50 stages
updated with 2–3 properties each in the same PR series.

---

## What we ship today (v0.6.0)

`v0.6.0` ships:

- The `Property` enum with three variants (`SetMember`, `Range`,
  `Unknown`).
- `Property::validate_against_types` for registration-time type
  checks.
- `Stage::check_properties` with the `NoExamples`/`Violations`
  error split.
- A starter backfill: `to_bool`, `text_length`, `text_contains`,
  `regex_match.matched`, `http_status`, `list_length` — the stages
  where a single natural `SetMember` or `Range` adds real value.

`v0.6.0` does NOT ship:

- Properties on text-transformation stages (blocked on
  `FieldLengthEq`).
- Properties on collection-filtering stages (blocked on
  `FieldLengthMax`).
- The "≥3 per stage" target — deferred and re-scoped to "properties
  where naturally expressible" in `STABILITY.md`.

---

## Open questions

1. **Should the new variants land in a v0.6.x patch or wait for
   v0.7.0?** Additive, so a minor release is correct. Calling it
   v0.7.0 lets us bundle other M3 work (optimizer, refinement types).
2. **Field-path grammar.** The current dot-separated path is sufficient
   for the scalar access used by `SetMember` and `Range`. The new
   variants reference whole sub-values — same grammar applies, but
   evaluators need to handle lists/records at any depth.
3. **Evaluator cost.** `FieldLengthEq` across large inputs/outputs
   could be expensive. Should the DSL cap the traversal depth, or
   leave that to callers (`noether stage verify` has a timeout)?
