//! Rebuild the `jobs` + `steps` projections from `audit_log`.
//!
//! Second projection rebuilder in the event-canonical arc (after
//! `boss-messages`). See `docs/design/projection-rebuilders.md`.
//!
//! Event topology — only the **state events** drive the rebuild;
//! the **marker events** (`status_changed`, `closed`, `completed`,
//! `signed_off`, `corrected`, `repinned`) are informational duplicates
//! and are skipped. `signed_off` is checked, not applied: a marker whose
//! stamp no state event carries is counted in
//! [`RebuildReport::sign_offs_unreproduced`] (backlog f146a13a).
//!
//! State events:
//! - `jobs.job.created`  — full Job row → INSERT
//! - `jobs.job.updated`  — full Job row → UPSERT (UPDATE if exists,
//!   INSERT if not — tolerates missing CREATE in pre-enrichment
//!   audit slices)
//! - `jobs.step.created` — full Step row → INSERT
//! - `jobs.step.updated` — full Step row → UPSERT
//! - `jobs.step.stamps_invalidated` — the stamps an edit voided
//!   (design 87329a13): each listed stamp on the row marked dead; an
//!   event from before the list existed voids every stamp alive on the
//!   row at that point, which is what the edit that emitted it left
//!   stale. And a void is permanent: a later state event whose payload
//!   carries a dead stamp alive (every payload written before voids
//!   existed) does not revive it — the upsert keeps the row's voids.
//!
//! Schema columns `created_at` / `updated_at` get filled from the
//! audit_log row's own `timestamp` field — the event-time recorded
//! by `DomainPublisher.emit`. Same shape as Layer 2 of the
//! immutable-audit-log: events are the canonical clock, projections
//! follow.

use boss_core::job::{Job, SignOffStamp, Step};
use boss_core::partition::Partition;
use boss_events::replay::{Applied, replay_projection};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use tracing::warn;

use crate::postgres::{
    blocked_by_uuids, job_status_str, priority_str, step_status_str, subject_parts,
};

/// Advisory-lock key for the jobs/steps rebuilder, derived from the
/// projection name so it is distinct from every other rebuilder's key.
const REBUILD_LOCK_KEY: i64 = boss_core::rebuild::lock_key("jobs");

#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RebuildReport {
    pub events_processed: u64,
    pub events_skipped: u64,
    pub jobs_inserted: u64,
    pub jobs_updated: u64,
    pub steps_inserted: u64,
    pub steps_updated: u64,
    /// Stamps a `jobs.step.stamps_invalidated` event voided.
    pub stamps_voided: u64,
    /// `jobs.step.signed_off` markers whose stamp no state event in the
    /// log carries (backlog f146a13a). Every marker the sign-off door
    /// wrote before the append recorded its own STEP_UPDATED lacks a
    /// `stamped_at`, so a stamp only such a marker recorded — one no
    /// later edit or completion carried — cannot be rebuilt exactly.
    /// The rebuild does not invent it: it leaves it off the row and
    /// counts it here, one warning per marker, so a replay that differs
    /// from the live row says so.
    pub sign_offs_unreproduced: u64,
}

/// What a `jobs.step.signed_off` marker says about the stamp it
/// announces. No `stamped_at`: the door never wrote one, so this names
/// a stamp but cannot rebuild it.
#[derive(Debug, serde::Deserialize)]
struct SignedOff {
    step_id: String,
    role: String,
    authority_id: String,
    shape_hash: String,
    /// Absent on every marker written before presence existed.
    #[serde(default)]
    presence_nonce: Option<String>,
    #[serde(skip)]
    audit_id: i64,
}

impl SignedOff {
    /// Whether `st` is the stamp this marker announced — as close to
    /// `SignOffStamp::same_stamp` as a marker without its instant allows.
    fn names(&self, st: &SignOffStamp) -> bool {
        st.role == self.role
            && st.authority_id == self.authority_id
            && st.shape_hash == self.shape_hash
            && (self.presence_nonce.is_none() || st.presence_nonce == self.presence_nonce)
    }
}

