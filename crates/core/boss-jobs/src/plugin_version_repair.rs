//! The one-time repair door for steps whose log says plugin version 0
//! while the row holds the stamped version (backlog 5a670a71 — core
//! correctness: determinism).
//!
//! WHY IT EXISTS. Until car 0f48e5f9 (backlog aba364fe) every write
//! path built a step's STEP_CREATED from the caller's Step, whose
//! `step_plugin_version` is 0, while the Pg INSERT stamped the row with
//! the version of the plugin active for its kind. The rebuilder
//! replays the payload, so a step created and never updated rebuilds
//! at 0 from a row at 1 or 3. That car fixed every NEW event; it could
//! not reach the steps already written. Measured 2026-09-25: 144
//! pending steps on open packets (130 `answer-question` stored v1, 14
//! `sign-off` stored v3) held only a version-0 STEP_CREATED.
//!
//! WHAT IT WRITES. Never a history rewrite and never a row edit: the
//! log is immutable, and the row is already right. For each step whose
//! divergence is EXACTLY that defect, ONE `jobs.step.updated` built
//! from the stored row the way every STEP_UPDATED is
//! (`events::step_state_payload`, stamped with the caller as `_actor`
//! and the packet's partition), recorded on the outbox in the same
//! transaction that bumps the row's `updated_at` to the event's
//! instant — which is what every STEP_UPDATED writer does, and what
//! makes a replay reproduce the row to the column.
//!
//! WHAT IT REFUSES — everything else, listed with the reason, written
//! never ([`judge`]). A step is corrected only when it is PENDING on an
//! OPEN packet (a STEP_UPDATED for a ready or active step re-fires the
//! dispatcher's routing of it, whatever its packet's status — the
//! adversarial review of car 55ae8de4), the state event a
//! replay would apply LAST is a `jobs.step.created` carrying version 0,
//! the row holds a non-zero version, and the two agree on every other
//! field. A step with no state event at all, one whose last state is a
//! STEP_UPDATED, and one whose log differs from its row in anything
//! but the plugin version are a different defect, and a door that
//! "fixed" them from the row would be deciding which of two records is
//! true without being asked.
//!
//! IDEMPOTENT BY CONSTRUCTION: a corrected step's last state event now
//! carries the row's version, so a second run finds nothing — even
//! before the relay drains, because the read takes the outbox's
//! undelivered rows as the tail of the log.
//!
//! The doors: `GET /api/jobs/repairs/step-plugin-version` (the dry
//! run) and `POST` (the write), and `boss repair step-plugin-version
//! [--dry-run]` over them. Nothing runs this on a schedule; an operator
//! runs it once, after reading the dry run.

use boss_core::job::{JobStatus, Step, StepStatus};
use serde::{Deserialize, Serialize};

/// The state event kind whose version-0 payload is the defect.
pub const CREATED: &str = crate::events::STEP_CREATED;

/// The state event a replay would apply last for a step, as the log
/// holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct Logged {
    /// `jobs.step.created` or `jobs.step.updated`.
    pub kind: String,
    pub step: Step,
}

/// What the door decides for one step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The log already carries the row's version: nothing to write.
    Agrees,
    /// Exactly the defect: one correcting STEP_UPDATED from the row.
    Correctable,
    /// Anything else — the reason says what, and nothing is written.
    Refused(String),
}

/// A status as its wire word (`ready`, `closed`), for a reason's text.
fn wire_word(status: impl Serialize) -> String {
    match serde_json::to_value(status) {
        Ok(serde_json::Value::String(s)) => s,
        _ => "(unnamed)".into(),
    }
}

