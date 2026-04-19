# Playbook: express-a-property

## Intent

Attach one or more declarative property claims to a stage so its behaviour is verifiable beyond its type signature. A property is checked against every declared example at registration time and can be re-checked against runtime traces.

## Preconditions

- You're authoring or updating a stage spec. Properties live under the top-level `"properties": []` array.
- You know which of the seven v0.7 property kinds matches your claim (see table below).

## Steps

1. **Pick the right kind** using the table:

   | Kind | Checks | Example |
   | --- | --- | --- |
   | `set_member` | Field value ∈ a fixed set of JSON values | `{"kind":"set_member","field":"output.severity","set":["LOW","HIGH"]}` |
   | `range` | Numeric field ∈ `[min, max]` (either optional) | `{"kind":"range","field":"output","min":0,"max":100}` |
   | `field_length_eq` | Two fields have equal length (string UTF-8 codepoints / array / object keys) | `{"kind":"field_length_eq","left_field":"output","right_field":"input"}` |
   | `field_length_max` | `subject_field` length ≤ `bound_field` length | `{"kind":"field_length_max","subject_field":"output","bound_field":"input.items"}` |
   | `subset_of` | Elements/keys/substring of `subject_field` appear in `super_field` | `{"kind":"subset_of","subject_field":"output.keys","super_field":"input.keys"}` |
   | `equals` | Two fields are JSON-value equal (identity, content preservation) | `{"kind":"equals","left_field":"output","right_field":"input"}` |
   | `field_type_in` | Runtime JSON type at `field` is one of the allowed kinds | `{"kind":"field_type_in","field":"output.x","allowed":["number","null"]}` |

2. **Write the path.** Properties navigate into either `input` or `output` — the first segment of `field` / `left_field` / `subject_field` / etc. must be exactly `input` or `output`, then dot-separated keys into the JSON value.

3. **Use `field_type_in` allowed kinds from the exact six**: `null`, `bool`, `number`, `string`, `array`, `object`. Anything else deserialises as `Property::Unknown` and `noether stage add` rejects it with a `shadowed_known_kind` error pointing at the typo.

4. **Validate at registration**:
   ```bash
   noether stage add spec.json          # ingest rejects unsatisfiable / typo'd properties
   noether stage verify <id> --with-properties   # re-run properties against every example
   ```

5. **Re-check against runtime traces** (optional but encouraged in CI):
   ```bash
   noether trace <composition_id>       # get the trace
   # Each stage invocation in the trace records (input, output); re-run properties against
   # those pairs to catch behavioural drift from the example-only check.
   ```

## Output shape

A property stored on a stage:

```json
{
  "properties": [
    {"kind": "range", "field": "output.soc_percent", "min": 0, "max": 100},
    {"kind": "field_length_eq", "left_field": "output", "right_field": "input"}
  ]
}
```

Violation (on `stage verify`):

```json
{
  "example_index": 2,
  "violation": {
    "kind": "out_of_range",
    "path": "output.soc_percent",
    "actual": 150.0,
    "bounds": {"min": 0, "max": 100}
  }
}
```

## Failure modes

| Symptom | Meaning | Remedy |
| --- | --- | --- |
| `property[i]: looks like a <kind> but failed to deserialise` | Typo inside a known kind's fields (most common: `allowed: ["bolean"]` on `field_type_in`) | Fix the typo — ingest correctly refuses to silently drop the check |
| `BadPath: <path>` | Path doesn't start with `input`/`output`, or dot-navigation fails mid-way | Rewrite the path; test with a tiny synthetic example first |
| `NotMeasurable` on length checks | Applied `field_length_eq`/`field_length_max` to a numeric/bool/null field | Length is only defined for string (codepoints), array (elements), object (keys). Use `equals` or `range` instead |
| `NotNumber` on `range` | Field resolved to a non-numeric value in at least one example | Either fix the example or widen the property to cover `Any` via `field_type_in` + a follow-up check |
| Property passes at ingest but violates at runtime | Examples were narrower than production traffic | Add more examples, or add runtime re-check via `noether trace` |

## Length semantics — exact contract

- **String**: UTF-8 **code-point** count (`str::chars().count()`), not bytes, not grapheme clusters. `"a̐"` → 2 codepoints; `"👨‍👩‍👧"` → 5 codepoints (3 emoji + 2 ZWJs).
- **Array**: element count.
- **Object**: key count.
- **Number / bool / null**: not measurable; property fails with `NotMeasurable`.

Cross-kind comparisons are mechanically defined (`field_length_eq { left: array, right: string }` compares element-count against codepoint-count) but almost never what an author means. Prefer paths of the same JSON kind.

## SubsetOf — branch-by-kind semantics

| Subject kind | Super kind | Semantics |
| --- | --- | --- |
| Array | Array | Every element of subject appears in super by JSON-value equality; duplicates allowed |
| Object | Object | Every `(key, value)` of subject appears in super; key-presence alone is NOT enough |
| String | String | **Substring** containment (contiguous), not a character-set subset. `"abc" ⊄ "bac"` even though all chars match |
| Any mixed pair | | `NotCollectionForSubset` violation (blame the non-collection side) |

## Verification

Minimal probe — a stage whose property claim is intentionally violated at example 2, should fail ingest cleanly:

```bash
cat > /tmp/bad_prop.json <<'JSON'
{"name":"clamp_soc","description":"clamp a battery SOC to [0,100]",
 "input":"Number","output":"Number","effects":["Pure"],"language":"python",
 "implementation":"def execute(x):\n    return x\n",
 "examples":[{"input":0,"output":0},{"input":150,"output":150},
             {"input":50,"output":50},{"input":100,"output":100},{"input":25,"output":25}],
 "properties":[{"kind":"range","field":"output","min":0,"max":100}]}
JSON
noether stage add /tmp/bad_prop.json   # expect failure citing example 2
```

## See also

- [`synthesize-a-new-stage`](synthesize-a-new-stage.md) — the spec context properties live inside.
- `crates/noether-core/src/stage/property.rs` — authoritative source for variants, evaluator, and violations.
- [`STABILITY.md`](../../STABILITY.md) — property set is additive in 1.x; existing kinds cannot be removed, but new kinds may appear in `Unknown` when read by an older binary.
