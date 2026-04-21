# Graph Optimizer

`noether run` applies a small set of structural rewrites to the composition graph between type-check and plan generation. Each pass is **semantics-preserving** — the rewritten graph must produce the same output as the original for every input the original would accept.

```
parse → resolve → check_graph → [optimize] → plan → execute
```

By the time a pass sees the graph, the resolver has collapsed signature pins to implementation ids and the type checker has confirmed the wiring. The `composition_id` was computed much earlier on the pre-resolution canonical form, so optimizer rewrites **never shift the identity of the composition**. Traces and cross-run correlation stay stable.

Pass ordering in `noether run` is fixed:

1. **`canonical_structural`** — lift the M1 canonical-form rewrites.
2. **`dead_branch`** — fold `Branch` nodes with a constant predicate.
3. **`memoize_pure`** — wire the `PureStageCache` into the executor.

`canonical_structural` runs first so later passes see the flattened form. Each pass is independently disable-able via environment variable (see below).

## The passes

### `canonical_structural`

Delegates to `lagrange::canonical::canonicalise` — the same function that shapes the form we hash. Rules (all tested as M1 laws):

- Flatten nested `Sequential`: `Sequential[Sequential[a, b], c]` → `Sequential[a, b, c]`
- Collapse singleton `Sequential`: `Sequential[a]` → `a`
- Fuse adjacent `Retry`: `Retry{ Retry{ s, 3, _ }, 2, _ }` → `Retry{ s, 6, _ }`

Before this pass existed, the rewrites only shaped the hashed form — the executor still walked every wrapper, every trace entry recorded it. `noether compose` emits nested Sequentials defensively; lifting the canonicalisation to execution time turns those wrappers into no-ops.

### `dead_branch`

Folds `Branch { predicate: Const(bool), … }` into the selected arm:

```
Branch {                                       Branch {
  predicate: Const(true),   →   if_true         predicate: Const(false),   →   if_false
  if_true: <arm>,                                if_true: <arm>,
  if_false: <arm>,                               if_false: <arm>,
}                                              }
```

Recurses into the selected arm, so chained dead branches collapse in one pass iteration. Non-constant predicates (the common case, and the whole point of `Branch`) are left alone. Non-bool constants are a type-check bug, so the pass is deliberately defensive: no guessing a truthiness rule, just leave the node.

Motivated by agent-generated graphs: `noether compose` occasionally emits `Branch(Const(true), real, fallback)` as a defensive shape even when the fallback can never run. Pruning lets the planner skip wiring the dead arm entirely.

### `memoize_pure`

Not an AST pass — an executor-level wiring. A `PureStageCache` is built from the store (pre-populated with every stage whose `EffectSet` contains `Effect::Pure`), then `run_composition_with_cache` uses it to short-circuit repeated `(stage_id, input_hash)` pairs within a single run.

The cache is in-memory, per-run, never persisted. Non-Pure stages are rejected at `get`/`put` via the pure-id set; the feature can't accidentally memoize something non-deterministic.

When the cache fires, the ACLI response includes a `memoize: { enabled, hits, misses }` block so operators can see what it saved.

## Opt-outs

Two independent env vars:

| Env var | Effect |
|---|---|
| `NOETHER_NO_OPTIMIZE=1` (or `=true`) | Skip `canonical_structural` and `dead_branch`. Useful for trace debugging and bug repros where the literal authored graph must reach the executor. |
| `NOETHER_NO_MEMOIZE=1` (or `=true`) | Skip the `PureStageCache` wiring. Useful for benchmarks where every dispatch must go through the executor. |

The two are independent — you can disable memoization while keeping the AST passes, or vice versa.

## Extending the optimizer

New passes implement the `OptimizerPass` trait:

```rust
pub trait OptimizerPass {
    fn name(&self) -> &'static str;
    fn rewrite(&self, node: CompositionNode) -> (CompositionNode, bool);
}
```

A pass must:

- **Return `(node, false)` when nothing changed** — the fixpoint runner uses this to terminate.
- **Recurse into child nodes** — the runner doesn't walk the tree for you.
- **Preserve leaf stage identities** — never rename or replace a `Stage`'s `id` field. Structural rewrites are safe; identity rewrites break content addressing.

The `optimize(node, passes, max_iterations)` runner iterates until no pass reports a change or the cap is hit. `DEFAULT_MAX_ITERATIONS = 16` — enough for deep graphs to converge twice over, low enough that an oscillating pass fails loudly via `OptimizerReport.hit_iteration_cap`.

## Roadmap

The M3 milestone originally listed four passes. Three are shipped (`canonical_structural`, `dead_branch`, `memoize_pure`). The remaining two need deeper design:

- **`fuse_pure_sequential`** — adjacent Pure stages merged into a single execution step. In a content-addressed system, you can't create a new "fused" stage (it would need its own hash); the mechanism has to be plan-level, not AST-level.
- **`hoist_invariant`** — pull loop-invariant work out of loops. Noether has no loop primitive yet, so this is blocked on a future milestone.

See [`docs/roadmap.md`](../roadmap.md) for the full M3 picture.
