//! Postgres adapter for the scheduling port.

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::port::{SchedulingError, SchedulingRepository};
use super::types::{
    AssignmentKind, AssignmentStatus, AvailabilityKind, AvailabilitySource, NewScheduledAssignment,
    NewTechAvailability, ScheduledAssignment, TechAvailability, TechShiftPattern, WeekGridBlock,
    WeekGridRow,
};

pub struct PgScheduling {
    pool: PgPool,
}

impl PgScheduling {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn storage<E: std::fmt::Display>(e: E) -> SchedulingError {
    SchedulingError::Storage(e.to_string())
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct AvailRow {
    id: Uuid,
    employee_id: String,
    kind: String,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    notes: Option<String>,
    source: String,
    created_at: DateTime<Utc>,
}

fn avail_from(r: AvailRow) -> Result<TechAvailability, SchedulingError> {
    Ok(TechAvailability {
        id: r.id,
        employee_id: r.employee_id,
        kind: AvailabilityKind::parse(&r.kind)
            .ok_or_else(|| SchedulingError::Storage(format!("bad kind: {}", r.kind)))?,
        starts_at: r.starts_at,
        ends_at: r.ends_at,
        notes: r.notes,
        source: AvailabilitySource::parse(&r.source)
            .ok_or_else(|| SchedulingError::Storage(format!("bad source: {}", r.source)))?,
        created_at: r.created_at,
    })
}

#[derive(sqlx::FromRow)]
struct AssignRow {
    id: Uuid,
    tech_id: String,
    target_job_id: Uuid,
    kind: String,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    status: String,
    notes: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

fn assign_from(r: AssignRow) -> Result<ScheduledAssignment, SchedulingError> {
    Ok(ScheduledAssignment {
        id: r.id,
        tech_id: r.tech_id,
        target_job_id: r.target_job_id,
        kind: AssignmentKind::parse(&r.kind)
            .ok_or_else(|| SchedulingError::Storage(format!("bad kind: {}", r.kind)))?,
        starts_at: r.starts_at,
        ends_at: r.ends_at,
        status: AssignmentStatus::parse(&r.status)
            .ok_or_else(|| SchedulingError::Storage(format!("bad status: {}", r.status)))?,
        notes: r.notes,
        created_at: r.created_at,
        updated_at: r.updated_at,
    })
}

#[derive(sqlx::FromRow)]
struct ShiftRow {
    id: Uuid,
    employee_id: String,
    day_of_week: i16,
    starts_at_time: NaiveTime,
    ends_at_time: NaiveTime,
    timezone: String,
    effective_from: NaiveDate,
    effective_to: Option<NaiveDate>,
    created_at: DateTime<Utc>,
}

fn shift_from(r: ShiftRow) -> TechShiftPattern {
    TechShiftPattern {
        id: r.id,
        employee_id: r.employee_id,
        day_of_week: r.day_of_week,
        starts_at_time: r.starts_at_time,
        ends_at_time: r.ends_at_time,
        timezone: r.timezone,
        effective_from: r.effective_from,
        effective_to: r.effective_to,
        created_at: r.created_at,
    }
}

// ---------------------------------------------------------------------------
// Impl
// ---------------------------------------------------------------------------

#[async_trait]
impl SchedulingRepository for PgScheduling {
    async fn create_availability(
        &self,
        new: NewTechAvailability,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<TechAvailability, SchedulingError> {
        if new.ends_at <= new.starts_at {
            return Err(SchedulingError::BadRequest(
                "ends_at must be after starts_at".into(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let row: AvailRow = sqlx::query_as(
            "INSERT INTO tech_availability
                (employee_id, kind, starts_at, ends_at, notes, source)
             VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING id, employee_id, kind, starts_at, ends_at, notes, source, created_at",
        )
        .bind(new.employee_id)
        .bind(new.kind.as_str())
        .bind(new.starts_at)
        .bind(new.ends_at)
        .bind(new.notes)
        .bind(new.source.as_str())
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        let availability = avail_from(row)?;
        // OUTBOX (phase 2): the created event (full row state)
        // records with the row.
        let event = stamp.event(
            super::events::AVAILABILITY_CREATED,
            serde_json::to_value(&availability).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(SchedulingError::Storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(availability)
    }

    async fn list_availability(
        &self,
        employee_id: Option<&str>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<TechAvailability>, SchedulingError> {
        let rows: Vec<AvailRow> = sqlx::query_as(
            "SELECT id, employee_id, kind, starts_at, ends_at, notes, source, created_at
             FROM tech_availability
             WHERE ($1::text IS NULL OR employee_id = $1)
               AND starts_at < $3 AND ends_at > $2
             ORDER BY employee_id, starts_at",
        )
        .bind(employee_id)
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.into_iter().map(avail_from).collect()
    }

    async fn delete_availability(
        &self,
        id: Uuid,
        now: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), SchedulingError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let r = sqlx::query("DELETE FROM tech_availability WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        if r.rows_affected() == 0 {
            return Err(SchedulingError::NotFound(id.to_string()));
        }
        // OUTBOX (phase 2): records only after the row actually
        // deleted (NotFound above returns pre-recording).
        let event = stamp.event(
            super::events::AVAILABILITY_DELETED,
            serde_json::json!({"id": id, "deleted_at": now}),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(SchedulingError::Storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(())
    }

    async fn create_assignment(
        &self,
        new: NewScheduledAssignment,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<ScheduledAssignment, SchedulingError> {
        if new.ends_at <= new.starts_at {
            return Err(SchedulingError::BadRequest(
                "ends_at must be after starts_at".into(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let row: AssignRow = sqlx::query_as(
            "INSERT INTO scheduled_assignments
                (tech_id, target_job_id, kind, starts_at, ends_at, status, notes)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             RETURNING id, tech_id, target_job_id, kind, starts_at, ends_at,
                       status, notes, created_at, updated_at",
        )
        .bind(new.tech_id)
        .bind(new.target_job_id)
        .bind(new.kind.as_str())
        .bind(new.starts_at)
        .bind(new.ends_at)
        .bind(new.status.as_str())
        .bind(new.notes)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        let assignment = assign_from(row)?;
        // OUTBOX (phase 2): the created event (full row state)
        // records with the row.
        let event = stamp.event(
            super::events::ASSIGNMENT_CREATED,
            serde_json::to_value(&assignment).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(SchedulingError::Storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(assignment)
    }

    async fn get_assignment(
        &self,
        id: Uuid,
    ) -> Result<Option<ScheduledAssignment>, SchedulingError> {
        let row: Option<AssignRow> = sqlx::query_as(
            "SELECT id, tech_id, target_job_id, kind, starts_at, ends_at,
                    status, notes, created_at, updated_at
             FROM scheduled_assignments WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?;
        row.map(assign_from).transpose()
    }

    async fn list_assignments(
        &self,
        tech_id: Option<&str>,
        target_job_id: Option<Uuid>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<ScheduledAssignment>, SchedulingError> {
        let rows: Vec<AssignRow> = sqlx::query_as(
            "SELECT id, tech_id, target_job_id, kind, starts_at, ends_at,
                    status, notes, created_at, updated_at
             FROM scheduled_assignments
             WHERE ($1::text IS NULL OR tech_id = $1)
               AND ($2::uuid IS NULL OR target_job_id = $2)
               AND starts_at < $4 AND ends_at > $3
             ORDER BY tech_id, starts_at",
        )
        .bind(tech_id)
        .bind(target_job_id)
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.into_iter().map(assign_from).collect()
    }

    async fn update_assignment_status(
        &self,
        id: Uuid,
        status: AssignmentStatus,
        now: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), SchedulingError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        // The row bind and the record share ONE instant
        // (stamp.timestamp = the event's audit_log.timestamp), so a
        // replay reproduces this row byte-identically. NOW() here
        // minted a second wall reading the log never saw.
        let r = sqlx::query(
            "UPDATE scheduled_assignments
             SET status = $2, updated_at = $3
             WHERE id = $1",
        )
        .bind(id)
        .bind(status.as_str())
        .bind(stamp.timestamp)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        if r.rows_affected() == 0 {
            return Err(SchedulingError::NotFound(id.to_string()));
        }
        // OUTBOX (phase 2): records with the update.
        let event = stamp.event(
            super::events::ASSIGNMENT_STATUS_CHANGED,
            serde_json::json!({"id": id, "status": status, "changed_at": now}),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(SchedulingError::Storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(())
    }

    async fn delete_assignment(
        &self,
        id: Uuid,
        now: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), SchedulingError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let r = sqlx::query("DELETE FROM scheduled_assignments WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
        if r.rows_affected() == 0 {
            return Err(SchedulingError::NotFound(id.to_string()));
        }
        // OUTBOX (phase 2): records only after the row actually
        // deleted.
        let event = stamp.event(
            super::events::ASSIGNMENT_DELETED,
            serde_json::json!({"id": id, "deleted_at": now}),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(SchedulingError::Storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(())
    }

    async fn upsert_shift_pattern(
        &self,
        employee_id: &str,
        day_of_week: i16,
        starts_at_time: NaiveTime,
        ends_at_time: NaiveTime,
        timezone: &str,
        effective_from: NaiveDate,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<TechShiftPattern, SchedulingError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let row: ShiftRow = sqlx::query_as(
            "INSERT INTO tech_shift_patterns
                (employee_id, day_of_week, starts_at_time, ends_at_time,
                 timezone, effective_from)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (employee_id, day_of_week, effective_from) DO UPDATE
               SET starts_at_time = EXCLUDED.starts_at_time,
                   ends_at_time   = EXCLUDED.ends_at_time,
                   timezone       = EXCLUDED.timezone
             RETURNING id, employee_id, day_of_week, starts_at_time, ends_at_time,
                       timezone, effective_from, effective_to, created_at",
        )
        .bind(employee_id)
        .bind(day_of_week)
        .bind(starts_at_time)
        .bind(ends_at_time)
        .bind(timezone)
        .bind(effective_from)
        .fetch_one(&mut *tx)
        .await
        .map_err(storage)?;
        let pattern = shift_from(row);
        // OUTBOX (phase 2): the upserted event (full row state)
        // records with the row.
        let event = stamp.event(
            super::events::SHIFT_PATTERN_UPSERTED,
            serde_json::to_value(&pattern).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(SchedulingError::Storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(pattern)
    }

    async fn list_shift_patterns(
        &self,
        employee_id: Option<&str>,
    ) -> Result<Vec<TechShiftPattern>, SchedulingError> {
        let rows: Vec<ShiftRow> = sqlx::query_as(
            "SELECT id, employee_id, day_of_week, starts_at_time, ends_at_time,
                    timezone, effective_from, effective_to, created_at
             FROM tech_shift_patterns
             WHERE ($1::text IS NULL OR employee_id = $1)
             ORDER BY employee_id, day_of_week",
        )
        .bind(employee_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        Ok(rows.into_iter().map(shift_from).collect())
    }

    async fn materialize_shift_patterns(
        &self,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<i64, SchedulingError> {
        // For each active pattern, generate a day-by-day series and
        // insert tech_availability rows where the pattern's
        // day_of_week matches. Idempotent via the partial index: we
        // skip rows that already overlap an existing shift-pattern row
        // for the same employee at the same starts_at.
        let r = sqlx::query(
            r#"
            WITH days AS (
              SELECT generate_series($1::date, ($2::date - INTERVAL '1 day'), INTERVAL '1 day')::date AS d
            ),
            candidates AS (
              SELECT p.employee_id,
                     p.timezone,
                     (d.d + p.starts_at_time) AT TIME ZONE p.timezone AS starts_at,
                     (d.d + p.ends_at_time)   AT TIME ZONE p.timezone AS ends_at
              FROM tech_shift_patterns p
              CROSS JOIN days d
              WHERE EXTRACT(DOW FROM d.d)::smallint = p.day_of_week
                AND d.d >= p.effective_from
                AND (p.effective_to IS NULL OR d.d < p.effective_to)
            )
            INSERT INTO tech_availability
                (employee_id, kind, starts_at, ends_at, source)
            SELECT c.employee_id, 'available', c.starts_at, c.ends_at, 'shift-pattern'
            FROM candidates c
            WHERE NOT EXISTS (
              SELECT 1 FROM tech_availability a
              WHERE a.employee_id = c.employee_id
                AND a.source = 'shift-pattern'
                AND a.starts_at = c.starts_at
            )
            "#,
        )
        .bind(from)
        .bind(to)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(r.rows_affected() as i64)
    }

    async fn week_grid(
        &self,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        employee_ids: Option<&[String]>,
    ) -> Result<Vec<WeekGridRow>, SchedulingError> {
        // Pull availability + assignments joined to jobs (for title
        // + kind) over the window. We do two queries + one merge in
        // Rust rather than a single SQL UNION so each shape stays
        // strongly typed.
        let emp_filter = employee_ids.map(|v| v.to_vec());

        #[derive(sqlx::FromRow)]
        struct A {
            id: Uuid,
            employee_id: String,
            kind: String,
            starts_at: DateTime<Utc>,
            ends_at: DateTime<Utc>,
            notes: Option<String>,
        }
        let avails: Vec<A> = sqlx::query_as(
            "SELECT id, employee_id, kind, starts_at, ends_at, notes
             FROM tech_availability
             WHERE starts_at < $2 AND ends_at > $1
               AND ($3::text[] IS NULL OR employee_id = ANY($3))
             ORDER BY employee_id, starts_at",
        )
        .bind(from)
        .bind(to)
        .bind(emp_filter.as_deref())
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;

        #[derive(sqlx::FromRow)]
        struct S {
            id: Uuid,
            tech_id: String,
            target_job_id: Uuid,
            kind: String,
            status: String,
            starts_at: DateTime<Utc>,
            ends_at: DateTime<Utc>,
            notes: Option<String>,
            job_title: Option<String>,
            workflow: Option<String>,
        }
        let assigns: Vec<S> = sqlx::query_as(
            "SELECT s.id, s.tech_id, s.target_job_id, s.kind, s.status,
                    s.starts_at, s.ends_at, s.notes, j.title AS job_title, j.kind AS workflow
             FROM scheduled_assignments s
             LEFT JOIN jobs j ON j.id = s.target_job_id
             WHERE s.starts_at < $2 AND s.ends_at > $1
               AND ($3::text[] IS NULL OR s.tech_id = ANY($3))
               AND s.status IN ('tentative', 'confirmed')
             ORDER BY s.tech_id, s.starts_at",
        )
        .bind(from)
        .bind(to)
        .bind(emp_filter.as_deref())
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;

        // Merge into per-employee rows.
        use std::collections::BTreeMap;
        let mut by_emp: BTreeMap<String, Vec<WeekGridBlock>> = BTreeMap::new();
        for a in avails {
            let kind = AvailabilityKind::parse(&a.kind)
                .ok_or_else(|| SchedulingError::Storage(format!("bad avail kind: {}", a.kind)))?;
            by_emp
                .entry(a.employee_id)
                .or_default()
                .push(WeekGridBlock::Availability {
                    id: a.id,
                    kind,
                    starts_at: a.starts_at,
                    ends_at: a.ends_at,
                    notes: a.notes,
                });
        }
        for s in assigns {
            let kind = AssignmentKind::parse(&s.kind)
                .ok_or_else(|| SchedulingError::Storage(format!("bad assign kind: {}", s.kind)))?;
            let status = AssignmentStatus::parse(&s.status).ok_or_else(|| {
                SchedulingError::Storage(format!("bad assign status: {}", s.status))
            })?;
            by_emp
                .entry(s.tech_id)
                .or_default()
                .push(WeekGridBlock::Assignment {
                    id: s.id,
                    kind,
                    status,
                    target_job_id: s.target_job_id,
                    target_job_title: s.job_title,
                    target_workflow: s.workflow,
                    starts_at: s.starts_at,
                    ends_at: s.ends_at,
                    notes: s.notes,
                });
        }

        Ok(by_emp
            .into_iter()
            .map(|(employee_id, mut blocks)| {
                blocks.sort_by_key(|b| match b {
                    WeekGridBlock::Availability { starts_at, .. } => *starts_at,
                    WeekGridBlock::Assignment { starts_at, .. } => *starts_at,
                });
                WeekGridRow {
                    employee_id,
                    blocks,
                }
            })
            .collect())
    }

    async fn calendar_token_for(
        &self,
        employee_id: &str,
    ) -> Result<Option<String>, SchedulingError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT token FROM tech_calendar_tokens WHERE employee_id = $1")
                .bind(employee_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage)?;
        Ok(row.map(|(t,)| t))
    }

    async fn rotate_calendar_token(
        &self,
        employee_id: &str,
        new_token: &str,
        now: DateTime<Utc>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<(), SchedulingError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        // created_at binds the stamp's instant (= the event's
        // audit_log.timestamp) so the replayed row matches this one
        // byte-for-byte. NOW() minted a second wall reading.
        sqlx::query(
            "INSERT INTO tech_calendar_tokens (employee_id, token, created_at) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (employee_id) DO UPDATE \
               SET token = EXCLUDED.token, created_at = EXCLUDED.created_at",
        )
        .bind(employee_id)
        .bind(new_token)
        .bind(stamp.timestamp)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        // OUTBOX (phase 2): records with the rotation so a published
        // ICS URL keeps resolving after replay.
        let event = stamp.event(
            super::events::CALENDAR_TOKEN_ROTATED,
            serde_json::json!({
                "employee_id": employee_id,
                "token": new_token,
                "rotated_at": now,
            }),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(SchedulingError::Storage)?;
        tx.commit().await.map_err(storage)?;
        Ok(())
    }

    async fn employee_by_calendar_token(
        &self,
        token: &str,
    ) -> Result<Option<String>, SchedulingError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT employee_id FROM tech_calendar_tokens WHERE token = $1")
                .bind(token)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage)?;
        Ok(row.map(|(e,)| e))
    }
}