/// Judge one step: its stored row, the status of the packet it sits
/// on, and the state event a replay would apply last. Pure — the
/// adapter reads all three under the row's lock and writes only on
/// [`Verdict::Correctable`].
///
/// Only a PENDING step on an OPEN packet is ever corrected (the
/// adversarial review of car 55ae8de4). A STEP_UPDATED is not inert:
/// the dispatcher's `handle_event` nominates any `ready` or `active`
/// step it carries without reading the packet's status, so a
/// correction for a ready step on a closed packet would offer work on
/// a packet that has ended, and one for any step on an ended packet
/// writes to a record nobody is running. A pending step is the one
/// shape no step consumer acts on — and it is the shape all 144 of the
/// measured steps had. The same rule makes the correction's move of
/// `updated_at` harmless to queue-age, whose fallback reads
/// `COALESCE(became_ready_at, updated_at)` for ready and active steps
/// only.
pub fn judge(stored: &Step, packet: JobStatus, logged: Option<&Logged>) -> Verdict {
    if logged.is_some_and(|l| l.step.step_plugin_version == stored.step_plugin_version) {
        return Verdict::Agrees;
    }
    if packet != JobStatus::Open {
        return Verdict::Refused(format!(
            "the packet is {}, not open: a correcting event on a packet that has ended is a \
             write to a record nobody is running, and the dispatcher reads a step event \
             without reading its packet's status",
            wire_word(packet)
        ));
    }
    if stored.status != StepStatus::Pending {
        return Verdict::Refused(format!(
            "the step is {}, not pending: the dispatcher nominates a ready or active step on \
             every STEP_UPDATED it reads, so a correction would re-fire that step's routing",
            wire_word(stored.status)
        ));
    }
    let Some(logged) = logged else {
        return Verdict::Refused(
            "the log holds no state event for this step, so a rebuild would not write the row \
             at all — a missing record, not a lost plugin version"
                .into(),
        );
    };
    if logged.kind != CREATED {
        return Verdict::Refused(format!(
            "the last state event is a {}, not the {CREATED} whose version-0 payload is the \
             defect: its version came from a row read, so the difference is something else",
            logged.kind
        ));
    }
    if logged.step.step_plugin_version != 0 {
        return Verdict::Refused(format!(
            "the {CREATED} carries plugin version {}, and the defect's event always carried 0",
            logged.step.step_plugin_version
        ));
    }
    let differing = differing_fields(stored, &logged.step);
    if differing.is_empty() {
        Verdict::Correctable
    } else {
        Verdict::Refused(format!(
            "the log's last state differs from the row in more than the plugin version: {}",
            differing.join(", ")
        ))
    }
}

/// The fields `stored` and `logged` disagree on, the plugin version
/// aside, as their serialized names. The completion instant is compared
/// at the row's precision — Postgres keeps microseconds, the payload
/// nanoseconds — so the same instant written twice is never a finding.
fn differing_fields(stored: &Step, logged: &Step) -> Vec<String> {
    let as_object = |step: &Step| {
        let mut s = step.clone();
        s.step_plugin_version = 0;
        s.completed_at = s
            .completed_at
            .map(|t| chrono::SubsecRound::trunc_subsecs(t, 6));
        match serde_json::to_value(s) {
            Ok(serde_json::Value::Object(map)) => Some(map),
            _ => None,
        }
    };
    // Fails closed: a step that cannot be compared is not the defect.
    let (Some(a), Some(b)) = (as_object(stored), as_object(logged)) else {
        return vec!["(a step that does not serialize cannot be compared)".into()];
    };
    let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .cloned()
        .collect()
}

/// What the door did (or, dry, would do) for one step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// Written: one STEP_UPDATED from the row.
    Corrected,
    /// A dry run's answer for a step the write would correct.
    WouldCorrect,
    /// Raced: the log caught up between the scan and the row lock.
    Agrees,
    /// Not this door's divergence; nothing written.
    Refused,
}