/// Drop every row in `steps` and `jobs` and replay every
/// `jobs.job.*` / `jobs.step.*` event from `audit_log` in id order.
/// Wrapped in a single transaction holding an advisory lock for the
/// duration — concurrent writes block briefly.
pub async fn rebuild_jobs_and_steps(pool: &PgPool) -> Result<RebuildReport, RebuildError> {
    let mut report = RebuildReport::default();
    // Sign-off markers whose stamp the replay has not yet seen carried
    // by a state event, keyed by step id (backlog f146a13a). The append
    // records its STEP_UPDATED BEFORE its marker, so a marker checks the
    // row as it stands; before that car an edit's STEP_UPDATED came
    // AFTER the marker, so a later state event clears one.
    let mut uncarried: std::collections::HashMap<String, Vec<SignedOff>> =
        std::collections::HashMap::new();

    // Steps cascade on jobs deletion (FK ON DELETE CASCADE), but we
    // delete steps first to make the order explicit and to make the
    // rebuild work even if the cascade is ever loosened.
    let stats = replay_projection(
        pool,
        REBUILD_LOCK_KEY,
        &["DELETE FROM steps", "DELETE FROM jobs"],
        "kind LIKE 'jobs.job.%' OR kind LIKE 'jobs.step.%'",
        async |conn, ev| {
            match ev.kind.as_str() {
                "jobs.job.created" | "jobs.job.updated" => {
                    let job: Job = match serde_json::from_value(ev.payload.clone()) {
                        Ok(j) => j,
                        Err(e) => {
                            warn!(
                                event_id = ev.audit_id,
                                kind = %ev.kind,
                                error = %e,
                                "skipping event with payload that doesn't deserialize as a Job (likely a pre-enrichment marker)"
                            );
                            return Ok(Applied::Skipped);
                        }
                    };
                    // `_partition` on the create event is where a
                    // Job's origin lives in the log — with the older
                    // `_simulated` bool as the fallback for events that
                    // predate it (packet 508cc38c). Read it here so a
                    // replay reproduces the same value the live write
                    // set — otherwise a rebuild would quietly turn the
                    // whole simulated company (or the shadow lane) real.
                    let partition = Partition::from_event_payload(&ev.payload);
                    let inserted_now = upsert_job(&mut *conn, &job, ev.ts, partition)
                        .await
                        .map_err(|e| e.to_string())?;
                    if inserted_now {
                        report.jobs_inserted += 1;
                    } else {
                        report.jobs_updated += 1;
                    }
                    Ok(Applied::Yes)
                }
                "jobs.step.created" | "jobs.step.updated" => {
                    let step: Step = match serde_json::from_value(ev.payload.clone()) {
                        Ok(s) => s,
                        Err(e) => {
                            warn!(
                                event_id = ev.audit_id,
                                kind = %ev.kind,
                                error = %e,
                                "skipping event with payload that doesn't deserialize as a Step"
                            );
                            return Ok(Applied::Skipped);
                        }
                    };
                    let mut step = step;
                    keep_the_voids(&mut *conn, &mut step)
                        .await
                        .map_err(|e| e.to_string())?;
                    let inserted_now = upsert_step(&mut *conn, &step, ev.ts)
                        .await
                        .map_err(|e| e.to_string())?;
                    if let Some(waiting) = uncarried.get_mut(&step.id.to_string()) {
                        waiting.retain(|m| !step.sign_offs.iter().any(|st| m.names(st)));
                    }
                    if inserted_now {
                        report.steps_inserted += 1;
                    } else {
                        report.steps_updated += 1;
                    }
                    Ok(Applied::Yes)
                }
                crate::events::STEP_STAMPS_INVALIDATED => {
                    let voided = apply_invalidation(&mut *conn, &ev)
                        .await
                        .map_err(|e| e.to_string())?;
                    report.stamps_voided += voided;
                    Ok(Applied::Yes)
                }
                // A marker, and the stamp it announces rides a state
                // event: the append's own STEP_UPDATED, recorded first
                // since backlog f146a13a, or an edit's after it. Nothing
                // is applied from the marker — it carries no
                // `stamped_at` — but one whose stamp no state event ever
                // carries is counted at the end, never passed in silence.
                crate::events::STEP_SIGNED_OFF => {
                    let mut marker: SignedOff = match serde_json::from_value(ev.payload.clone()) {
                        Ok(m) => m,
                        Err(e) => {
                            warn!(
                                event_id = ev.audit_id,
                                error = %e,
                                "signed_off marker names no stamp; skipping"
                            );
                            return Ok(Applied::Skipped);
                        }
                    };
                    marker.audit_id = ev.audit_id;
                    let on_row = stored_stamps(&mut *conn, &marker.step_id)
                        .await
                        .map_err(|e| e.to_string())?
                        .unwrap_or_default();
                    if !on_row.iter().any(|st| marker.names(st)) {
                        uncarried
                            .entry(marker.step_id.clone())
                            .or_default()
                            .push(marker);
                    }
                    Ok(Applied::Skipped)
                }
                // Marker events — the sibling state event already carried
                // full row state. Counted as skipped; not anomalous.
                "jobs.job.status_changed"
                | "jobs.job.closed"
                | "jobs.step.completed"
                // A correction's row rides its sibling JOB_UPDATED
                // (design 4105b020); it fell to the unknown-kind arm and
                // warned once per correction on every replay.
                | "jobs.step.corrected"
                // A re-pin's rows ride its sibling JOB_UPDATED /
                // STEP_UPDATED / STEP_CREATED (design 7cf202a9).
                | "jobs.job.repinned" => Ok(Applied::Skipped),
                other => {
                    warn!(event_id = ev.audit_id, kind = %other, "unknown jobs.* event kind; skipping");
                    Ok(Applied::Skipped)
                }
            }
        },
    )
    .await
    .map_err(RebuildError::Storage)?;

    for marker in uncarried.values().flatten() {
        warn!(
            event_id = marker.audit_id,
            step_id = %marker.step_id,
            role = %marker.role,
            authority_id = %marker.authority_id,
            "a sign-off stamp no state event carries: its marker records no stamped_at, so the rebuilt row lacks it"
        );
        report.sign_offs_unreproduced += 1;
    }
    report.events_processed = stats.processed;
    report.events_skipped = stats.skipped;
    Ok(report)
}

