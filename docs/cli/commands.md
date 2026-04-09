# CLI Commands

All commands produce ACLI-compliant JSON on stdout. Exit code is `0` on success, non-zero on error.

## Global flags

These flags apply to every subcommand.

### `--registry <URL>`

```bash
noether --registry https://registry.example.com stage list
noether --registry https://registry.example.com compose "problem"
NOETHER_REGISTRY=https://registry.example.com noether stage search "http get"
```

Points the CLI at a remote **noether-cloud** registry instead of the local JSON file store.

When set, the CLI:

1. **Fetches** all active stages from `GET /stages?lifecycle=active` and populates an in-memory cache.
2. **Reads** (get, list, search, compose) from the local cache — no network latency on every operation.
3. **Writes** (stage submit, lifecycle update) to both the remote registry and the local cache.

The `NOETHER_REGISTRY` environment variable is the recommended way to configure this persistently:

```bash
export NOETHER_REGISTRY=https://registry.noether.example.com

noether stage list          # reads from remote registry
noether compose "problem"   # LLM-composed graph using remote stages
noether run graph.json      # executes using remote stage metadata
```

See the [Remote Registry guide](../guides/remote-registry.md) for a full setup walkthrough.

---



```bash
noether version
```

Returns version and build metadata.

## `noether introspect`

```bash
noether introspect
```

Returns the full ACLI manifest: all commands, their arguments, and output schemas.

## `noether stage`

### `stage list`

```bash
noether stage list
```

Lists all active stages in the store.

### `stage get <hash>`

```bash
noether stage get 8f3a1b…
```

Returns the full stage spec for a given `StageId`.

### `stage activate <hash>`

```bash
noether stage activate 8f3a1b…
```

Promote a Draft stage to Active lifecycle. Supports ID prefix matching.

### `stage search <query>`

```bash
noether stage search "parse json and extract field"
noether stage search "http fetch with timeout" --limit 5
```

Semantic search across all active stages. Returns stages ranked by weighted cosine similarity across three indexes (signature 30%, description 50%, examples 20%).

## `noether store`

### `store stats`

```bash
noether store stats
```

Returns stage counts by lifecycle state, total examples, signed/unsigned split.

### `store dedup`

```bash
noether store dedup
```

Detects functionally duplicate stages (same signature, different metadata). Reports candidates; does not auto-merge.

## `noether run`

```bash
noether run graph.json
noether run --dry-run graph.json       # type-check + plan, no execution
noether run --input '{"k":"v"}' graph.json
```

Executes a Lagrange composition graph. `--dry-run` validates types and prints the execution plan without running any stages.

## `noether compose`

```bash
noether compose "problem description"
noether compose --dry-run "problem"          # graph only, no execution
noether compose --model gemini-2.0-flash "problem"
```

LLM-powered composition. Searches the semantic index for candidate stages, builds a graph, type-checks it, and optionally executes it.

### Provider environment variables

| Variable | Description |
|---|---|
| `NOETHER_LLM_PROVIDER` | LLM provider to use: `mistral`, `openai`, `anthropic`, `vertex`, `mock` |
| `NOETHER_EMBEDDING_PROVIDER` | Embedding provider to use: `mistral`, `openai`, `anthropic`, `vertex`, `mock` |
| `VERTEX_AI_PROJECT` | GCP project ID (Vertex AI) |
| `VERTEX_AI_LOCATION` | GCP region (Vertex AI) |
| `VERTEX_AI_TOKEN` | Auth token (Vertex AI) |
| `VERTEX_AI_MODEL` | Model name (Vertex AI) |
| `OPENAI_API_KEY` | API key (OpenAI / Ollama) |
| `OPENAI_MODEL` | Model name (OpenAI / Ollama) |
| `OPENAI_API_BASE` | Base URL (OpenAI-compatible endpoint, e.g. Ollama) |
| `ANTHROPIC_API_KEY` | API key (Anthropic) |
| `ANTHROPIC_MODEL` | Model name (Anthropic) |

Falls back to mock LLM if no provider env vars are set.

## `noether build`

```bash
# Native binary (default)
noether build graph.json --output my-tool
noether build graph.json --output my-tool --serve :8080   # start HTTP server immediately

# Browser WASM app
noether build --target browser graph.json --output ./dist
```

Compiles a composition graph into a deployable artifact.

**`--target native`** (default) — produces a self-contained binary that:

- Runs once and prints ACLI JSON when invoked without flags.
- Starts an HTTP server on `--serve :PORT` that accepts `POST /run` with `{"input": ...}`.

**`--target browser`** — compiles all Rust stages to WebAssembly via `wasm-pack` and emits three files into `--output`:

| File | Purpose |
|---|---|
| `index.html` | Self-contained app shell with the reactive `NoetherRuntime` |
| `noether.js` | wasm-bindgen JS glue (`execute`, `execute_stage`, `get_graph_json`) |
| `noether_bg.wasm` | Compiled stage graph |

Serve with any static file server:
```bash
cd dist && python3 -m http.server 3000
```

## `noether trace`

```bash
noether trace <composition_id>
```

Retrieves the full execution trace for a past composition run, including per-stage inputs, outputs, timing, and retry history.

## Output format (ACLI)

All responses follow the Agent-friendly CLI protocol:

```json
{
  "ok": true,
  "command": "stage list",
  "result": { … },
  "meta": { "version": "0.1.0" }
}
```

On error:

```json
{
  "ok": false,
  "command": "run",
  "error": {
    "code": "TYPE_ERROR",
    "message": "stage abc… output Record{url} is not subtype of Record{url,body}"
  },
  "meta": { "version": "0.1.0" }
}
```
