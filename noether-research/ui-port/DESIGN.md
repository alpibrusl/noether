# Noether UI Platform — Design Document

> **Status: Implemented**
> Phase 1 (UI Port): 2026-04
> Phase 2 (Full-Stack / RemoteStage): 2026-04
> Phase 3 (UI Completeness): 2026-04

This document describes the architecture as **actually built** — not as a proposal.
It is kept current as features land.

---

## The thesis

React's core insight: `UI = f(state)`.
Noether's core insight: `output = stage(input)`.

These are the same thing. A UI component is a stage whose output type is `VNode`.

Noether UI applies Noether's content-addressing, structural typing, and composition model to
reactive user interfaces. Components are identified by content hash — not by name or import path.
Two apps using the same hash are provably running the same component.

---

## Overall architecture

```
┌───────────────────────────────────────────────────────────────────────────┐
│  Build Time (host)                                                         │
│                                                                            │
│  graph.json ──► noether build --target browser                            │
│                   ├─ Type-check graph (incl. RemoteStage boundaries)      │
│                   ├─ wasm-pack ──► noether_bg.wasm + noether.js           │
│                   └─ index.html  (NoetherRuntime + atom/event bootstrap)  │
│                                                                            │
│  graph.json ──► noether build --target native [--serve :8080]             │
│                   ├─ Type-check graph                                      │
│                   └─ Rust binary  (ACLI HTTP server built-in)             │
│                                                                            │
│  graph.json ──► noether build --target react-native                       │
│                   ├─ browser build (into assets/)                         │
│                   └─ Expo project  (App.tsx + package.json + app.json)    │
└─────────────────────────────┬─────────────────────────────────────────────┘
                              │
┌─────────────────────────────▼─────────────────────────────────────────────┐
│  Browser / WebView Runtime                                                 │
│                                                                            │
│  NoetherRuntime (vanilla JS, ~500 LOC, zero dependencies)                 │
│    atoms: { count: 0, _route: "/home" }                                   │
│    ─────────────────────────────────────────────                          │
│    Local stages:  atoms ──► execute_stage(id, json) ──► WASM ──► VNode   │
│    Remote stages: atoms ──► fetch(POST url, json) ──► ACLI ──► VNode     │
│    ─────────────────────────────────────────────                          │
│    VNode ──► DOM diff + patch                                              │
│    DOM events ──► atom mutations ──► re-render                            │
└───────────────────────────────────────────────────────────────────────────┘
```

---

## Part 1 — VNode type

`VNode` is a variant of `NType`, Noether's structural type system:

```rust
pub enum NType {
    // ...existing types...
    VNode,  // A virtual DOM node — opaque to the type checker
}
```

`VNode` is **opaque**: the type system knows it exists, but does not inspect its internal
tag/props/children structure. The JS runtime owns VNode semantics.

### VNode JSON shape

```json
{
  "tag": "div",
  "props": {
    "class": "card",
    "onClick": { "$event": "increment" },
    "key": "item-42"
  },
  "children": [
    { "tag": "h2", "props": {}, "children": [{ "$text": "Count: 42" }] },
    { "tag": "button", "props": {}, "children": [{ "$text": "+1" }] }
  ]
}
```

Text nodes use `{ "$text": "..." }`. Events are declarative (`{ "$event": "name" }`) — not function
references. The `key` prop enables keyed reconciliation for list items.

---

## Part 2 — noether-engine feature gating

`noether-engine` compiles in two modes:

| Feature | Target | What's included |
|---|---|---|
| `native` (default) | OS binary | All modules: Nix executor, reqwest, rusqlite |
| _(no features)_ | `wasm32` | Core only: InlineExecutor, runner, checker, lagrange, planner, trace |

Gated behind `#[cfg(feature = "native")]`:
- `agent`, `composition_cache`, `index`, `llm`, `providers`, `registry_client`
- `executor::nix`, `executor::runtime`, `executor::composite`
- `executor::stages::io` (reqwest), `executor::stages::kv` (rusqlite)
- `trace::JsonFileTraceStore`

WASM check:
```bash
cargo check -p noether-engine --no-default-features --target wasm32-unknown-unknown
```

---

## Part 3 — `noether build --target browser`

```bash
noether build graph.json --target browser -o dist/
```

**Pipeline:**

