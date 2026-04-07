# WASM Compilation Target — Design Document

> **Status: Research / Pre-proposal**
> Last updated: 2026-04

## The problem with Nix execution

Today, Noether stages run via `nix run nixpkgs#python3 -- stage.py`. This gives hermetic execution but has fundamental limits:

- **Latency**: 200ms–2s per stage invocation (Nix subprocess spawn)
- **Browser**: Cannot run in a browser — no subprocess, no Nix
- **Edge**: Cannot run on Cloudflare Workers, Deno Deploy, etc.
- **Mobile**: Cannot run on iOS/Android

WASM solves all four. A `.wasm` binary runs in browsers, on edge runtimes, in mobile apps, and on servers — sub-millisecond, sandboxed, deterministic.

---

## Target architecture

```
stage.py (Python) ──── MicroPython/Emscripten ──→ stage.wasm
stage.rs (Rust)   ──── rustc --target wasm32 ──→ stage.wasm
stage.js (JS)     ──── wasm-pack / Javy ────────→ stage.wasm
stage.wit (WIT)   ──── wac compose ─────────────→ composed.wasm
```

All paths produce a `.wasm` file that implements the Noether Canonical ABI:

```wit
// noether:stage/world
package noether:stage@0.1.0;

interface execute {
  execute: func(input: string) -> result<string, string>;
  //              ↑ JSON-encoded NType value     ↑ JSON error
}

world stage {
  export execute;
}
```

This is a **WebAssembly Interface Types (WIT)** world. Any language that compiles to WASM and implements this world is a valid Noether stage.

---

## The WIT ↔ NType mapping

Noether's type system and WIT are structurally similar:

| NType | WIT |
|---|---|
| `Text` | `string` |
| `Number` | `f64` |
| `Bool` | `bool` |
| `List<T>` | `list<T>` |
| `Record { a: T }` | `record { a: T }` |
| `Any` | `string` (JSON-encoded) |

The Canonical ABI uses JSON encoding for `Any` — this is a pragmatic escape hatch until WIT gains sum types expressive enough for NType's full Union/Stream variants.

---

## noether build --target wasm

The new compilation path:

```bash
noether build graph.json --output my-app.wasm --target wasm
```

Produces a single `.wasm` component that:
1. Embeds all custom stages (each as a sub-component)
2. Implements the top-level `execute` function of the composition graph
3. Can be loaded by any WASM runtime

For browser use:
```bash
noether build graph.json --output my-app.html --target browser
# Produces: standalone HTML page with embedded WASM + reactive runtime
```

---

## Python stage → WASM paths

Python is the trickiest because CPython doesn't compile to WASM. Options:

| Option | Maturity | Startup | Notes |
|---|---|---|---|
| **Javy** (JS + QuickJS → WASM) | Production | ~1ms | Requires Python→JS transpilation step |
| **MicroPython** compiled to WASM | Mature | ~5ms | Limited stdlib, fits most Noether stages |
| **Pyodide** | Production | ~3s first load, cached | Too heavy for edge, good for browser |
| **Rewrite gate** | N/A | N/A | Prompt the LLM to re-implement in Rust for WASM |

Recommended path: **MicroPython for simple stages, rewrite gate for performance-critical ones**.

The rewrite gate:
```bash
noether stage wasm-compile <stage-id>
# If Python: attempts MicroPython compilation
# If fails: invokes LLM to rewrite as Rust, compiles with rustc --target wasm32
```

---

## Content addressing + WASM

WASM binaries are deterministic (given the same source, the same binary is produced by the same compiler). This enables a new guarantee:

```
SHA-256(stage.wasm) == stage_id (when the implementation is WASM)
```

The stage ID becomes the WASM content hash. The binary cache (CDN) can serve any component by hash, cached forever.

```
https://registry.noether.dev/stages/a4f9bc3e...  → stage.wasm (immutable, forever)
```

---

## Security model

WASM is sandboxed by the runtime. Capabilities map to WASI imports:

| Noether Effect | WASI capability |
|---|---|
| `Network` | `wasi:http/outgoing-handler` |
| `FsRead` | `wasi:filesystem/preopens` |
| `FsWrite` | `wasi:filesystem/preopens` |
| `Pure` | (no imports) |

A `Pure` stage compiles to a WASM component with no imports at all — the runtime can verify purity at load time by inspecting the import section.

---

## Open questions

❓ **Python compilation**: MicroPython covers ~80% of stage code (stdlib stages, most user stages). What's the strategy for stages that use `requests`, `pandas`, `numpy`? Provide a compatibility layer or require rewrite?

❓ **Composition at WASM level**: The Bytecode Alliance's `wac` tool composes WASM components by wiring imports/exports. Can Noether's Lagrange graph compile directly to a `wac` composition file? The mapping looks 1:1 for Sequential/Parallel.

❓ **Streaming**: `NType::Stream<T>` — WASM's linear execution doesn't naturally support streaming. Does the stage produce an iterator, and the runtime polls it?

❓ **Startup cost**: Even MicroPython adds ~5ms startup. For interactive UI (NoetherReact), is 5ms acceptable? Probably yes for event handlers, borderline for keystroke-level interactions.

---

## Implementation milestones

1. **M1 — WIT world** (1 day): Define `noether:stage/world` in WIT, write a test Rust stage that implements it
2. **M2 — Rust stages** (3 days): `noether build --target wasm` for stages written in Rust
3. **M3 — MicroPython bridge** (1 week): Compile a representative Python stage to WASM via MicroPython
4. **M4 — wac composition** (3 days): Compile a Lagrange Sequential graph to a `wac` composition file
5. **M5 — browser target** (3 days): `noether build --target browser` produces runnable HTML

Total to M5: ~3 weeks.
