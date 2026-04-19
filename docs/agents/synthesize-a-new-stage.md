# Playbook: synthesize-a-new-stage

## Intent

Author a new stage from scratch — either (a) automatically via the Composition Agent during `noether compose`, or (b) by hand via `noether stage add <spec.json>`. The output is a content-addressed, signed, registered stage that future compositions can reference.

## Preconditions

- **Manual path**: a spec file matching `.cli/schemas/stage-spec.json`. Required: `name`, `implementation`. Recommended: `input`, `output`, `effects`, `examples` (≥5), `tags`, `aliases`, `properties`.
- **Agent path**: an LLM configured, and the composition agent determined no existing stage fits (via `noether compose`).
- A local signing key. Auto-generated on first `stage add`; persists at `~/.noether/signing_key`.

## Steps — manual path

1. **Author the spec** (simple form recommended for authoring; `full` form is for importing pre-signed stages):
   ```json
   {
     "name": "price_sum",
     "description": "Sum a list of numeric prices",
     "input": {"List": "Number"},
     "output": "Number",
     "effects": ["Pure"],
     "language": "python",
     "implementation": "def execute(prices):\n    return sum(prices)\n",
     "examples": [
       {"input": [], "output": 0},
       {"input": [1.5], "output": 1.5},
       {"input": [1, 2, 3], "output": 6},
       {"input": [0.1, 0.2], "output": 0.3},
       {"input": [99.99, 0.01], "output": 100.0}
     ],
     "tags": ["scalar", "math"],
     "properties": [
       {"kind": "range", "field": "output", "min": 0}
     ]
   }
   ```
2. **Register**:
   ```bash
   noether stage add spec.json
   ```
   The CLI computes the content hash, signs the id with your key, validates examples against the declared signature, rejects typo'd property kinds (`Property::shadowed_known_kind`), and stores under the local tenant.
3. **Verify**:
   ```bash
   noether stage verify <id> --with-properties
   ```
   Runs every declared example through the executor and checks each property claim. Fails loudly if any example mismatches or a property is violated.
4. **Promote to Active** if it was registered as Draft:
   ```bash
   noether stage activate <id>
   ```
   Store enforces ≤1 Active per `signature_id` and auto-deprecates any previous Active with the same signature.

## Steps — agent path (inside `noether compose`)

The Composition Agent synthesizes when semantic search returns no satisfying stage. The agent:

1. Asks the LLM for a spec (name, input, output, description, implementation, examples, effects inferred).
2. Builds via `StageBuilder::build_signed` with an ephemeral session key.
3. Checks `index.check_duplicate_before_insert` (cosine ≥0.92) to avoid duplicating a near-identical stage; reuses if found signed.
4. Inserts + promotes to Active (auto-deprecates any colliding Active).
5. Returns `SynthesisResult { stage_id, implementation, language, effects, attempts, is_new }`.
6. Composition is retried with the new stage available in the store.

Nothing in the agent path is interactive; the agent is the only caller. To inspect what got synthesized, re-run `compose --verbose`.

## Output shape

`noether stage add`:

```json
{
  "ok": true,
  "result": {
    "id": "<64-hex-sha256>",
    "signature_id": "<64-hex-sha256>",
    "lifecycle": "Draft",
    "validation": {"passed": true, "errors": [], "warnings": []}
  }
}
```

## Failure modes

| Error | Meaning | Remedy |
| --- | --- | --- |
| `too few examples: need at least N, got M` | Spec had <5 examples for a stdlib-signed stage | Add more `examples[]` entries covering edge cases |
| `input type mismatch on example N` | Example input doesn't satisfy declared `input` type | Fix the example or widen the declared type |
| `output type mismatch on example N` | Example output doesn't satisfy declared `output` type | Fix the output or widen the type |
| `property[i]: looks like a \`<kind>\` but failed to deserialise` | Typo'd property kind (e.g. `allowed: ["bolean"]`) | Fix the property — ingest rejects typo'd `Unknown` that shadows a known kind |
| `content hash mismatch` | `id` in spec doesn't match `sha256(signature)` | Either use the simple form (id computed by CLI) or recompute the id |
| `AlreadyExists` → auto-deprecate | Another Active stage has the same `signature_id` | Expected; the old one is now Deprecated with this stage as successor |

## Verification

Minimal end-to-end registration check:

```bash
# Create a tiny spec, register, verify — all local, no network.
cat > /tmp/probe.json <<'JSON'
{"name":"probe_identity","description":"echo input unchanged",
 "input":"Any","output":"Any","effects":["Pure"],"language":"python",
 "implementation":"def execute(x):\n    return x\n",
 "examples":[{"input":1,"output":1},{"input":"a","output":"a"},
             {"input":[1,2],"output":[1,2]},{"input":{"a":1},"output":{"a":1}},
             {"input":null,"output":null}]}
JSON
noether stage add /tmp/probe.json | jq '.result.id'
```

## See also

- [`find-an-existing-stage`](find-an-existing-stage.md) — always check before synthesizing; duplicate effort wastes the content-addressing story.
- [`express-a-property`](express-a-property.md) — make the stage's claims verifiable, not just a description string.
- `crates/noether-core/src/stage/builder.rs` — `StageBuilder` is the canonical in-Rust construction path (`build_stdlib` / `build_signed` / `build_unsigned`).
