# Cloud Registry — Design Document

> **Status: Research / Pre-proposal**
> Last updated: 2026-04

## What is the cloud registry?

A globally distributed, content-addressed registry for Noether stages.

Today, stages live in `~/.noether/store.json` on a single machine. The cloud registry makes stages:
- **Discoverable** — search by type signature, capability, or natural language across all public stages
- **Distributable** — `noether stage pull a4f9bc3e...` fetches a stage from anywhere
- **Immutable** — once published, a hash never changes content (tombstone is metadata only)
- **Composable with trust** — stages are signed; consumers verify before execution

This is npm for computation, not for code.

---

## Design principles

**1. The hash is the URL.** Every stage is permanently addressable at:
```
https://registry.noether.dev/stages/<sha256-hex>
```
No versions. No semver conflicts. The content *is* the identity.

**2. Names are mutable pointers.** Human-readable names (`http_get`, `sort_list`) are registry metadata that points to a hash. Names can be updated (pointing to a newer hash). The hash never changes.

**3. Signatures are mandatory.** Every stage in the registry must carry an Ed25519 signature. The registry verifies on publish; clients verify on fetch.

**4. The registry stores metadata, not execution.** The registry holds stage specs and WASM binaries (when available). Execution stays with the client. The registry is never a trusted execution environment.

---

## API surface

```
GET  /stages/<hash>              → stage spec JSON
GET  /stages/<hash>/wasm         → .wasm binary (if compiled)
POST /stages                     → publish a stage (requires signature)

GET  /search?q=<text>            → semantic search
GET  /search/typed?in=T&out=U    → type-compatible search
GET  /names/<namespace>/<name>   → resolve name → hash

GET  /authors/<pubkey>           → stages by author
GET  /stats                      → registry statistics
```

The type-compatible search endpoint (`/search/typed`) is the key differentiator from npm or PyPI — you can ask "give me all stages that accept `Record { url: Text }` and return `Record { title: Text, content: Text }`".

---

## Publishing

```bash
# One-time: generate author keypair
noether registry keygen

# Publish a stage (signs with local key, uploads)
noether stage publish <stage-id>

# Fetch a stage from the registry
noether stage pull a4f9bc3e...

# Search the registry
noether stage search "parse HTML and extract links" --registry
```

Publishing is idempotent: publishing the same hash twice is a no-op.

---

## Trust model

```
Author keypair (Ed25519)
  └─ signs StageSignature
       └─ hash = SHA-256(canonical JSON)
            └─ registry stores (hash, signature, pubkey, spec)
                 └─ client verifies: signature valid AND hash matches content
```

The registry is a **transparency log** — every published stage is append-only. You can audit the full history of what was published and when.

Revocation = tombstone. A tombstone is a signed registry entry that says "the author requests this hash not be used." The content is still retrievable (history is immutable) but clients can choose to honour tombstones.

---

## Namespaces

```
noether:core/http_get@a4f9bc3e    ← official stdlib
acme:travel/flight_search@b7d2e1  ← organization stage
user:alice/my_transform@c9f3a4    ← personal stage
```

Namespaces are claimed by keypair. `noether:core` is the official Noether org key.

---

## Federated registries

Multiple registries can coexist, similar to Go's `GOPROXY` chain:

```toml
# ~/.noether/config.toml
registries = [
  "https://registry.noether.dev",          # official
  "https://registry.acme.com",             # company internal
  "https://registry.community-stages.dev", # community
]
```

A client searches registries in order. Since stages are content-addressed, the same hash in two registries is provably the same stage.

---

## The stdlib becomes a public registry entry

Today the stdlib is compiled into the binary. With the cloud registry:

```bash
# On first run, fetch stdlib from registry (cached forever by hash)
noether init
# Fetches all 50 stdlib stages and caches them in ~/.noether/store.json
```

Updates to the stdlib are new hashes — old stages are never modified. Clients can pin to specific hashes for maximum reproducibility.

---

## Relationship to existing tools

| Tool | Overlap | Noether difference |
|---|---|---|
| **npm** | Package registry, semantic search | Content-addressed, typed I/O, executable not importable |
| **MTHDS Know-How Graph** | Typed search (`accepts/produces`) | Execution layer, WASM distribution |
| **Warg/wkg** (WASM registry) | Content-hash component distribution | Stage metadata, semantic search, type compatibility |
| **AgentHub** (arXiv 2025) | Immutable manifests by hash | Runtime execution, composition engine |

---

## Revenue model (commercial repo)

The open-source registry is self-hostable. The commercial offering:

| Tier | Features |
|---|---|
| Free | Public stages, community registry, 100 fetches/day |
| Pro | Private stages, org namespaces, 100k fetches/day |
| Enterprise | Self-hosted, SSO, audit logs, SLA |

**The moat is not the registry software** (anyone can run it). The moat is:
- The official stdlib (curated, tested, signed)
- The semantic search quality (embeddings trained on stage data)
- The network effect (more stages → better search → more users → more stages)

---

## Open questions

❓ **Billing unit**: Is it per-fetch, per-stage-execution (if the registry offers execution), or per-namespace/seat?

❓ **Private stages in public compositions**: If a published composition graph references a private stage hash, how does a consumer fetch it? Capability tokens? Stage bundling?

❓ **WASM compilation service**: Should the registry offer on-demand compilation of Python stages to WASM? Or is this always client-side?

❓ **Search embeddings**: The current semantic index uses Vertex AI embeddings. For the public registry, what's the embedding strategy — hosted model, community embeddings, multiple providers?

---

## Implementation milestones

1. **M1 — HTTP registry server** (3 days): Rust/Axum server, GET/POST for stages, SQLite backend
2. **M2 — `noether stage publish/pull`** (2 days): CLI commands, signature verification
3. **M3 — Type-compatible search** (3 days): `/search/typed` endpoint using existing semantic index
4. **M4 — Namespace + keypair management** (3 days): `noether registry keygen`, namespace claiming
5. **M5 — Federation** (1 week): Multi-registry config, proxy protocol

Total to M5: ~3 weeks. Can run locally for development from day one.
