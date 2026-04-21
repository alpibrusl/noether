# Playbook: debug-a-failed-graph

## Intent

Interpret a Noether CLI failure — exit code, stderr shape, ACLI envelope — and decide the remediation. Noether exits consistently: `0` = success, `1` = parse/IO/resolution error, `2` = policy / validation / capability / effect violation, `3` = runtime execution error. Every failure is a structured ACLI envelope, not free-form text.

## Preconditions

- Access to the stderr + stdout of the failing invocation.
- Ability to re-run with `--dry-run` (plan without executing) and/or `--verbose` (show composition reasoning).
- If a trace file exists (`~/.noether/traces/<composition_id>.json`), you can inspect it with `noether trace <id>`.

## Steps — by exit code

### Exit 1 — graph didn't reach the executor

Common shapes:

| ACLI `error.code` / message fragment | Cause | Remedy |
| --- | --- | --- |
| `invalid graph JSON` | Lagrange parser rejected the file | Check the `op` discriminator on every node; `CompositionNode` is tagged by `op` (serde `tag = "op"`) |
| `stage reference: not found` | A stage id in the graph doesn't match any store entry | `noether stage list` to confirm; check tenant scoping if using a remote registry |
| `stage reference: ambiguous prefix` | Shortened id matches >1 stage | Use ≥12 hex chars or the full 64-char id |
| `pinning resolution: unknown signature` | `Stage { pinning: Signature, id: <sig> }` but no Active stage has that `signature_id` | List candidates with `noether stage list | jq '.result[] | .signature_id'`; activate a Draft with that signature |
| `pinning resolution: multiple Active` | Store violates the ≤1-Active-per-signature invariant | Rare — only possible via direct SQL. Fix via `noether stage activate` on the intended one (auto-deprecates the others) |
| `deprecation cycle detected at stage <id>` | Corrupt deprecation chain in the store | Repair the store: `UPDATE stages SET lifecycle = ...` or re-push clean data |
| `deprecation chain exceeded 10 hops` | Chain is longer than the cap | Flatten in the store; run the resolver on a clean snapshot |

### Exit 2 — validation / policy / budget rejected the graph

All exit-2 failures happen before execution. Read the ACLI `error.message` field — the format is `"<N> violation(s):\n  <detail>"`.

| Category | Common violations | Remedy |
| --- | --- | --- |
| Type check | `input type X is not subtype of declared Y at edge (A → B)` | Either fix the graph wiring (the output of A doesn't match B's input) or broaden the stage signature |
| Capability | `stage X requires capability Network, not in allow list` | `--allow-capabilities network,fs-read,...` or drop the capability-requiring stage |
| Effect | `composition emits Llm, not in --allow-effects` | Add to allow list or use a different stage |
| Signature | `stage X has ed25519_signature but signer_public_key is not trusted` | Add the signer's pubkey to the trust store, or skip verification with `--no-verify-signatures` (not recommended) |
| Cost budget | `composition exceeds cost budget: <est>¢ > <budget>¢` | Raise `--budget-cents` or compose a cheaper graph |
| Stage validation | `too few examples`, `input type mismatch on example N`, `property[i] shadowed_known_kind` | See `synthesize-a-new-stage` and `express-a-property` playbooks |

### Exit 3 — runtime failure

Execution passed all pre-flight checks and the graph actually ran, but a stage returned an error.

