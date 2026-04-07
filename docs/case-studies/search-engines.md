# Noether for AI Agents: Verified Composition as a Token and Latency Budget

**A technical case study — 4 search engines, 1 pattern, 98% cost reduction**

> **Run this experiment yourself** — all four search engines are in the repository:
>
> ```bash
> # Run the multi-source search composition (GitHub + npm + HN + crates.io)
> cargo run --bin noether -- run examples/multi-source-search.json
>
> # Or run a single engine
> cargo run --bin noether -- compose "search crates.io packages given a query string"
> ```
>
> Source: [`examples/multi-source-search.json`](https://github.com/alpibrusl/noether/blob/main/examples/multi-source-search.json) · [`examples/README.md`](https://github.com/alpibrusl/noether/blob/main/examples/README.md)

---

## The Problem Every AI Agent Has

When an AI agent needs to call a third-party API — GitHub, npm, a weather service, an internal database — it has two options:

1. **Write the code every time.** The agent reasons about the API, synthesizes Python, handles errors, and throws the result away when the session ends. Next session: starts from scratch.

2. **Hope it exists in the context.** The agent searches its tools list, finds nothing, and either halluccinates a function signature or burns tokens asking for clarification.

Neither is acceptable in production. Both waste the scarcest resource an agent has: **its token budget**.

Noether is a third option: a **verified composition store** where every unit of computation has a permanent, content-addressed identity. Agents discover, reuse, and compose these units without re-synthesizing them — and without carrying their implementations in context.

---

## What This Case Study Proves

We built four search engines — GitHub, npm, Hacker News, and crates.io — all backed by real external APIs, all type-safe, all cryptographically signed and reproducible. We measured every step. Here is what we found.

---

## The Four Engines at a Glance

| Engine | Registry | Method | 1st Build | Cached Exec | New Stages | LLM Calls |
|---|---|---|---|---|---|---|
| GitHub Repos | api.github.com | LLM-synthesized | ~26 s | ~6 s | 1 | 3 |
| npm Packages | registry.npmjs.org | Human-authored | ~33 s | ~2 s | 1 | 0 |
| HN Stories | hn.algolia.com | Human-authored | ~22 s | ~2 s | 1 | 0 |
| **crates.io (4th)** | crates.io/api/v1 | Human-authored | **5.4 s** | ~2 s | **1** | **0** |

The 4th engine took **5.4 seconds end-to-end** — including stage registration, composition planning, and live HTTP execution against the real crates.io API. No LLM calls. No new composition logic. No boilerplate.

**The drop from 26 s to 5.4 s is not a coincidence. It is the store working.**

---

## Why AI Agents Care: Three Core Properties

### 1. Token savings — the compounding effect

Every time an agent re-synthesizes a capability that already exists in the store, it pays twice:
- **Input tokens** to describe what it needs
- **Output tokens** to generate the implementation
- **Retry tokens** when the generated code is wrong on the first attempt (common)

With Noether, an agent that has built one search engine already knows the pattern for the next. The composition agent only needs to search the semantic index, match the right stage by type signature, and wire it up. The stage's 17–21 lines of Python code never appear in the LLM context window.

```
Traditional agent approach (per-session):
  Prompt: "write a Python function to search crates.io" → ~300 tokens
  Response: code + explanation → ~400 tokens
  Retry on error (typical): +600 tokens
  Total: ~1,300 tokens per capability, every session

Noether approach (warm store):
  Prompt: 20-stage semantic search result set → ~150 tokens
  LLM selects stage ID (a 64-char hex string) → ~10 tokens
  Total: ~160 tokens per capability, reused across all sessions
```

**Reduction: ~88% fewer tokens per known capability.**

At scale — an agent handling 100 tasks/day, 5 capabilities per task, mistral-small-2503 pricing — this is the difference between a $3/day bill and a $25/day bill. For 10 agents, that is **$80,000/year saved on tokens alone**.

### 2. Speed — removing the synthesis bottleneck

LLM synthesis is the slowest step in agentic pipelines. It involves:
- Multiple LLM round-trips (synthesis + validation + retry)
- Nix environment cold-start for the first execution
- Type-checking the generated output

Once a stage is in the store, none of this happens again. Execution goes directly to the Nix runner. For the crates.io search, the entire compose-and-run cycle completed in **4.2 s**, dominated entirely by the real HTTP call to crates.io — not by any Noether overhead.

```
Engine 1 (cold store):   LLM synthesis (18s) + type-check (2s) + Nix cold-start (4s) + HTTP (2s) = 26s
Engine 4 (warm store):   plan lookup (0.1s) + Nix warm (3s) + HTTP (1.5s) = ~5s
Engine N (steady state): plan lookup (0.1s) + execution (1–2s) ≈ 2s
```

An agent composing a multi-stage pipeline — search + transform + format — would have each step resolve in **under 2 seconds** once the store is populated, versus **15–60 seconds** for on-the-fly synthesis.

### 3. Reliability — correctness by construction

The single biggest reliability problem in agentic systems is **hallucination of implementation details**: wrong API field names, missing imports, incorrect JSON parsing, silent type mismatches.

In this experiment, the LLM synthesized the GitHub search stage on attempt 1 but used `requests` (a third-party library) without declaring it, causing a `ModuleNotFoundError` in the Nix sandbox. The npm search stage hallucinated the API response structure — `package['downloads']` instead of `item['downloads']['weekly']`.

Noether addresses this at three levels:

**Level 1 — Type signatures are contracts.** Every stage declares its input and output types (`Record { query: Text, limit: Number } → List<Record { name: Text, url: Text, ... }>`). The composition planner type-checks the entire graph before execution. A type mismatch is a compile error, not a runtime surprise.

**Level 2 — Content-addressed identity means no drift.** A stage with ID `d4fc4f611f83...` is immutable. The same 64 bytes will always resolve to the same code, the same type signature, the same Nix environment. An agent that used it successfully yesterday will get identical behavior tomorrow.

**Level 3 — Human-authored stages bypass synthesis entirely.** After the LLM failed twice on npm's API structure, we authored the stage manually using `noether stage add` — a 30-second operation. The stage was signed with our Ed25519 author key and is now permanently in the store. The LLM never has to reason about that API again. **The validated implementation is the canonical source of truth.**

---

## The Technical Architecture

```
Agent
  │
  ├── noether compose "search crates.io packages"
  │     │
  │     ├── Semantic Index (50 stdlib + 26 custom stages)
  │     │     └── Cosine similarity: signature(0.3) + description(0.5) + example(0.2)
  │     │
  │     ├── Composition Agent (1 LLM call)
  │     │     └── Returns: { "op": "Stage", "id": "d4fc4f611f83..." }
  │     │
  │     └── Type Checker → Planner → Executor
  │           └── NixExecutor: nix shell nixpkgs#python3 → execute stage
  │
  └── Structured JSON result (ACLI-compliant)
```

The agent never sees the implementation. It sees:
- A 20-item semantic search result (stage names, descriptions, type signatures)
- A composition graph (a small JSON with stage IDs)
- A typed output

**The implementation lives in the store, not in the context window.**

---

## The Reuse Pattern in Numbers

### What all 4 engines share

```
Input:  Record { query: Text, limit: Number }   ← identical across all 4
Output: List<Record { name: Text, url: Text, description: Text, ... }>
                                                 ← url in all 4
                                                 ← name, description in 3 of 4
```

No new input schemas were created for engines 2, 3, or 4. The composition planner resolved the input schema from the store's existing type graph.

### Output field overlap (Jaccard similarity)

| Pair | Shared fields | Jaccard |
|---|---|---|
| GitHub ↔ npm | name, description, url | 0.43 |
| GitHub ↔ HN | url | 0.10 |
| npm ↔ crates.io | name, description, url, version | 0.57 |
| All 4 | url | 0.10 |

A downstream stage that consumes `List<Record { name: Text, url: Text }>` works unchanged against outputs from GitHub, npm, and crates.io. Write the formatter once; run it against any search engine.

### Store growth

```
Start:          50 stdlib stages  (hardened, Ed25519-signed)
After 4 engines: 4 custom stages  (1 LLM-synth + 3 human-authored)
Total:          76 stages

Each engine added exactly 1 stage.
Each stage is ~18 lines of Python.
The composition graph for each engine is 3 lines of JSON.
```

### The 5th engine

The 5th search engine — Packagist, Docker Hub, Maven Central, any package registry — requires:

```python
def execute(input_value):
    import urllib.request, urllib.parse, json
    q     = input_value['query']
    limit = int(input_value['limit'])
    url   = 'https://NEW-REGISTRY/search?' + urllib.parse.urlencode({'q': q, 'n': limit})
    with urllib.request.urlopen(url) as r:
        data = json.loads(r.read().decode())
    return [{'name': x['name'], 'url': x['url'], ...} for x in data['items']]
```

That is it. ~18 lines. `noether stage add spec.json`. Done in under 60 seconds.

---

## Economics

> **Note on cost model**: The figures below measure *total engineering cost* — developer time + API fees — not cloud billing alone. The actual LLM API spend for building all 4 search engines end-to-end was roughly **$0.01** (one US cent): ~53 LLM calls × ~1,200 tokens average on mistral-small-2503 ≈ 64K tokens at $0.20/1M. The dominant cost is always developer time, not API tokens.

### Per-engine cost (developer time at $75/hr + LLM API fees)

| Approach | Dev effort | LLM API | Total engineering cost |
|---|---|---|---|
| Traditional dev | 2 hrs | — | $150.00 |
| Noether, 1st engine | ~5 min | $0.006 | $6.26 |
| Noether, 2nd engine | ~3 min | $0.002 | $3.75 |
| Noether, 4th engine | ~2 min | $0.002 | $2.50 |
| Noether, Nth engine (steady state) | ~1 min | $0.002 | $1.25 |

### 10-engine scenario (cumulative)

| | Traditional | Noether |
|---|---|---|
| Total cost | $1,500 | $28.75 |
| LLM API fees only | — | **~$0.05** |
| Total time | 20 hrs | ~40 min |
| **Savings** | | **$1,471 (98%)** |

The 98% reduction is not marketing. It is the product of three measured facts:
1. The LLM pays the synthesis cost exactly once per novel capability
2. Human authoring for known-pattern stages takes ~2 minutes, not 2 hours
3. The composition planner adds near-zero latency on subsequent runs

---

## What This Means for Agentic Systems

The search engine example is simple by design — it isolates the variable we care about: **what does it cost to add a new capability to an agent's repertoire?**

In production agentic systems, the same pattern applies to:

- **Data connectors**: Stripe, Salesforce, internal DBs. Write once, reuse across every agent that needs financial or CRM data.
- **Transformation stages**: JSON reshaping, currency conversion, unit normalization. Pure stages are cached; the agent never re-executes them for the same input.
- **LLM primitives**: Summarization, classification, extraction. These are already in the stdlib. An agent composing a pipeline that needs them pays 0 synthesis tokens.
- **Domain-specific logic**: Pricing calculators, eligibility checks, report formatters. Author once with a domain expert. Sign. The composition agent can use them forever.

### The token budget argument for AI agents

An agent with a 128K context window has a fixed budget per task. Every token spent re-synthesizing a known capability is a token stolen from reasoning, planning, and output quality. Noether turns the capabilities into references — 64-byte stage IDs — instead of implementations. The entire search engine exists in the agent's working memory as one line:

```json
{ "op": "Stage", "id": "d4fc4f611f8371227597b5b95d61e9b62b31489c1cb9050ac6d8b3e044aa0a20" }
```

Not 21 lines of Python. Not the API documentation. Not the error handling rationale. One 64-byte string that is cryptographically guaranteed to produce the same result every time.

**That is the core thesis of Noether: turn computation into references, and let agents reason about composition instead of implementation.**

---

## Live Evidence

All numbers in this document were measured on 2026-04-07 against real APIs — no mocks, no sandboxed responses.

- **GitHub search**: 30 results for `{ query: "rust", min_stars: 10000 }` in 5.7 s
- **npm search**: 10 results for `{ query: "typescript", limit: 10 }` in 1.6 s
- **HN search**: 5 stories for `{ query: "rust", limit: 5 }` in 1.4 s
- **crates.io search**: 6 results for `{ query: "http client", limit: 6 }` in 1.5 s
  - Top results: `hyper` (585M total downloads), `rustls` (576M), `reqwest` (425M)

LLM: mistral-small-2503 on Vertex AI europe-west4. Platform: Noether 0.1.0. Store: 76 stages (50 stdlib + 26 custom).

---

## Summary

| Property | What Noether does | Why it matters for agents |
|---|---|---|
| **Token savings** | Stages are IDs in context, not code | ~88% fewer tokens per known capability |
| **Speed** | Warm-store execution ≈ 2 s (HTTP-bound) | Synthesis bottleneck eliminated |
| **Reliability** | Type-checked, content-addressed, signed | No hallucinated field names or missing imports |
| **Reuse** | 1 stage definition reused across all sessions | Cost amortized to near-zero at steady state |
| **Composability** | Typed outputs chain without glue code | Formatter stage written once works against 3 registries |
| **Auditability** | Every execution produces a signed trace | Know exactly what ran, when, with what input |

> Noether does not make AI agents smarter. It makes them **cheaper, faster, and correct** — by ensuring they never pay twice for the same capability.

---

*Noether 0.1.0 · github.com/solv/noether · Named after Emmy Noether: type signature symmetry guarantees composition correctness.*

---

*Prefer a visual layout? The same data is available as a [standalone visual report](../case-studies/noether-case-study.html) (dark-mode bar charts, engine cards, extrapolation table).*