1. Parse and type-check the Lagrange graph (including `RemoteStage` boundaries)
2. Collect non-stdlib custom stages from the store
3. Generate a temporary Rust WASM crate:
   - `Cargo.toml` with `noether-engine` (no-default-features), `wasm-bindgen`
   - `src/lib.rs` with:
     - `#[wasm_bindgen] fn execute(input_json: &str) -> String` — legacy full-graph entry
     - `#[wasm_bindgen] fn execute_stage(id: &str, json: &str) -> String` — per-stage entry
     - `#[wasm_bindgen] fn get_graph_json() -> String` — exposes graph AST to JS
   - A `WasmExecutor` that dispatches by stage ID to inline stage functions
4. Run `wasm-pack build --target web --release`
5. Copy `*.wasm` and `*.js` to the output directory
6. Generate `index.html` (NoetherRuntime inlined, atom/event bootstrap, scoped styles)

**Output:**
```
dist/
  index.html       ← open via any HTTP server
  noether_bg.wasm  ← compiled stages
  noether.js       ← wasm-bindgen glue
```

---

## Part 4 — `noether build --target native [--serve <addr>]`

Compiles the graph into a self-contained Rust binary with an embedded HTTP server.

```bash
# Build and immediately start serving
noether build api/graph.json --output ./api-server --serve :8080

# Build only
noether build api/graph.json --output ./api-server
./api-server --serve :8080
```

The generated binary:
- Accepts `--input <JSON>`, `--dry-run`, `--version`, `--help`
- Accepts `--serve <addr>` to start as an ACLI HTTP server (`POST /`)
- Emits ACLI-shaped JSON on stdout
- Handles CORS automatically

When `--serve` is passed to `noether build`, the CLI execs the binary after installation
(replaces the process on Unix; blocks on Windows). One command covers build + run.

---

## Part 5 — `noether build --target react-native`

```bash
noether build ui/graph.json --target react-native --output ./TodoMobile
cd TodoMobile && yarn install && npx expo start
```

Delegates to the browser build first, then generates a minimal Expo project that renders
the WASM app in a full-screen WebView:

```
<output>/
  assets/
    index.html        ← browser build entry point
    noether_bg.wasm   ← compiled stage graph
    noether.js        ← wasm-bindgen glue
  App.tsx             ← React Native root: full-screen WebView
  app.json            ← Expo configuration
  package.json        ← npm/yarn dependencies (Expo SDK 51, react-native-webview)
  tsconfig.json
  README.md
```

The WebView injects a `postMessage` bridge so `runtime.navigate()` can trigger native navigation.

---

## Part 6 — Full-stack: RemoteStage

`RemoteStage` is a `CompositionNode` variant that calls a remote Noether HTTP API:

```json
{
  "op": "RemoteStage",
  "url": "http://localhost:8080",
  "input": { "kind": "Record", "value": { "action": {"kind":"Text"}, "items": {"kind":"List","item":{"kind":"Text"}} } },
  "output": { "kind": "Record", "value": { "items": {"kind":"List","item":{"kind":"Text"}}, "new_item": {"kind":"Text"} } }
}
```

### Type safety

The declared `input`/`output` types are checked at `noether build` time. If a `RemoteStage`
output is piped into a stage that expects a different type, the build fails before any code is
generated — compile-time safety across the network boundary.

### Execution

| Runtime | How RemoteStage executes |
|---|---|
| Native (test/CLI) | `reqwest::blocking::Client::post(url)` |
| Browser | `fetch(url, { method: 'POST', body: JSON.stringify(input) })` |

The JS runtime (`NoetherRuntime`) parses the full graph JSON on first render and walks the
`CompositionNode` tree, dispatching local `Stage` nodes to WASM and `RemoteStage` nodes to
`fetch()`. This makes the JS runtime a full graph orchestrator — not just a WASM wrapper.

### Full-stack composition graph

```
Sequential {
  RemoteStage { url: "http://localhost:8080" }   ← calls the native API
  Stage { id: "<todo-ui-hash>" }                  ← local WASM stage
}
```

Type flow: `api_input → RemoteStage → Record{items,new_item} → todo-ui → VNode`

---

## Part 7 — Client-side routing

```js
runtime.navigate('/todos');          // push history + re-render
runtime.navigate('/settings', state); // with optional History state
```

### How it works

1. The `NoetherRuntime` constructor listens to `popstate` (browser back/forward)
2. `_route` is auto-injected into every pipeline input from `window.location.pathname` — no
   user atom declaration required
