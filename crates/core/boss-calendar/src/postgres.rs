//! `PgCalendar` — Postgres-backed `CalendarClient`. The
//! `calendar_no_hard_overlap` GIST exclusion constraint on the
//! `calendar_reservations` table does the load-bearing
//! conflict-detection work; this adapter translates the Postgres
//! error into `CalendarError::Conflict { existing }`.

use std::collections::BTreeSet;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use boss_core::calendar::{
    BusinessCalendar, Reservation, ReservationId, ReservationRequest, ReservationStrength,
    TimeWindow,
};
use boss_core::job::Subject;

use boss_core::publish::PublishMode;

use crate::port::{BusinessCalendarsOutcome, CalendarClient, CalendarError, account_for};

pub struct PgCalendar {
    pool: PgPool,
}

impl PgCalendar {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Row shape mirroring `calendar_reservations`. Kept private — the
/// public surface uses `boss_core::calendar::Reservation` and the
/// `From` impl below converts.
#[derive(sqlx::FromRow)]
struct ReservationRow {
    id: Uuid,
    resource_kind: String,
    resource_id: String,
    start_ts: DateTime<Utc>,
    end_ts: DateTime<Utc>,
    reason_kind: String,
    reason_ref_id: String,
    strength: String,
    notes: Option<String>,
    created_by: String,
    created_at: DateTime<Utc>,
    cancelled_at: Option<DateTime<Utc>>,
}

impl TryFrom<ReservationRow> for Reservation {
    type Error = CalendarError;