/// One step the scan found diverging, and what became of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRepair {
    pub step_id: String,
    pub job_id: String,
    pub kind: String,
    pub status: String,
    /// The kind of the state event a replay applies last; `None` when
    /// the log holds none for the step.
    pub logged_kind: Option<String>,
    /// The version that event carries; `None` with `logged_kind`.
    pub logged_version: Option<i32>,
    /// The version the row holds.
    pub stored_version: i32,
    /// The status of the packet the step sits on (`open`, `closed`,
    /// ...) and its partition (`real`, `simulated`, `shadow`), so the
    /// dry run says which packets a write would touch. Defaulted on
    /// read so a CLI newer than its server still parses the report.
    #[serde(default)]
    pub packet_status: String,
    #[serde(default)]
    pub partition: String,
    pub outcome: Outcome,
    /// Why a step was refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The door's whole answer. `steps` is every divergent step in step-id
/// order, listed in full — a refusal is as much a finding as a write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepairReport {
    /// `false` for the dry run: nothing was written.
    pub written: bool,
    pub steps: Vec<StepRepair>,
}

impl RepairReport {
    pub fn count(&self, outcome: &Outcome) -> usize {
        self.steps.iter().filter(|s| &s.outcome == outcome).count()
    }
}

#[cfg(feature = "postgres")]
pub(crate) mod pg {
    //! The Postgres half: the scan, and one transaction per step.
    //!
    //! "The log" here is what a rebuild would replay: `audit_log`, plus
    //! the outbox rows the relay has not yet landed there. The relay
    //! drains in outbox order, so an undelivered row is always later
    //! than every audit row for the same step — which is what lets a
    //! second run, straight after the first, see the correction it just
    //! recorded and write nothing.

    use super::*;
    use crate::events::{STEP_CREATED, STEP_UPDATED};
    use crate::port::JobsError;
    use crate::postgres::{StepRow, parse_job_status, parse_partition, row_to_step};
    use boss_core::publisher::EventStamp;
    use sqlx::PgPool;
    use uuid::Uuid;

    fn storage(e: impl std::fmt::Display) -> JobsError {
        JobsError::Storage(e.to_string())
    }

    /// Every step whose last state event's plugin version differs from
    /// its row, or that has no state event at all. A SUPERSET of what
    /// the door corrects: [`judge`] decides each one under its row
    /// lock, reading the whole history, so this scan only has to
    /// never miss one. A payload without the key replays at 0 (the
    /// field's serde default), so it reads as 0 here too.
    ///
    /// A payload the rebuild skips must be skipped here as well, or its
    /// version stands in for the event the rebuild really applies last
    /// and a divergence behind it reads as agreement. SQL cannot run
    /// the Step deserializer, so this skips the two shapes that decide
    /// it for the fields the scan reads: `title` is the Step's ONE
    /// field without a serde default, so a payload without a string
    /// title never deserializes; and a `step_plugin_version` present
    /// but not a number (a JSON null among them) fails too. A payload
    /// malformed in some OTHER field still passes this filter — the
    /// residual the review's nit accepted as not worth the Step
    /// deserializer over the whole log.
    const CANDIDATES: &str = r#"
        WITH logged AS (
            SELECT DISTINCT ON (e.step_id) e.step_id, e.version
            FROM (
                SELECT payload->>'id' AS step_id,
                       COALESCE(payload->>'step_plugin_version', '0') AS version,
                       0 AS src, id AS ord
                FROM audit_log
                WHERE kind IN ($1, $2)
                  AND jsonb_typeof(payload->'title') = 'string'
                  AND COALESCE(jsonb_typeof(payload->'step_plugin_version'), 'number') = 'number'
                UNION ALL
                SELECT o.payload->>'id',
                       COALESCE(o.payload->>'step_plugin_version', '0'),
                       1, o.id
                FROM event_outbox o
                WHERE o.delivered_at IS NULL AND o.kind IN ($1, $2)
                  AND jsonb_typeof(o.payload->'title') = 'string'
                  AND COALESCE(jsonb_typeof(o.payload->'step_plugin_version'), 'number') = 'number'
                  AND NOT EXISTS (SELECT 1 FROM audit_log a WHERE a.event_id = o.event_id)
            ) e
            ORDER BY e.step_id, e.src DESC, e.ord DESC
        )
        SELECT s.id
        FROM steps s
        LEFT JOIN logged l ON l.step_id = s.id::text
        WHERE l.version IS DISTINCT FROM s.step_plugin_version::text
        ORDER BY s.id
    "#;

