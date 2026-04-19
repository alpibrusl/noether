# Playbook: compose-a-graph

## Intent

Translate a natural-language problem description into a valid, type-checked, executable composition graph using the Composition Agent.

## Preconditions

- At least one LLM provider is configured via env. Noether auto-selects in this order: `VERTEX_AI_*` > `ANTHROPIC_API_KEY` > `OPENAI_API_KEY` > `MISTRAL_API_KEY`.
- The stdlib is registered. `noether stage list` should return ≥70 stages. If empty, reload with `noether stage sync <dir>` (or wait for the first-run loader).
- The semantic index has been built (happens automatically on first agent invocation).

## Steps

1. **Invoke the agent.** Agent searches the top-20 semantic candidates, builds a prompt, calls the LLM, parses the response as `CompositionGraph`, type-checks it, retries up to 3 times on failure.
   ```bash
   noether compose "extract the price from each line item and sum the total"
   ```
2. **Supply input.** Use `--input '<json>'` for structured input or pipe JSON on stdin.
3. **Dry-run before executing.** `--dry-run` produces the graph + plan without running:
   ```bash
   noether compose --dry-run "<problem>" --input '{"text": "..."}'
   ```
4. **Pin model explicitly** when agent-to-agent consistency matters: `--model gemini-2.0-flash` / `--model claude-sonnet-4-6` / etc.
5. **If synthesis happens**, the response includes `synthesized: [{ stage_id, language, attempts, is_new }]`. The new stage is registered in the store under your signing key and persists for future compositions.

## Output shape

ACLI envelope:

```json
{
  "ok": true,
  "command": "compose",
  "result": {
    "composition_id": "<sha256>",
    "attempts": 1,
    "from_cache": false,
    "synthesized": [],
    "graph": { "description": "...", "root": {"op": "Sequential", "stages": [...]}, "version": "0.1.0" },
    "output": <stage output>,
    "trace": { "duration_ms": 42, "stages": [...] },
    "warnings": []
  },
  "meta": { "version": "0.7.1", "duration_ms": 42 }
}
```

`composition_id` is computed on the **pre-resolution** graph (M1 identity contract). The same `problem` + same stdlib state produces the same id; changes to Active implementations do not perturb it.

## Failure modes

| Error code | Meaning | Remedy |
| --- | --- | --- |
| `composition failed: ...` (exit 2) | LLM couldn't produce a type-checkable graph in `max_retries` attempts (default 3) | Refine the problem statement; add `--verbose` to see the LLM transcripts |
| `X capability violation(s)` (exit 2) | Composed graph needs a capability you didn't grant | Pass `--allow-capabilities network,fs-read,...` |
| `X effect violation(s)` (exit 2) | Composed graph has an effect the policy forbids | Pass `--allow-effects network,llm,...` |
| `composition exceeds cost budget` (exit 2) | `--budget-cents N` set and estimate exceeds it | Raise the budget or accept the cheaper composition |
| `execution failed: ...` (exit 3) | Type-check passed, but runtime error | Read the trace; often a missing LLM key or a stage that panicked on real input |

## Verification

Minimal runnable probe that exercises the full path:

```bash
noether compose --dry-run "count the words in a sentence" --input '{"text": "hello world"}'
```

Expect `ok: true`, `graph.root.op` to be `Stage` or `Sequential`, and `type_check.input/output` to be consistent with the stage signature.

## See also

- [`find-an-existing-stage`](find-an-existing-stage.md) — if you want to bypass the LLM and assemble a graph manually.
- [`synthesize-a-new-stage`](synthesize-a-new-stage.md) — what happens inside the agent when no existing stage fits.
- [`debug-a-failed-graph`](debug-a-failed-graph.md) — how to interpret the common failure modes above.
