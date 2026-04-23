# Concepts

The five ideas you need to hold in your head. Every other page assumes
this one.

## Stage

A **stage** is an immutable unit of computation.

```
stage : { input: T } → { output: U }   # structural type signature
      + EffectSet                       # declared side effects
      + Option<Implementation>          # Rust fn, Python, JavaScript, Bash
      + [Example…]                      # at least one input/output pair
      + [Property…]                     # optional declarative checks
```

Two stages with the same hash are provably the same computation —
across machines, across repositories, forever.

```
identity = SHA-256(canonical_json(signature + implementation_hash))
```

The identity hash is called the `StageId` (or `ImplementationId` — the
two are type-aliases today). A second hash — the `SignatureId` —
covers only `(name, input, output, effects)` and is stable across
bug-fix reimplementations of the same stage.

Stages live in a **store**: content-addressed, immutable, with a
lifecycle (`Draft → Active → Deprecated → Tombstone`). Replacing a
stage means publishing a new one with the same `SignatureId`; the
store auto-deprecates the old implementation.

## Type system

Types are **structural**, not nominal. Two types are compatible if
their structure matches — there's no registry of named types to
coordinate on. Width and depth subtyping both apply:

```
Record { a: Text, b: Number, c: Bool }   <:   Record { a: Text, b: Number }
         ^^^^^^^^^^^^^^^^^^^^^^^^^^                ^^^^^^^^^^^^^^^^^^
         subtype can carry extra fields            ...and fewer fields is OK
```

The `NType` enum covers:

- **Primitives** — `Text`, `Number`, `Bool`, `Null`, `Bytes`, `VNode`, `Any`.
- **Containers** — `List(T)`, `Map<K, V>`, `Record { f: T, … }`,
  `Stream<T>`.
- **Union** — flattened, deduped, sorted. `union()` is the only
  normalising constructor.
- **Parametric** (v0.8) — `Var("T")`. `identity: <T> → <T>`.
- **Row-polymorphic record** (v0.8) — `RecordWith { fields, rest }`.
  Captures known fields plus a row variable for the rest.
- **Refined** (v0.8) — `Refined { base, refinement }`. A base type
  with a runtime-checkable predicate: `Range { min, max }`,
  `OneOf { options }`, `NonEmpty`.

`Any` is a bidirectional escape hatch — `is_subtype_of(T, Any)` and
`is_subtype_of(Any, T)` both hold. Use sparingly; it defeats the
checker at that edge.

**The checker verifies graph topology, not stage bodies.** A stage
that declares `Text → Number` but returns a string fails at runtime,
not check time. Refinement predicates are the exception: their
runtime enforcement via `ValidatingExecutor` (on main, ships next
tag) closes the loop for refined types.

## Effects

Every stage declares its effects in the signature:

| Effect | Meaning |
|---|---|
| `Pure` | No side effects. Same input always produces the same output. |
| `Fallible` | May return a typed error the caller must handle. |
| `Network` | Makes outbound network calls (sandbox toggles `--share-net`). |
| `Llm { model }` | Invokes an LLM. `model` is a hint; policy keys on `EffectKind::Llm`. |
| `NonDeterministic` | Same input may produce different output. |
| `Process` | Spawns, signals, or waits on OS processes. |
| `Cost { cents }` | Declared monetary cost. Consumed by `--budget-cents`. |
| `FsRead(path)` | Reads from a specific filesystem path. |
| `FsWrite(path)` | Writes to a specific filesystem path. |
| `Unknown` | Effect-inference couldn't classify. Treated conservatively. |

Effects drive three separate pre-flight checks that run before the
executor starts:

- **Capability policy** (`--allow-capabilities`) — blocks stages that
  need capabilities the caller hasn't granted.
- **Effect policy** (`--allow-effects`) — blocks stages whose effect
  kinds aren't in the allowed list.
- **Budget check** (`--budget-cents`) — blocks when the sum of
  `Cost { cents }` exceeds the ceiling.

`FsRead(path)` and `FsWrite(path)` also feed `IsolationPolicy::from_effects`
so the sandbox bind-mounts exactly the paths the stage declared.

## Composition graph

A composition is a tree of `CompositionNode` values. The operators:

| Op | Meaning |
|---|---|
| `Stage { id, pinning }` | Invoke a stage. `pinning: "signature"` resolves to whichever impl is Active; `"both"` requires an exact implementation. |
| `RemoteStage { url, id }` | Invoke a stage hosted on a remote registry. |
| `Const { value }` | Inject a literal JSON value. |
| `Sequential { stages }` | Run in order; output of N feeds input of N+1. |
| `Parallel { branches }` | Run a record-keyed set of branches concurrently; output is a record. |
| `Branch { predicate, if_true, if_false }` | Classic branch. |
| `Fanout { source, targets }` | Feed one source output into many branches. |
| `Merge { sources, target }` | Combine multiple sources into one target input. |
| `Retry { stage, max_attempts, backoff_ms }` | Re-run on `Fallible` failure. |
| `Let { bindings, body }` | Name intermediate results; reference them in `body`. |

The graph is just JSON. Every operator has a stable `"op"` tag; the
full schema lives in `crates/noether-engine/src/lagrange/ast.rs`.

Two graphs that canonicalise to the same tree have the same
**composition ID** (SHA-256 of the canonical form). That's how replay
works: `noether trace <composition_id>` pulls up the previous run of
the same-shape graph.

## Content addressing

Everything identity-bearing in Noether is a hash. Never a name,
never a version string, never a pointer to a database row.

| Hash | What it identifies |
|---|---|
| `SignatureId` | `(name, input, output, effects)` |
| `ImplementationId` / `StageId` | `(signature + implementation_hash)` |
| `CompositionId` | Canonical form of a composition graph |

**Consequences:**

- A graph JSON that produced result R yesterday produces the same R
  today, regardless of whether the underlying stage implementations
  rotated — as long as the graph references stages by `SignatureId`
  and the Active implementation still exists.
- Two projects that both write `to_upper` independently end up with
  the same `SignatureId` if their signatures match. No naming
  collision, no registry lock-in.
- Modifying an implementation produces a new `StageId` with the same
  `SignatureId`. Graphs pinned to `"signature"` pick up the fix
  automatically; graphs pinned to `"both"` stay on the old
  implementation until re-pinned.

The [STABILITY.md](https://github.com/alpibrusl/noether/blob/main/STABILITY.md)
contract is what makes this work in practice — it pins exactly which
hashes stay stable across 1.x and which can drift.