/// Upsert a Job row, stamping `created_at` (only on insert) and
/// `updated_at` from the audit_log event timestamp. Returns
/// `true` if the row was inserted (new), `false` if updated.
/// `partition` (and its derived `simulated` bool — both columns are
/// written, expand/contract per 20260915221611) is written on INSERT
/// and deliberately absent from the DO UPDATE list: a Job's origin is
/// decided when it is created and never revisited, so a later
/// `jobs.job.updated` must not be able to move it. The storage
/// enforces the immutability rather than trusting every caller to.
/// `opened_at` (backlog 6c2eba00) rides with it, for the same reason
/// and on the same terms.
async fn upsert_job(
    conn: &mut sqlx::PgConnection,
    job: &Job,
    ts: DateTime<Utc>,
    partition: Partition,
) -> Result<bool, RebuildError> {
    let (subj_kind, subj_ref) = subject_parts(&job.subject);
    // The admission instant, derived from the log rather than invented
    // (backlog 6c2eba00, design f2cdff23 question `backfill`): the
    // payload's own stamp for a packet admitted after the field
    // existed, and the create event's recorded instant for one filed
    // before it — which is the same derivation the migration's
    // back-fill applies, so a full replay reproduces the columns it
    // wrote rather than drifting from them. A packet whose create
    // event is not in the log has no row here to fill.
    let opened_at = job.opened_at.unwrap_or(ts);
    let result = sqlx::query(
        r#"
        INSERT INTO jobs (id, kind, subject_kind, subject_id, title, owner_id,
                          status, priority, opened_on, opened_at, due_on, closed_on, metadata, tags,
                          workflow_version, created_at, updated_at, partition, simulated)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $16, $17, $18)
        ON CONFLICT (id) DO UPDATE SET
            kind = EXCLUDED.kind,
            workflow_version = EXCLUDED.workflow_version,
            subject_kind = EXCLUDED.subject_kind,
            subject_id = EXCLUDED.subject_id,
            title = EXCLUDED.title,
            owner_id = EXCLUDED.owner_id,
            status = EXCLUDED.status,
            priority = EXCLUDED.priority,
            opened_on = EXCLUDED.opened_on,
            due_on = EXCLUDED.due_on,
            closed_on = EXCLUDED.closed_on,
            metadata = EXCLUDED.metadata,
            tags = EXCLUDED.tags,
            updated_at = EXCLUDED.updated_at
        RETURNING (xmax = 0) AS inserted
        "#,
    )
    .bind(*job.id.inner().as_uuid())
    .bind(&job.kind)
    .bind(subj_kind)
    .bind(subj_ref)
    .bind(&job.title)
    .bind(&job.owner_id)
    .bind(job_status_str(job.status))
    .bind(priority_str(job.priority))
    .bind(job.opened_on)
    .bind(opened_at)
    .bind(job.due_on)
    .bind(job.closed_on)
    .bind(&job.metadata)
    .bind(&job.tags)
    .bind(job.workflow_version)
    .bind(ts)
    .bind(partition.as_str())
    .bind(partition.fails_closed())
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;
    use sqlx::Row;
    Ok(result.get::<bool, _>("inserted"))
}

