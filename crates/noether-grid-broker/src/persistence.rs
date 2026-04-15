//! Durability backend for the broker.
//!
//! The default backend is in-memory only — restart loses everything,
//! workers re-enrol via heartbeat within ~10s, and a missed sprint-tick
//! is superseded by the next cron edge. Fine for an intra-LAN pilot
//! and for the dashboard demo.
//!
//! With `--features postgres` and `--postgres-url`, every mutation is
//! also written to a postgres table, and the broker hydrates its
//! in-memory caches from postgres on boot. This is what you want for
//! a production deployment where a broker restart shouldn't lose the
//! cost ledger or the agent-quota spend totals.
//!
//! Schema lives in `migrations/0001_init.sql` and is applied
//! idempotently when postgres is configured.

use crate::state::JobEntry;
use noether_grid_protocol::{JobId, WorkerAdvertisement, WorkerId};

/// What every mutation tells the persistence layer about. Variants
/// mirror the broker's HTTP surface 1-to-1 so the trait stays trivial
/// to implement and reason about.
///
/// All variants are constructed regardless of the active feature set;
/// only the postgres backend reads the fields. The blanket
/// `#[allow(dead_code)]` suppresses the no-postgres-build warning.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum Mutation {
    UpsertWorker(WorkerAdvertisement),
    UpdateHeartbeat {
        worker_id: WorkerId,
        in_flight_jobs: u32,
        capabilities: Vec<noether_grid_protocol::LlmCapability>,
        last_seen: chrono::DateTime<chrono::Utc>,
    },
    DrainWorker(WorkerId),
    RemoveWorker(WorkerId),
    UpsertJob {
        job_id: JobId,
        entry: JobEntry,
    },
    QuotaSpend {
        api_key: String,
        cents: u64,
    },
}

#[derive(Debug, Default, Clone)]
pub struct HydrationSnapshot {
    pub workers: Vec<crate::state::WorkerEntry>,
    pub quota_spend: std::collections::HashMap<String, u64>,
}

/// Single persistence handle. When the inner postgres connection is
/// `None`, every method is a no-op — the broker stays in-memory.
pub struct Persistence {
    #[cfg(feature = "postgres")]
    inner: Option<postgres_impl::PostgresPersistence>,
}

impl Default for Persistence {
    fn default() -> Self {
        Self::in_memory()
    }
}

impl Persistence {
    pub fn in_memory() -> Self {
        Self {
            #[cfg(feature = "postgres")]
            inner: None,
        }
    }

    #[cfg(feature = "postgres")]
    pub async fn postgres(url: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let pg = postgres_impl::PostgresPersistence::connect(url).await?;
        Ok(Self { inner: Some(pg) })
    }

    pub async fn record(&self, m: Mutation) {
        let _ = m;
        #[cfg(feature = "postgres")]
        if let Some(pg) = &self.inner {
            pg.record(m).await;
        }
    }

    pub async fn hydrate(&self) -> HydrationSnapshot {
        #[cfg(feature = "postgres")]
        if let Some(pg) = &self.inner {
            return pg.hydrate().await;
        }
        HydrationSnapshot::default()
    }
}

#[cfg(feature = "postgres")]
mod postgres_impl {
    use super::*;
    use deadpool_postgres::{Config, Pool, Runtime};
    use tokio_postgres::NoTls;

    pub struct PostgresPersistence {
        pool: Pool,
    }

    impl PostgresPersistence {
        pub async fn connect(url: &str) -> Result<Self, Box<dyn std::error::Error>> {
            let mut cfg = Config::new();
            cfg.url = Some(url.to_string());
            let pool = cfg.create_pool(Some(Runtime::Tokio1), NoTls)?;
            Self::run_migrations(&pool).await?;
            Ok(Self { pool })
        }

        async fn run_migrations(pool: &Pool) -> Result<(), Box<dyn std::error::Error>> {
            const SCHEMA: &str = include_str!("../migrations/0001_init.sql");
            let client = pool.get().await?;
            client.batch_execute(SCHEMA).await?;
            Ok(())
        }