    /// One step's state events in the order a rebuild applies them.
    const HISTORY: &str = r#"
        SELECT kind, payload FROM (
            SELECT 0 AS src, id AS ord, kind, payload
            FROM audit_log
            WHERE payload->>'id' = $1 AND kind IN ($2, $3)
            UNION ALL
            SELECT 1, o.id, o.kind, o.payload
            FROM event_outbox o
            WHERE o.delivered_at IS NULL AND o.payload->>'id' = $1 AND o.kind IN ($2, $3)
              AND NOT EXISTS (SELECT 1 FROM audit_log a WHERE a.event_id = o.event_id)
        ) e
        ORDER BY src, ord
    "#;

    pub(crate) async fn repair(
        pool: &PgPool,
        write: bool,
        stamp: &EventStamp,
    ) -> Result<RepairReport, JobsError> {
        let candidates: Vec<Uuid> = sqlx::query_scalar(CANDIDATES)
            .bind(STEP_CREATED)
            .bind(STEP_UPDATED)
            .fetch_all(pool)
            .await
            .map_err(storage)?;
        let mut steps = Vec::with_capacity(candidates.len());
        for id in candidates {
            if let Some(found) = one(pool, id, write, stamp).await? {
                steps.push(found);
            }
        }
        Ok(RepairReport {
            written: write,
            steps,
        })
    }

    /// The step row the judgement reads, and (by `{lock}`) how.
    const STEP: &str = "SELECT id, job_id, kind, title, spec_slug, assignee_id, status, \
                        sort_order, blocked_by, sign_offs_required, assurance_required, \
                        sign_offs, fields, completed_on, metadata, notes, step_plugin_version, \
                        embedded_job, completed_by, completed_at \
                        FROM steps WHERE id = $1";

