# Stage Identity

Every stage in Noether has a **content-addressed identity**: a SHA-256 hash derived
entirely from the stage's behaviour specification, not from its name or author.

This is the most important design decision in the system.

---

## How a stage ID is computed

A `StageId` is the hex-encoded SHA-256 of the canonical JSON serialisation of the
stage's `StageSignature`:

```
StageId = SHA-256(canonical_json(StageSignature))

StageSignature = {
    input:               NType,   // structural input type
    output:              NType,   // structural output type
    effects:             BTreeSet<Effect>,
    implementation_hash: String   // SHA-256 of the implementation code
}
```

The canonical JSON uses `BTreeMap`/`BTreeSet` everywhere — keys are sorted,
output is deterministic across all platforms and compiler versions.

The Rust code:

```rust
pub fn compute_stage_id(sig: &StageSignature) -> Result<StageId, _> {
    let json = serde_json::to_string(sig)?;       // BTreeMap → sorted keys
    let hash = Sha256::digest(json.as_bytes());
    Ok(StageId(hex::encode(hash)))
}
```

---

## What this means

### Names are metadata, not identity

```bash
# Two stages with different descriptions but identical signatures get the same ID.
# Renaming a stage does not change its ID.
noether stage get 39731ebb
# "description": "Make an HTTP GET request"
# id: 39731ebb   ← determined by types + effects + impl_hash, not the name
```

### Changing behaviour changes the ID

If you change a stage's input type, output type, effects, or implementation, the ID
changes.  Any composition graph referencing the old ID will fail to resolve — an
**explicit, auditable break** rather than a silent regression.

### Composition graphs are content-addressed too

A `CompositionGraph` gets a SHA-256 ID from its serialised root node.  Running
`noether run graph.json` on two different machines with the same graph file produces
the same `composition_id` and therefore comparable traces.

---

## Ed25519 signatures

A stage can optionally carry an Ed25519 signature:

```
stage.ed25519_signature = sign(stage.id.bytes, signing_key)
```

The signature binds an **author keypair** to a **specific content hash**.
It does not sign the description or metadata — those can change without invalidating
the signature.

Stdlib stages are signed with a deterministic key derived from:

```rust
let seed = SHA-256(b"noether-stdlib-signing-key-v0.1.0");
let key  = Ed25519SigningKey::from_bytes(&seed);
```

This key is reproducible from the source code — anyone can verify stdlib signatures
without a certificate authority.

---

## Lifecycle

A stage progresses through four states:

```
Draft → Active → Deprecated { successor_id } → Tombstone
```

| State | Meaning |
|---|---|
| `Draft` | Submitted, not yet promoted. Visible to direct ID lookup but not in search. |
| `Active` | In production. Returned by `stage list` and included in the semantic index. |
| `Deprecated` | Superseded by `successor_id`. Still executable; search de-ranks it. |
| `Tombstone` | Removed from the semantic index. Still retrievable by ID (history is immutable). |

Lifecycle transitions are enforced:

- `Draft → Active` ✓
- `Active → Deprecated` ✓ (requires `successor_id` pointing to an existing stage)
- `Active → Tombstone` ✓
- `Tombstone → anything` ✗ (terminal state)
- `Draft → Tombstone` ✗ (must go through Active first)

---

## Why not use names?

| Name-based systems | Content-addressed (Noether) |
|---|---|
| `sort_list v1.2.3` can silently change | `6aae3697` always means the same thing |
| Version ranges introduce ambiguity | An ID either resolves or doesn't |
| Yanked packages leave broken deps | Tombstoned stages still resolve (just deprecated) |
| Two packages with the same name conflict | Two stages with the same signature are the same stage |
| Registry required for resolution | Any peer with the stage bytes can verify |

Content addressing is borrowed from Git, Nix, and IPFS.  Noether applies the same
principle to typed, composable computational units.

---

## Verifying a stage

```bash
# Fetch a stage and verify its content hash
noether stage get 8dfa010b

# The registry's POST /stages endpoint runs this automatically:
# 1. Recompute SHA-256(canonical_json(stage.signature))
# 2. Assert it equals stage.id
# 3. Verify Ed25519 signature if present
# This check is itself a Noether stage: f608988c
```

---

## Signature Identity

In addition to the full `StageId` (which includes the `implementation_hash`), each stage
has a **signature identity** that captures *what* the stage does without regard to *how*:

```
signature_id = SHA-256(name + input + output + effects)
```

The signature ID is used for **versioning**: only one Active version of a stage may exist
per `signature_id` at any time. When a new version of a stage with the same signature ID
is registered via `noether stage add`, the system auto-deprecates the previous Active
version and sets its `successor_id` to the new stage.

This means:

- **Same interface, new implementation** produces a new `StageId` but the same `signature_id`.
- The old version is automatically deprecated with a pointer to the new one.
- Composition graphs referencing the old `StageId` still resolve (deprecated stages remain
  executable) but agents are guided toward the successor via search ranking.

Per [`STABILITY.md`](../../STABILITY.md), `signature_id` is **stable across the 1.x
line**: a bugfix that changes `implementation_hash` changes `StageId` but never
`signature_id`. This is the identity that graphs should pin by default, so they
pick up implementation fixes automatically.

> **Naming note.** Prior to v0.6.0 this field was called `canonical_id` and the
> type was `CanonicalId`. Both the old name (as a JSON field alias and a
> deprecated type alias) and the new one are accepted in v0.6.x; the old
> names are removed in v0.7.0.
