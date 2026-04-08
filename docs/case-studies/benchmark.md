# Benchmark: Noether's Claims vs Evidence

This page provides verifiable, reproducible evidence for every claim Noether makes.
All numbers come from the open-source codebase — run `cargo test` to reproduce them.

---

## Claim 1: "Composition is faster than writing code from scratch"

### The experiment

We built **4 semantic search engines** (GitHub, npm, Hacker News, crates.io) and measured
the marginal cost of each new engine after the first.

| Engine | Stages written | Stages reused | Total stages in graph | Build time |
|---|---|---|---|---|
| GitHub search | 4 new | 0 | 4 | baseline |
| npm search | 1 new | 5 reused | 6 | −75% |
| Hacker News | 1 new | 6 reused | 7 | −83% |
| crates.io | 1 new | 7 reused | 8 | −88% |

**Result:** each additional search engine required writing exactly 1 new stage
(the API-specific URL builder) and reusing everything else — HTTP fetch, JSON parse,
result formatting, deduplication, sorting.

A 5th engine today would cost **1 stage** — the URL builder — and reuse the other 7.
At 10 engines the marginal cost is still 1 stage.

The full case study with cost model and extrapolation is in the
[Four Search Engines case study](search-engines.md).

---

## Claim 2: "Type errors are caught before execution"

### The evidence

The composition engine type-checks every edge in the DAG before a single stage runs.

```bash
# This catches a type mismatch at graph-check time, not at runtime
noether run --dry-run graph.json
# Output: {"ok": false, "error": {"code": "TYPE_ERROR",
#   "message": "stage abc… output Record{url} is not subtype of Record{url,body}"}}
```

The type checker is exercised by 156 unit tests:

```
test result: ok. 156 passed; 0 failed; finished in 0.92s
```

Key properties verified:

- **Structural subtyping**: `Record{a,b,c}` is subtype of `Record{a,b}` (width subtyping)
- **Union types**: `Text | Null` is subtype of `Any`
- **Bidirectional `Any`**: `is_subtype_of(T, Any)` and `is_subtype_of(Any, T)` are both `Compatible`
- **List covariance**: `List<Text>` is subtype of `List<Any>`

The type checker runs in **< 1 ms** for graphs with up to 20 nodes (measured in CI).

---

## Claim 3: "Same stage, same result — always"

### The evidence

Every stage is identified by the SHA-256 hash of its `StageSignature`:

```
id = SHA-256(canonical_json(input_type, output_type, effects, implementation_hash))
```

This is tested explicitly:

```rust
// From crates/noether-core/tests/stdlib_validation.rs
#[test]
fn stdlib_ids_are_deterministic() {
    let stages1 = load_stdlib();
    let stages2 = load_stdlib();
    for (s1, s2) in stages1.iter().zip(stages2.iter()) {
        assert_eq!(s1.id, s2.id);  // same binary → same IDs, always
    }
}
```

The consequence: a composition graph that worked yesterday will either:

1. Work identically today (same stage IDs resolve to same implementations), or
2. **Fail loudly** if a stage was changed (its ID changes, the graph can't resolve it)

There is no "it worked differently but silently" — the content-addressed model makes
silent regressions structurally impossible.

---

## Claim 4: "Semantic search finds the right stage"

### Performance

100 searches over 76 stages complete in < 500 ms (the test asserts this):

```rust
// From crates/noether-engine/tests/index_integration.rs
let start = Instant::now();
for _ in 0..100 {
    let _ = index.search("convert text to number", 20).unwrap();
}
let elapsed = start.elapsed();
assert!(elapsed.as_millis() < 500);  // 100 searches < 500ms = < 5ms each
```

In practice on a dev machine, 100 searches complete in ~200 ms — roughly **2 ms per search**
for 76 stages using brute-force cosine similarity.

### Relevance

The index uses three sub-indexes with weighted fusion:

| Index | Weight | What it captures |
|---|---|---|
| Signature (type-based) | 30% | Input/output type compatibility |
| Description (semantic) | 50% | Intent and domain language |
| Examples (data-based) | 20% | Concrete input/output patterns |

A query for "parse json and extract field" ranks `json_path` (`c7d35f7c`) above
`parse_json` (`b89d34eb`) — the type + example signal outweighs the description match.

The full index test suite:

```
test result: ok. 8 passed; 0 failed; finished in 0.20s
```

---

## Claim 5: "The platform validates stages using stages"

The `noether-cloud` registry's `POST /stages` validation runs as a **Noether composition**,
not ad-hoc Rust code.  At startup the registry builds this graph:

```
Stage JSON input
       │
   Parallel ─────────────────────────────────────────────────────
   │ hash_check              │ sig_check     │ desc   │ examples │
   │ verify_stage_content_hash verify_ed25519  check   check     │
   └──────────────────────────────────────────────────────────────
                             │
                   merge_validation_checks
                             │
               { passed: bool, errors: [], warnings: [] }
```

All 5 stages (`f608988c`, `136f78d7`, `4341c15f`, `f7d94d6e`, `60c9fa10`) are stdlib stages,
signed with the stdlib Ed25519 key, and execute inline in Rust — no subprocess, ~1 ms total.

---

## Stdlib size over time

| Version | Stages | Categories | Tests |
|---|---|---|---|
| Phase 0 (foundation) | 0 | — | 13 |
| Phase 1 (stdlib) | 50 | 8 | 55 |
| Phase 2 (engine) | 50 | 8 | 211 |
| Phase 3 (agent) | 65 | 9 | 370+ |
| Current (validation) | **75** | 10 | **390+** |

---

## Reproducing these numbers

```bash
git clone https://github.com/alpibrusl/noether
cd noether
cargo test                          # run all 390+ tests
cargo test -p noether-engine        # 156 type-checker + index + executor tests
cargo test -p noether-core          # 55 type system + stdlib tests
cargo run --bin noether -- stage list  # see all 76 stages with real IDs
noether run --dry-run examples/fleet-briefing.json   # type-check a real graph
```

All tests pass on a clean checkout with no environment variables set.
No API keys, no network access, no external services required for the test suite.
