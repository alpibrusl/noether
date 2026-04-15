-- noether-grid-broker — postgres state schema
--
-- Applied idempotently on broker boot when --features postgres is enabled
-- and --postgres-url is set. Loss of any of these tables is recoverable:
-- workers re-enrol via heartbeat, jobs are ephemeral (one cron edge
-- supersedes them), quotas reset to zero spent.

CREATE TABLE IF NOT EXISTS grid_workers (
    worker_id      TEXT PRIMARY KEY,
    url            TEXT NOT NULL,
    advertisement  JSONB NOT NULL,           -- full WorkerAdvertisement
    last_seen      TIMESTAMPTZ NOT NULL,
    in_flight_jobs INTEGER NOT NULL DEFAULT 0,
    draining       BOOLEAN NOT NULL DEFAULT false
);

CREATE INDEX IF NOT EXISTS grid_workers_last_seen_idx
    ON grid_workers (last_seen DESC);

CREATE TABLE IF NOT EXISTS grid_jobs (
    job_id         TEXT PRIMARY KEY,
    status         TEXT NOT NULL,            -- queued | running | ok | failed | abandoned
    created_at     TIMESTAMPTZ NOT NULL,
    dispatched_to  TEXT,
    result         JSONB                     -- null until terminal
);

CREATE INDEX IF NOT EXISTS grid_jobs_created_idx
    ON grid_jobs (created_at DESC);

CREATE INDEX IF NOT EXISTS grid_jobs_status_idx
    ON grid_jobs (status);

CREATE TABLE IF NOT EXISTS grid_quotas (
    api_key        TEXT PRIMARY KEY,
    monthly_cents  BIGINT NOT NULL,
    spent_cents    BIGINT NOT NULL DEFAULT 0
);