    fn try_from(row: ReservationRow) -> Result<Self, CalendarError> {
        // The reserved subject — the `resource_kind`/`resource_id`
        // columns are the calendar's storage label for it. The kind is
        // open (registry-validated), so no closed parse is needed.
        let subject = Subject::new(row.resource_kind, row.resource_id);
        let window = TimeWindow::new(row.start_ts, row.end_ts)
            .map_err(|e| CalendarError::Storage(format!("invalid window in row: {e}")))?;
        // `reason_kind` is a free-form tag — stored and returned verbatim.
        let reason_kind = row.reason_kind;
        let strength = parse_strength(&row.strength)?;
        Ok(Reservation {
            id: ReservationId::from_uuid(row.id),
            subject,
            window,
            reason_kind,
            reason_ref_id: row.reason_ref_id,
            strength,
            notes: row.notes,
            created_by: row.created_by,
            created_at: row.created_at,
            cancelled_at: row.cancelled_at,
        })
    }
}

fn parse_strength(s: &str) -> Result<ReservationStrength, CalendarError> {
    match s {
        "hard" => Ok(ReservationStrength::Hard),
        "soft" => Ok(ReservationStrength::Soft),
        other => Err(CalendarError::Storage(format!(
            "unknown strength in row: `{other}`"
        ))),
    }
}

/// Postgres SQLSTATE for exclusion-constraint violation. Used to
/// recognise GIST conflicts so we can fetch + return the
/// conflicting rows.
const SQLSTATE_EXCLUSION_VIOLATION: &str = "23P01";

#[async_trait]
impl CalendarClient for PgCalendar {
    async fn reserve_at(
        &self,
        req: ReservationRequest,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<ReservationId, CalendarError> {
        if req.window.duration_seconds() <= 0 {
            return Err(CalendarError::Invalid(
                "window end must be after start".into(),
            ));
        }
        // Reservability is data: only a subject kind flagged
        // `calendar_reservable` in the subject_kinds registry can be
        // reserved. The single-occupancy guarantee is per individual
        // subject, enforced by the GIST exclusion constraint below.
        let reservable: Option<bool> = sqlx::query_scalar(
            "SELECT COALESCE((metadata->>'calendar_reservable')::bool, false)
             FROM subject_kinds WHERE kind = $1 AND retired_at IS NULL",
        )
        .bind(&req.subject.kind)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CalendarError::Storage(e.to_string()))?;
        if reservable != Some(true) {
            return Err(CalendarError::Invalid(format!(
                "subject kind `{}` is not calendar-reservable",
                req.subject.kind
            )));
        }
        let id = Uuid::new_v4();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| CalendarError::Storage(e.to_string()))?;
        let res = sqlx::query(
            "INSERT INTO calendar_reservations
             (id, resource_kind, resource_id, start_ts, end_ts,
              reason_kind, reason_ref_id, strength, notes, created_by, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        )
        .bind(id)
        .bind(&req.subject.kind)
        .bind(&req.subject.id)
        .bind(req.window.start)
        .bind(req.window.end)
        .bind(&req.reason_kind)
        .bind(&req.reason_ref_id)
        .bind(req.strength.db_value())
        .bind(req.notes.as_deref())
        .bind(&req.created_by)
        .bind(now)
        .execute(&mut *tx)
        .await;

        if let Err(e) = res {
            // Dropping the tx rolls back the failed INSERT.
            drop(tx);
            if let sqlx::Error::Database(db_err) = &e
                && db_err.code().as_deref() == Some(SQLSTATE_EXCLUSION_VIOLATION)
            {
                let existing = self.find_overlapping(&req.subject, req.window).await?;
                return Err(CalendarError::Conflict { existing });
            }
            return Err(CalendarError::Storage(e.to_string()));
        }

        // OUTBOX (phase 2): the reserved event (full post-INSERT row
        // state, built from the same values just written) records
        // with the row.
        let reservation = Reservation {
            id: ReservationId::from_uuid(id),
            subject: req.subject,
            window: req.window,
            reason_kind: req.reason_kind,
            reason_ref_id: req.reason_ref_id,
            strength: req.strength,
            notes: req.notes,
            created_by: req.created_by,
            created_at: now,
            cancelled_at: None,
        };
        let event = stamp.event(
            crate::events::RESERVATION_RESERVED,
            serde_json::to_value(&reservation).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(CalendarError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| CalendarError::Storage(e.to_string()))?;
        Ok(ReservationId::from_uuid(id))
    }

    async fn list(
        &self,
        subject: &Subject,
        window: TimeWindow,
    ) -> Result<Vec<Reservation>, CalendarError> {
        let rows: Vec<ReservationRow> = sqlx::query_as(
            "SELECT id, resource_kind, resource_id, start_ts, end_ts,
                    reason_kind, reason_ref_id, strength, notes,
                    created_by, created_at, cancelled_at
             FROM calendar_reservations
             WHERE cancelled_at IS NULL
               AND resource_kind = $1
               AND resource_id = $2
               AND start_ts < $4
               AND end_ts > $3
             ORDER BY start_ts",
        )
        .bind(&subject.kind)
        .bind(&subject.id)
        .bind(window.start)
        .bind(window.end)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CalendarError::Storage(e.to_string()))?;
        rows.into_iter().map(Reservation::try_from).collect()
    }

    async fn get(&self, id: ReservationId) -> Result<Option<Reservation>, CalendarError> {
        let row: Option<ReservationRow> = sqlx::query_as(
            "SELECT id, resource_kind, resource_id, start_ts, end_ts,
                    reason_kind, reason_ref_id, strength, notes,
                    created_by, created_at, cancelled_at
             FROM calendar_reservations WHERE id = $1",
        )
        .bind(*id.inner().as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| CalendarError::Storage(e.to_string()))?;
        row.map(Reservation::try_from).transpose()
    }

