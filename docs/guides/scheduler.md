# Scheduler

`noether-scheduler` runs Lagrange graphs on a cron schedule. Every fire
executes a composition, captures the full trace, and optionally POSTs the
result to a webhook. It's a separate binary from `noether` — install only
if you have recurring compositions.

If you already run caloron-noether's sprint-tick loop, a health check
pipeline, or a nightly digest, this is the tool.

## Install

```bash
cargo install noether-scheduler
```

Or download a pre-built binary for your platform from [GitHub
Releases](https://github.com/alpibrusl/noether/releases/latest):

```bash
tar xzf noether-scheduler-*.tar.gz
sudo mv noether-scheduler /usr/local/bin/
noether-scheduler --help
```

## Invoke

Three forms, all equivalent:

```bash
noether-scheduler --config scheduler.json    # flag
noether-scheduler scheduler.json             # positional
noether-scheduler                             # defaults to ./scheduler.json
```

Logging is controlled by `RUST_LOG`:

```bash
RUST_LOG=noether_scheduler=info noether-scheduler --config scheduler.json
```

## Config file

A single JSON file lists the jobs and tells the scheduler where to resolve
stages from.

```json title="scheduler.json"
{
  "registry_url": "https://registry.alpibru.com",
  "jobs": [
    {
      "name": "sprint-tick",
      "cron": "* * * * *",
      "graph": "compositions/sprint_tick.json",
      "input": {
        "sprint_id": "sprint-1",
        "repo": "owner/repo"
      }
    },
    {
      "name": "retro",
      "cron": "0 18 * * 5",
      "graph": "compositions/retro.json",
      "input": { "sprint_id": "sprint-1" },
      "webhook": "https://hooks.example.com/retro-ready"
    }
  ]
}
```

### Top-level fields

| Field | Required | Purpose |
|---|---|---|
| `jobs` | yes | list of scheduled jobs |
| `store_path` | one of | resolve stages from a local JSON file store (default `.noether/store.json`) |
| `registry_url` | one of | resolve stages from a remote registry (e.g. `https://registry.alpibru.com`) |
| `registry_api_key` | optional | `X-API-Key` header for private registries |

Set exactly one of `store_path` / `registry_url`. If neither is set, the
scheduler falls back to a local file store at `.noether/store.json`.

### Per-job fields

| Field | Required | Purpose |
|---|---|---|
| `name` | yes | job identifier — appears in logs, trace metadata, and webhook payloads |
| `cron` | yes | standard 5-field crontab (`minute hour day month weekday`) |
| `graph` | yes | filesystem path to a Lagrange JSON graph |
| `input` | no | static JSON value passed as the graph's root input. Any JSON value: scalar, record, list |
| `webhook` | no | URL to POST the result to after each run |

## Webhook payload

When `webhook` is set, the scheduler POSTs JSON after every run (successful
or not):

```json
{
  "job": "sprint-tick",
  "composition_id": "8f3a…",
  "ok": true,
  "output": { … },
  "duration_ms": 412,
  "fired_at": "2026-04-14T08:30:00Z"
}
```

Failures include `"ok": false` and `"error"` fields in place of `"output"`.
Webhook responses are not inspected — 2xx and 5xx are treated identically;
the scheduler logs and moves on. Use idempotent webhook handlers.

## Cron expressions

Standard 5-field crontab. Examples:

| Expression | Meaning |
|---|---|
| `* * * * *` | every minute |
| `*/5 * * * *` | every 5 minutes |
| `0 * * * *` | every hour on the hour |
| `0 9 * * 1-5` | 09:00 on weekdays |
| `0 18 * * 5` | 18:00 on Fridays |
| `0 0 1 * *` | midnight on the 1st of each month |

The scheduler runs in the process's local timezone (UTC in containers by
default). Set `TZ=Europe/Madrid` (or equivalent) on the host or container
if you want local-time semantics.

## Deployment

### systemd

```ini title="/etc/systemd/system/noether-scheduler.service"
[Unit]
Description=Noether scheduler
After=network.target

[Service]
ExecStart=/usr/local/bin/noether-scheduler --config /etc/noether/scheduler.json
Restart=always
RestartSec=10
Environment=RUST_LOG=noether_scheduler=info
WorkingDirectory=/var/lib/noether

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now noether-scheduler
sudo journalctl -u noether-scheduler -f
```

### Docker

A reference image isn't published on GHCR yet. Build it locally from the
noether workspace:

```dockerfile title="Dockerfile"
FROM rust:1.87-slim-bookworm AS builder
WORKDIR /build
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev
RUN cargo install noether-scheduler --root /out

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates
COPY --from=builder /out/bin/noether-scheduler /usr/local/bin/
WORKDIR /data
ENTRYPOINT ["noether-scheduler", "--config", "/data/scheduler.json"]
```

## Relationship with the registry

The scheduler is a pure consumer of the stage store — it never writes
stages. Either:

- Point at `store_path` for a self-contained local deployment (good for
  the caloron-noether sprint loop, single-machine setups, CI).
- Point at `registry_url` to share a stage catalogue across multiple
  schedulers, registries, and CLI users. The public registry at
  `https://registry.alpibru.com` works unauthenticated for reads.

See the [Remote Registry](remote-registry.md) guide for the full picture
of how the registry, the scheduler, and the CLI interact.

## Troubleshooting

**`Failed to read config from scheduler.json`** — the config path resolves
against the process's working directory. Pass an absolute path with
`--config /full/path/scheduler.json` if you're running under systemd or a
container with a working directory that doesn't contain the file.

**`Invalid scheduler config JSON`** — run the file through `jq .` first.
The error message is `serde_json`'s, so missing commas and trailing
commas surface as cryptic position numbers. A `jq` pass usually pinpoints
them.

**Job fires but `graph` resolution fails** — the graph path is also
resolved against the working directory, not against the scheduler config
file's location. Use absolute paths in production, or pin
`WorkingDirectory` in the systemd unit.

**Stage IDs in the graph aren't found** — if you're using `registry_url`,
make sure the registry is reachable (`curl $registry_url/health`). If
you're using `store_path`, the file must contain the referenced stages;
the scheduler doesn't seed a stdlib into an empty file the way the CLI
does. Run `noether stage sync ./stages/` against the same store path to
bootstrap it.

**Webhook not firing** — check logs for the POST attempt. Networks
failures are logged but never retried; schedule job frequency to be
idempotent, or add retry logic in your webhook handler.
