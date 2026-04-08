# Cloud Registry

`noether-cloud` is the hosted, persistent stage registry that makes stages
available across machines, teams, and agents.

This page describes the implemented architecture.
For setup instructions, see the [Remote Registry guide](../guides/remote-registry.md).

---

## Architecture

```
noether CLI / AI agent
        │  NOETHER_REGISTRY=https://...
        ▼
┌────────────────────────────────┐
│   noether-cloud/registry       │  Axum HTTP server
│                                │
│  POST   /stages                │  Submit + validate a stage
│  GET    /stages                │  List by lifecycle
│  GET    /stages/:id            │  Fetch by content hash
│  DELETE /stages/:id            │  Delete a stage (requires API key)
│  POST   /stages/:id/lifecycle  │  Promote / deprecate
│  GET    /stages/search         │  Semantic search
│  POST   /compositions/run      │  Execute a composition graph
│  GET    /health                │  Stats + readiness
└────────────┬───────────────────┘
             │
    ┌────────┴────────┐
    │  Stage store    │
    │  (pluggable)    │
    │                 │
    │  JsonFileStore  │  dev / single machine
    │  PostgresStore  │  production / multi-replica
    └─────────────────┘
```

---

## Validation pipeline

Every `POST /stages` runs a **Noether composition** before inserting the stage:

```
Input: Stage JSON
       │
   Parallel ─────────────────────────────────────────────────────
   │ hash_check (f608988c)    sig_check (136f78d7)
   │ desc_check (4341c15f)    examples_check (f7d94d6e)
   └──────────────────────────────────────────────────────────────
                    │
         merge_validation_checks (60c9fa10)
                    │
       { passed: bool, errors: [], warnings: [] }
```

All 5 stages are stdlib stages, executed inline in Rust — no subprocess, ~1 ms.
The registry validates stages *using* Noether stages: genuine
"eating its own dog food."

---

## Backends

### JsonFileStore (development)

```bash
NOETHER_STORE_PATH=./data/registry.json noether-registry
```

Single-writer, single-machine.  Good for local development and low-traffic
private registries.

### PostgresStore (production)

```bash
DATABASE_URL=postgres://user:pass@host:5432/noether noether-registry
```

Uses `tokio-postgres` + `deadpool-postgres` (connection pool).  Stages stored as
JSONB — all fields are queryable.  Schema applied automatically on first connect.
Supports multiple registry replicas.

---

## Self-hosting with Docker Compose

```yaml
# noether-cloud/infra/docker-compose.yml
services:
  postgres:
    image: postgres:16
    environment:
      POSTGRES_DB: noether
      POSTGRES_PASSWORD: noether
  registry:
    build: { context: ../.. , dockerfile: noether-cloud/infra/Dockerfile.local }
    environment:
      DATABASE_URL: postgres://postgres:noether@postgres/noether
    ports: ["8080:8080"]
    depends_on: [postgres]
```

```bash
cd noether-cloud/infra
docker compose up -d
curl http://localhost:8080/health
```

---

## Two-repo strategy

| Repo | Licence | Contains |
|---|---|---|
| `alpibrusl/noether` | EUPL-1.2 | Core CLI, type system, stdlib, engine, docs |
| `(private) noether-cloud` | Commercial | Registry server, scheduler, cloud infra |

The open-source `noether` binary can point at any registry via `NOETHER_REGISTRY`.
The commercial registry is one option; teams can self-host the open-source registry
server too.