3. `navigate(path)` calls `history.pushState` then triggers a re-render
4. Built-in `navigate` event type in VNode props:
   ```json
   { "onClick": { "$event": "navigate", "$path": "/todos" } }
   ```

### `noether.router` stdlib stage

| Field | Type | Description |
|---|---|---|
| `route` | `Text` | Current path (usually `_route` atom) |
| `default` | `Text` | Fallback route key |
| `routes` | `Record` | Path → VNode mapping |
| _(output)_ | `VNode` | Matched view |

Matching order: exact match → longest prefix match → default key → error.

```json
{
  "op": "Stage",
  "id": "<noether.router-hash>",
  "input": {
    "kind": "Record", "value": {
      "route":   { "kind": "Text" },
      "default": { "kind": "Text" },
      "routes":  { "kind": "Record", "value": {} }
    }
  }
}
```

---

## Part 8 — Scoped styles per stage

Stages can declare per-component CSS via `ui_style`:

```json
{
  "name": "my-card",
  "ui_style": ".card { border: 1px solid var(--edge); padding: 16px; border-radius: 8px; }",
  "implementation_code": "..."
}
```

The browser build:

1. Takes the first 8 chars of the stage content hash as scope prefix: `.nr-<id8>`
2. Rewrites every top-level CSS selector: `.card { ... }` → `.nr-abc12345 .card { ... }`
3. Inlines all scoped `<style>` blocks into `index.html` before the user-defined `style` block

At-rules (`@media`, `@keyframes`, etc.) pass through verbatim — their inner rules inherit the
outer scope via normal CSS cascade.

The `Stage` struct gains:
```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub ui_style: Option<String>,
```

---

## Part 9 — Keyed list reconciliation

`NoetherRuntime._patchChildren` is now key-aware. When any child in the new VNode list carries
`props.key`, the keyed algorithm is used:

1. Build `Map<key, {domNode, oldVnode}>` from existing DOM children (tagged with `__nrKey`)
2. For each new child: reuse + patch the matching old DOM node, or create fresh
3. Remove any DOM nodes whose key is no longer in the new list
4. Reorder nodes to match the new child order

Stages opt in by emitting `"key": "stable-id"` in VNode props — no other changes needed.
Keyless children fall back to the original index-based algorithm (backward compatible).

**Why this matters:** Without keyed reconciliation, adding an item to the start of a list of
N items causes N DOM updates. With keys, it's one insertion.

---

## Graph JSON `ui` extension

Graphs targeting the browser may include an optional `ui` block:

```json
{
  "description": "My app",
  "root": { "op": "Stage", "id": "..." },
  "ui": {
    "atoms": {
      "count": 0,
      "query": "",
      "items": []
    },
    "events": {
      "increment": "atoms => ({ count: atoms.count + 1 })",
      "set-query": "(atoms, e) => ({ query: e.target.value })"
    },
    "style": ":root { --accent: hotpink; }"
  }
}
```

The `events` values are raw JavaScript function expression strings. Full DOM event access
is available via the second argument.

---

## NoetherRuntime API

Source: `crates/noether-cli/src/noether_runtime.js`

| Method | Description |
|---|---|
| `defineAtom(name, initial)` | Declare a reactive state slot |
| `setAtom(name, value\|fn)` | Update atom and schedule re-render |
| `defineEvent(name, handler)` | Register `(atoms, domEvent) → atom patch` handler |
| `navigate(path, state?)` | Push History + set `_route` atom + re-render |
| `render()` | Execute graph, diff VNode tree, patch DOM |
| `_patch(parent, newVNode, oldVNode)` | Recursive DOM differ |
| `_patchChildren(el, newChildren, oldChildren)` | Keyed + indexed reconciliation |
| `_executeGraph(node, input)` | Async CompositionNode walker (local + remote) |
| `_execLocal(stageId, input)` | Calls WASM `execute_stage` |
| `_execRemote(url, input)` | `fetch` POST → ACLI response → output |

### Built-in event protocols

| Protocol | Description |
|---|---|
| `{ "$event": "set", "$target": "atom", "$attr": "value" }` | Bind input value to atom |
| `{ "$event": "toggle", "$target": "atom" }` | Flip boolean atom |
| `{ "$event": "set-value", "$target": "atom", "$value": x }` | Set atom to literal |
| `{ "$event": "navigate", "$path": "/route" }` | Client-side navigation |