| Error code | Cause | Remedy |
| --- | --- | --- |
| `StageFailed: <id>: <stderr>` | User code raised an exception / exited non-zero | Read the stderr fragment; often a dependency missing (`# requires:` not installed), a malformed input the stage didn't anticipate, or an LLM API error |
| `TimedOut: <id>: <secs>s` | Stage exceeded `NixConfig::timeout_secs` (default 30s) | Raise `NOETHER_STAGE_TIMEOUT_SECS`; profile the stage for O(n²) hotspots |
| `StageNotFound: <id>` | No executor had an implementation — synthesized stages must be registered to Nix/Inline before dispatch | Check that `CompositeExecutor::register_synthesized` was called (happens automatically in `compose`); for direct `run`, verify the stage has `implementation_code` set |
| `cost budget exceeded at runtime: spent N¢ of M¢` | Actual cost overran the budget mid-execution | Raise budget; audit which stage consumed it via `noether trace <id>` |
| `pinning resolution: <e>` (rare at exit 3) | Deprecation chain changed between plan and execute | Re-run — the resolver is idempotent against a stable store |

## Structured diagnosis with `noether trace`

Every graph execution writes a trace to `~/.noether/traces/<composition_id>.json`:

```bash
noether trace <composition_id>
```

Output (abbreviated):

```json
{
  "composition_id": "...",
  "duration_ms": 142,
  "stages": [
    {
      "stage_id": "...",
      "name": "word_count",
      "input": {"text": "hello world"},
      "output": {"count": 2},
      "duration_ms": 5,
      "effects_observed": ["Pure"]
    },
    {
      "stage_id": "...",
      "name": "llm_classify",
      "input": {...},
      "error": {"kind": "StageFailed", "message": "API rate limit"},
      "duration_ms": 1200
    }
  ]
}
```

The trace is the ground truth — stderr is a summary.

## Isolation-specific failures (Phase 1 — v0.7.x bwrap)

Noether's isolation layer ships in two phases:

- **Phase 1 (v0.7.x, shipped)** — bubblewrap as a subprocess wrapper. Fresh namespaces, UID-mapped to `nobody`, cap-drop ALL, sandbox-private `/work` tmpfs, network unshared unless declared. Good enough for LLM-synthesized code you haven't audited; not a hardened multi-tenant boundary.
- **Phase 2 (v0.8, roadmapped)** — replace the bwrap subprocess with in-process `unshare` + Landlock + seccomp. Same `IsolationPolicy` surface, ~10× lower startup, finer-grained syscall control. See issue [#44](https://github.com/alpibrusl/noether/issues/44) for the compliance-matrix work landing alongside.

Failure modes below apply to Phase 1. With `--isolate=bwrap`:

| Message | Cause | Remedy |
| --- | --- | --- |
| `bubblewrap (bwrap) not found on PATH` | Missing bwrap | Install `bubblewrap` (apt/brew/nix) |
| `bwrap resolved via $PATH` (warning) | bwrap found outside trusted system paths | Install to `/usr/bin` or a system nix profile; indicates potential PATH-planting risk |
| `nix is installed at /usr/bin/nix (outside /nix/store)` | Distro-packaged nix needs host libs the sandbox doesn't bind | Install via Determinate/upstream installer (places nix under /nix/store), or run with `--isolate=none` |
| `refusing to run without isolation` | `--require-isolation` set and bwrap unavailable | Install bwrap; or drop the flag if CI-requirement doesn't apply |
| Stage runs but can't resolve DNS with `network=true` effect | NSS config missing inside sandbox | Check `/etc/resolv.conf` / `/etc/hosts` / `/etc/nsswitch.conf` exist on host (they're `--ro-bind-try`d when network is declared) |

## Verification — make a failure happen on purpose

```bash
# Type error the checker should catch:
echo '{"description":"bad","version":"0.1.0",
       "root":{"op":"Stage","id":"0000000000000000000000000000000000000000000000000000000000000000","pinning":{"kind":"both"}}}' > /tmp/bad.json
noether run /tmp/bad.json   # expect exit 1 with stage-not-found
```

## See also

- [`STABILITY.md`](../../STABILITY.md) — which error codes are part of the 1.x contract and which may be renamed.
- [`SECURITY.md`](../../SECURITY.md) — isolation failure modes in more detail.
- `crates/noether-engine/src/executor/mod.rs` — `ExecutionError` enum is the authoritative source for exit-3 variants.
