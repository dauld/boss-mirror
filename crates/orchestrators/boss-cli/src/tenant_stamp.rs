//! The tenant publish stamp: a successful `boss tenant publish` records
//! itself in the database it published into, and the launcher
//! publishes only while there is no such record (backlog 6a8d4972,
//! design e187198f "the instance is the truth", car 2).
//!
//! WHY. Measured 2026-09-18: infra/oss-quickstart/services-launcher.sh
//! -> tenant-launch.sh -> infra/seed-tenant.sh ran `boss tenant
//! publish` at EVERY services-container start with no guard, so a
//! converge's ConfigMap rebuild implied a publish. Car 1 made every
//! door insert-if-absent and `--take` the only overwrite; this car
//! makes the LAUNCHER's automatic publish once per database: the
//! seeds bootstrap a fresh instance (the OSS quickstart, the
//! playground, a switched database), and after that a new repo row
//! reaches a running instance through an operator's own `boss tenant
//! publish` — still insert-if-absent, the stamp gates nothing the
//! operator runs by hand — or a boot with BOSS_TENANT_TAKE naming the
//! registries to overwrite.
//!
//! THE STAMP IS A ROW, NOT A BELIEF. One row per successful publish in
//! `tenant_publishes` (infra/postgres/schema/20260918200200-…), append-
//! only: the first row's date is what the launcher prints, a later row
//! is the record of a republish and what it took. It is written by the
//! verb through BOSS_POSTGRES_URL — the sim tenant's reset-baseline
//! stamp is written the same way — because the stamp is a fact about THIS
//! database, and the services container is where both the verb and
//! the URL are. A publish run with no URL says so and leaves no row.
//!
//! THE ROW PROJECTS A FACT IN THE LOG (backlog dbdc4d31, 2026-09-19).
//! Until this car the row was the only record: no audit_log event said
//! "this database was published from `<boss_commit>` by `<actor>`, taking
//! `<registries>`", and a publish that wrote N registry rows plus a
//! stamp left nothing in the system of record for a rebuilder to see.
//! Now every stamp stages ONE `tenant.published` event — payload = the
//! stamp's columns, `_actor` = the actor it names — on the
//! transactional outbox in the SAME transaction as the row, the door
//! the ledger verbs use (`boss ledger lock` → record_ledger_event_in_tx
//! → boss_events::outbox), and boss-event-relay lands it in audit_log
//! post-commit. The row stays as the launcher's fast read.
//!
//! A PORT WITH TWO ADAPTERS, the crate's shape: [`PublishStamps`] is
//! what the verb needs, [`InMemoryStamps`] proves the verb's decisions
//! without a database, [`PgStamps`] is the one the binary runs and is
//! pinned against a real TestDb below.

use anyhow::{Context, Result};
use async_trait::async_trait;
use boss_core::actor::ActorId;
use boss_core::event::Event;
use boss_core::publisher::EventStamp;
use chrono::{DateTime, Utc};

/// The one event kind a publish leaves: declared in event_kinds
/// (20260919-a-tenant-publish-is-a-fact-in-the-log.sql), which the
/// emitted-kinds-are-declared lint holds against this constant.
pub const TENANT_PUBLISHED: &str = "tenant.published";

/// One recorded publish — the row as `tenant_publishes` holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stamp {
    pub tenant_id: String,
    pub published_at: DateTime<Utc>,
    /// The actor running the verb when one is named (BOSS_ACTOR, the
    /// actor file); the launcher runs unnamed, and its writes were
    /// signed `automation:tenant-seed`, so that is what it records.
    pub published_by: String,
    /// The publishing binary's build commit — never the tenant
    /// directory's, which a ConfigMap delivers without a .git.
    pub boss_commit: String,
    /// What `--take` named on this run; empty for a plain publish.
    pub took: Vec<String>,
    /// How many doors the plan wrote through.
    pub writes: i32,
}

/// What the launcher reads: the first publish and how many there are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub first: Stamp,
    pub count: i64,
    pub last_at: DateTime<Utc>,
}

