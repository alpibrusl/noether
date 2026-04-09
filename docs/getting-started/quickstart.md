# Quickstart

This guide walks you through the four core Noether workflows in under 10 minutes.

## 1 — Explore the stdlib

Noether ships with 80+ stdlib stages covering scalar ops, collections, I/O, LLM primitives, data processing, and text.

```bash
# List all stdlib stages
noether stage list

# Search for stages related to HTTP
noether stage search "fetch http json"

# Inspect a specific stage
noether stage get <hash>
```

Every stage has a permanent `StageId` (SHA-256 of its signature). The same hash is the same computation on any machine.

## 2 — Run a composition graph

A composition graph is a JSON file describing how stages connect. Noether type-checks it before running.

```bash
# Dry-run: type-check and print the execution plan, no execution
noether run --dry-run graph.json

# Execute the graph
noether run graph.json
```

Example graph (`hello.json`):

```json
{
  "op": "Sequential",
  "steps": [
    {
      "op": "Stage",
      "stage_id": "<hash-of-text_upper>",
      "input": { "op": "Const", "value": { "text": "hello noether" } }
    }
  ]
}
```

## 3 — LLM-powered compose

Describe your problem in plain English. Noether searches the semantic index for candidate stages, builds a composition graph with the LLM, type-checks it, and executes it.

```bash
# Generate and execute
noether compose "find the 5 most-starred Rust crates on GitHub today"

# Generate the graph only (no execution)
noether compose --dry-run "parse this CSV and compute the mean of column 'price'"

# Choose a model
noether compose --model gemini-2.0-flash "..."
```

## 4 — Build a standalone binary

Compile a composition graph into a single self-contained binary. The binary can run standalone or serve results over HTTP.

```bash
# Build
noether build graph.json --output my-tool

# Run once
./my-tool

# Run as HTTP microservice with browser dashboard
./my-tool --serve :8080
```

Open `http://localhost:8080` for the interactive dashboard.

## Next steps

- [First Composition](first-composition.md) — build a real multi-stage graph step by step
- [Composition Graphs reference](../guides/composition-graphs.md)
- [CLI Commands reference](../cli/commands.md)
