# Noether Todo — Full-Stack Example

This example demonstrates a **full-stack Noether application** using `RemoteStage` to connect a browser UI to a native backend API. The type checker verifies the connection at build time — no runtime schema negotiation needed.

## Architecture

```
atoms { action, items, new_item, index }
  │
  ▼
[RemoteStage http://localhost:8080]      ← fetch() via JS runtime
  │  input:  Record { action, items, new_item, index }
  │  output: Record { items: List<Text>, new_item: Text }
  ▼
[todo-ui stage]                          ← WASM execute_stage()
  │  input:  Record { items, new_item }
  │  output: VNode
  ▼
DOM
```

The `RemoteStage` carries its types inline — the Noether type checker validates the full pipeline at `build` time without the server needing to run.

## Project layout

```
examples/todo/
  api/
    todo-api-stage.json   ← Rust stage: processes add/remove/list actions
    graph.json            ← single-stage backend graph
  ui/
    todo-ui-stage.json    ← Rust stage: renders Record { items, new_item } → VNode
    graph.json            ← Sequential: RemoteStage → todo-ui, with UI atoms + events
  README.md               ← this file
```

## Prerequisites

- Rust toolchain + `cargo`
- `wasm-pack` — `cargo install wasm-pack`
- Noether CLI — `cargo build --release -p noether-cli` (from workspace root)

## Running the example

### 1 — Register stages (first run only)

```bash
cd /path/to/solv-noether

# Register the API stage
noether stage add examples/todo/api/todo-api-stage.json

# Register the UI stage
noether stage add examples/todo/ui/todo-ui-stage.json
```

> The stage IDs in `graph.json` files are pre-filled for the stages in this repo.
> If you re-register stages after editing them, update the `"id"` in `ui/graph.json`.

### 2 — Build the API backend

```bash
noether build examples/todo/api/graph.json --output ./todo-api-server
```

This compiles the Rust `todo-api` stage into a self-contained native binary that exposes a `POST /` HTTP endpoint.

### 3 — Build the browser UI

```bash
noether build examples/todo/ui/graph.json --target browser --output ./todo-dist
```

This:
1. Type-checks the full pipeline including the `RemoteStage` declared types
2. Compiles the `todo-ui` stage to WASM via `wasm-pack`
3. Generates `index.html` with the `NoetherRuntime` embedded

### 4 — Run (two terminals)

**Terminal 1 — API server:**
```bash
./todo-api-server --serve :8080
```

**Terminal 2 — Browser:**
```bash
cd todo-dist
python3 -m http.server 3000
# then open http://localhost:3000
```

## How it works

1. The browser loads `index.html`, which initialises the `NoetherRuntime` with the WASM module.
2. On each render, the runtime calls `_executeGraph` with the current atom state `{ action, items, new_item, index }`.
3. The `RemoteStage` node triggers a `fetch()` to `http://localhost:8080`, posting `{ input: { action, ... } }`.
4. The backend processes the action (add/remove/list) and returns `{ items: [...], new_item: "" }`.
5. The response is piped into the `todo-ui` WASM stage (`execute_stage`), which produces a VNode.
6. The runtime diffs the VNode against the previous DOM tree and patches only what changed.

## Event flow

| User action | Atom mutation | Next render |
|-------------|---------------|-------------|
| Types in input | `new_item` atom updated via `set` event | Re-renders with new input value |
| Clicks "Add" | `action = "add"` | RemoteStage sees action + new_item, adds to list, clears new_item |
| Clicks "✕" on item | `action = "remove", index = i` | RemoteStage removes item at index |

## Type safety

The type checker verifies the full pipeline at build time:

```
Record { action, items, new_item, index }
  → RemoteStage [declared: → Record { items: List<Text>, new_item: Text }]
  → todo-ui [input: Record { items: List<Text>, new_item: Text }]
  → VNode
```

If the RemoteStage output type doesn't match `todo-ui`'s input, `noether build` fails with a type error — no runtime debugging needed.