    async fn cancel_at(
        &self,
        id: ReservationId,
        _actor: &str,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), CalendarError> {
        let uuid = *id.inner().as_uuid();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| CalendarError::Storage(e.to_string()))?;
        // Idempotent: leave `cancelled_at` alone if already set. The
        // guard doubles as the event gate — RETURNING yields the row
        // only when this call actually flipped it.
        let row: Option<ReservationRow> = sqlx::query_as(
            "UPDATE calendar_reservations
             SET cancelled_at = $2
             WHERE id = $1 AND cancelled_at IS NULL
             RETURNING id, resource_kind, resource_id, start_ts, end_ts,
                       reason_kind, reason_ref_id, strength, notes,
                       created_by, created_at, cancelled_at",
        )
        .bind(uuid)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| CalendarError::Storage(e.to_string()))?;
        match row {
            Some(row) => {
                // OUTBOX (phase 2): the cancelled event (full
                // post-cancel row state) records with the flip. An
                // already-cancelled row records nothing — before,
                // the handler's fetch-back emitted a DUPLICATE
                // cancelled event on every repeat cancel.
                let reservation = Reservation::try_from(row)?;
                let event = stamp.event(
                    crate::events::RESERVATION_CANCELLED,
                    serde_json::to_value(&reservation).unwrap_or_default(),
                );
                boss_events::outbox::record_event_in_tx(&mut tx, &event)
                    .await
                    .map_err(CalendarError::Storage)?;
                tx.commit()
                    .await
                    .map_err(|e| CalendarError::Storage(e.to_string()))?;
            }
            None => {
                // Row could be missing OR already cancelled. Probe to
                // distinguish — NotFound is a different kind of
                // problem than no-op-cancel.
                let exists: Option<(Uuid,)> =
                    sqlx::query_as("SELECT id FROM calendar_reservations WHERE id = $1")
                        .bind(uuid)
                        .fetch_optional(&self.pool)
                        .await
                        .map_err(|e| CalendarError::Storage(e.to_string()))?;
                if exists.is_none() {
                    return Err(CalendarError::NotFound(id));
                }
                // Row existed and was already cancelled — Ok, no event.
            }
        }
        Ok(())
    }

    async fn cancel_by_reason_at(
        &self,
        reason_kind: &str,
        reason_ref_id: &str,
        _actor: &str,
        now: chrono::DateTime<chrono::Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<usize, CalendarError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| CalendarError::Storage(e.to_string()))?;
        // RETURNING enumerates exactly the rows this cascade flipped,
        // so the per-row events record in the SAME transaction as the
        // flips — replacing the handler's racy list-then-emit around
        // the UPDATE.
        let rows: Vec<ReservationRow> = sqlx::query_as(
            "UPDATE calendar_reservations
             SET cancelled_at = $3
             WHERE reason_kind = $1
               AND reason_ref_id = $2
               AND cancelled_at IS NULL
             RETURNING id, resource_kind, resource_id, start_ts, end_ts,
                       reason_kind, reason_ref_id, strength, notes,
                       created_by, created_at, cancelled_at",
        )
        .bind(reason_kind)
        .bind(reason_ref_id)
        .bind(now)
        .fetch_all(&mut *tx)
        .await
        .map_err(|e| CalendarError::Storage(e.to_string()))?;
        let n = rows.len();
        for row in rows {
            let reservation = Reservation::try_from(row)?;
            let event = stamp.event(
                crate::events::RESERVATION_CANCELLED,
                serde_json::to_value(&reservation).unwrap_or_default(),
            );
            boss_events::outbox::record_event_in_tx(&mut tx, &event)
                .await
                .map_err(CalendarError::Storage)?;
        }
        tx.commit()
            .await
            .map_err(|e| CalendarError::Storage(e.to_string()))?;
        Ok(n)
    }

    async fn get_business_calendar(
        &self,
        code: &str,
    ) -> Result<Option<BusinessCalendar>, CalendarError> {
        // The calendar header (None if no such code). `weekend` is a
        // SMALLINT[] — sqlx maps it to Vec<i16>.
        let header: Option<(String, Vec<i16>)> =
            sqlx::query_as("SELECT name, weekend FROM business_calendars WHERE code = $1")
                .bind(code)
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| CalendarError::Storage(e.to_string()))?;
        let Some((name, weekend)) = header else {
            return Ok(None);
        };
        // The concrete closed (non-business) dates for this calendar.
        let days: Vec<(NaiveDate,)> = sqlx::query_as(
            "SELECT day FROM business_calendar_closed_days WHERE calendar_code = $1 ORDER BY day",
        )
        .bind(code)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CalendarError::Storage(e.to_string()))?;

        let weekend: BTreeSet<u8> = weekend.into_iter().map(|d| d as u8).collect();
        let closed: BTreeSet<NaiveDate> = days.into_iter().map(|(d,)| d).collect();
        Ok(Some(BusinessCalendar {
            code: code.to_string(),
            name,
            weekend,
            closed,
        }))
    }

    async fn publish_business_calendars(
        &self,
        calendars: &[BusinessCalendar],
        mode: PublishMode,
    ) -> Result<BusinessCalendarsOutcome, CalendarError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| CalendarError::Storage(e.to_string()))?;
        let mut out = BusinessCalendarsOutcome {
            received: calendars.len(),
            ..Default::default()
        };
        for cal in calendars {
            // What the table holds for this code, read inside the
            // transaction so the outcome names the row the write saw.
            let held = held_in_tx(&mut tx, &cal.code).await?;
            // Insert-if-absent leaves a held row untouched — the
            // instance is the truth (design e187198f); take replaces
            // the header and the closed-day set wholesale.
            if held.is_none() || mode.is_take() {
                let weekend: Vec<i16> = cal.weekend.iter().map(|&w| w as i16).collect();
                sqlx::query(
                    "INSERT INTO business_calendars (code, name, weekend)
                     VALUES ($1, $2, $3)
                     ON CONFLICT (code) DO UPDATE
                       SET name = EXCLUDED.name,
                           weekend = EXCLUDED.weekend,
                           updated_at = NOW()",
                )
                .bind(&cal.code)
                .bind(&cal.name)
                .bind(&weekend)
                .execute(&mut *tx)
                .await
                .map_err(|e| CalendarError::Storage(e.to_string()))?;
                sqlx::query("DELETE FROM business_calendar_closed_days WHERE calendar_code = $1")
                    .bind(&cal.code)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| CalendarError::Storage(e.to_string()))?;
                for day in &cal.closed {
                    sqlx::query(
                        "INSERT INTO business_calendar_closed_days (calendar_code, day) VALUES ($1, $2)",
                    )
                    .bind(&cal.code)
                    .bind(day)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| CalendarError::Storage(e.to_string()))?;
                }
            }
            account_for(&mut out, held.as_ref(), cal, mode);
        }
        tx.commit()
            .await
            .map_err(|e| CalendarError::Storage(e.to_string()))?;
        Ok(out)
    }
}

