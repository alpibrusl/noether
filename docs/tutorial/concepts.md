# Core concepts: a 5-minute mental model

Before the walkthrough, a short tour of the four ideas Noether is built on. Every page of docs after this one assumes you've read this one.

If you've ever used Git, Nix, or a typed functional language, most of this will feel familiar. If you haven't, the analogies still work — read past them.

---

## 1. A stage is a computation with a content-addressed identity

A stage is a small, reusable unit of computation with:

- a name,
- an **input type**,
- an **output type**,
- a set of declared **effects** (more on these in §3),
- and an implementation — inline Rust, a Python function, a Bash script.

The stage's identity isn't its name. It's a SHA-256 hash of
`{ name, input, output, effects, implementation_hash }`. Two stages with the same hash are **provably the same computation** — on any machine, forever. Rename the stage, change the input type, change the implementation, and you get a *different* stage with a *different* hash. The old one still resolves; it just no longer matches.

```bash
noether stage list
# …
# 7b2f9a1c http_get      (signature id: a3…)
# …
```

The left column is the `StageId` (full hash). The right column in parens is the `SignatureId` — a stable identity for the *interface* independent of the implementation. Graphs typically pin by `SignatureId` so a bugfix in the implementation is picked up automatically; they can pin by `StageId` when they need the exact byte-level implementation.

> **Why this matters.** It means you can't "yank" a stage. The bytes that produced yesterday's result are either still resolvable (then today's re-run is identical) or the reference is a dangling hash (then you get a clear error, not a silent regression).

See [architecture/stage-identity.md](../architecture/stage-identity.md) for the canonicalisation rules.

---

## 2. Types are structural, not nominal

Noether doesn't have classes or interfaces you need to declare conformance to. A type is just its shape.

The type `Record { a: Number, b: Text, c: Bool }` is a subtype of `Record { a: Number, b: Text }` — not because someone said so, but because every value of the first shape is, mechanically, also a value of the second shape. This is called **width subtyping**.

```
Record { a: Number, b: Text, c: Bool }   is subtype of   Record { a: Number, b: Text }
List<Number>                             is subtype of   List<Any>
Any                                      is subtype of   T     (for any T)
T                                        is subtype of   Any   (for any T)
```

The last two — `Any` being bidirectional — is the escape hatch. Use it sparingly: it turns off the type checker at that edge.

The type checker runs before any stage executes. If stage `A` produces `Record { body: Text, status: Number }` and you wire it into stage `B` that expects `Record { html: Text }`, the checker reports the mismatch **at compose time**, not at run time. No network call happens. No Python process spawns.

See [architecture/type-system.md](../architecture/type-system.md) for the full type grammar.

---

## 3. Effects are declared, not inferred at run time

Every stage's signature carries an `EffectSet`:

- `Pure` — deterministic, no side effects.
- `Fallible` — can return an error value.
- `Network` — makes outbound HTTP/DNS calls.
- `Llm` — calls an LLM provider.
- `Cost` — consumes paid credits (LLM tokens, API quota).
- `NonDeterministic` — reads time, entropy, or process state.

Effects are authored in the stage spec, not inferred from the source. The **composition engine** sums the effects of every stage in the graph before execution and compares against the allowed set:

```bash
noether run graph.json --allow-effects pure,fallible,network
```

If the graph's effect closure contains `Llm` and you didn't allow it, the run is rejected with exit 2, before any work happens. Same shape for capabilities (filesystem access, network reachability) and for the optional cost budget (`--budget-cents`).

> **Why this matters.** It makes graphs auditable. A composition that claims to be `Pure` is mechanically forbidden from making a network call — not by convention, by refusal to execute.

---

## 4. A composition is a typed graph, not a script

When you write a pipeline in a scripting language, the type discipline is whatever the author remembered. In Noether, the composition is itself a data structure — a **Lagrange graph** — with operators like `Sequential`, `Parallel`, `Branch`, `Fanout`, `Merge`, `Retry`, and `Let`. Each operator has a well-defined type rule.

A minimal graph running one stage:

```json
{
  "description": "one-stage graph",
  "version": "0.1.0",
  "root": {
    "op": "Stage",
    "id": "a3c9…"
  }
}
```

A two-stage pipeline:

```json
{
  "description": "fetch and count",
  "version": "0.1.0",
  "root": {
    "op": "Sequential",
    "stages": [
      { "op": "Stage", "id": "a3c9…" },
      { "op": "Stage", "id": "7d11…" }
    ]
  }
}
```

The canonical form of this graph gets a `composition_id` (SHA-256 of the JCS-serialised root). That id is stable across cosmetic rewrites — nested `Sequential`s flatten, `Parallel` branch order is normalised — so two graphs that compute the same thing get the same id.

The engine runs the checker against the graph, builds an `ExecutionPlan`, and only then dispatches the stages. Every execution writes a trace indexed by the composition id, so `noether trace <id>` reproduces the full story.

See [architecture/composition-engine.md](../architecture/composition-engine.md) for the operator semantics.

---

## Reproducibility vs isolation (important distinction)

Noether pins two separate things:

| Boundary | What it pins | Tool |
|---|---|---|
| **Reproducibility** | The runtime: Python/Node versions, libc, every transitive package | Nix (`/nix/store`) |
| **Isolation** | The stage's view of the host: filesystem, network, env vars | bubblewrap (`bwrap`), Linux only, default from v0.7 |

The Nix boundary means "same inputs produce same outputs on any machine." It does **not** mean "a malicious stage can't read your SSH key." That's what isolation is for. From v0.7 onwards, stages run in a bubblewrap sandbox by default (`--isolate=auto`, falls back to `none` with a warning if bwrap isn't installed). See [SECURITY.md](https://github.com/alpibrusl/noether/blob/main/SECURITY.md) for the full threat model.

---

## What to read next

- **[Walkthrough — citecheck as stages](index.md)** — the hands-on tutorial.
- **[Compose with an LLM](llm-compose.md)** — let the agent author a graph from a problem statement.
- **[When things go wrong](when-things-go-wrong.md)** — how to read Noether errors.
