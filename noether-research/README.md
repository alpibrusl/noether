# Noether Research

> **Status: Experimental.** These are design documents and proofs-of-concept for directions
> beyond Noether's current roadmap. Nothing here is stable or committed.

This directory contains research into extending Noether toward three frontier areas:

1. **[NoetherReact](./noether-react/DESIGN.md)** — content-addressed, typed reactive UI
2. **[WASM Compilation](./wasm-target/DESIGN.md)** — stages that compile to WebAssembly and run in browsers
3. **[Cloud Registry](./cloud-registry/DESIGN.md)** — federated, content-addressed stage distribution

---

## Why these three?

They form a natural progression:

```
Today:      stage (Python/JS/Bash) → Nix subprocess → JSON output
                                                            ↓
WASM:       stage (any lang) → .wasm component → browser / edge / server
                                                            ↓
NoetherReact: stage output = VNode → reactive runtime → live DOM
                                                            ↓
Registry:   stage identity (hash) → distributed CDN → any client
```

Each step unlocks the next. WASM gives you browser-native execution without Nix. NoetherReact gives you reactive UI from WASM stages. The cloud registry makes both available globally, identified by hash, no install required.

---

## Connection to existing work

| Research area | Related external work | Gap Noether fills |
|---|---|---|
| WASM target | Bytecode Alliance Component Model (WIT/wac) | UI layer, reactive runtime |
| NoetherReact | Solid.js (fine-grained reactivity), Elm (typed model) | Content-addressed components, AI composition |
| Cloud registry | MTHDS Know-How Graph, AgentHub | Execution, not just discovery |

---

## How to contribute

These documents are living designs — open questions are clearly marked with `❓`.
If you have answers, open a PR.

If you want to prototype something, start a directory here:
```
noether-research/
  noether-react/
    DESIGN.md         ← architecture design
    prototype/        ← proof-of-concept code (any language)
  wasm-target/
    DESIGN.md
    prototype/
  cloud-registry/
    DESIGN.md
```

Prototypes don't need to compile, pass tests, or be production-quality.
They need to be clear enough that someone else can pick them up.
