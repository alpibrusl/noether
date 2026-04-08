# Noether Examples

Runnable composition graphs for the `noether run` command.
All stage IDs are the real SHA-256 content-addressed IDs from the stdlib.

## Running an example

```bash
# Build from source first
cargo build --release

# Dry-run (type-check + plan, no execution)
noether run --dry-run examples/weather-report.json

# Run with input
noether run examples/multi-source-search.json --input '{"query": "rust async runtime"}'

# Run the fleet briefing
noether run examples/fleet-briefing.json --input '{
  "stations_config": {"lat": 48.137, "lon": 11.576, "radius": 20},
  "weather_config":  {"lat": 48.137, "lon": 11.576, "timezone": "Europe/Berlin"}
}'
```

## Examples

| File | Description | Stages | APIs used |
|---|---|---|---|
| [`weather-report.json`](weather-report.json) | Fetch city weather, extract fields, format report | 6 | wttr.in |
| [`multi-source-search.json`](multi-source-search.json) | Search GitHub + HN + crates.io in one call | 2 | GitHub, HN Algolia, crates.io |
| [`fleet-briefing.json`](fleet-briefing.json) | EV fleet operator morning briefing | 8 (parallel) | OpenChargeMap, Open-Meteo |
| [`travel-intelligence.json`](travel-intelligence.json) | OTA travel market intelligence | 8 (parallel) | OpenChargeMap, Open-Meteo |
| [`todo/`](todo/) | Full-stack todo app — browser WASM + native backend | 2 + RemoteStage | — |
| [`stage-explorer/`](stage-explorer/) | Browse the stdlib in a browser — full-stack search UI | 2 + RemoteStage | — |

### Full-stack examples (browser + backend)

The `todo/` and `stage-explorer/` examples demonstrate the full Noether stack:
`noether build --target browser` for the WASM frontend and
`noether build --target native --serve :8080` for the backend.
See each directory's `README.md` for setup instructions.

## Stage IDs used

These are the real content-addressed IDs from `noether stage list`:

| Stage ID | Description |
|---|---|
| `39731ebb` | Make an HTTP GET request |
| `62bdb044` | Extract the body text from an HTTP response record |
| `b89d34eb` | Parse a JSON string into a structured value |
| `b4dfc6b0` | Interpolate variables into a template string using `{{key}}` syntax |
| `c7d35f7c` | Extract a value from JSON data using a JSONPath expression |
| `8dfa010b` | Search GitHub repositories, Hacker News stories, and crates.io Rust crates |
| `923a69d9` | Format a list of research results into readable Markdown |
| `4bcc4817` | Generate an HTML fleet morning briefing for EV heavy trucks |
| `3201c0fe` | Generate an HTML travel intelligence briefing for OTA analysts |

Verify any ID with: `noether stage get <id>`

## How stage IDs work

Every stage has a content-addressed ID: the SHA-256 hash of its `StageSignature`
(input type, output type, effects, implementation hash).  These IDs are **deterministic
and permanent** — the same stage always produces the same ID, on any machine, in any version.

This means `fleet-briefing.json` will always run the same `4bcc4817` stage — there is
no floating version, no "latest", no silent API change.  If the stage changes, its ID changes,
and the graph stops resolving — an explicit, auditable break rather than a silent regression.
