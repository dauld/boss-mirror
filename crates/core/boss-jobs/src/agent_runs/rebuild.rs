//! Rebuild the `agent_runs` projection from `audit_log`.
//!
//! One state event drives it — `agents.run.recorded` — whose payload is
//! the whole run including the price it was charged at. The price is
//! replayed, NOT recomputed: the rate card is mutable data, and
//! re-pricing an old run against today's card would make a rebuild
//! change history. `recorded_at` comes from the audit row's own
//! timestamp, which is the instant the live write bound into the row.

use boss_events::replay::{Applied, replay_projection};
use sqlx::PgPool;
use tracing::warn;

use super::postgres::insert_run_sql;

const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("agent_runs");

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub events_processed: u64,
    pub events_skipped: u64,
    pub runs_inserted: u64,
}

/// Drop every `agent_runs` row and replay `agents.run.recorded` in
/// audit_log id order.
pub async fn rebuild_agent_runs(pool: &PgPool) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport::default();

    let stats = replay_projection(
        pool,
        REBUILD_LOCK_KEY,
        &["DELETE FROM agent_runs"],
        "kind = 'agents.run.recorded'",
        async |conn, ev| {
            // The payload is a recorded run minus `recorded_at`, which
            // is the audit row's timestamp. Rebuild the full value by
            // pairing the two rather than defaulting the instant.
            let mut payload = ev.payload.clone();
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("recorded_at".into(), serde_json::json!(ev.ts));
            }
            let run: super::types::AgentRun = match serde_json::from_value(payload) {
                Ok(r) => r,
                Err(e) => {
                    warn!(
                        event_id = ev.audit_id,
                        error = %e,
                        "skipping agents.run.recorded whose payload does not deserialize as an AgentRun"
                    );
                    return Ok(Applied::Skipped);
                }
            };
            insert_run(&mut *conn, &run).await.map_err(|e| e.to_string())?;
            report.runs_inserted += 1;
            Ok(Applied::Yes)
        },
    )
    .await
    .map_err(RebuildError::Storage)?;

    report.events_processed = stats.processed;
    report.events_skipped = stats.skipped;
    Ok(report)
}

/// Insert one replayed run. Separate from the live adapter's INSERT
/// because that one prices and this one replays the recorded price —
/// but the COLUMN LIST is shared, so the two cannot drift on shape.
async fn insert_run(
    conn: &mut sqlx::PgConnection,
    run: &super::types::AgentRun,
) -> Result<(), sqlx::Error> {
    sqlx::query(&insert_run_sql())
        .bind(&run.run.run_id)
        .bind(run.run.actor_id.to_string())
        .bind(run.run.started_at)
        .bind(run.run.finished_at)
        .bind(run.run.outcome.as_str())
        .bind(run.run.error.as_deref())
        .bind(i64::try_from(run.run.input_tokens).unwrap_or(i64::MAX))
        .bind(i64::try_from(run.run.output_tokens).unwrap_or(i64::MAX))
        .bind(i32::try_from(run.run.tool_calls).unwrap_or(i32::MAX))
        .bind(run.usd_micros.map(|m| i64::try_from(m).unwrap_or(i64::MAX)))
        .bind(run.priced_by.as_deref())
        .bind(run.run.job_id)
        .bind(run.run.branch.as_deref())
        .bind(&run.run.detail)
        .bind(run.recorded_at)
        .execute(conn)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rebuilder and the live adapter INSERT through the same
    /// generated statement, so this pins the thing that generation is
    /// for: as many placeholders as columns, always. The binds below are
    /// still hand-written, and a column added without a matching
    /// `.bind` is what this count makes loud.
    #[test]
    fn the_insert_has_one_placeholder_per_column() {
        let sql = insert_run_sql();
        let columns = super::super::postgres::RUN_COLUMNS.split(',').count();
        assert_eq!(columns, 15, "agent_runs has fifteen columns");
        for n in 1..=columns {
            assert!(
                sql.contains(&format!("${n}")),
                "placeholder ${n} is missing"
            );
        }
        assert!(
            !sql.contains(&format!("${}", columns + 1)),
            "one placeholder too many: every bind below would be off by one"
        );
    }

    #[test]
    fn the_lock_key_is_distinct_from_the_jobs_rebuilder() {
        assert_ne!(REBUILD_LOCK_KEY, boss_core::rebuild::lock_key("jobs"));
    }
}
