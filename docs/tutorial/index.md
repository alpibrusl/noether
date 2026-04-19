# Tutorial: turn `citecheck` into verified Noether stages

In the [ACLI tutorial](https://alpibrusl.github.io/acli/tutorial/) we built `citecheck`, a CLI that verifies citations in Markdown. The logic was straightforward but monolithic: one Python file, `verify()` calls `_fetch()` calls `_extract_text()` calls `_contains_claim()`.

That's fine for a CLI. But the moment you want to:

- **Reuse** `_fetch` from another pipeline without importing your CLI module
- **Cache** fetches content-addressably (same URL → same bytes forever)
- **Serve** the verification as an HTTP API without rewriting anything
- **Compile** the whole pipeline to a single standalone binary
- **Prove** to an auditor that two runs produced identical results

…you need composition, not code. That's what Noether gives you.

In this tutorial you'll rebuild `citecheck`'s verification as four composable Noether stages, compose them into a graph, run it, serve it over HTTP, and publish to the registry. No LLM required for the core tutorial; an optional LLM stage at the end adds semantic verification.

| Part | Runs without LLM? | Ends with... |
|---|---|---|
| [1. Quick-start](#quick-start) | ✅ | Running an existing stdlib stage |
| [2. Basic example: 4 stages](#basic-example-four-citecheck-stages) | ✅ | A full citation-verification graph |
| [3. Serve, trace, build](#serve-trace-build) | ✅ | HTTP API + standalone binary, no framework |
| [4. Adding an LLM stage](#adding-an-llm-stage) | ❌ needs Vertex AI or Gemini API | Semantic claim verification as a Noether stage |
| [5. Integrate with code assistants](#integrate-with-code-assistants) | ✅ | Cursor/Claude/Copilot can discover stages via the registry |
| [6. Where to next](#where-to-next) | — | AgentSpec and Caloron tutorials that build on these stages |

You don't need to have followed the ACLI tutorial — this one stands alone. But if you did, you'll recognize the building blocks.

## Quick-start

Install Noether. Pre-built binaries are on the [releases page](https://github.com/alpibrusl/noether/releases/latest); choose your platform, extract, put on PATH. The archive name includes the version, so check the latest release and substitute it in:

```bash
# macOS (Apple Silicon) — replace v0.7.1 with the current release tag
curl -L https://github.com/alpibrusl/noether/releases/latest/download/noether-v0.7.1-aarch64-apple-darwin.tar.gz | tar xz
chmod +x noether && mv noether ~/.local/bin/

# Linux (x86_64)
curl -L https://github.com/alpibrusl/noether/releases/latest/download/noether-v0.7.1-x86_64-unknown-linux-gnu.tar.gz | tar xz
chmod +x noether && mv noether ~/.local/bin/

# Windows: download the -x86_64-pc-windows-msvc.zip
```

Or install via cargo:

```bash
cargo install noether-cli
```

Verify:

```bash
noether version
```

See what's already in the local store:

```bash
noether stage list
```

The stdlib comes with 80 stages covering HTTP, text processing, JSON, branching, LLM adapters, and more. Find the HTTP GET stage:

```bash
noether stage search "http get"
```

Run it without writing any code:

```bash
echo '{"url": "https://example.com", "headers": {}, "timeout_s": 10}' \
  | noether run --stage http_get
```

You should see the fetched page's status, headers, and body — plus a content-addressable ID of that execution for auditability. You just ran a verified, type-checked HTTP fetch without importing any library.

!!! tip "Why content-addressed matters"
    Every stage has a stable cryptographic ID derived from its type signature + implementation. Two stages with the same ID are provably the same computation. Two runs of the same stage on the same input produce identical outputs *and* an auditable trace that proves so.

## Basic example: four `citecheck` stages

We'll rebuild `citecheck verify` as a Noether graph:

```
url ─► [fetch] ─► page ─► [extract_text] ─► text ─┐
claim ──────────────────────────────────────────► [claim_match] ─► verdict
```

Each box is a stage. The engine type-checks the whole graph before any stage runs: if `fetch` outputs `{status: Number, html: Text}` and `extract_text` expects `{html: Text}` you're fine; if they don't match you get an error at compose time, not run time.

### Setup

```bash
mkdir -p ~/citecheck-noether && cd ~/citecheck-noether
mkdir -p stages graphs
```

### Stage 1: `http_get` (already in stdlib)

Noether stdlib already ships `http_get`. Check its signature:

```bash
noether stage search "http get" --limit 1
```

```json
{
  "id": "7b2f...",
  "name": "http_get",
  "signature": "Record { headers: Record, timeout_s: Number, url: Text } → Record { body: Text, headers: Record, status: Number }",
  "description": "HTTP GET with timeout; returns status, headers, body.",
  "tags": ["web", "http", "io"]
}
```

We reuse it directly — no need to rewrite fetch.

### Stage 2: `html_to_text` — custom stage

Write the spec:

```json title="~/citecheck-noether/stages/html_to_text.json"
{
  "name": "html_to_text",
  "description": "Strip HTML tags, return visible text with normalized whitespace.",
  "input": {"Record": [["html", "Text"]]},
  "output": "Text",
  "effects": [],
  "language": "python",
  "implementation": "# requires: beautifulsoup4==4.12.3, lxml==5.2.2\nimport sys, json, re\nfrom bs4 import BeautifulSoup\n\ndata = json.load(sys.stdin)\nsoup = BeautifulSoup(data['html'], 'lxml')\nfor tag in soup(['script', 'style', 'noscript']):\n    tag.decompose()\ntext = re.sub(r'\\s+', ' ', soup.get_text(separator=' ')).strip()\nprint(json.dumps(text))",
  "examples": [
    {"input": {"html": "<p>Hello <b>world</b></p>"}, "output": "Hello world"},
    {"input": {"html": "<script>x=1</script><p>clean</p>"}, "output": "clean"},
    {"input": {"html": "<p>  lots   of   spaces  </p>"}, "output": "lots of spaces"}
  ],
  "tags": ["web", "html", "text", "pure"]
}
```

Register it:

```bash
noether stage add stages/html_to_text.json
```

The engine:
1. Validated the spec (required fields, type syntax)
2. Computed the content hash based on signature + implementation
3. Installed the Python dependencies in a sandboxed Nix environment
4. Ran the examples to verify your implementation produces the expected outputs
5. Stored it with lifecycle `Draft` (call `noether stage activate <id>` to promote)

You can now find it:

```bash
noether stage search "html text"
```

### Stage 3: `claim_match` — custom stage

```json title="~/citecheck-noether/stages/claim_match.json"
{
  "name": "claim_match",
  "description": "Check if a claim appears (case-insensitive) inside a body of text.",
  "input": {"Record": [["text", "Text"], ["claim", "Text"]]},
  "output": {"Record": [["found", "Bool"], ["preview", "Text"]]},
  "effects": [],
  "language": "python",
  "implementation": "import sys, json\ndata = json.load(sys.stdin)\ntext_lower = data['text'].lower()\nclaim_lower = data['claim'].lower()\nfound = claim_lower in text_lower\npreview = ''\nif found:\n    idx = text_lower.find(claim_lower)\n    preview = data['text'][max(0, idx-40): idx + len(data['claim']) + 40]\nprint(json.dumps({'found': found, 'preview': preview}))",
  "examples": [
    {"input": {"text": "Rust is fast and safe", "claim": "fast"}, "output": {"found": true, "preview": "Rust is fast and safe"}},
    {"input": {"text": "hello", "claim": "world"}, "output": {"found": false, "preview": ""}},
    {"input": {"text": "Case INSENSITIVE", "claim": "insensitive"}, "output": {"found": true, "preview": "Case INSENSITIVE"}}
  ],
  "tags": ["text", "match", "pure"]
}
```

```bash
noether stage add stages/claim_match.json
```

### Stage 4: `verify_verdict` — custom stage

Combines HTTP status and claim match into a single verdict:

```json title="~/citecheck-noether/stages/verify_verdict.json"
{
  "name": "verify_verdict",
  "description": "Combine HTTP status and claim match into a final verdict for citecheck.",
  "input": {"Record": [["http_status", "Number"], ["claim_found", "Bool"]]},
  "output": {"Record": [["verdict", "Text"]]},
  "effects": [],
  "language": "python",
  "implementation": "import sys, json\nd = json.load(sys.stdin)\nstatus = d['http_status']\nfound = d['claim_found']\nif not (200 <= status < 300):\n    verdict = 'broken'\nelif not found:\n    verdict = 'missing_claim'\nelse:\n    verdict = 'ok'\nprint(json.dumps({'verdict': verdict}))",
  "examples": [
    {"input": {"http_status": 200, "claim_found": true}, "output": {"verdict": "ok"}},
    {"input": {"http_status": 404, "claim_found": false}, "output": {"verdict": "broken"}},
    {"input": {"http_status": 200, "claim_found": false}, "output": {"verdict": "missing_claim"}}
  ],
  "tags": ["text", "logic", "pure"]
}
```

```bash
noether stage add stages/verify_verdict.json
```

### Compose the graph

Now tie the stages together. Noether uses Lagrange JSON to describe graphs:

```json title="~/citecheck-noether/graphs/verify.json"
{
  "name": "citecheck_verify",
  "description": "Verify one URL+claim citation.",
  "input": {"Record": [["url", "Text"], ["claim", "Text"]]},
  "output": {"Record": [["http_status", "Number"], ["claim_found", "Bool"], ["verdict", "Text"]]},
  "graph": {
    "sequence": [
      {
        "stage": "http_get",
        "bind": {"url": "$.url", "headers": {}, "timeout_s": 10}
      },
      {
        "parallel": [
          {
            "stage": "html_to_text",
            "bind": {"html": "$.body"},
            "output_as": "text"
          },
          {
            "pass_through": ["status"]
          }
        ]
      },
      {
        "stage": "claim_match",
        "bind": {"text": "$.text", "claim": "$input.claim"},
        "output_as": "match"
      },
      {
        "stage": "verify_verdict",
        "bind": {
          "http_status": "$.status",
          "claim_found": "$.match.found"
        }
      }
    ]
  }
}
```

Validate the graph (type-checks but doesn't run):

```bash
noether lint graphs/verify.json
```

Run it:

```bash
echo '{"url": "https://www.rust-lang.org", "claim": "reliable"}' \
  | noether run graphs/verify.json
```

Output:

```json
{
  "ok": true,
  "command": "noether",
  "data": {
    "http_status": 200,
    "claim_found": true,
    "verdict": "ok",
    "trace_id": "run_c3d4e5..."
  },
  "meta": {"duration_ms": 412, "version": "0.1.0"}
}
```

Retrieve the full trace:

```bash
noether trace run_c3d4e5
```

You get every stage's input, output, duration, and content hash. This is the audit trail: anyone can re-run the graph on the same input and verify the outputs match.

## Serve, trace, build

### Serve as HTTP

```bash
noether serve graphs/verify.json --port 8080
```

In another terminal:

```bash
curl -X POST http://localhost:8080/run \
  -H "Content-Type: application/json" \
  -d '{"url": "https://www.rust-lang.org", "claim": "reliable"}'
```

You just turned a Lagrange graph into a type-safe HTTP microservice. No Flask, no FastAPI, no routing — Noether did it.

### Build as a standalone binary

```bash
noether build graphs/verify.json --output ./citecheck-verify
```

Produces a single native binary with the graph and all custom stages embedded. Ship it to a server that has no Python, no Node, no Noether itself — and it runs.

```bash
./citecheck-verify --input '{"url": "https://www.rust-lang.org", "claim": "reliable"}'
```

Or the WASM target for the browser:

```bash
noether build graphs/verify.json --target browser --output ./citecheck-wasm
```

### Publish to the registry

The free public registry at `https://registry.alpibru.com` accepts stage pushes:

```bash
NOETHER_REGISTRY=https://registry.alpibru.com \
  noether stage add stages/html_to_text.json
```

Anyone can now pull and reuse your stage:

```bash
NOETHER_REGISTRY=https://registry.alpibru.com \
  noether stage search "html text"
```

This is where Noether's content-addressing pays off at scale: `html_to_text` with the same signature and implementation has the same ID everywhere. Two people publishing the same stage produce the same ID — one wins, the other's push is a no-op.

## Adding an LLM stage

!!! warning "From here on, you need an LLM"
    Either configure Vertex AI (`GOOGLE_CLOUD_PROJECT` + ADC) or set `GEMINI_API_KEY`.
    Everything before this section runs with zero API calls.

We'll add a fifth stage that uses Gemini to semantically verify a claim. Then compose it into a richer graph.

### Stage 5: `semantic_verify`

```json title="~/citecheck-noether/stages/semantic_verify.json"
{
  "name": "semantic_verify",
  "description": "Use Gemini to decide whether a page supports, contradicts, partially supports, or is unrelated to a claim.",
  "input": {"Record": [["claim", "Text"], ["content", "Text"]]},
  "output": {"Record": [["support", "Text"], ["reason", "Text"], ["evidence", "Text"]]},
  "effects": ["network", "llm"],
  "language": "python",
  "implementation": "# requires: google-genai>=0.3\nimport sys, json, os\nfrom google import genai\n\ndata = json.load(sys.stdin)\nclaim, content = data['claim'], data['content'][:8000]\n\nif os.environ.get('GOOGLE_CLOUD_PROJECT'):\n    client = genai.Client(vertexai=True, project=os.environ['GOOGLE_CLOUD_PROJECT'], location=os.environ.get('GOOGLE_CLOUD_LOCATION', 'europe-west1'))\nelse:\n    client = genai.Client(api_key=os.environ['GEMINI_API_KEY'])\n\nprompt = f\"\"\"Given a CLAIM and page CONTENT, decide whether the source supports the claim.\nReturn JSON only: {{\\\"support\\\": one of 'supports'|'partial'|'unrelated'|'contradicts', \\\"reason\\\": one sentence, \\\"evidence\\\": <=200 char quote}}\n\nCLAIM: {claim}\n\nCONTENT:\n{content}\n\"\"\"\n\nresp = client.models.generate_content(model='gemini-2.0-flash', contents=prompt)\ntext = resp.text.strip()\nif text.startswith('```'):\n    text = text.split('```')[1].removeprefix('json').strip()\nresult = json.loads(text)\n# Ensure all keys present\nprint(json.dumps({'support': result.get('support', 'unrelated'), 'reason': result.get('reason', ''), 'evidence': result.get('evidence', '')}))",
  "examples": [
    {"input": {"claim": "rust is fast", "content": "Rust has minimal runtime overhead."}, "output": {"support": "partial", "reason": "example", "evidence": "example"}}
  ],
  "tags": ["llm", "verification", "citation"]
}
```

Register:

```bash
noether stage add stages/semantic_verify.json
```

### Enhanced graph with semantic check

```json title="~/citecheck-noether/graphs/verify_semantic.json"
{
  "name": "citecheck_verify_semantic",
  "description": "Verify citation with both literal match and semantic LLM check.",
  "input": {"Record": [["url", "Text"], ["claim", "Text"]]},
  "output": {"Record": [["literal_verdict", "Text"], ["support", "Text"], ["evidence", "Text"]]},
  "graph": {
    "sequence": [
      {"stage": "http_get", "bind": {"url": "$.url", "headers": {}, "timeout_s": 10}},
      {"stage": "html_to_text", "bind": {"html": "$.body"}, "output_as": "text"},
      {
        "parallel": [
          {"stage": "claim_match", "bind": {"text": "$.text", "claim": "$input.claim"}, "output_as": "literal"},
          {"stage": "semantic_verify", "bind": {"claim": "$input.claim", "content": "$.text"}, "output_as": "semantic"}
        ]
      },
      {
        "stage": "verify_verdict",
        "bind": {"http_status": 200, "claim_found": "$.literal.found"},
        "output_as": "literal_verdict"
      }
    ]
  }
}
```

```bash
echo '{"url": "https://www.rust-lang.org", "claim": "Rust is fast and reliable"}' \
  | noether run graphs/verify_semantic.json
```

You now have a graph that runs a literal match and an LLM-based semantic verification in parallel, then combines both signals. Noether type-checks the whole thing before any HTTP or LLM call happens — if you wire a stage output to the wrong input type, you find out at compose time.

## Integrate with code assistants

Noether's CLI is ACLI-compliant, so the same pattern from the [ACLI tutorial](https://alpibrusl.github.io/acli/tutorial/#integrate-with-code-assistants) applies. The twist: Noether also exposes the **stage catalogue** as a discoverable resource.

Generate a skill file:

```bash
noether skill > NOETHER_SKILLS.md
```

That captures the CLI. For the stages, agents use `noether stage search` and `noether stage get <id>` at runtime. Point your assistant at both.

### Claude Code

```markdown title="CLAUDE.md"
# This project uses Noether for verified composition.

## Stages available
Run `noether stage list` for the catalogue, `noether stage search <query>` to find specific stages.
Register new stages with `noether stage add <spec.json>`.

## Running graphs
Run with `noether run <graph.json>`. Serve with `noether serve <graph.json> --port <N>`.
Build standalone binaries with `noether build <graph.json>`.

See NOETHER_SKILLS.md for full CLI reference.
```

### Cursor

```markdown title=".cursor/rules/noether.md"
---
description: Noether composition rules
alwaysApply: false
---

When building any pipeline that could be decomposed into reusable stages, prefer
Noether stages over ad-hoc Python. Commands:
- `noether stage search <query>` to find reusable stages
- `noether lint <graph.json>` to type-check before running
- `noether run <graph.json>` to execute

The stage store is content-addressed — the same (signature, implementation) always
produces the same ID.
```

### Copilot

```markdown title=".github/copilot-instructions.md"
## Composition pipelines

Use Noether stages for any data/LLM pipeline that benefits from reproducibility:
- Discover: `noether stage search`
- Register custom: `noether stage add <spec.json>`
- Compose: Lagrange JSON graphs, type-checked before execution

Run `noether introspect` for the machine-readable CLI.
```

### Aider / opencode / Codex / Gemini

Same pattern — point the assistant at `NOETHER_SKILLS.md`:

```bash
aider --read NOETHER_SKILLS.md              # Aider
# or for opencode / Gemini CLI / Codex: add it as a rule file
```

### The agent-native flow

What `noether stage search` gives an agent is fundamentally different from what a random API gives it. Because stages are typed and content-addressed, the agent can:

1. Search semantically for a stage that solves part of its problem
2. Read the signature — the type system tells it whether it fits
3. Compose it with other stages — type errors surface at graph time
4. Run with full audit trail — the trace is cryptographically tied to the stage IDs

This is why the ACLI + Noether combination matters: ACLI makes the CLI itself self-describing; Noether makes the *operations* self-describing and reusable.

## Where to next

- **[ACLI tutorial](https://alpibrusl.github.io/acli/tutorial/)** — the monolithic `citecheck` CLI that we've just decomposed into stages
- **[AgentSpec tutorial](https://alpibrusl.github.io/agentspec/tutorial/)** — wrap the `citecheck_verify` graph in an agent that decides when and how to run it, with signed portfolios of past verifications
- **[Caloron tutorial](https://alpibrusl.github.io/caloron-noether/tutorial/)** — run an autonomous sprint that extends the stage catalogue — the agent searches the registry, finds missing capabilities, and adds new stages to the graph

Each tutorial builds on the same `citecheck` use case from a different angle. Read in any order.
