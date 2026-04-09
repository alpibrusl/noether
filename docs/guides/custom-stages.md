# Building Custom Stages

A **stage** is the fundamental unit of computation in Noether.  Every stage has:

- A content-addressed ID (SHA-256 of its signature)
- A structural input and output type (`NType`)
- An effect declaration (`Pure`, `Network`, `Fallible`, `Llm`, ...)
- At least one example (input → output pair)
- An optional Ed25519 signature

This guide shows three paths to a new stage: using `StageBuilder` in Rust (for
stdlib contributions), using the CLI (for quick custom stages), and publishing to
a noether-cloud registry.

---

## Path 1: StageBuilder (Rust, for stdlib)

Add a stage to the stdlib by creating a `StageBuilder` in `noether-core`:

```rust
use noether_core::stage::{Stage, StageBuilder};
use noether_core::types::NType;
use ed25519_dalek::SigningKey;
use serde_json::json;

pub fn my_stage(key: &SigningKey) -> Stage {
    StageBuilder::new("my_stage_name")
        .description("Convert a temperature in Celsius to Fahrenheit")
        .input(NType::record([("celsius", NType::Number)]))
        .output(NType::record([("fahrenheit", NType::Number)]))
        .pure()                                         // no side-effects
        .example(
            json!({"celsius": 0.0}),
            json!({"fahrenheit": 32.0}),
        )
        .example(
            json!({"celsius": 100.0}),
            json!({"fahrenheit": 212.0}),
        )
        .example(
            json!({"celsius": -40.0}),
            json!({"fahrenheit": -40.0}),
        )
        .example(
            json!({"celsius": 20.0}),
            json!({"fahrenheit": 68.0}),
        )
        .example(
            json!({"celsius": 37.0}),
            json!({"fahrenheit": 98.6}),
        )
        .build_stdlib(key)           // signs with the stdlib Ed25519 key
        .expect("valid stage")
}
```

`build_stdlib` requires exactly **5 examples** (for semantic search quality) and
computes the `implementation_hash` from `"noether-stdlib-v0.1.0:{name}"`.

### Wire in the implementation

Add the Rust function in `noether-engine/src/executor/stages/`:

```rust
// crates/noether-engine/src/executor/stages/scalar.rs
pub fn celsius_to_fahrenheit(input: &Value) -> Result<Value, ExecutionError> {
    let c = input["celsius"].as_f64().ok_or_else(|| ExecutionError::StageFailed {
        stage_id: StageId("celsius_to_fahrenheit".into()),
        message: "celsius must be a number".into(),
    })?;
    Ok(serde_json::json!({"fahrenheit": c * 9.0 / 5.0 + 32.0}))
}
```

Then register it in `find_implementation`:

```rust
// crates/noether-engine/src/executor/stages/mod.rs
"Convert a temperature in Celsius to Fahrenheit" => {
    Some(scalar::celsius_to_fahrenheit)
}
```

The description string must match exactly between the `StageBuilder` and the match arm.

### The content hash is computed automatically

```rust
let stage = my_stage(&stdlib_signing_key());
println!("{}", stage.id.0);  // deterministic SHA-256 hash
```

The ID never changes as long as the signature (types + effects + implementation_hash)
does not change.

---

## Path 2: JSON stage spec (for custom stages via CLI)

You don't need to modify the Noether codebase for custom stages.
Create a stage spec as JSON and submit it:

```json
{
  "description": "Convert a temperature in Celsius to Fahrenheit",
  "signature": {
    "input": {"Record": {"celsius": "Number"}},
    "output": {"Record": {"fahrenheit": "Number"}},
    "effects": ["Pure"],
    "implementation_hash": "sha256_of_your_implementation_code"
  },
  "examples": [
    {"input": {"celsius": 0},   "output": {"fahrenheit": 32}},
    {"input": {"celsius": 100}, "output": {"fahrenheit": 212}},
    {"input": {"celsius": -40}, "output": {"fahrenheit": -40}},
    {"input": {"celsius": 20},  "output": {"fahrenheit": 68}},
    {"input": {"celsius": 37},  "output": {"fahrenheit": 98.6}}
  ],
  "lifecycle": "Draft"
}
```

Submit to a running registry:

```bash
curl -X POST http://localhost:8080/stages \
  -H "Content-Type: application/json" \
  -d @my-stage.json
```

The registry validates the content hash, optional signature, and description,
then returns the assigned `StageId`.

---

## Path 3: LLM-generated stage

Use `noether compose` to let the LLM draft a composition that uses your new stage:

```bash
noether compose "convert temperature readings from a CSV file from Celsius to Fahrenheit and write the result"
```

The LLM searches the semantic index, finds your stage (if published), and wires it
into a graph.

---

## Type system quick reference

> **Simplified syntax (v0.6.0+):** Stage spec files accept both the canonical
> format (`{"kind":"Text"}`) and a simplified shorthand. In the shorthand,
> primitive types are plain strings (`"Text"`, `"Number"`, `"Bool"`) and records
> use a compact tuple-list notation. The `normalize_type` function converts
> either format to the canonical `NType` representation.
>
> ```json
> // Simplified (accepted in stage spec JSON files)
> {
>   "input": {"Record": [["celsius", "Number"]]},
>   "output": {"Record": [["fahrenheit", "Number"]]}
> }
>
> // Canonical (always accepted)
> {
>   "input": {"Record": {"celsius": {"kind": "Number"}}},
>   "output": {"Record": {"fahrenheit": {"kind": "Number"}}}
> }
> ```

```
NType::Text                          — UTF-8 string
NType::Number                        — IEEE 754 f64
NType::Bool                          — true / false
NType::Null                          — JSON null
NType::Bytes                         — raw bytes
NType::Any                           — escape hatch (bidirectional compatible)
NType::List(Box<NType>)              — homogeneous list
NType::Map { key, value }            — homogeneous map
NType::Record(BTreeMap<String, NType>) — named fields (structural)
NType::Union(BTreeSet<NType>)        — disjoint union (use NType::union() constructor)

// Helpers
NType::optional(t)  →  Union { t, Null }
NType::record([("field", NType::Text), ...])
```

### Subtyping rules

- `Record{a, b, c}` is subtype of `Record{a, b}` — width subtyping
- `Text` is subtype of `Text | Null` — union member
- `Any` is compatible with everything — both directions
- `List<Text>` is subtype of `List<Any>` — covariance

---

## Effect declarations

```rust
.pure()                          // shorthand for EffectSet::pure()
.effects(EffectSet::new([
    Effect::Pure,
    Effect::Network,
    Effect::Fallible,
    Effect::Llm,
    Effect::FileSystem,
]))
```

Effects are declared but not enforced in v1 — they inform agents about what a stage
does and allow smarter composition decisions.

---

## Signing a stage

Signing binds an Ed25519 keypair to the stage's content hash:

```rust
use noether_core::stage::{sign_stage_id, verify_stage_signature};

// Sign
let sig_hex = sign_stage_id(&stage.id, &my_signing_key);
stage.ed25519_signature = Some(sig_hex);
stage.signer_public_key = Some(hex::encode(my_signing_key.verifying_key().to_bytes()));

// Verify
let valid = verify_stage_signature(&stage.id, &sig_hex, &pub_hex)?;
```

The stdlib signing key is deterministically derived from a fixed seed —
see `noether_core::stdlib::stdlib_signing_key()`.

Unsigned stages are accepted by the registry (with a warning) but cannot be
promoted to `Active` without a valid signature in production deployments.
