//! Shared TRUNCATE-replay skeleton for projection rebuilders.
//!
//! Every projection rebuilder shares the same control flow: open a
//! transaction, take the rebuild advisory lock, wipe the projection
//! tables, stream the matching `audit_log` rows in id order, fold each
//! through a domain-specific `apply` step, then commit. Only three
//! things actually vary between rebuilders — the lock key, the wipe
//! statements, and the event filter — plus the apply step itself,
//! which is the whole point. This module owns the invariant skeleton so
//! each rebuilder keeps only its genuinely per-projection parts.
//!
//! The skeleton is the place the five-property correctness protocol
//! cares about: one audited definition of "replay the log into a
//! projection" (one transaction, advisory-locked, audit-ordered) beats a
//! dozen copies that can drift apart. The apply step stays in the domain
//! crate, where the projection logic belongs.

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

/// One `audit_log` row handed to a projection's apply step.
pub struct ReplayEvent {
    /// `audit_log.id` — the monotonic sequence the replay walks in
    /// order. Carried so the apply step can log which event it skipped.
    pub audit_id: i64,
    /// `audit_log.event_id` — the event's own id, which a record can
    /// name as its cause (a sign-off stamp's `voided_by_event`, design
    /// 87329a13) and a replay must reproduce exactly.
    pub event_id: uuid::Uuid,
    pub kind: String,
    /// `audit_log.timestamp` — the recorded event time. Apply steps that
    /// stamp a projection column from it read it here; those that don't
    /// (e.g. messages) ignore it.
    pub ts: DateTime<Utc>,
    pub payload: serde_json::Value,
}

/// Whether the apply step folded the event into the projection or passed
/// on it (unknown kind, malformed payload, no-op delete). The driver
/// tallies these into [`ReplayStats`]; everything richer is the apply
/// step's own report.
pub enum Applied {
    Yes,
    Skipped,
}

/// What the driver itself counts. The per-projection report (domain
/// counters like "employees upserted") is owned by the caller's apply
/// closure, which captures its own report and mutates it in place.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplayStats {
    pub processed: u64,
    pub skipped: u64,
}

/// Replay the `audit_log` into a projection inside one advisory-locked
/// transaction.
///
/// Steps, in order:
/// 1. `BEGIN` a transaction.
/// 2. Take `pg_advisory_xact_lock(lock_key)` — held for the whole
///    transaction, so concurrent domain writes briefly queue and two
///    rebuilds of the same projection never interleave. Derive `lock_key`
///    from [`boss_core::rebuild::lock_key`].
/// 3. Run each statement in `wipe` (a `TRUNCATE … CASCADE`, a single
///    `DELETE`, or a list of `DELETE`s — whatever clears this projection).
/// 4. Stream every `audit_log` row matching `kind_filter`, in `id` order,
///    and fold each through `apply`.
/// 5. `COMMIT`.
///
/// `kind_filter` is interpolated into the query verbatim, so it must be a
/// trusted constant (e.g. `"kind LIKE 'people.employee.%'"`), never
/// caller input. Every call site passes a `&'static str`.
///
/// `apply` receives a `&mut PgConnection` borrowed from the transaction —
/// the same handle the domain helpers (`upsert_*`, `insert_*`) already
/// take — and the decoded [`ReplayEvent`]. It returns [`Applied`] so the
/// driver can keep the processed/skipped tally; any richer accounting is
/// the closure's to do against its own captured report.
pub async fn replay_projection<F>(
    pool: &PgPool,
    lock_key: i64,
    wipe: &[&str],
    kind_filter: &str,
    mut apply: F,
) -> Result<ReplayStats, String>
where
    F: AsyncFnMut(&mut PgConnection, ReplayEvent) -> Result<Applied, String>,
{
    let mut tx = pool.begin().await.map_err(|e| e.to_string())?;

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(lock_key)
        .execute(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    for stmt in wipe {
        sqlx::query(stmt)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }

    let rows: Vec<(i64, uuid::Uuid, String, DateTime<Utc>, serde_json::Value)> =
        sqlx::query_as(&format!(
            "SELECT id, event_id, kind, timestamp, payload FROM audit_log \
             WHERE {kind_filter} ORDER BY id"
        ))
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| e.to_string())?;

    let mut stats = ReplayStats::default();
    for (audit_id, event_id, kind, ts, payload) in rows {
        stats.processed += 1;
        let event = ReplayEvent {
            audit_id,
            event_id,
            kind,
            ts,
            payload,
        };
        match apply(&mut *tx, event).await? {
            Applied::Yes => {}
            Applied::Skipped => stats.skipped += 1,
        }
    }

    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(stats)
}