        pub async fn record(&self, m: Mutation) {
            let client = match self.pool.get().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("postgres pool unavailable for mutation: {e}");
                    return;
                }
            };
            let res = match &m {
                Mutation::UpsertWorker(adv) => client
                    .execute(
                        "INSERT INTO grid_workers (worker_id, url, advertisement, last_seen, in_flight_jobs, draining)
                         VALUES ($1, $2, $3::jsonb, NOW(), 0, false)
                         ON CONFLICT (worker_id) DO UPDATE SET
                            url = EXCLUDED.url,
                            advertisement = EXCLUDED.advertisement,
                            last_seen = NOW(),
                            draining = false",
                        &[
                            &adv.worker_id.0,
                            &adv.url,
                            &serde_json::to_value(adv).unwrap_or_default(),
                        ],
                    )
                    .await
                    .map(|_| ()),
                Mutation::UpdateHeartbeat {
                    worker_id,
                    in_flight_jobs,
                    capabilities,
                    last_seen,
                } => {
                    let caps_json = if capabilities.is_empty() {
                        None
                    } else {
                        Some(serde_json::to_value(capabilities).unwrap_or_default())
                    };
                    client
                        .execute(
                            "UPDATE grid_workers
                             SET last_seen = $1,
                                 in_flight_jobs = $2,
                                 advertisement = COALESCE(
                                     CASE WHEN $3::jsonb IS NULL THEN advertisement
                                          ELSE jsonb_set(advertisement, '{capabilities}', $3::jsonb) END,
                                     advertisement
                                 )
                             WHERE worker_id = $4",
                            &[last_seen, &(*in_flight_jobs as i32), &caps_json, &worker_id.0],
                        )
                        .await
                        .map(|_| ())
                }
                Mutation::DrainWorker(id) => client
                    .execute(
                        "UPDATE grid_workers SET draining = true WHERE worker_id = $1",
                        &[&id.0],
                    )
                    .await
                    .map(|_| ()),
                Mutation::RemoveWorker(id) => client
                    .execute("DELETE FROM grid_workers WHERE worker_id = $1", &[&id.0])
                    .await
                    .map(|_| ()),
                Mutation::UpsertJob { job_id, entry } => {
                    let result = entry
                        .result
                        .as_ref()
                        .and_then(|r| serde_json::to_value(r).ok());
                    client
                        .execute(
                            "INSERT INTO grid_jobs (job_id, status, created_at, dispatched_to, result)
                             VALUES ($1, $2, $3, $4, $5)
                             ON CONFLICT (job_id) DO UPDATE SET
                                 status = EXCLUDED.status,
                                 dispatched_to = EXCLUDED.dispatched_to,
                                 result = COALESCE(EXCLUDED.result, grid_jobs.result)",
                            &[
                                &job_id.0,
                                &format!("{:?}", entry.status).to_lowercase(),
                                &entry.created_at,
                                &entry.dispatched_to.as_ref().map(|w| w.0.clone()),
                                &result,
                            ],
                        )
                        .await
                        .map(|_| ())
                }
                Mutation::QuotaSpend { api_key, cents } => client
                    .execute(
                        "UPDATE grid_quotas SET spent_cents = spent_cents + $1 WHERE api_key = $2",
                        &[&(*cents as i64), api_key],
                    )
                    .await
                    .map(|_| ()),
            };
            if let Err(e) = res {
                tracing::warn!("postgres mutation failed: {e}");
            }
        }

        pub async fn hydrate(&self) -> HydrationSnapshot {
            let client = match self.pool.get().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("postgres hydrate skipped: {e}");
                    return HydrationSnapshot::default();
                }
            };
            let mut snapshot = HydrationSnapshot::default();
            if let Ok(rows) = client
                .query(
                    "SELECT worker_id, advertisement, last_seen, in_flight_jobs, draining
                     FROM grid_workers",
                    &[],
                )
                .await
            {
                for row in rows {
                    let _: String = row.get("worker_id");
                    let raw: serde_json::Value = row.get("advertisement");
                    if let Ok(adv) = serde_json::from_value::<WorkerAdvertisement>(raw) {
                        snapshot.workers.push(crate::state::WorkerEntry {
                            advertisement: adv,
                            last_seen: row.get("last_seen"),
                            in_flight_jobs: row.get::<_, i32>("in_flight_jobs") as u32,
                            draining: row.get("draining"),
                        });
                    }
                }
            }
            if let Ok(rows) = client
                .query("SELECT api_key, spent_cents FROM grid_quotas", &[])
                .await
            {
                for row in rows {
                    snapshot
                        .quota_spend
                        .insert(row.get("api_key"), row.get::<_, i64>("spent_cents") as u64);
                }
            }
            tracing::info!(
                "hydrated {} worker(s), {} quota row(s) from postgres",
                snapshot.workers.len(),
                snapshot.quota_spend.len()
            );
            snapshot
        }
    }
}
