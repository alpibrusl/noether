# Semantic Search

`noether stage search` finds stages by meaning, not by name.  The same index powers
`noether compose` — the LLM agent searches it before generating graphs.

---

## How the index works

The semantic index is a three-index fusion:

```
Query: "parse JSON and extract a field"
         │
  ┌──────┴──────────────────────────────────────────┐
  │ Signature index (weight 0.3)                     │
  │ Embedding of: input_type + output_type + effects │
  ├──────────────────────────────────────────────────┤
  │ Description index (weight 0.5)                   │
  │ Embedding of: stage.description                  │
  ├──────────────────────────────────────────────────┤
  │ Example index (weight 0.2)                       │
  │ Embedding of: first example input + output       │
  └──────────────────────────────────────────────────┘
         │
  Cosine similarity → weighted fusion → ranked results
```

The fusion means a query for "extract field from JSON" ranks `c7d35f7c` (json_path)
above `b89d34eb` (parse_json) even though both mention JSON — the example signal shows
`json_path` takes a path expression and returns a single value, which better matches the intent.

---

## Performance

100 queries over 76 stages complete in < 500 ms (brute-force cosine similarity).
In practice on a dev machine: ~2 ms per search.

At 10,000 stages the brute-force approach would take ~200 ms; the index interface
is designed to swap in HNSW or other ANN libraries behind the same `EmbeddingProvider`
trait without changing the API.

---

## Usage

```bash
# Basic search
noether stage search "convert text to uppercase"

# Limit results
noether stage search "http request" --limit 5

# Against a remote registry
NOETHER_REGISTRY=https://registry.example.com \
  noether stage search "parse CSV into records"
```

Output:

```json
{
  "ok": true,
  "command": "stage search",
  "data": {
    "results": [
      {
        "id": "1b68a050",
        "description": "Convert text to uppercase",
        "score": 0.97,
        "input":  "Text",
        "output": "Text"
      },
      {
        "id": "ef422946",
        "description": "Convert text to lowercase",
        "score": 0.81,
        "input":  "Text",
        "output": "Text"
      }
    ]
  }
}
```

---

## Embedding providers

| Provider | When used | Quality |
|---|---|---|
| `MockEmbeddingProvider` | Tests, no env vars | Hash-based (deterministic, not semantic) |
| `VertexAiEmbeddingProvider` | `VERTEX_AI_*` env vars set | Production-quality semantic embeddings |

The mock provider uses a deterministic hash of the text as the embedding vector.
This is sufficient for testing the index machinery but does not capture semantic
similarity — use Vertex AI for real agent workflows.

---

## Near-duplicate detection

Before inserting a new stage, the registry checks for near-duplicates:

```rust
index.check_duplicate_before_insert(&stage.description, threshold: 0.92)
```

If an existing stage has > 92% description similarity to the new stage, a warning
is added to the validation report.  This is a warning, not a hard error — the author
decides whether the new stage is genuinely different.

---

## Rebuilding the index

The index is rebuilt from the store at startup and updated incrementally when stages
are added or tombstoned:

```rust
// At startup
let index = SemanticIndex::from_stages(stages, embedding_provider, config)?;

// On stage insert
index.add_stage(&new_stage)?;

// On tombstone
index.remove_stage(&stage_id);
```

The index is held in memory — no persistence layer.  A registry restart rebuilds it
from the store in < 100 ms for 76 stages.