---

## Rust stage format

Stages with `implementation_language: "rust"` compile into the WASM binary:

```rust
fn execute(input: &serde_json::Value) -> serde_json::Value {
    let count = input["count"].as_i64().unwrap_or(0);
    serde_json::json!({
        "tag": "div",
        "props": { "class": "counter" },
        "children": [
            { "tag": "span", "props": {}, "children": [{ "$text": format!("Count: {count}") }] },
            { "tag": "button",
              "props": { "onClick": { "$event": "increment" } },
              "children": [{ "$text": "+1" }] }
        ]
    })
}
```

---

## Example: full-stack todo app

```
examples/todo/
  api/
    todo-api-stage.json   ← Rust stage: { action, items, new_item, index } → { items, new_item }
    graph.json            ← single Stage node
  ui/
    todo-ui-stage.json    ← Rust stage: { items, new_item } → VNode
    graph.json            ← Sequential: RemoteStage → todo-ui + ui.atoms + ui.events
  README.md
```

```bash
# Terminal 1 — build and serve the API
noether stage add examples/todo/api/todo-api-stage.json
noether build examples/todo/api/graph.json --output ./todo-api --serve :8080

# Terminal 2 — build the browser UI
noether stage add examples/todo/ui/todo-ui-stage.json
noether build examples/todo/ui/graph.json --target browser -o dist/todo-ui
python3 -m http.server 3000 --directory dist/todo-ui
```

---

## Open questions

❓ **Event model at scale**: Complex events (drag-and-drop, canvas gestures, animations) don't
map cleanly to `{ "$event": "name" }`. Sufficient for forms and CRUD; a richer protocol is
needed for interactive data viz.

❓ **WASM binary size**: The counter example produces ~2.4 MB `.wasm` (includes the full
Noether stdlib). Future: split stdlib stages into separate lazy-loaded WASM components.

❓ **Streaming VNode**: `NType::Stream<VNode>` would enable progressive renders and live-updating
UI components. Currently all renders are batch-synchronous.

❓ **WASM Component Model (WIT)**: The current build embeds all stages into one WASM binary.
Migrating to proper WIT-based composition would enable sharing compiled stage components across
apps without recompilation.

❓ **Server-side rendering (SSR)**: The runtime currently runs entirely in the browser.
Generating initial HTML server-side (from the native binary) and hydrating in the browser
would improve first-paint performance.

---

## What's deferred

- **WIT / WASM Component Model** — currently all stages compile into one binary; proper WIT
  isolation is a future optimization
- **Streaming VNode** — `NType::Stream<VNode>` for progressive renders
- **SSR** — server-side HTML generation from the native binary + browser hydration
- **Python stages in browser** — Python is not supported for WASM builds; Rust only

---

## Implementation inventory

| Feature | File(s) | Status |
|---|---|---|
| `NType::VNode` | `noether-core/src/types/primitive.rs` | ✓ |
| `RemoteStage` AST node | `noether-engine/src/lagrange/ast.rs` | ✓ |
| RemoteStage type checker | `noether-engine/src/checker.rs` | ✓ |
| RemoteStage native executor | `noether-engine/src/executor/runner.rs` | ✓ |
| WASM feature gating | `noether-engine/Cargo.toml` + `src/lib.rs` | ✓ |
| `noether build --target browser` | `noether-cli/src/commands/build_browser.rs` | ✓ |
| `noether build --target native --serve` | `noether-cli/src/commands/build.rs` | ✓ |
| `noether build --target react-native` | `noether-cli/src/commands/build_mobile.rs` | ✓ |
| NoetherRuntime JS (graph orchestrator) | `noether-cli/src/noether_runtime.js` | ✓ |
| Keyed list reconciliation | `noether_runtime.js` `_patchChildrenKeyed` | ✓ |
| Client-side routing (`navigate`) | `noether_runtime.js` | ✓ |
| `noether.router` stdlib stage | `noether-core/src/stdlib/ui.rs` | ✓ |
| Router executor impl | `noether-engine/src/executor/stages/ui.rs` | ✓ |
| `ui_style` on Stage struct | `noether-core/src/stage/schema.rs` + `builder.rs` | ✓ |
| `scope_css` helper | `noether-cli/src/commands/build_browser.rs` | ✓ |
| Full-stack todo example | `examples/todo/` | ✓ |
