# Getting Started

From zero to a running composition in under five minutes.

## Install

=== "crates.io"

    ```bash
    cargo install noether-cli
    noether version
    ```

    Needs Rust 1.75+ — install via [rustup.rs](https://rustup.rs/) if you
    don't have it.

=== "Pre-built binary"

    Download the latest release for your platform from
    [GitHub Releases](https://github.com/alpibrusl/noether/releases/latest).

    | Platform | Archive |
    |---|---|
    | Linux x86_64 | `noether-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` |
    | Linux aarch64 | `noether-vX.Y.Z-aarch64-unknown-linux-gnu.tar.gz` |
    | macOS x86_64 | `noether-vX.Y.Z-x86_64-apple-darwin.tar.gz` |
    | macOS aarch64 (Apple silicon) | `noether-vX.Y.Z-aarch64-apple-darwin.tar.gz` |
    | Windows x86_64 | `noether-vX.Y.Z-x86_64-pc-windows-msvc.zip` |

    ```bash
    tar xzf noether-*.tar.gz
    sudo mv noether /usr/local/bin/
    noether version
    ```

=== "Source"

    ```bash
    git clone https://github.com/alpibrusl/noether
    cd noether
    cargo build --release -p noether-cli
    export PATH="$PWD/target/release:$PATH"
    ```

**Nix is optional.** You need it only to execute Python / JavaScript / Bash
stages in a hermetic sandbox. Rust-native stdlib stages run without it.

## Point at the public registry

Noether ships with a public registry that hosts the stdlib plus ~400 curated
community stages. Read access is open — no credentials needed.

```bash
export NOETHER_REGISTRY=https://registry.alpibru.com
```

Add that to your shell profile and every `noether` command resolves against
the registry, merging with your local store.

## 1 — Find the right stages

```bash
noether stage list                     # browse everything
noether stage list --signed-by stdlib  # just the stdlib
noether stage search "parse CSV"       # semantic search
noether stage get <prefix>             # 8-char prefix is fine
```

Every stage has a permanent `StageId` — SHA-256 of its signature. Same hash,
same computation, forever.

## 2 — Write a graph

A composition graph is JSON. Hand-author with the 8-char prefixes that
`stage list` prints — the CLI resolves them to full IDs at load time.

```json title="graph.json"
{
  "description": "count CSV rows",
  "version": "0.1.0",
  "root": {
    "op": "Sequential",
    "stages": [
      { "op": "Stage", "id": "<csv-parse-prefix>" },
      { "op": "Stage", "id": "<list-length-prefix>" }
    ]
  }
}
```

Type-check before executing:

```bash
noether run --dry-run graph.json
```

If the output of stage 1 isn't a subtype of stage 2's input, you get a
precise error pointing at the edge — no surprises at runtime.

## 3 — Execute

```bash
# Inline input
noether run graph.json --input '{"csv": "a,b\n1,2\n3,4"}'

# Or pipe
echo '{"csv": "a,b\n1,2\n3,4"}' | noether run graph.json
```

Every run produces a **trace** — a structured record of which stages ran,
what they got, what they returned, how long each took, and a SHA-256 of
every input/output. Re-run the same graph on the same input and identical
traces are returned from cache.

```bash
noether trace <composition_id>
```

## 4 — Let the LLM build the graph for you

For problems where you don't want to hand-author the graph, let the
composition agent search the registry and wire stages together:

```bash
# Pick one provider.
export MISTRAL_API_KEY=...          # api.mistral.ai
# or: export OPENAI_API_KEY=sk-...
# or: export ANTHROPIC_API_KEY=sk-ant-...
# or: export VERTEX_AI_PROJECT=... VERTEX_AI_MODEL=gemini-2.5-flash

noether compose "convert text to uppercase and get its length"
noether compose --dry-run "sort a list and take the top 3"
noether compose --verbose "parse CSV and count rows"  # show reasoning
```

The agent searches the semantic index for top-20 candidates, builds a
Lagrange graph, type-checks it, retries on failure, and hands you back a
structured ACLI response.

## Next

- **[Composition Graphs](../guides/composition-graphs.md)** — full operator
  reference, including `Let` for carrying input through pipelines.
- **[Building Custom Stages](../guides/custom-stages.md)** — the
  `def execute(input)` contract, spec format, signing.
- **[Remote Registry](../guides/remote-registry.md)** — publishing,
  scheduling, self-hosting.
- **[LLM-Powered Compose](../guides/llm-compose.md)** — how `noether
  compose` constructs graphs, debugging tips.