    /// Judge one step and, when `write` and the verdict is Correctable,
    /// record the correction in the same transaction.
    ///
    /// The WRITE judges under the step's row lock (`FOR UPDATE`) and
    /// its packet's (`FOR SHARE`): every step writer UPDATEs the step
    /// row, so none can land an event between the read of the history
    /// and the append, and a close UPDATEs the packet row, so the
    /// packet cannot end between the open-packet check and the event.
    /// The DRY RUN takes no lock at all (the review of car 55ae8de4): it
    /// writes nothing, so it has no judgement to protect, and a read
    /// that queued behind — or held up — a live step writer across
    /// every divergent step would make the safe half of the door the
    /// one that disturbs the system. `None` for a step deleted since
    /// the scan.
    async fn one(
        pool: &PgPool,
        id: Uuid,
        write: bool,
        stamp: &EventStamp,
    ) -> Result<Option<StepRepair>, JobsError> {
        let (step_lock, packet_lock) = if write {
            (" FOR UPDATE", " FOR SHARE")
        } else {
            ("", "")
        };
        let mut tx = pool.begin().await.map_err(storage)?;
        let row = sqlx::query_as::<_, StepRow>(&format!("{STEP}{step_lock}"))
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let stored = row_to_step(row)?;
        let (packet_word, partition): (String, String) = sqlx::query_as(&format!(
            "SELECT status, partition FROM jobs WHERE id = $1{packet_lock}"
        ))
        .bind(*stored.job_id.inner().as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        let packet = parse_job_status(&packet_word);
        let history: Vec<(String, serde_json::Value)> = sqlx::query_as(HISTORY)
            .bind(id.to_string())
            .bind(STEP_CREATED)
            .bind(STEP_UPDATED)
            .fetch_all(&mut *tx)
            .await
            .map_err(storage)?;
        // The rebuild skips a payload that does not deserialize as a
        // Step, so the state it applies last is the last one that does.
        let logged = history.into_iter().rev().find_map(|(kind, payload)| {
            serde_json::from_value::<Step>(payload)
                .ok()
                .map(|step| Logged { kind, step })
        });
        let verdict = judge(&stored, packet, logged.as_ref());
        let (outcome, reason) = match (&verdict, write) {
            (Verdict::Agrees, _) => (Outcome::Agrees, None),
            (Verdict::Correctable, false) => (Outcome::WouldCorrect, None),
            (Verdict::Correctable, true) => (Outcome::Corrected, None),
            (Verdict::Refused(why), _) => (Outcome::Refused, Some(why.clone())),
        };
        if outcome == Outcome::Corrected {
            // Built from the row, the way every STEP_UPDATED is, and
            // stamped with the packet's partition like every step
            // write; the row's `updated_at` moves to the event's
            // instant, as it does at every STEP_UPDATED, so a replay
            // reproduces it. (Only a pending step gets here, and
            // queue-age reads `updated_at` for ready and active steps
            // alone, so the move changes no age anyone reads.)
            let event = stamp
                .clone()
                .with_partition(parse_partition(&partition))
                .event(STEP_UPDATED, crate::events::step_state_payload(&stored));
            sqlx::query("UPDATE steps SET updated_at = $2 WHERE id = $1")
                .bind(id)
                .bind(stamp.timestamp)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(JobsError::Storage)?;
        }
        tx.commit().await.map_err(storage)?;
        Ok(Some(StepRepair {
            step_id: stored.id.to_string(),
            job_id: stored.job_id.to_string(),
            kind: stored.kind.clone(),
            status: crate::postgres::step_status_str(stored.status).to_string(),
            logged_kind: logged.as_ref().map(|l| l.kind.clone()),
            logged_version: logged.as_ref().map(|l| l.step.step_plugin_version),
            stored_version: stored.step_plugin_version,
            packet_status: packet_word,
            partition,
            outcome,
            reason,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::{JobId, StepStatus};

    fn row(version: i32) -> Step {
        let mut s = Step::new(JobId::new(), "answer-question", "Answer the question", 3);
        assert_eq!(s.status, StepStatus::Pending);
        s.metadata = serde_json::json!({"question": "q1"});
        s.step_plugin_version = version;
        s
    }

    fn logged(kind: &str, step: &Step) -> Logged {
        Logged {
            kind: kind.into(),
            step: step.clone(),
        }
    }

    #[test]
    fn a_created_event_at_zero_under_a_stamped_row_is_correctable() {
        let stored = row(3);
        let mut event = stored.clone();
        event.step_plugin_version = 0;
        assert_eq!(
            judge(&stored, JobStatus::Open, Some(&logged(CREATED, &event))),
            Verdict::Correctable
        );
    }

    /// The adversarial review's scenario (car 55ae8de4): the defect
    /// exactly, on a READY step of a CLOSED packet. The correction's
    /// STEP_UPDATED would re-fire the dispatcher's nomination, which
    /// reads no packet status — so it is refused, and the reason names
    /// the packet first.
    #[test]
    fn a_ready_step_on_a_closed_packet_is_refused() {
        let mut stored = row(3);
        stored.status = StepStatus::Ready;
        let mut event = stored.clone();
        event.step_plugin_version = 0;
        let Verdict::Refused(why) =
            judge(&stored, JobStatus::Closed, Some(&logged(CREATED, &event)))
        else {
            panic!("a step on an ended packet is never corrected");
        };
        assert!(why.contains("packet is closed"), "{why}");
    }

    #[test]
    fn only_a_pending_step_on_an_open_packet_is_corrected() {
        for packet in [JobStatus::Draft, JobStatus::Closed, JobStatus::Cancelled] {
            let stored = row(3);
            let mut event = stored.clone();
            event.step_plugin_version = 0;
            let verdict = judge(&stored, packet, Some(&logged(CREATED, &event)));
            assert!(
                matches!(&verdict, Verdict::Refused(why) if why.contains("not open")),
                "{packet:?}: {verdict:?}"
            );
        }
        for status in [
            StepStatus::Ready,
            StepStatus::Active,
            StepStatus::Completed,
            StepStatus::Skipped,
        ] {
            let mut stored = row(3);
            stored.status = status;
            let mut event = stored.clone();
            event.step_plugin_version = 0;
            let verdict = judge(&stored, JobStatus::Open, Some(&logged(CREATED, &event)));
            assert!(
                matches!(&verdict, Verdict::Refused(why) if why.contains("not pending")),
                "{status:?}: {verdict:?}"
            );
        }
    }

    /// A step whose log already agrees is `Agrees` whatever its packet
    /// — there is nothing to write, so nothing to refuse.
    #[test]
    fn a_log_that_agrees_agrees_on_any_packet() {
        let mut stored = row(3);
        stored.status = StepStatus::Completed;
        assert_eq!(
            judge(&stored, JobStatus::Closed, Some(&logged(CREATED, &stored))),
            Verdict::Agrees
        );
    }

    #[test]
    fn a_log_that_carries_the_rows_version_agrees() {
        let stored = row(3);
        assert_eq!(
            judge(&stored, JobStatus::Open, Some(&logged(CREATED, &stored))),
            Verdict::Agrees
        );
        let unserved = row(0);
        assert_eq!(
            judge(
                &unserved,
                JobStatus::Open,
                Some(&logged(CREATED, &unserved))
            ),
            Verdict::Agrees
        );
    }

    #[test]
    fn a_step_the_log_never_recorded_is_refused() {
        let Verdict::Refused(why) = judge(&row(1), JobStatus::Open, None) else {
            panic!("a step with no state event is not this door's divergence");
        };
        assert!(why.contains("no state event"), "{why}");
    }

    #[test]
    fn a_last_state_that_is_an_update_is_refused() {
        let stored = row(1);
        let mut event = stored.clone();
        event.step_plugin_version = 0;
        let Verdict::Refused(why) = judge(
            &stored,
            JobStatus::Open,
            Some(&logged(crate::events::STEP_UPDATED, &event)),
        ) else {
            panic!("only a STEP_CREATED at 0 is the defect");
        };
        assert!(why.contains(crate::events::STEP_UPDATED), "{why}");
    }

    #[test]
    fn a_created_event_at_a_nonzero_version_is_refused() {
        let stored = row(3);
        let mut event = stored.clone();
        event.step_plugin_version = 1;
        let Verdict::Refused(why) = judge(&stored, JobStatus::Open, Some(&logged(CREATED, &event)))
        else {
            panic!("the defect's event says 0, never another version");
        };
        assert!(why.contains("version 1"), "{why}");
    }

    #[test]
    fn a_log_that_differs_in_another_field_is_refused_naming_it() {
        let stored = row(1);
        let mut event = stored.clone();
        event.step_plugin_version = 0;
        event.title = "Answered some other question".into();
        event.metadata = serde_json::json!({"question": "q2"});
        let Verdict::Refused(why) = judge(&stored, JobStatus::Open, Some(&logged(CREATED, &event)))
        else {
            panic!("a second difference is a different defect");
        };
        assert!(why.contains("metadata") && why.contains("title"), "{why}");
        assert!(!why.contains("step_plugin_version"), "{why}");
    }

    /// The row keeps microseconds and the payload nanoseconds, so a
    /// completion instant differing only below a microsecond is the
    /// same instant — never a refusal.
    #[test]
    fn a_completion_instant_is_compared_at_the_rows_precision() {
        use chrono::TimeZone;
        let mut stored = row(1);
        stored.completed_at = Some(
            chrono::Utc
                .timestamp_opt(1_790_000_000, 123_456_000)
                .unwrap(),
        );
        let mut event = stored.clone();
        event.step_plugin_version = 0;
        event.completed_at = Some(
            chrono::Utc
                .timestamp_opt(1_790_000_000, 123_456_789)
                .unwrap(),
        );
        assert_eq!(
            judge(&stored, JobStatus::Open, Some(&logged(CREATED, &event))),
            Verdict::Correctable
        );
    }
}
