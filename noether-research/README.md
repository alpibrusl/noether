# Noether Research

This directory contains design documents for Noether features, kept up to date as research matures into shipped code.

## Active

| Directory | Status | Description |
|---|---|---|
| [`ui-port/`](ui-port/DESIGN.md) | **Implemented** | Complete Noether UI platform: VNode type, WASM browser build, JS reactive runtime, full-stack RemoteStage, client-side routing, scoped styles, keyed reconciliation, mobile target |

## What has been built (quick reference)

### Noether as a UI framework
Noether stages whose output type is `VNode` become UI components. The type system, composition graph, and CLI all handle them natively. Stages are compiled to WebAssembly and run directly in the browser.

### Full-stack with type-safe network boundary
`RemoteStage` is a `CompositionNode` variant that declares a remote HTTP API endpoint inline in the composition graph. Its input/output types are checked statically at `noether build` time, giving compile-time safety across the network boundary.

### `noether build` targets

| `--target` | Output | Use case |
|---|---|---|
| `native` (default) | Self-contained binary | Backend APIs, CLIs |
| `browser` | `index.html` + WASM | Web apps |
| `react-native` | Expo project | iOS / Android apps |

### `--serve <addr>` shorthand
`noether build api/graph.json --output ./server --serve :8080` builds the native binary and immediately execs it as an HTTP server — one command for the full dev workflow.

### Routing
`runtime.navigate(path)` + `popstate` listener + `_route` auto-injected atom. The new `noether.router` stdlib stage maps route paths to VNodes with prefix matching.

### Scoped styles
Stages declare `ui_style` CSS. The browser build scopes every selector to `.nr-<id8>` so stage styles never collide.

### Keyed list reconciliation
`NoetherRuntime._patchChildren` is now key-aware. Stages emit `"key": "stable-id"` in VNode props to enable efficient reuse of DOM nodes across re-renders.

## Archived

The original research sub-documents have been merged into `ui-port/DESIGN.md`:

- `noether-react/` — NoetherReact: content-addressed reactive UI (merged)
- `wasm-target/` — WASM compilation target design (merged)

Cloud registry research is tracked separately in `noether-cloud` (in progress).
