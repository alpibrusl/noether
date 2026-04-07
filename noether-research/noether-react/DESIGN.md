# NoetherReact — Design Document

> **Status: Research / Pre-proposal**
> Last updated: 2026-04

## The thesis

React's core insight: `UI = f(state)`.
Noether's core insight: `output = stage(input)`.

These are the same thing. A UI component is a stage whose output type is `VNode`.

**NoetherReact** is what you get when you apply Noether's content-addressing, structural typing, and composition model to reactive user interfaces.

---

## Why this matters

Current UI frameworks identify components by **name** or **import path**:

```javascript
import Button from './Button'           // mutable path
import { Button } from '@acme/ui@2.1'  // mutable version
```

The same component in two different repos has two different identities — even if the implementation is byte-for-byte identical.

NoetherReact identifies components by **content hash**:

```json
{ "op": "Stage", "id": "a4f9bc3e..." }
```

Two apps using the same hash are provably using the same component. Rename it, move it, republish it — the hash doesn't change unless the computation changes. This eliminates an entire class of dependency and versioning bugs.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Component Tree (Lagrange graph of VNode-producing stages)   │
├─────────────────────────────────────────────────────────────┤
│  Reactive Runtime (~200 lines JS)                            │
│  - State atoms backed by Noether KV store                    │
│  - Atom changes → trigger subgraph re-execution              │
│  - VNode diff → surgical DOM patches                         │
├─────────────────────────────────────────────────────────────┤
│  WASM Stage Execution (see ../wasm-target/)                  │
│  - Stages compiled to .wasm via noether build --target wasm  │
│  - Sub-millisecond execution in browser                      │
├─────────────────────────────────────────────────────────────┤
│  Content-addressed Component Registry                        │
│  - Components identified by hash, fetched on demand         │
│  - CDN-cacheable forever (hash = immutable identity)         │
└─────────────────────────────────────────────────────────────┘
```

---

## VNode type

A new `NType` variant:

```rust
pub enum NType {
    // ... existing types ...
    VNode,   // A virtual DOM node — text, element, or fragment
}
```

`VNode` in JSON is a recursive structure:

```json
{
  "tag": "div",
  "props": { "class": "card", "onClick": { "$event": "increment" } },
  "children": [
    { "tag": "h2", "props": {}, "children": [{ "$text": "Count: 42" }] },
    { "tag": "button", "props": {}, "children": [{ "$text": "+1" }] }
  ]
}
```

Events are declarative (`{"$event": "increment"}`) — not function references. The reactive runtime maps event names to atom mutations.

---

## Component = Stage

A counter component is a stage with `input: Record { count: Number }` → `output: VNode`:

```python
def execute(input_value):
    count = input_value.get('count', 0)
    return {
        "tag": "div",
        "props": {"class": "counter"},
        "children": [
            {"tag": "p", "props": {}, "children": [{"$text": f"Count: {count}"}]},
            {"tag": "button",
             "props": {"onClick": {"$event": "increment"}},
             "children": [{"$text": "+1"}]}
        ]
    }
```

The type checker verifies that `count: Number` is satisfied before the stage runs.

---

## Reactive runtime

The runtime is ~200 lines of vanilla JavaScript. No dependencies.

```javascript
class NoetherRuntime {
  constructor(graphJson, mountEl) {
    this.graph = graphJson;
    this.atoms = {};         // name → reactive value
    this.mountEl = mountEl;
  }

  // Register a state atom
  atom(name, initial) {
    this.atoms[name] = initial;
  }

  // Trigger re-render when an atom changes
  set(name, value) {
    this.atoms[name] = value;
    this.render();
  }

  // Re-execute the composition graph with current atom state
  async render() {
    const input = Object.fromEntries(Object.entries(this.atoms));
    const result = await this.executeGraph(this.graph, input);
    this.patch(this.mountEl, result);
  }

  // VNode diffing — only updates changed DOM nodes
  patch(el, vnode) { /* ... ~80 lines ... */ }

  // Wires declarative events to atom mutations
  handleEvent(eventName) { /* ... */ }
}
```

---

## Composition: the Lagrange graph becomes the component tree

A counter page is a composition graph:

```json
{
  "description": "Counter page",
  "root": {
    "op": "Sequential",
    "stages": [
      {
        "op": "Parallel",
        "branches": {
          "count":   { "op": "Stage", "id": "kv_get_id..." },
          "user":    { "op": "Stage", "id": "session_get_id..." }
        }
      },
      { "op": "Stage", "id": "counter_component_id..." }
    ]
  }
}
```

The type checker validates `counter_component` receives `Record { count: Number, user: Record {...} }` before the page ever renders.

---

## Automatic memoization

Because stages are pure functions identified by content hash, the reactive runtime can cache outputs:

```
input_hash = SHA-256(JSON(input))
if cache[stage_id][input_hash] exists → skip execution, return cached VNode
```

This is automatic `useMemo` with mathematical guarantees — no dependency arrays, no manual cache keys.

---

## AI-composable UI

Because components are stages, `noether compose` works for UI too:

```bash
noether compose "build a table showing fleet routes with status badges and weather data"
# → LLM searches the component registry for Table, Badge, WeatherWidget stages
# → assembles a type-checked Lagrange graph
# → returns a working UI composition
```

This is the part no existing framework can do.

---

## Open questions

❓ **Event model**: How do complex events (drag-and-drop, animations, canvas) map to declarative `{"$event": ...}` payloads without becoming a custom DSL?

❓ **WASM latency**: Python → WASM compilation is non-trivial. What's the path for existing Python stages? PyPy? Emscripten? Or do WASM stages need to be written in Rust/C?

❓ **Server components**: Should server-side stages (network, LLM) be distinguishable from client-side stages (pure DOM)? Or is `EffectSet` sufficient for this?

❓ **Streaming**: `NType::Stream<VNode>` — can the reactive runtime stream partial renders as a stage produces VNodes incrementally?

❓ **CSS**: Out of scope (stages output `VNode` with class strings), or should there be a `StyleSheet` type that participates in content-addressing too?

---

## Relationship to existing frameworks

| Framework | What NoetherReact borrows | What NoetherReact adds |
|---|---|---|
| **Elm** | Pure functions, typed model-view-update | Content addressing, AI composition |
| **Solid.js** | Fine-grained reactivity, no VDOM overhead | Language-agnostic stages, hash identity |
| **htmx** | Server-rendered HTML, events trigger requests | Type checking, composition graphs |
| **WASM Component Model** | Typed interfaces, composition tooling | Reactive runtime, UI stdlib |

---

## Implementation milestones

1. **M1 — VNode type** (1 day): Add `NType::VNode`, add `VNode` to type checker
2. **M2 — Counter prototype** (2 days): One Python stage → VNode, manual JS runtime
3. **M3 — Reactive runtime** (3 days): Atom state, event wiring, DOM diff
4. **M4 — `noether build --target browser`** (1 week): Bundle graph + stages + runtime → single `.html`
5. **M5 — WASM stages** (depends on `../wasm-target/`): Sub-millisecond component execution

Total estimated to M4: ~2 weeks of focused work.
