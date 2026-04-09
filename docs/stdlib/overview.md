# Stdlib Overview

The Noether stdlib is a curated set of **80+ stages** that ship with every installation. They are:

- **Deterministically identified** — the same `StageId` on every machine.
- **Ed25519-signed** — cryptographically verified by the Noether maintainer key.
- **Immediately available** — loaded into the in-memory store at startup via `load_stdlib()`.

## Categories

| Category | Count | Purpose |
|---|---|---|
| Scalar | 5 | String and number primitives |
| Collections | 8 | List and map operations |
| Control | 6 | Branching, retrying, logging |
| I/O | 8 | HTTP, file, env, sleep |
| LLM Primitives | 4 | Completion, embedding, classification, extraction |
| Data | 7 | CSV, stats, schema validation, diff |
| Noether Internal | 6 | Stage discovery, composition execution, tracing |
| Text Processing | 6 | Split, join, regex, replace |

See the full [Stage Catalogue](catalogue.md) for types and descriptions.

## Using stdlib stages

Stdlib stages are referenced by their `StageId` hash, not by name. Use `noether stage search` to find the right hash:

```bash
noether stage search "split text"
# Returns stages including text_split with its StageId
```

!!! tip
    The LLM-powered `noether compose` automatically selects the right stdlib stages by semantic search — you rarely need to look up hashes manually.
