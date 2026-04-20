# When things go wrong: reading Noether errors

Noether commands exit with one of four codes, and every failure is a structured ACLI envelope on stdout with a short error line on stderr. This page is the human-paced version of what to do when you see a non-zero exit.

| Exit | Class | Happened at |
|---|---|---|
| `0` | Success | — |
| `1` | Parse / IO / resolution | Before the checker got the graph |
| `2` | Validation / policy | After the checker, before execution |
| `3` | Runtime | During execution |

The short rule: **1 = your file or your store, 2 = your policy or your types, 3 = the stage itself**.

---

## Exit 1 — the graph never reached the executor

The checker didn't get a chance to run. Usually the file didn't parse, or a stage reference didn't resolve.

```
$ noether run graph.json
{ "ok": false, "error": { "code": "…", "message": "stage reference: ambiguous prefix 7b2f" } }
```

Common shapes and what they mean:

| Message fragment | Cause | Remedy |
|---|---|---|
| `failed to read <path>` | The file doesn't exist or isn't readable | Check the path; `ls` the parent directory |
| `invalid graph JSON` | The Lagrange parser rejected the file | Verify every node has an `op` field; every operator's required fields are present |
| `stage reference: not found` | The `id` in the graph doesn't match any store entry | `noether stage list`; if using a remote registry, check `NOETHER_REGISTRY` |
| `stage reference: ambiguous prefix` | A shortened id matches more than one stage | Use a longer prefix (≥12 hex chars) or the full 64-char id |
| `pinning resolution: unknown signature` | `Stage { pinning: Signature }` but no Active stage with that `signature_id` | Activate a Draft with that signature, or change the graph to `Pinning: Both` with a specific implementation id |
| `pinning resolution: multiple Active` | Store violates "≤1 Active per signature" — rare | `noether stage activate` on the intended one auto-deprecates the others |
| `failed to hash composition graph: …` | The graph contains something JCS can't canonicalise. Rare — effectively only triggered by hand-crafted pathological input | Inspect and rebuild the graph |

The `stage reference` errors are by far the most common. When you copy an id from someone else's example and it doesn't resolve, the usual cause is that the stage exists only in their store.

---

## Exit 2 — the graph parsed, but policy or types rejected it

This is the checker doing its job. Nothing ran. The ACLI `error.message` is usually `"<N> violation(s):\n  <detail lines>"`.

### Type errors

```
type check failed:
  input type Record { body: Text, status: Number } is not a subtype of
  declared Record { html: Text } at edge (stage 7b2f → stage a3c9)
```

Read the edge. One side's output doesn't match the other side's input. Either fix the wiring (often a `Let` or `Parallel` can carry the missing field) or broaden a stage signature.