/// The row's stamps as the replay has built them so far, or `None`
/// when the replay has not written the row yet.
///
/// FAILS CLOSED on stamps it cannot read (backlog 4174c4a9, the review
/// of car e1a62aa5). It answered an empty list, and both readers then
/// carried no void: the upsert let a dead stamp in its payload revive,
/// and an invalidation voided nothing — a rebuild that differs from the
/// live row, silently. The replay wrote this row itself from a payload
/// that parsed, so an unreadable one is a defect to stop on, and the
/// rebuild's transaction rolls back rather than keep a guess.
async fn stored_stamps(
    conn: &mut sqlx::PgConnection,
    id: &str,
) -> Result<Option<Vec<SignOffStamp>>, RebuildError> {
    let row: Option<(serde_json::Value,)> =
        sqlx::query_as("SELECT sign_offs FROM steps WHERE id = $1::uuid")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await
            .map_err(|e| RebuildError::Storage(e.to_string()))?;
    row.map(|(sign_offs,)| {
        serde_json::from_value(sign_offs).map_err(|e| {
            RebuildError::Storage(format!(
                "step {id}: the replayed sign_offs do not parse as stamps: {e}"
            ))
        })
    })
    .transpose()
}

/// A VOID IS PERMANENT (design 87329a13), in the replay as at the live
/// write. A state event carries the whole Step, stamps included, and
/// every one written before voids existed carries a dead stamp alive;
/// replayed verbatim after the invalidation that killed it, it would
/// bring the stamp back. The row's voids land on the payload's copies
/// of the same stamps before the upsert — only a void crosses.
async fn keep_the_voids(
    conn: &mut sqlx::PgConnection,
    step: &mut Step,
) -> Result<(), RebuildError> {
    if step.sign_offs.is_empty() {
        return Ok(());
    }
    if let Some(stored) = stored_stamps(conn, &step.id.to_string()).await? {
        step.apply_voids(&stored);
    }
    Ok(())
}

/// Apply one `jobs.step.stamps_invalidated` to the row it names, and
/// answer how many stamps it voided. The event lists the stamps it
/// voided (`voided`), and exactly those are marked dead, as the edit
/// marked them. An event written before the list existed says only that
/// the step's shape moved; every stamp alive on the row at that point
/// was left stale by that edit, so every one dies, dated by the event
/// and naming it — the same derivation the backfill migration
/// (20260925145943) applies to the rows already written.
async fn apply_invalidation(
    conn: &mut sqlx::PgConnection,
    ev: &boss_events::replay::ReplayEvent,
) -> Result<u64, RebuildError> {
    let Some(step_id) = ev.payload.get("step_id").and_then(|v| v.as_str()) else {
        warn!(
            event_id = ev.audit_id,
            "stamps_invalidated without a step_id; skipping"
        );
        return Ok(0);
    };
    let Some(mut stamps) = stored_stamps(conn, step_id).await? else {
        warn!(
            event_id = ev.audit_id,
            step_id, "stamps_invalidated for a step the replay has not written; skipping"
        );
        return Ok(0);
    };
    let before = stamps.clone();
    // A list that does not parse is read as no list (backlog 4174c4a9):
    // it used to void NOTHING, leaving alive a stamp the live edit had
    // killed. Every live-side void kills every stamp alive on the row at
    // that write (`Step::void_stamps_if_moved`), so the legacy reading
    // reproduces it, and a stamp is never left alive by a bad payload.
    let listed = ev.payload.get("voided").and_then(|listed| {
        serde_json::from_value::<Vec<SignOffStamp>>(listed.clone())
            .map_err(|e| {
                warn!(
                    event_id = ev.audit_id,
                    step_id,
                    error = %e,
                    "stamps_invalidated lists stamps that do not parse; voiding every stamp alive on the row"
                );
            })
            .ok()
    });
    match listed {
        Some(listed) => {
            boss_core::job::apply_voids(&mut stamps, &listed);
        }
        None => {
            for st in stamps.iter_mut().filter(|st| st.voided_at.is_none()) {
                st.voided_at = Some(ev.ts);
                st.voided_by_event = Some(ev.event_id);
            }
        }
    }
    let voided = stamps
        .iter()
        .zip(&before)
        .filter(|(now, was)| now.voided_at.is_some() && was.voided_at.is_none())
        .count() as u64;
    if voided > 0 {
        let written =
            serde_json::to_value(&stamps).map_err(|e| RebuildError::Storage(e.to_string()))?;
        sqlx::query("UPDATE steps SET sign_offs = $2 WHERE id = $1::uuid")
            .bind(step_id)
            .bind(written)
            .execute(&mut *conn)
            .await
            .map_err(|e| RebuildError::Storage(e.to_string()))?;
    }
    Ok(voided)
}