/// The calendar the table holds for `code` (`None` when absent), with
/// its closed-day set, read inside the batch's transaction.
async fn held_in_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    code: &str,
) -> Result<Option<BusinessCalendar>, CalendarError> {
    let header: Option<(String, Vec<i16>)> =
        sqlx::query_as("SELECT name, weekend FROM business_calendars WHERE code = $1")
            .bind(code)
            .fetch_optional(&mut **tx)
            .await
            .map_err(|e| CalendarError::Storage(e.to_string()))?;
    let Some((name, weekend)) = header else {
        return Ok(None);
    };
    let days: Vec<(NaiveDate,)> = sqlx::query_as(
        "SELECT day FROM business_calendar_closed_days WHERE calendar_code = $1 ORDER BY day",
    )
    .bind(code)
    .fetch_all(&mut **tx)
    .await
    .map_err(|e| CalendarError::Storage(e.to_string()))?;
    Ok(Some(BusinessCalendar {
        code: code.to_string(),
        name,
        weekend: weekend.into_iter().map(|d| d as u8).collect(),
        closed: days.into_iter().map(|(d,)| d).collect(),
    }))
}

impl PgCalendar {
    /// Pull every active hard reservation on `resource` whose
    /// window overlaps `window`. Used to populate the body of a
    /// `Conflict` error after the GIST constraint rejects an
    /// INSERT.
    async fn find_overlapping(
        &self,
        subject: &Subject,
        window: TimeWindow,
    ) -> Result<Vec<Reservation>, CalendarError> {
        let rows: Vec<ReservationRow> = sqlx::query_as(
            "SELECT id, resource_kind, resource_id, start_ts, end_ts,
                    reason_kind, reason_ref_id, strength, notes,
                    created_by, created_at, cancelled_at
             FROM calendar_reservations
             WHERE cancelled_at IS NULL
               AND strength = 'hard'
               AND resource_kind = $1
               AND resource_id = $2
               AND start_ts < $4
               AND end_ts > $3
             ORDER BY start_ts",
        )
        .bind(&subject.kind)
        .bind(&subject.id)
        .bind(window.start)
        .bind(window.end)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| CalendarError::Storage(e.to_string()))?;
        rows.into_iter().map(Reservation::try_from).collect()
    }
}
