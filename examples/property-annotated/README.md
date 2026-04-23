# `take_first_n` — a property-annotated stage

A worked example of a stage that declares **three** properties the engine checks against the declared examples at `noether stage add` time (and can re-run later via `noether stage verify`).

## What the stage does

`take_first_n` takes a list and a count, returns the first `n` elements of the list:

```
{ items: List<Any>, n: Number } → List<Any>
```

## What properties are declared

| Property | Kind | What it claims |
|---|---|---|
| `n` is non-negative | `Range` | `input.n ≥ 0` for every example |
| Output no longer than input | `FieldLengthMax` | `len(output) ≤ len(input.items)` |
| Every output element came from input | `SubsetOf` | every element in `output` appears in `input.items` |

Together these rule out a broken implementation that (a) accepts a negative `n`, (b) invents elements that weren't in the input, or (c) returns more elements than it was given. None of those are caught by the type checker — they're value-level invariants, which is what the property DSL exists for.

## Register and verify

From this directory:

```bash
# Register the stage (properties are checked against examples at add-time)
noether stage add take_first_n.json

# Re-run the properties later (e.g. after a registry pull, in CI)
noether stage verify <id-or-prefix>

# Restrict to property checks only (skip signature verification)
noether stage verify <id-or-prefix> --properties
```

A successful `stage add` means every declared example passes every declared property. If you edit an example so the output has more elements than the input, `stage add` refuses — the `FieldLengthMax` property catches the violation before the stage lands in the store.

## Why properties beat runtime assertions

Every example runs through the property checker at registration time. Violations can't ship. And unlike a unit test that sits in some parallel test suite, the properties travel with the stage spec — any consumer pulling the stage from a registry gets them automatically and can re-verify with `noether stage verify`.

## Further reading

- [`express-a-property`](../../docs/agents/express-a-property.md) — the dense agent-facing reference for picking property kinds and wiring field paths
- [`STABILITY.md — Stage properties wire format`](../../STABILITY.md#stage-properties-wire-format--additive-kinds) — the kinds frozen at 1.0 and what each one means