/// Upsert a Step row. Same timestamp-stamping shape as `upsert_job`.
async fn upsert_step(
    conn: &mut sqlx::PgConnection,
    step: &Step,
    ts: DateTime<Utc>,
) -> Result<bool, RebuildError> {
    let result = sqlx::query(
        r#"
        INSERT INTO steps (id, job_id, kind, title, spec_slug, assignee_id, status, sort_order,
                           blocked_by, sign_offs_required, assurance_required, sign_offs, fields,
                           completed_on, metadata, notes, step_plugin_version,
                           embedded_job, created_at, updated_at, became_ready_at,
                           completed_by, completed_at)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $19,
                CASE WHEN $7 = 'ready' THEN $19 END, $20, $21)
        ON CONFLICT (id) DO UPDATE SET
            job_id = EXCLUDED.job_id,
            kind = EXCLUDED.kind,
            title = EXCLUDED.title,
            spec_slug = EXCLUDED.spec_slug,
            assignee_id = EXCLUDED.assignee_id,
            status = EXCLUDED.status,
            sort_order = EXCLUDED.sort_order,
            blocked_by = EXCLUDED.blocked_by,
            sign_offs_required = EXCLUDED.sign_offs_required,
            assurance_required = EXCLUDED.assurance_required,
            sign_offs = EXCLUDED.sign_offs,
            fields = EXCLUDED.fields,
            completed_on = EXCLUDED.completed_on,
            metadata = EXCLUDED.metadata,
            notes = EXCLUDED.notes,
            step_plugin_version = EXCLUDED.step_plugin_version,
            embedded_job = EXCLUDED.embedded_job,
            updated_at = EXCLUDED.updated_at,
            -- First event that shows the row in `ready` wins, exactly
            -- as the live UPDATE's COALESCE writes the stamp once at
            -- the flip — replay reproduces `became_ready_at` from the
            -- event-time the log carried all along (2a0b034e). Rows
            -- that PREDATE the column even get it backfilled here,
            -- which is the rebuilder doing its one job: reproducing
            -- truth from the log.
            became_ready_at = COALESCE(steps.became_ready_at, EXCLUDED.became_ready_at),
            -- The completion stamps replay verbatim from the event
            -- that carried them: the STEP_UPDATED payload is the whole
            -- Step, so a rebuild reproduces who and when exactly as the
            -- live write stamped them (c17871fe).
            completed_by = EXCLUDED.completed_by,
            completed_at = EXCLUDED.completed_at
        RETURNING (xmax = 0) AS inserted
        "#,
    )
    .bind(*step.id.inner().as_uuid())
    .bind(*step.job_id.inner().as_uuid())
    .bind(&step.kind)
    .bind(&step.title)
    .bind(&step.spec_slug)
    .bind(&step.assignee_id)
    .bind(step_status_str(step.status))
    .bind(step.sort_order)
    .bind(blocked_by_uuids(&step.blocked_by))
    .bind(serde_json::to_value(&step.sign_offs_required).unwrap_or_default())
    // Same TEXT encoding as the live save path — the omission of
    // this column silently downgraded replayed steps to session
    // assurance (packet d7b8158e).
    .bind(
        step.assurance_required
            .and_then(|a| serde_json::to_value(a).ok())
            .and_then(|v| v.as_str().map(str::to_string)),
    )
    .bind(serde_json::to_value(&step.sign_offs).unwrap_or_default())
    .bind(serde_json::to_value(&step.fields).unwrap_or_default())
    .bind(step.completed_on)
    .bind(&step.metadata)
    .bind(&step.notes)
    .bind(step.step_plugin_version)
    .bind(step.embedded_job.map(|j| *j.inner().as_uuid()))
    .bind(ts)
    .bind(step.completed_by.as_ref().map(ToString::to_string))
    .bind(step.completed_at)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| RebuildError::Storage(e.to_string()))?;
    use sqlx::Row;
    Ok(result.get::<bool, _>("inserted"))
}