See [concepts](concepts.md#2-types-are-structural-not-nominal) for the subtyping rules.

### Effect policy violations

```
1 effect violation(s):
  composition emits Llm, not in --allow-effects
```

Either allow the effect:

```bash
noether run graph.json --allow-effects pure,fallible,llm,network,cost
```

…or drop the stage that emits it. You can list the stages contributing each effect with `noether run --dry-run graph.json` — the envelope includes the inferred effect breakdown.

### Capability violations

```
1 capability violation(s):
  stage 7b2f9a1c requires capability Network, not in allow list
```

Capabilities are coarser than effects — they're what the sandbox grants. Pass `--allow-capabilities network,fs-read,…` to widen them.

### Signature violations

```
1 signature violation(s):
  stage a3c9… has ed25519_signature but signer_public_key is not trusted
```

Either add the signer's pubkey to the trust store, or rebuild the graph from stages you trust. The stdlib is signed with a deterministic key derived from the Noether version string — it's reproducible from source.

### Cost budget rejected pre-flight

```
composition exceeds cost budget: 12¢ > 5¢
```

Raise `--budget-cents` or compose a cheaper graph. The estimate comes from each stage's declared cost; `Llm` stages carry per-call estimates.

---

## Exit 3 — the graph ran, a stage returned an error

Everything passed pre-flight and the graph actually started. A stage then failed. The error line tells you which and why, the trace tells you what it got as input.

```
{ "ok": false, "error": {
    "code": "…",
    "message": "StageFailed: 7b2f9a1c: http_get: connection refused"
  }
}
```

| Error variant | Cause | Remedy |
|---|---|---|
| `StageFailed: <id>: <stderr>` | User code raised an exception or exited non-zero | Read the stderr fragment; often a missing `# requires:` dependency, an unexpected input shape, or an LLM API error |
| `TimedOut: <id>: <N>s` | Stage exceeded `NixConfig::timeout_secs` (default 30s) | Raise `NOETHER_STAGE_TIMEOUT_SECS`, or profile the stage — 30s is generous for a typed unit |
| `StageNotFound: <id>` | No executor had an implementation | Check the store; for `noether compose` output, synthesis registration should be automatic |
| `cost budget exceeded at runtime: spent N¢ of M¢` | Runtime cost overran | Raise the budget; inspect the trace to see which stage consumed it |

### `noether trace` is the ground truth

```bash
noether trace <composition_id>
```

Every graph execution writes a trace to `~/.noether/traces/`. Each entry has per-stage input, output, duration, observed effects, and (on failure) the error record.

```json
{
  "composition_id": "…",
  "duration_ms": 142,
  "stages": [
    {
      "stage_id": "…",
      "name": "html_to_text",
      "input": { "html": "…" },
      "output": "…",
      "duration_ms": 5,
      "effects_observed": ["Pure"]
    },
    {
      "stage_id": "…",
      "name": "llm_classify",
      "input": { … },
      "error": { "kind": "StageFailed", "message": "API rate limit" },
      "duration_ms": 1200
    }
  ]
}
```

When a user reports "noether did the wrong thing," the trace is the first artifact to ask for.

---

## Isolation-specific failures (v0.7+)

From v0.7, stages run in a bubblewrap sandbox by default. The relevant failure modes:

| Message | Cause | Remedy |
|---|---|---|
| `bubblewrap (bwrap) not found on PATH` | bwrap isn't installed | Install it (`apt install bubblewrap`, `brew install bubblewrap`, or via Nix). `--isolate=auto` falls back to `none` with a warning; `--isolate=bwrap` fails hard |
| `bwrap resolved via $PATH` (warning) | bwrap found outside a system-owned path | Install to `/usr/bin` or a trusted Nix profile. Flags a PATH-planting risk |
| `nix is installed at /usr/bin/nix (outside /nix/store)` | Distro-packaged Nix needs host libs the sandbox can't bind | Install via the Determinate/upstream installer (places `nix` under `/nix/store`), or run with `--isolate=none` |
| `refusing to run without isolation` | `--require-isolation` / `NOETHER_REQUIRE_ISOLATION=1` set, bwrap unavailable | Install bwrap; or drop the flag if the strict requirement doesn't apply |
| Network declared but DNS fails inside the sandbox | `/etc/resolv.conf` / `/etc/hosts` / `/etc/nsswitch.conf` missing on host | The sandbox binds these via `--ro-bind-try` — if they don't exist on the host, DNS won't resolve |

See [SECURITY.md](https://github.com/alpibrusl/noether/blob/main/SECURITY.md) for the full isolation model.

---

## Stage-authoring failures

These come from `noether stage add` rather than `run`:

| Message | Cause | Remedy |
|---|---|---|
| `too few examples` | Spec has fewer examples than required (default ≥3) | Add examples — they're run as the primary validation |
| `input type mismatch on example N` | The example's `input` isn't a value of the declared input type | Either fix the example or loosen the input type |
| `output type mismatch on example N` | The implementation produced something not matching the declared output | The bug is in the implementation; inspect the actual output |
| `property[i] shadowed_known_kind` | A declared property tries to override a known-kind property (e.g. `Pure`) | Either drop the shadowing or rename the property |
| `stage id collision` | A stage with the same `StageId` already exists | Means identical bytes are already stored — often a no-op push, can be ignored |

---

## Diagnosis recipes

### "My graph runs locally but not in CI"

Likely isolation-related. CI containers often lack bwrap. Either install it or set `NOETHER_ISOLATION=none` in the CI env — but prefer fixing CI's isolation stack over widening production's posture.

### "The trace shows a stage succeeded but the output is wrong"

Check the stage's `examples` array in the spec. If the examples don't exercise the edge case you hit, add one covering it, then `noether stage add` the updated spec. The old stage lifecycle auto-deprecates to the new one (same `SignatureId`, new `StageId`).

### "`noether compose` keeps picking the wrong stage"

Run with `--verbose`. The top-20 candidate list shows what the semantic index thought relevant. If the correct stage wasn't in the top 20, its description/examples/tags don't match your problem statement — either refine the problem or improve the stage's searchable metadata.

### "I can't reproduce someone else's trace"

Content addressing guarantees that identical stages + identical graph + identical input produce identical output — **provided the same implementations resolve on both machines**. If the other side is pinning by `SignatureId`, a different Active implementation could have been picked. Ask for their `composition_id` and the full stage IDs used; the trace has both.

---

## What to read next

- **[Concepts](concepts.md)** — the mental model the error messages refer to.
- **[Walkthrough](index.md)** — hands-on examples of the commands covered here.
- **[Agent playbook: debug-a-failed-graph](../agents/debug-a-failed-graph.md)** — the dense reference version of this page, for agents.
