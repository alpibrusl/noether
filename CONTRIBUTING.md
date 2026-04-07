# Contributing to Noether

Thank you for your interest in contributing. Noether is early-stage and moves fast — the best way to get involved is to open an issue first to discuss what you'd like to build.

## Setup

```bash
git clone https://github.com/your-org/noether
cd noether
cargo build
cargo test
cargo clippy -- -D warnings
```

## Where to contribute

### High-impact areas

**New stdlib stages** — The 50-stage stdlib is a starting point, not a ceiling. Any well-defined, reusable computation belongs here. See [`crates/noether-core/src/stdlib/`](crates/noether-core/src/stdlib/) for the pattern.

Good candidates:
- Domain-specific I/O stages (APIs, databases, file formats)
- Data transformation stages (XML parsing, Parquet, Arrow)
- LLM utility stages (chunk text, embed batch, rerank)
- Crypto/hashing stages

**Language runtimes** — Noether currently executes Python, JavaScript, and Bash via Nix. Adding Ruby, Go, or Deno is straightforward — see [`crates/noether-engine/src/executor/nix.rs`](crates/noether-engine/src/executor/nix.rs).

**LLM providers** — Only Vertex AI is wired up today. OpenAI, Anthropic direct, Ollama, and Groq would each take ~50 lines. See [`crates/noether-engine/src/llm/`](crates/noether-engine/src/llm/).

**Type system extensions** — Generic types (`List<T>` where T is a type variable), row polymorphism, and recursive types are all missing. See [`crates/noether-core/src/types/`](crates/noether-core/src/types/).

### Research directions

See [`noether-research/`](noether-research/) for the WASM compilation target and reactive UI research. These are explicitly experimental — open questions, not roadmap commitments.

## Code conventions

- `cargo fmt` before committing
- `cargo clippy -- -D warnings` must pass
- All public functions need doc comments
- New stdlib stages need at least one `example` in the stage spec
- Tests live next to the code they test (`#[cfg(test)]` modules)

## Stage contributions

The easiest contribution: add a new stage. The format is a JSON spec file:

```json
{
  "name": "my_stage",
  "description": "One-line description used by semantic search",
  "input":  { "kind": "Record", "value": { ... } },
  "output": { "kind": "...", "value": ... },
  "language": "python",
  "examples": [{ "input": {...}, "output": {...} }],
  "implementation": "def execute(input_value):\n    ..."
}
```

Test it:
```bash
noether stage add my-stage.json
noether run --dry-run   # type check
noether run my-graph.json --input '{...}'
```

If it belongs in the stdlib, add it to [`crates/noether-core/src/stdlib/stages/`](crates/noether-core/src/stdlib/stages/) following the existing pattern.

## Pull request process

1. Open an issue to discuss the change
2. Fork and branch from `main`
3. Keep PRs focused — one logical change per PR
4. All tests must pass: `cargo test`
5. Update `CLAUDE.md` if you change architecture or add new commands

## Reporting bugs

Include:
- Noether version (`noether version`)
- The command that failed
- Full output including stderr
- OS and Nix version (if relevant)

## License

By contributing, you agree your contributions are licensed under EUPL-1.2.
