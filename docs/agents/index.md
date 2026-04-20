# For AI Agents — the philosophy

These docs are **not** the first thing a human should read. They're the first thing an **AI agent** should read — dense, keyed by intent, machine-readable. Humans browsing the site see them listed so the format is visible; humans doing real work should start at [Home](../index.md) or [Getting Started](../getting-started/index.md) instead.

## Why two doc sets?

Noether's primary readers are AI agents, not humans. Agents pay per token, read linearly only when they have to, and want "verify with this one-liner" more than they want analogies or motivation. Serving narrative-shaped documentation to a token-constrained agent wastes their context budget; serving schema + error codes + runnable probes to a human makes the project look unapproachable.

Rather than compromise both audiences with one doc set, Noether maintains two:

- **Human-facing**: [`README.md`](https://github.com/alpibrusl/noether/blob/main/README.md), these MkDocs pages (`architecture/`, `getting-started/`, `guides/`, `tutorial/`), `SECURITY.md`, `STABILITY.md`. Narrative, worked examples, diagrams.
- **Agent-facing**: [`AGENTS.md`](https://github.com/alpibrusl/noether/blob/main/AGENTS.md) at the repo root + the playbook fragments in this directory + the `noether agent-docs` CLI subcommand. Dense, keyed by intent, every fragment has a runnable verification step.

The agent docs are the **authoritative reference** for the API surface they cover; human docs link into them for depth. Only one place to maintain a signature, an error code, a failure-mode table — drift is minimized.

## Playbook shape

Every playbook in `docs/agents/` follows a fixed skeleton so an agent knows exactly what each section contains:

```
# Playbook: <key>

## Intent
One sentence. What this playbook enables.

## Preconditions
Bullet list. Environment state required before the steps work.

## Steps
Numbered. Each step includes the exact CLI invocation or API call.

## Output shape
JSON schema fragment. What the agent should expect back.

## Failure modes
Table: error code → cause → remedy. No prose-only explanations.

## Verification
One-liner the agent can run to sanity-check its reading.

## See also
Cross-references to adjacent playbooks.
```

## Accessing playbooks as structured JSON

From the command line (inside a shell, inside an agent that can shell out, inside a CI runner — anywhere with a Noether install):

```bash
# List available playbooks + one-line intents.
noether agent-docs

# Dump a specific playbook as JSON (body, title, intent, key fields).
noether agent-docs compose-a-graph

# Keyword-search across all playbooks.
noether agent-docs --search "deprecation cycle"
```

The playbook markdown files are compiled into the `noether` binary via `include_str!`, so the command works offline and regardless of where the binary was installed. A contract test enforces that every playbook's H1 matches `# Playbook: <key>` so renames can't drift from the CLI key.

## The five current playbooks

| Key | Intent |
|---|---|
| [`compose-a-graph`](compose-a-graph.md) | Translate a natural-language problem description into a valid composition graph using the Composition Agent. |
| [`find-an-existing-stage`](find-an-existing-stage.md) | Find a stage already in the store that matches a signature or intent, instead of synthesizing a new one. |
| [`synthesize-a-new-stage`](synthesize-a-new-stage.md) | Author, sign, validate, and register a new stage when nothing in the catalogue fits. |
| [`express-a-property`](express-a-property.md) | Attach declarative property claims (the seven DSL kinds) so a stage's behaviour is verifiable beyond its type signature. |
| [`debug-a-failed-graph`](debug-a-failed-graph.md) | Interpret Noether CLI failures — exit code, stderr, ACLI envelope — and choose the remediation. |

New playbooks are cheap to add as support questions or adoption friction surface. The format is intentionally terse; most playbooks come in under 600 tokens.

## Related agent resources

- `noether introspect` — full CLI command tree as JSON (ACLI standard). Higher-level than the playbooks; lower detail.
- `noether stage search "<query>"` — semantic-index search over the entire registered stage set. The primary "what does this store contain?" query an agent makes.
- `noether agent-docs --search "<term>"` — keyword search across these playbooks only.
