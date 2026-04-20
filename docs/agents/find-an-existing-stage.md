# Playbook: find-an-existing-stage

## Intent

Find a stage already in the store that matches a given type signature or intent, instead of synthesizing a new one. Cheaper, more reproducible, and the provenance is clearer (signed by the stdlib key or a known tenant).

## Preconditions

- Local store populated (`noether stage list` returns ≥1 stage) — the stdlib alone gives ~70 general-purpose stages.
- Semantic index built. It's constructed automatically on first search; you don't need to force it.

## Steps

1. **Semantic search** is the fastest path. Scored 0.3 signature + 0.5 description + 0.2 example via cosine similarity:
   ```bash
   noether stage search "sum a list of numbers"
   ```
   Top results come with `id`, `description`, `signature (input → output)`, `score`, `tags`. The 8-char `id` prefix is unique enough to paste into a graph.
2. **Filter by tag** when you know the category (browse tags with `noether stage list --tags`):
   ```bash
   noether stage search "http request" --tag network
   noether stage list --tag text
   ```
3. **Inspect a specific stage** once you have a candidate:
   ```bash
   noether stage get <id-or-prefix>
   ```
   Returns the full `Stage` struct: signature, effects, examples, properties, capabilities, cost estimate.
4. **Verify behaviourally** when the stakes are high — run the stage's own declared examples through the executor:
   ```bash
   noether stage verify <id>   # signatures + properties by default; --signatures / --properties restrict
   ```
5. **Use the stage in a graph** by dropping its id (full or 8-char prefix) into a `CompositionNode::Stage`. Default pinning is `Signature` (the resolver picks the currently-Active implementation at execution time), stable across bugfix rewrites.

## Output shape

`noether stage search` result:

```json
{
  "ok": true,
  "result": {
    "query": "sum a list of numbers",
    "hits": [
      {
        "id": "a1b2c3d4...",
        "signature_id": "...",
        "description": "Sum all numeric elements in a list",
        "signature": {"input": "List<Number>", "output": "Number", "effects": ["Pure"]},
        "score": 0.87,
        "tags": ["scalar", "list"],
        "lifecycle": "Active"
      }
    ]
  }
}
```

## Failure modes

| Symptom | Meaning | Remedy |
| --- | --- | --- |
| `no stages match` | Semantic index has results but all below threshold (0.5 default) | Broaden the query; drop the `--tag` filter; fall back to `noether compose` for synthesis |
| `ambiguous prefix` | Multiple stages share the prefix | Use ≥12 hex chars or the full 64-char id |
| `stage not found` on `get` | Id isn't in the local store | Check the tenant (`--registry` env); `stage list` to confirm; `sync` if you have a stages dir |
| High-score match exists but signature mismatches your use | The description embedding matched an unrelated stage | Read `signature` carefully; a `Text → Text` match is not interchangeable with `Record { body: Text } → Text` |

## Verification

```bash
# Expect ≥1 hit for a common stdlib intent.
noether stage search "count words" | jq '.result.hits | length'
```

## See also

- [`compose-a-graph`](compose-a-graph.md) — once you have your stages, combine them automatically with the agent.
- [`synthesize-a-new-stage`](synthesize-a-new-stage.md) — when nothing in search is close enough.
- `noether introspect` — the full command tree if you need flags this playbook didn't cover.
