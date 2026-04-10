# Noether — Type-safe Composition for AI Agents

When your AI coding assistant needs to build a data pipeline, it writes Python from scratch every time. 300 tokens for a CSV parser. 500 tokens for an API call + JSON extraction. Each time, from zero — no reuse, no type safety, no guarantee the code is correct until it runs.

Noether is different. Instead of generating code, it **composes pre-built, typed stages** into pipelines. The type checker validates every connection before anything executes. Stages are reusable — the same `csv_parse` stage works in every pipeline that needs CSV parsing.

---

## Demo 1: What a composition graph looks like

A Noether pipeline is a JSON file called a **composition graph**. Here's a real one that parses CSV and counts the rows:

```json
{
  "description": "Parse CSV data and count the number of rows",
  "version": "0.1.0",
  "root": {
    "op": "Sequential",
    "stages": [
      {
        "op": "Stage",
        "id": "72cdbe88...",
        "_comment": "csv_parse: Record{text, has_header, delimiter} → List<Map<Text,Text>>"
      },
      {
        "op": "Stage",
        "id": "bb1b2e4d...",
        "_comment": "list_length: List<Any> → Number"
      },
      {
        "op": "Stage",
        "id": "85c780f2...",
        "_comment": "to_text: Any → Text"
      }
    ]
  }
}
```

Each `Stage` node references a pre-built, typed function by its content hash (SHA-256). The `Sequential` operator chains them: the output of `csv_parse` feeds into `list_length`, then into `to_text`.

**Type-check it** (catches errors before anything runs):

```bash
$ noether run --dry-run pipeline.json

{
  "ok": true,
  "data": {
    "type_check": {
      "input":  "Record { delimiter: Null | Text, has_header: Bool | Null, text: Text }",
      "output": "Text"
    },
    "plan": { "steps": 3 }
  }
}
```

**Execute it** with real data:

```bash
$ noether run pipeline.json \
    --input '{"text": "name,score\nAlice,95\nBob,72\nCarol,88", "has_header": true, "delimiter": null}'

{
  "ok": true,
  "data": {
    "output": "3.0",
    "trace": {
      "duration_ms": 0,
      "stages": [
        { "stage_id": "72cdbe88...", "status": "Ok", "duration_ms": 0 },
        { "stage_id": "bb1b2e4d...", "status": "Ok", "duration_ms": 0 },
        { "stage_id": "85c780f2...", "status": "Ok", "duration_ms": 0 }
      ]
    }
  }
}
```

3 students, counted in 0ms. Every stage traced. Reproducible — same graph + same input = same output, always.

