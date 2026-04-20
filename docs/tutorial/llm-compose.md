# Compose with an LLM: from problem to running graph

The [walkthrough](index.md) shows you how to author stages and graphs by hand. This page is the other mode: describe the problem in English, let the LLM assemble a graph from existing stages (and, if needed, synthesise new ones), have the type checker verify it, and run it. Everything the LLM produces is a regular Noether graph — there's no shortcut around type checking, signing, or effect policy.

!!! warning "You need an LLM provider"
    This flow requires credentials for one of the supported providers. `noether compose` picks them up from env in this priority order: `VERTEX_AI_*` > `ANTHROPIC_API_KEY` > `OPENAI_API_KEY` > `MISTRAL_API_KEY`. If none are set, `compose` exits with a clear provider-missing error.

---

## The shape of the command

```
noether compose [FLAGS] "<problem description>"
```

Key flags:

| Flag | What it does |
|---|---|
| `--dry-run` | Produce the graph, type-check it, skip execution. Use this first. |
| `--input '<json>'` | Pipeline input. Alternatively pipe JSON on stdin. |
| `--model <name>` | Pin the LLM (default: `VERTEX_AI_MODEL` env → `gemini-2.5-flash`). |
| `--verbose` | Show the candidate list, prompt, and each attempt's raw response. |
| `--force` | Ignore the cached composition and call the LLM again. |
| `--budget-cents N` | Reject graphs whose estimated cost exceeds N cents. |
| `--allow-effects <list>` | Restrict the effect closure. Default: all allowed. |

---

## Step 1 — start with `--dry-run`

Never run a first-draft composition without looking at it.

```bash
noether compose --dry-run \
  "count how many times the word 'rust' appears in a text" \
  --input '{"text": "rust rust rust"}'
```

Output (abbreviated):

```json
{
  "ok": true,
  "command": "compose",
  "result": {
    "composition_id": "8f3a…",
    "attempts": 1,
    "from_cache": false,
    "synthesized": [],
    "graph": {
      "description": "count occurrences of 'rust'",
      "version": "0.1.0",
      "root": {
        "op": "Stage",
        "id": "count_substring_signature_id_hex"
      }
    },
    "output": null,
    "trace": null,
    "warnings": []
  },
  "meta": { "version": "0.7.1", "duration_ms": 842 }
}
```

Read `result.graph.root`. That's the graph the agent picked. If it's wrong — wrong stage, wrong wiring, wrong operator — re-run with a sharper problem statement before burning a real execution.

`composition_id` is computed on the pre-resolution graph. The same `problem` against the same stdlib produces the same id; changes to which implementation is Active don't perturb it.

---

## Step 2 — understand what synthesis means

If the agent can't find an existing stage that does the job, it **synthesises** a new one — writes Python, type-checks the signature, runs the declared examples through `noether stage test`, and registers the result. The `synthesized` array in the envelope tells you what was created:

```json
"synthesized": [
  {
    "stage_id": "c41f2…",
    "name": "weighted_mean",
    "language": "python",
    "attempts": 2,
    "is_new": true
  }
]
```

That stage is now in your store. The next composition with the same need hits it via search and doesn't call the LLM. You can inspect it:

```bash
noether stage get c41f2
noether stage test c41f2        # re-run the declared examples
noether stage verify c41f2 --properties   # check any declared properties hold
```

Synthesised stages are signed with your local signing key (generated on first run at `~/.noether/signing_key`). Their lifecycle starts at `Draft`; promote to `Active` with `noether stage activate <id>` when you're confident.

---

## Step 3 — guard with budget and effects

Once the dry-run looks right, run for real. Two guards are worth setting up front:

```bash
noether compose \
  --budget-cents 5 \
  --allow-effects pure,fallible,network,llm,cost \
  "pull the H1 titles off example.com and classify them" \
  --input '{"url": "https://example.com"}'
```

- `--budget-cents 5` rejects any composition whose pre-flight cost estimate exceeds 5¢. Costs are declared on each stage in the spec; `Llm`/`Cost`-tagged stages carry per-call estimates.
- `--allow-effects` is the effect closure. If the graph needs `Network` but you only passed `pure,llm`, the run fails with exit 2, before any execution.

Both guards apply at pre-flight and at runtime. If an LLM call mid-graph tips over `--budget-cents`, the executor halts with `cost budget exceeded at runtime`.

---

## Step 4 — trace the run

Every real execution writes a trace to `~/.noether/traces/<composition_id>.json`:

```bash
noether trace 8f3a…
```

Output includes per-stage input, output, duration, and effects observed. When a synthesised stage behaves unexpectedly, the trace is the first thing to read — it's the ground truth, stderr is a summary.

---

## When composition fails

`noether compose` returns a non-zero exit on three distinct failure modes. Each has a different remedy:

| Exit | Typical message | What went wrong | Remedy |
|---|---|---|---|
| 2 | `composition failed: …` | LLM couldn't produce a type-checkable graph in 3 attempts | Re-run with `--verbose`; refine the problem statement; check that the stdlib actually has stages close to what you're asking for |
| 2 | `X effect violation(s)` | Graph's effect closure includes something `--allow-effects` forbids | Either allow the effect or ask the LLM for a simpler approach |
| 2 | `composition exceeds cost budget` | Pre-flight estimate exceeds `--budget-cents` | Raise the budget or accept a cheaper graph |
| 3 | `execution failed: …` | Graph type-checked and ran, but a stage returned an error | Read the trace; often a missing provider credential or an input the stage didn't anticipate |

The detailed exit-code contract lives in [when things go wrong](when-things-go-wrong.md).

---

## Reading the verbose transcript

`--verbose` exposes three phases the agent goes through:

1. **Semantic search** — the top-20 stage candidates for the problem. If none of them look close to what you meant, the LLM will have a harder time.
2. **Prompt** — the exact prompt sent to the LLM. Includes candidate list, type system description, operator reference.
3. **Response(s)** — each attempt's raw response and the type-check result. On failure, the next attempt carries the error back to the LLM.

```bash
noether compose --verbose --dry-run "<problem>" 2>&1 | less
```

If you see the LLM confidently emitting a graph that references stage names that *aren't* in the candidate list, you've found an improvement opportunity: either add a stage to the stdlib that covers the missing capability, or refine the problem to stay within what's indexed.

---

## Caching

`noether compose` keeps a per-problem cache at `~/.noether/compose_cache.json`. A repeat call with the same problem + same input shape + same stdlib state returns the cached graph without calling the LLM. The `from_cache: true` field in the envelope tells you when that happened. `--force` bypasses the cache.

Cache invalidates when the stdlib changes (new stages could have changed the agent's choice). It does not invalidate on unrelated store activity.

---

## What to read next

- **[Walkthrough — citecheck as stages](index.md)** — the manual-authoring path, for when `compose` isn't appropriate (e.g., you need specific stages the LLM wouldn't pick).
- **[When things go wrong](when-things-go-wrong.md)** — reading errors from both `compose` and `run`.
- **[Agent playbook: compose-a-graph](../agents/compose-a-graph.md)** — the dense reference version of this page, intended for agents calling `noether compose` from another tool.
