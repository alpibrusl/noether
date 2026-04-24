# Install

## From crates.io

```bash
cargo install noether-cli
```

Optionally install the scheduler binary for scheduled compositions:

```bash
cargo install noether-scheduler
```

## Prebuilt binaries

Download from [GitHub Releases](https://github.com/alpibrusl/noether/releases/latest)
— Linux, macOS, and Windows artifacts are built per tag.

## From source

```bash
git clone https://github.com/alpibrusl/noether
cd noether
cargo build --release -p noether-cli
./target/release/noether version
```

## Optional runtime dependencies

### Nix — for non-Rust stages

Noether's stdlib stages are all Rust-native and run without Nix. You
only need Nix if you author or execute stages written in Python,
JavaScript, or Bash. Those stages run in a Nix-pinned hermetic runtime
so the same source produces byte-identical results across machines.

Install Nix: [nixos.org/download](https://nixos.org/download). No
further Noether configuration needed — `noether run` detects Nix
automatically when a graph references a non-Rust stage.

### bubblewrap — the sandbox backend

The v0.7+ isolation layer wraps every non-Rust stage subprocess with a
`bwrap` call: fresh namespaces, UID-mapped to `nobody`, cap-drop ALL,
sandbox-private tmpfs at `/work`.

```bash
# Debian / Ubuntu
sudo apt install bubblewrap

# Fedora
sudo dnf install bubblewrap

# macOS (via nix-darwin or Docker)
# bwrap is Linux-only; use --isolate=none on macOS.
```

On Linux with bubblewrap present, `noether run --isolate=auto` (the
default) sandboxes every subprocess. Without bubblewrap, `auto` falls
back to unsandboxed with a warning. Pass `--require-isolation` (or set
`NOETHER_REQUIRE_ISOLATION=1`) to turn that fallback into a hard error
in CI.

## Grid (broker + worker)

`noether-grid` pools LLM capacity across a company — see the
[broker README](https://github.com/alpibrusl/noether/tree/main/crates/noether-grid-broker)
for the full pitch and deployment guide.

Install from crates.io (v0.8.2+):

```bash
cargo install noether-grid-broker
cargo install noether-grid-worker
```

Or download prebuilt binaries from
[GitHub Releases](https://github.com/alpibrusl/noether/releases/latest)
— Linux / macOS / Windows artifacts published per tag, same as the CLI.

Both crates carry a `"RESEARCH —"` prefix in their crates.io descriptions:
shipped and supported, but the operator surface is still evolving
release-to-release. Pin to exact versions in production.

## Verify the install

```bash
noether version
noether stage list | head
```

If `stage list` prints the 85-stage stdlib, you're set.

## LLM providers (for `noether compose`)

`noether compose` needs an LLM to translate problem descriptions into
composition graphs. It picks the first available provider at runtime:

| Provider | Env var | Notes |
|---|---|---|
| Mistral native | `MISTRAL_API_KEY` | EU-hosted. Preferred default. |
| OpenAI | `OPENAI_API_KEY` | Override base with `OPENAI_API_BASE` for Ollama / Together / etc. |
| Anthropic | `ANTHROPIC_API_KEY` | LLM only (no embeddings). |
| Vertex AI | `VERTEX_AI_PROJECT` + creds | Supports Mistral, Gemini, Claude via Model Garden. |
| Subscription CLI | `NOETHER_LLM_PROVIDER=claude-cli` (or `gemini-cli`, `cursor-cli`, `opencode`) | Uses your seat's auth; no API key. |
| Mock | (fallback) | Deterministic, used in tests. |

If none of these is configured, `noether compose` still runs — against
the mock provider — but the results are not useful for real problems.
`noether stage search` and `noether run` never need an LLM.
