# Examples

Three worked examples, each self-contained. Paste-runnable against
v0.8.

## 1. Run a stdlib stage directly

The stdlib includes `to_text` (converts any value to its string form)
and `text_length` (returns the number of characters). Chain them.

```bash
# 1. Find the stage ids you need.
noether stage search "convert any value to its text"
# → prints the id for `to_text`

noether stage search "number of characters in a text"
# → prints the id for `text_length`

# For the example, grab the 8-char prefixes and drop them into a graph:
cat > rows.json <<'EOF'
{
  "description": "count characters in stringified input",
  "root": {
    "op": "Sequential",
    "stages": [
      { "op": "Stage", "id": "<to_text-prefix>" },
      { "op": "Stage", "id": "<text_length-prefix>" }
    ]
  }
}
EOF

noether run --input '{"rows": [1, 2, 3]}' rows.json
# → { "ok": true, "result": { "output": 22 } }
```

The `Sequential` operator pipes `to_text`'s output directly into
`text_length`'s input. The type checker verified at dry-run time that
`Text → Number` is a valid edge.

## 2. Compose a graph from a problem description

With an LLM provider configured, let the agent assemble the graph.

```bash
export MISTRAL_API_KEY=…        # or OPENAI_API_KEY, ANTHROPIC_API_KEY, etc.

noether compose "take a list of numbers, keep only the ones above 10, and return their sum"
```

Output:

```json
{
  "ok": true,
  "result": {
    "composition_id": "3fa8…",
    "graph": { "op": "Sequential", "stages": [ … ] },
    "output": 42
  }
}
```

Use `--dry-run` to see the graph without executing, and `--verbose` to
see which candidate stages the semantic index surfaced and what the
LLM picked.

## 3. Author a custom stage

Define a stage as JSON, with a Python implementation.

```json
{
  "name": "celsius_to_fahrenheit",
  "description": "Convert a Celsius temperature to Fahrenheit",
  "input":  { "Record": [["celsius", "Number"]] },
  "output": { "Record": [["fahrenheit", "Number"]] },
  "effects": ["Pure"],
  "language": "python",
  "implementation": "def execute(input):\n    return {'fahrenheit': input['celsius'] * 9 / 5 + 32}",
  "examples": [
    { "input": {"celsius": 0},   "output": {"fahrenheit": 32.0} },
    { "input": {"celsius": 100}, "output": {"fahrenheit": 212.0} }
  ]
}
```

Register it:

```bash
noether stage add celsius_to_fahrenheit.json
# → validates structure + examples, hashes signature, signs, promotes Active
```

Python stage contract:

- Top-level `def execute(input): …` that takes the parsed input dict and
  returns the output dict.
- **Do not** read from `sys.stdin` or `print` the result. The Noether
  runtime handles I/O — your `execute` is called with the parsed input
  and its return value is JSON-serialised.
- **Do not** add a top-level `if __name__ == "__main__":` block. The
  runtime synthesises its own `__main__` wrapper.

Now it's in your local store:

```bash
noether stage search "celsius"
noether stage get <id-prefix>
```

Use it in a graph by dropping its id into a `Stage` node, same as any
stdlib stage.

## Worked example in the repo

A fully annotated example with declared properties (runtime-checked
invariants beyond the type signature) lives at
[`examples/property-annotated/`](https://github.com/alpibrusl/noether/tree/main/examples/property-annotated)
in the source repo.