[![Demo 1: Compose and Execute](https://asciinema.org/a/zGgMmxgKpG78iUtH.svg)](https://asciinema.org/a/zGgMmxgKpG78iUtH)

---

## Demo 2: Type safety catches broken pipelines

Now let's swap the order — feed `list_length` (which returns a `Number`) into `csv_parse` (which expects a `Record`):

```json
{
  "root": {
    "op": "Sequential",
    "stages": [
      { "op": "Stage", "id": "bb1b2e4d...", "_comment": "list_length: List<Any> → Number" },
      { "op": "Stage", "id": "72cdbe88...", "_comment": "csv_parse: expects Record{text,...}" }
    ]
  }
}
```

```bash
$ noether run --dry-run broken.json

{
  "ok": false,
  "error": {
    "code": "GENERAL_ERROR",
    "message": "type check failed:
      type mismatch at position 0: output Number is not subtype of
      input Record { delimiter: Null | Text, has_header: Bool | Null, text: Text }"
  }
}
```

**The broken pipeline never executes.** No wasted compute, no runtime crash, no debugging. The error is caught in under 1ms, before any stage runs.

In traditional code generation, this bug only surfaces at runtime — after the agent writes the code, runs it, reads the traceback, and tries to fix it.

[![Demo 2: Type Safety](https://asciinema.org/a/9TB5bLcqHigMbmA7.svg)](https://asciinema.org/a/9TB5bLcqHigMbmA7)

---

## Demo 3: Parallel processing preserves data

When you chain stages sequentially, each one transforms the data — and the original is lost:

```
text → text_length → 42 → text_upper → ???
                     ↑ the text is gone, only a number remains
```

The `Parallel` operator solves this. Here's the graph — 4 branches analyze the same text simultaneously:

```json
{
  "root": {
    "op": "Sequential",
    "stages": [
      {
        "op": "Parallel",
        "branches": {
          "char_count": { "op": "Stage", "id": "3dd4e4c6...", "_comment": "text_length" },
          "uppercased": { "op": "Stage", "id": "1b68a050...", "_comment": "text_upper"  },
          "reversed":   { "op": "Stage", "id": "fbd972ad...", "_comment": "text_reverse" },
          "trimmed":    { "op": "Stage", "id": "bd8e4390...", "_comment": "text_trim"    }
        }
      },
      { "op": "Stage", "id": "b96bc6ef...", "_comment": "json_serialize" }
    ]
  }
}
```

Every branch receives the **full original text**. Results merge into a record keyed by branch name:

```bash
$ noether run parallel.json --input '"Noether composes typed pipelines for AI agents."'

{
  "ok": true,
  "data": {
    "output": {
      "char_count": 48.0,
      "uppercased": "NOETHER COMPOSES TYPED PIPELINES FOR AI AGENTS.",
      "reversed":   ".stnega IA rof senilpip depyt sesopmoc rehtoN",
      "trimmed":    "Noether composes typed pipelines for AI agents."
    }
  }
}
```

4 results from 1 input. No data loss. The executor ran them concurrently.

[![Demo 3: Parallel Processing](https://asciinema.org/a/E0MdzCOx24zYIXu7.svg)](https://asciinema.org/a/E0MdzCOx24zYIXu7)

---

## Demo 4: Reuse without duplication

Three different graphs, same building blocks. Here are all three — notice how `csv_parse` (`72cdbe88...`) appears in every one:

**Pipeline A** — count rows:
```json
{ "stages": [ csv_parse, list_length, to_text ] }
```

**Pipeline B** — export as JSON:
```json
{ "stages": [ csv_parse, json_serialize ] }
```

**Pipeline C** — deduplicate, then count:
```json
{ "stages": [ csv_parse, list_dedup, list_length, to_text ] }
```

```bash
$ noether run pipeline-a.json --input '...'    # → "7.0" rows
$ noether run pipeline-b.json --input '...'    # → [{"status":"open",...}, ...]
$ noether run pipeline-c.json --input '...'    # → "6.0" unique rows
```

| Stage | A | B | C |
|-------|:---:|:---:|:---:|
| csv_parse | ✓ | ✓ | ✓ |
| list_length | ✓ | | ✓ |
| to_text | ✓ | | ✓ |
| json_serialize | | ✓ | |
| list_dedup | | | ✓ |

5 unique stages. 9 total usages. **Zero lines of code written.**

[![Demo 4: Stage Reuse](https://asciinema.org/a/7f1Ri88zn1TxslDP.svg)](https://asciinema.org/a/7f1Ri88zn1TxslDP)

---

## Demo 5: An AI assistant using Noether

This is a real interaction. The user asks Claude Code to analyze some data. Instead of writing Python, the assistant uses `noether compose`:

> **User:** I have a CSV file with student grades. Count how many students there are.

**What the assistant does:**

```bash
# Step 1: The assistant calls noether compose
$ noether compose "parse CSV data and count the number of rows" \
    --input '{"text":"name,score,grade\nAlice,95,A\nBob,72,B\nCarol,88,A\nDave,61,C\nEve,79,B","has_header":true,"delimiter":null}'
```

**What Noether does internally:**

1. **Searches** the stage store for "parse CSV" and "count rows" → finds `csv_parse` (score: 0.70) and `list_length` (score: 0.66)
2. **Sends** the top 20 candidates to the LLM (Gemini / Claude / GPT)
3. **LLM returns** a composition graph: `csv_parse → list_length`
4. **Type checker** validates: `Record{text,...} → List<Map> → Number` ✓
5. **Executor** runs it: output = `5.0`

**What the assistant gets back:**

```json
{
  "ok": true,
  "data": {
    "output": 5.0,
    "attempts": 1,
    "from_cache": false,
    "trace": {
      "duration_ms": 0,
      "stages": [
        { "stage_id": "72cdbe88...", "status": "Ok" },
        { "stage_id": "bb1b2e4d...", "status": "Ok" }
      ]
    }
  }
}
```

> **Assistant:** There are 5 students in the CSV file.

The assistant didn't write any code. It called one command, got a structured JSON response, and reported the answer. The pipeline is cached — if the user asks a follow-up question about the same CSV structure, Noether responds instantly with 0 LLM tokens.

**The `--verbose` flag shows the full reasoning:**

```bash
$ noether compose --verbose "parse CSV data and count rows"

[compose] Semantic search: "parse CSV data and count rows"
[compose] Found 20 candidates:
   1. 0.710  72cdbe88  Parse CSV text into a list of row maps
   2. 0.697  bb1b2e4d  Return the number of elements in a list
   3. 0.575  ...       Read a sheet from an Excel file
   ...

[compose] System prompt: 18901 chars, 20 candidate stages
[compose] LLM call (attempt 1/3, model: gemini-2.5-flash)
[compose] LLM response: { "op": "Sequential", "stages": [csv_parse, list_length] }
[compose] ✓ Type check passed on attempt 1
```

---

## How to set this up for your coding assistant

Add a `CLAUDE.md` (or equivalent instructions file) to your project:

```markdown
## Data pipelines

This project uses Noether for data pipeline composition.
When asked to parse, transform, or analyze structured data,
use `noether compose "description"` instead of writing Python.

Available commands:
  noether compose "description"           # compose + execute
  noether compose --dry-run "description" # compose + type-check only
  noether run graph.json --input '...'    # execute a saved graph
  noether stage search "query"            # find available stages

All output is JSON with an `ok` field. Branch on ok, read data.output.
If compose fails, fall back to writing code.
```

The assistant discovers Noether through the ACLI protocol — every command returns structured JSON, no exit code parsing needed.

---

## Token cost comparison

| Pipeline variations | Compose (Noether) | Generate (code) |
|---|---|---|
| 1 | ~2,150 tokens | ~600 tokens |
| 3 | ~2,450 tokens | ~1,800 tokens |
| **4** | **~2,600 tokens** | **~2,400 tokens** |
| 5 | ~2,750 tokens | ~3,000 tokens |
| 10 | ~3,500 tokens | ~6,000 tokens |

Noether costs more for a single pipeline but **saves tokens at 4+ variations**. And cached results cost **0 tokens**.

---

## Try it

```bash
# Build from source (requires Rust toolchain)
git clone https://github.com/alpibrusl/noether
cd noether && cargo build --release -p noether-cli
export PATH="$PWD/target/release:$PATH"

# Verify it works
noether stage search "parse CSV"

# Set up an LLM provider (pick one)
export VERTEX_AI_PROJECT=your-project VERTEX_AI_MODEL=gemini-2.5-flash
# or: export OPENAI_API_KEY=sk-...
# or: export ANTHROPIC_API_KEY=sk-ant-...

# Compose your first pipeline
noether compose "parse CSV data and count rows"

# Or run a pre-built graph
noether run --dry-run demo/benchmark/scenarios/01-type-safety/valid-graph.json
```
