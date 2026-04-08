# Stage Explorer

A full-stack Noether application that lets you browse and search the Noether standard library from a browser.

Demonstrates:
- **`noether build --target browser`** — compiles a composition graph into a WASM + HTML app
- **`noether build --target native --serve :8080`** — builds and immediately runs a backend API
- **`RemoteStage`** — type-safe browser-to-server communication with no HTTP boilerplate
- **Atoms + events** — reactive UI state driven by pure Rust stage functions

```
┌─────────────────────────────────┐       ┌────────────────────────────┐
│  Browser (WASM)                 │  HTTP │  Native binary (--serve)   │
│                                 │ ────► │                            │
│  atoms: { query, limit }        │       │  InlineExecutor            │
│       ↓ RemoteStage             │  POST │  explorer-api stage        │
│  { query, results }             │ ◄──── │  returns { query, results }│
│       ↓ explorer-ui stage       │       │                            │
│  VNode (search + cards)         │       └────────────────────────────┘
└─────────────────────────────────┘
```

## Quick start

### 1. Register the stages

```bash
# From the repo root
noether stage add examples/stage-explorer/api/explorer-api-stage.json
noether stage add examples/stage-explorer/ui/explorer-ui-stage.json
```

### 2. Build the backend

```bash
noether build examples/stage-explorer/api/graph.json \
  --target native \
  --output ./stage-explorer-api \
  --serve :8080
```

This builds a self-contained binary and immediately starts it as an HTTP server on port 8080. Keep this running.

### 3. Build the frontend

In a new terminal:

```bash
noether build examples/stage-explorer/ui/graph.json \
  --target browser \
  --output ./stage-explorer-ui
```

### 4. Open the app

```bash
# Serve the dist directory (any static server works)
cd stage-explorer-ui && python3 -m http.server 3000
```

Then open http://localhost:3000 in your browser.

## What you'll see

- A search input — type any keyword (`sort`, `text`, `http`, `llm`, `number`, …)
- Click **Search** to query the backend
- Results appear as cards showing: stage ID prefix, description, input → output types
- The backend runs `explorer-api`, which does keyword matching over an embedded catalog of all 77 stdlib stages

## How it works

**Backend** (`api/graph.json`):
- Root is a single `explorer-api` stage (synthesized Rust)
- Takes `{ query: Text, limit: Number? }` as input
- Searches an embedded catalog of stdlib stage descriptions
- Returns `{ query: Text, results: List<StageRecord> }`
- Built with `--serve` so it starts as a POST `/execute` HTTP server

**Frontend** (`ui/graph.json`):
- Root is a `Sequential` graph with two nodes
- Node 1: `RemoteStage` → POSTs `{ query, limit }` atoms to `:8080`, receives `{ query, results }`
- Node 2: `explorer-ui` stage → converts results to a `VNode` tree
- Atoms `{ query, limit }` drive the render cycle
- Events: `type` updates `query` atom silently; `search` triggers a re-render (and thus a backend call)

## Upgrading to real semantic search

The backend currently uses keyword matching against an embedded catalog. To upgrade to real cosine-similarity search using the live stage store:

1. Set `MISTRAL_API_KEY` or `VERTEX_AI_PROJECT` before building
2. Change `api/graph.json` to reference the stdlib `store_search` stage directly:

```json
{
  "root": {
    "op": "Stage",
    "id": "fbee00ae2c431a9d7e99bb3eb1abe24eed63aa583730cca21c022765118050b0"
  }
}
```

The `--serve` binary will use the `RuntimeExecutor` with your embedding provider for real semantic search.
