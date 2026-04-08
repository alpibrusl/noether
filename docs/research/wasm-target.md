# WASM Target

Compile Noether's Pure Rust stdlib stages to WebAssembly for zero-latency in-browser execution.

---

## Status

`noether build --target browser` is implemented and produces a self-contained HTML bundle.
The compiler emits a JavaScript reactive runtime that wires stage outputs to DOM nodes.
Pure stages execute in-process (no network round-trips).

See [noether build CLI reference](../cli/commands.md) for flags and output options.

---

## What gets compiled to WASM

Only **`InlineExecutor`-backed stages** (the Pure Rust stdlib) are compiled to WASM.
These are all deterministic, have no side effects, and benefit the most from in-process execution.

Stages that require `NixExecutor` (Python/Bash code) are **not** included in the WASM bundle —
they require a subprocess and cannot run in a browser sandbox.

| Stage kind | WASM bundle | Notes |
|---|---|---|
| Pure Rust stdlib | ✅ Included | `InlineExecutor`, zero overhead |
| Python / Bash (Nix) | ❌ Excluded | Requires subprocess |
| LLM stages | ❌ Excluded | Require network |
| Composition graphs mixing both | Partial | Pure subgraphs compiled; Nix stages need a backend |

---

## Architecture

```
noether build graph.json --target browser --output ./dist/app.html
        │
        ├── Type-check graph (same as regular run)
        ├── Walk graph; emit JS module per Pure stage
        ├── Compile Rust stdlib to WASM (wasm32-unknown-unknown)
        ├── Emit reactive JS runtime (signals-based)
        └── Bundle into single-file HTML (inline WASM + JS)
```

The emitted HTML is self-contained — no server needed, no CDN dependencies.

---

## Limitations

- Hot paths only: mixing Pure WASM stages with `NixExecutor` stages in the same composition
  requires a backend proxy for the Nix stages.
- WASM binary size: the full Pure stdlib compiles to ~400 KB (Brotli-compressed).
- Streaming types (`Stream<T>`) are not yet supported in the WASM runtime.

---

## See also

- [noether build command reference](../cli/commands.md)
- [Composition Graphs guide](../guides/composition-graphs.md)
- [NoetherReact research](noether-react.md) — using stage graphs as reactive UI components