fn rfc3339(t: &DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// PURE: the fact a stamp projects — `tenant.published` with the
/// stamp's columns as its payload, signed as the actor the stamp
/// names (one value, read from one place, so `published_by` and
/// `_actor` cannot disagree). The event's own timestamp is minted
/// wall-clock by the stamp builder, as every live record is;
/// `published_at` rides in the payload as the column it is.
pub fn published_event(stamp: &Stamp) -> Event {
    // `ActorId: FromStr<Err = Infallible>`: every spelling parses.
    let actor: ActorId = stamp
        .published_by
        .parse()
        .unwrap_or_else(|never: std::convert::Infallible| match never {});
    EventStamp::new("tenant", actor).event(
        TENANT_PUBLISHED,
        serde_json::json!({
            "tenant_id": stamp.tenant_id,
            "published_at": rfc3339(&stamp.published_at),
            "published_by": stamp.published_by,
            "boss_commit": stamp.boss_commit,
            "took": stamp.took,
            "writes": stamp.writes,
        }),
    )
}

impl Published {
    /// ONE line, the date first: tenant-launch.sh takes the first word
    /// as the stamp date for its own line, and an operator reads the
    /// rest. Pinned by `renders_the_date_first`.
    pub fn render(&self) -> String {
        format!(
            "{} tenant {} published by {} (boss {}); {} publish{}, last {}",
            rfc3339(&self.first.published_at),
            self.first.tenant_id,
            self.first.published_by,
            self.first.boss_commit,
            self.count,
            if self.count == 1 { "" } else { "es" },
            rfc3339(&self.last_at),
        )
    }
}

/// The port: what `boss tenant publish` and `boss tenant published`
/// need from the database, and nothing else.
#[async_trait]
pub trait PublishStamps: Send + Sync {
    /// The first publish recorded in this database, with the count,
    /// or `None` for a database no publish has stamped.
    async fn published(&self) -> Result<Option<Published>>;
    /// Append one stamp AND the [`published_event`] it projects, as
    /// one atomic write. Never updates, never deletes.
    async fn record(&self, stamp: &Stamp) -> Result<()>;
}

/// In memory, for the verb's tests: the same port, no database. Holds
/// the rows and the events beside them, so a test reads what the
/// adapter recorded on both sides.
#[cfg(test)]
#[derive(Default)]
pub struct InMemoryStamps(std::sync::Mutex<Vec<(Stamp, Event)>>);

#[cfg(test)]
impl InMemoryStamps {
    pub fn events(&self) -> Vec<Event> {
        self.0
            .lock()
            .map(|rows| rows.iter().map(|(_, e)| e.clone()).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
#[async_trait]
impl PublishStamps for InMemoryStamps {
    async fn published(&self) -> Result<Option<Published>> {
        let rows = self.0.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(summarise(rows.iter().map(|(s, _)| s.clone())))
    }

    async fn record(&self, stamp: &Stamp) -> Result<()> {
        self.0
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .push((stamp.clone(), published_event(stamp)));
        Ok(())
    }
}

/// PURE: the first-by-date stamp, the count and the latest date, from
/// rows in any order — the same answer the SQL below gives.
#[cfg(test)]
fn summarise(rows: impl Iterator<Item = Stamp>) -> Option<Published> {
    let mut rows: Vec<Stamp> = rows.collect();
    rows.sort_by_key(|s| s.published_at);
    let first = rows.first()?.clone();
    let last_at = rows.last()?.published_at;
    Some(Published {
        first,
        count: rows.len() as i64,
        last_at,
    })
}

/// The database. `BOSS_POSTGRES_URL` is the services container's
/// spelling (docker-compose.yml, the cluster manifests); the same one
/// the sim tenant's baseline stamp and every service read.
pub struct PgStamps(sqlx::PgPool);

impl PgStamps {
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(url)
            .await
            .context("connecting to BOSS_POSTGRES_URL for the tenant publish stamp")?;
        Ok(Self(pool))
    }

    #[cfg(test)]
    pub fn from_pool(pool: sqlx::PgPool) -> Self {
        Self(pool)
    }
}

#[async_trait]
impl PublishStamps for PgStamps {
    async fn published(&self) -> Result<Option<Published>> {
        let first: Option<(String, DateTime<Utc>, String, String, Vec<String>, i32)> =
            sqlx::query_as(
                "SELECT tenant_id, published_at, published_by, boss_commit, took, writes \
                 FROM tenant_publishes ORDER BY published_at, id LIMIT 1",
            )
            .fetch_optional(&self.0)
            .await
            .context("reading tenant_publishes")?;
        let Some((tenant_id, published_at, published_by, boss_commit, took, writes)) = first else {
            return Ok(None);
        };
        let (count, last_at): (i64, DateTime<Utc>) =
            sqlx::query_as("SELECT COUNT(*), MAX(published_at) FROM tenant_publishes")
                .fetch_one(&self.0)
                .await
                .context("counting tenant_publishes")?;
        Ok(Some(Published {
            first: Stamp {
                tenant_id,
                published_at,
                published_by,
                boss_commit,
                took,
                writes,
            },
            count,
            last_at,
        }))
    }

    async fn record(&self, stamp: &Stamp) -> Result<()> {
        // The row and its fact commit or abort together: the event is
        // staged on the outbox inside the row's transaction (backlog
        // dbdc4d31), so a stamp never exists without the log entry a
        // rebuilder would reproduce it from, nor the entry without
        // the row.
        let mut tx = self
            .0
            .begin()
            .await
            .context("opening the tenant publish stamp transaction")?;
        sqlx::query(
            "INSERT INTO tenant_publishes \
             (tenant_id, published_at, published_by, boss_commit, took, writes) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&stamp.tenant_id)
        .bind(stamp.published_at)
        .bind(&stamp.published_by)
        .bind(&stamp.boss_commit)
        .bind(&stamp.took)
        .bind(stamp.writes)
        .execute(&mut *tx)
        .await
        .context("recording the tenant publish stamp in tenant_publishes")?;
        boss_events::outbox::record_event_in_tx(&mut tx, &published_event(stamp))
            .await
            .map_err(|e| anyhow::anyhow!(e))
            .context("staging tenant.published on the event outbox")?;
        tx.commit()
            .await
            .context("committing the tenant publish stamp and its event")?;
        Ok(())
    }
}

/// The one line `boss tenant publish` prints about its stamp. PURE
/// over the port so the decisions are pinned without a database: no
/// URL is a printed fact, not an error (an operator's workstation
/// publishes through the gateway and holds no database), and a stamp
/// that did not land after a publish that did IS an error — the
/// launcher's contract is "published and stamped", and its retry
/// republishes idempotently until both hold.
pub async fn stamp_after_publish(
    stamps: Option<&dyn PublishStamps>,
    stamp: &Stamp,
) -> Result<String> {
    match stamps {
        None => Ok(format!(
            "not stamped: BOSS_POSTGRES_URL is unset here, so this publish leaves no row in \
             tenant_publishes; the launcher's once-per-database guard reads that table (tenant {})",
            stamp.tenant_id
        )),
        Some(s) => {
            s.record(stamp).await?;
            Ok(format!(
                "stamped: tenant {} publish recorded in tenant_publishes at {} by {} (boss {}){}",
                stamp.tenant_id,
                rfc3339(&stamp.published_at),
                stamp.published_by,
                stamp.boss_commit,
                if stamp.took.is_empty() {
                    String::new()
                } else {
                    format!("; took {}", stamp.took.join(","))
                }
            ))
        }
    }
}

/// `boss tenant published`'s verdict: the line and the exit code the
/// launcher's guard reads — 0 stamped (the date is the first word), 1
/// no stamp in this database. An unreadable database is the caller's
/// error (exit 2 in the verb), never one of these.
pub async fn published_verdict(stamps: &dyn PublishStamps) -> Result<(String, i32)> {
    Ok(match stamps.published().await? {
        Some(p) => (p.render(), 0),
        None => (
            "no tenant publish stamped in this database: the launcher publishes on the next boot; \
             boss tenant publish <dir> publishes now"
                .to_string(),
            1,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn stamp(tenant: &str, at: &str, took: &[&str]) -> Stamp {
        Stamp {
            tenant_id: tenant.to_string(),
            published_at: Utc.from_utc_datetime(
                &chrono::NaiveDateTime::parse_from_str(at, "%Y-%m-%dT%H:%M:%S").unwrap(),
            ),
            published_by: "automation:tenant-seed".to_string(),
            boss_commit: "478231fb".to_string(),
            took: took.iter().map(|s| s.to_string()).collect(),
            writes: 12,
        }
    }

    #[tokio::test]
    async fn an_unstamped_store_answers_none_and_exit_1() {
        let s = InMemoryStamps::default();
        assert_eq!(s.published().await.unwrap(), None);
        let (line, code) = published_verdict(&s).await.unwrap();
        assert_eq!(code, 1);
        assert!(line.starts_with("no tenant publish stamped"), "{line}");
    }

    #[tokio::test]
    async fn the_first_publish_is_the_stamp_and_later_ones_are_counted() {
        let s = InMemoryStamps::default();
        // Recorded out of order: the FIRST by date is the stamp, not
        // the first written.
        s.record(&stamp("acme", "2026-09-19T08:00:00", &["agents"]))
            .await
            .unwrap();
        s.record(&stamp("acme", "2026-09-18T19:00:00", &[]))
            .await
            .unwrap();
        let p = s.published().await.unwrap().unwrap();
        assert_eq!(p.first, stamp("acme", "2026-09-18T19:00:00", &[]));
        assert_eq!(p.count, 2);
        assert_eq!(
            p.last_at,
            stamp("acme", "2026-09-19T08:00:00", &[]).published_at
        );
    }

    #[tokio::test]
    async fn renders_the_date_first() {
        // tenant-launch.sh reads `${stamp%% *}` as the date.
        let s = InMemoryStamps::default();
        s.record(&stamp("acme", "2026-09-18T19:00:00", &[]))
            .await
            .unwrap();
        let (line, code) = published_verdict(&s).await.unwrap();
        assert_eq!(code, 0);
        assert_eq!(
            line,
            "2026-09-18T19:00:00Z tenant acme published by automation:tenant-seed (boss 478231fb); \
             1 publish, last 2026-09-18T19:00:00Z"
        );
        s.record(&stamp("acme", "2026-09-19T08:00:00", &["agents"]))
            .await
            .unwrap();
        let (line, _) = published_verdict(&s).await.unwrap();
        assert!(
            line.ends_with("2 publishes, last 2026-09-19T08:00:00Z"),
            "{line}"
        );
    }

    #[tokio::test]
    async fn no_url_is_a_printed_fact_and_a_store_records_the_stamp() {
        let st = stamp("acme", "2026-09-18T19:00:00", &["employees", "agents"]);
        let line = stamp_after_publish(None, &st).await.unwrap();
        assert!(
            line.starts_with("not stamped: BOSS_POSTGRES_URL is unset"),
            "{line}"
        );

        let s = InMemoryStamps::default();
        let line = stamp_after_publish(Some(&s), &st).await.unwrap();
        assert_eq!(
            line,
            "stamped: tenant acme publish recorded in tenant_publishes at 2026-09-18T19:00:00Z \
             by automation:tenant-seed (boss 478231fb); took employees,agents"
        );
        assert_eq!(s.published().await.unwrap().unwrap().count, 1);
        // The row is a projection; the FACT is the event recorded
        // with it (backlog dbdc4d31): one tenant.published carrying
        // the stamp's columns.
        let events = s.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, TENANT_PUBLISHED);
        assert_eq!(events[0].payload, published_event(&st).payload);
    }

    /// The event a publish leaves in the log — the stamp's columns as
    /// the payload, signed by the actor the stamp names, so the
    /// tenant_publishes row can be rebuilt from it (backlog dbdc4d31).
    #[test]
    fn the_published_event_carries_the_stamps_columns() {
        let st = stamp("acme", "2026-09-18T19:00:00", &["employees", "agents"]);
        let e = published_event(&st);
        assert_eq!(e.kind, TENANT_PUBLISHED);
        assert_eq!(e.kind, "tenant.published");
        assert_eq!(e.source, "tenant");
        assert_eq!(e.payload["tenant_id"], "acme");
        assert_eq!(e.payload["published_at"], "2026-09-18T19:00:00Z");
        assert_eq!(e.payload["published_by"], "automation:tenant-seed");
        assert_eq!(e.payload["boss_commit"], "478231fb");
        assert_eq!(
            e.payload["took"],
            serde_json::json!(["employees", "agents"])
        );
        assert_eq!(e.payload["writes"], 12);
        // Provenance: `_actor` is the same value as published_by, read
        // from one place, never two arguments that can disagree.
        assert_eq!(e.payload["_actor"], "automation:tenant-seed");
        assert_eq!(e.payload.as_object().unwrap().len(), 7);
    }

    /// The adapter the binary runs, against the real table: the SQL's
    /// first-by-date, count and max agree with the in-memory answer.
    #[tokio::test(flavor = "multi_thread")]
    async fn pg_stamps_read_and_write_tenant_publishes() {
        let db = boss_testing::TestDb::new().await;
        let pg = PgStamps::from_pool(db.pool.clone());
        assert_eq!(pg.published().await.unwrap(), None);
        let (_, code) = published_verdict(&pg).await.unwrap();
        assert_eq!(code, 1);

        pg.record(&stamp("acme", "2026-09-19T08:00:00", &["agents"]))
            .await
            .unwrap();
        pg.record(&stamp("acme", "2026-09-18T19:00:00", &[]))
            .await
            .unwrap();
        let p = pg.published().await.unwrap().unwrap();
        assert_eq!(p.first, stamp("acme", "2026-09-18T19:00:00", &[]));
        assert_eq!(p.count, 2);
        assert_eq!(
            p.last_at,
            stamp("acme", "2026-09-19T08:00:00", &[]).published_at
        );
        let (line, code) = published_verdict(&pg).await.unwrap();
        assert_eq!(code, 0);
        assert!(
            line.starts_with("2026-09-18T19:00:00Z tenant acme"),
            "{line}"
        );

        // The row is what was recorded — `took` as a text array, the
        // write count — and nothing was updated in place.
        let rows: Vec<(String, Vec<String>, i32)> = sqlx::query_as(
            "SELECT tenant_id, took, writes FROM tenant_publishes ORDER BY published_at",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            rows,
            vec![
                ("acme".to_string(), vec![], 12),
                ("acme".to_string(), vec!["agents".to_string()], 12),
            ]
        );

        // Each row's fact is staged on the transactional outbox in
        // the SAME transaction (backlog dbdc4d31): one tenant.published
        // per stamp, payload = the columns, for the relay to land in
        // audit_log.
        let events: Vec<(String, String, serde_json::Value)> = sqlx::query_as(
            "SELECT source, kind, payload FROM event_outbox WHERE kind = $1 \
             ORDER BY payload->>'published_at'",
        )
        .bind(TENANT_PUBLISHED)
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(events.len(), 2);
        for (source, kind, payload) in &events {
            assert_eq!(source, "tenant");
            assert_eq!(kind, "tenant.published");
            assert_eq!(payload["tenant_id"], "acme");
            assert_eq!(payload["boss_commit"], "478231fb");
            assert_eq!(payload["published_by"], "automation:tenant-seed");
            assert_eq!(payload["_actor"], "automation:tenant-seed");
            assert_eq!(payload["writes"], 12);
        }
        assert_eq!(events[0].2["published_at"], "2026-09-18T19:00:00Z");
        assert_eq!(events[0].2["took"], serde_json::json!([]));
        assert_eq!(events[1].2["took"], serde_json::json!(["agents"]));
    }
}
