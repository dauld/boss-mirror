//! Postgres adapter for the `JobsRepository` port.

use async_trait::async_trait;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::port::{
    AssignmentRow, JobFilter, JobScope, JobsError, JobsRepository, LaunchCalendarRow,
};

pub struct PgJobs {
    pool: PgPool,
    /// Connection URL retained alongside the pool so subprocess
    /// helpers (`boss-rebuild-all` for the demo-loop reset) can
    /// reuse it without parsing config or guessing.
    db_url: Option<String>,
    /// NATS URL retained for the same reason: the demo-loop reset
    /// purges the JetStream delivery buffer, and this service reads
    /// its broker address from config (`nats_url`), not from the
    /// environment — so guessing at `BOSS_NATS_URL` would silently
    /// skip the purge on every real deployment.
    nats_url: Option<String>,
}

impl PgJobs {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            db_url: None,
            nats_url: None,
        }
    }

    /// Constructor variant that retains the connection URLs for the
    /// demo-loop reset: `db_url` for the `boss-rebuild-all` subprocess,
    /// `nats_url` for the delivery-buffer purge. `db_url` falls back to
    /// `BOSS_POSTGRES_URL` or a hardcoded default when absent;
    /// `nats_url` falls back to `BOSS_NATS_URL`, and the purge is
    /// skipped if neither is set.
    pub fn with_urls(pool: PgPool, db_url: String, nats_url: String) -> Self {
        Self {
            pool,
            db_url: Some(db_url),
            nats_url: Some(nats_url),
        }
    }
}

// ---------------------------------------------------------------------------
// Row types
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct EstateNodeRow {
    id: String,
    label: String,
    address: String,
    role: String,
    cpu: Option<i32>,
    memory_gb: Option<i32>,
    disk_gb: Option<i32>,
    notes: Option<String>,
    retired: bool,
}

#[derive(sqlx::FromRow)]
struct JobRow {
    id: uuid::Uuid,
    kind: String,
    workflow_version: i32,
    subject_kind: String,
    subject_id: String,
    title: String,
    owner_id: String,
    status: String,
    priority: String,
    opened_on: chrono::NaiveDate,
    due_on: Option<chrono::NaiveDate>,
    closed_on: Option<chrono::NaiveDate>,
    metadata: serde_json::Value,
    tags: Vec<String>,
    simulated: bool,
}

#[derive(sqlx::FromRow)]
struct StepRow {
    id: uuid::Uuid,
    job_id: uuid::Uuid,
    kind: String,
    title: String,
    spec_slug: Option<String>,
    assignee_id: Option<String>,
    status: String,
    sort_order: i32,
    blocked_by: Vec<uuid::Uuid>,
    sign_offs_required: serde_json::Value,
    assurance_required: Option<String>,
    sign_offs: serde_json::Value,
    fields: serde_json::Value,
    completed_on: Option<chrono::NaiveDate>,
    metadata: serde_json::Value,
    notes: Option<String>,
    step_plugin_version: i32,
    embedded_job: Option<uuid::Uuid>,
}

/// Joined row backing [`PgJobs::list_assignments`] — a `StepRow`
/// (flattened) plus the minimum Job context the pull surface needs.
/// `j.kind` is aliased to `workflow` so it doesn't collide with the
/// flattened `StepRow.kind` (the step kind).
#[derive(sqlx::FromRow)]
struct AssignmentRowSql {
    #[sqlx(flatten)]
    step: StepRow,
    job_title: String,
    due_on: Option<chrono::NaiveDate>,
    workflow: String,
    workflow_version: i32,
    subject_kind: String,
    subject_id: String,
    priority: String,
    simulated: bool,
    tags: Vec<String>,
}

// ---------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------

fn row_to_job(r: JobRow) -> Job {
    Job {
        id: JobId::from_uuid(r.id),
        kind: r.kind,
        workflow_version: r.workflow_version,
        subject: parse_subject(&r.subject_kind, &r.subject_id),
        title: r.title,
        owner_id: r.owner_id,
        status: parse_job_status(&r.status),
        priority: parse_priority(&r.priority),
        opened_on: r.opened_on,
        due_on: r.due_on,
        closed_on: r.closed_on,
        metadata: r.metadata,
        tags: r.tags,
        simulated: r.simulated,
    }
}

fn row_to_step(r: StepRow) -> Result<Step, JobsError> {
    Ok(Step {
        id: StepId::from_uuid(r.id),
        job_id: JobId::from_uuid(r.job_id),
        kind: r.kind,
        title: r.title,
        spec_slug: r.spec_slug,
        assignee_id: r.assignee_id,
        status: parse_step_status(&r.status).ok_or_else(|| step_status_err(&r.status))?,
        sort_order: r.sort_order,
        blocked_by: r.blocked_by.into_iter().map(StepId::from_uuid).collect(),
        sign_offs_required: serde_json::from_value(r.sign_offs_required).unwrap_or_default(),
        // An unrecognised value reads as None — "the kind's floor" —
        // rather than failing the whole row. A step whose requirement
        // cannot be parsed is still a step someone needs to see.
        assurance_required: r
            .assurance_required
            .as_deref()
            .and_then(|v| serde_json::from_value(serde_json::Value::String(v.to_string())).ok()),
        sign_offs: serde_json::from_value(r.sign_offs).unwrap_or_default(),
        fields: serde_json::from_value(r.fields).unwrap_or_default(),
        completed_on: r.completed_on,
        metadata: r.metadata,
        notes: r.notes,
        step_plugin_version: r.step_plugin_version,
        embedded_job: r.embedded_job.map(JobId::from_uuid),
    })
}

/// Outbound serializer — Subject → `(subject_kind, subject_id)`
/// tuple for SQL bind on INSERT/UPDATE.
///
/// Wave 7: calls the trait's `kind()` and `id()` directly. Both
/// return slices borrowed from the Subject (Wave 7 widened
/// `kind()` from `&'static str` to `&str` so CustomSubject can
/// surface its runtime `custom_kind`).
///
/// Pairs with [`parse_subject`] below, which is the explicit
/// row → Subject entry point.
pub(crate) fn subject_parts(s: &impl boss_core::primitives::Subject) -> (&str, &str) {
    (s.kind(), s.id())
}

/// Inbound deserializer — `(subject_kind, subject_id)` columns
/// → Subject.
///
/// Trivial: Subject is a `(kind, id)` tuple, so any non-empty pair
/// round-trips unchanged — there's no kind→variant `match`.
/// Validation against the subject_kinds registry happens at write
/// time (`check_custom_subject` in the HTTP handler); reads trust
/// the row.
fn parse_subject(kind: &str, ref_id: &str) -> Subject {
    Subject::new(kind, ref_id)
}

fn parse_job_status(s: &str) -> JobStatus {
    match s {
        "draft" => JobStatus::Draft,
        "open" => JobStatus::Open,
        "blocked" => JobStatus::Blocked,
        "pending-sign-off" => JobStatus::PendingSignOff,
        "closed" => JobStatus::Closed,
        "cancelled" => JobStatus::Cancelled,
        _ => JobStatus::Draft,
    }
}

pub(crate) fn job_status_str(s: JobStatus) -> &'static str {
    match s {
        JobStatus::Draft => "draft",
        JobStatus::Open => "open",
        JobStatus::Blocked => "blocked",
        JobStatus::PendingSignOff => "pending-sign-off",
        JobStatus::Closed => "closed",
        JobStatus::Cancelled => "cancelled",
    }
}

fn parse_priority(s: &str) -> Priority {
    match s {
        "emergency" => Priority::Emergency,
        "urgent" => Priority::Urgent,
        "standard" => Priority::Standard,
        "scheduled" => Priority::Scheduled,
        _ => Priority::Standard,
    }
}

pub(crate) fn priority_str(p: Priority) -> &'static str {
    match p {
        Priority::Emergency => "emergency",
        Priority::Urgent => "urgent",
        Priority::Standard => "standard",
        Priority::Scheduled => "scheduled",
    }
}

/// Strict inverse of [`step_status_str`] — for DB round-trips, where
/// the value was written by it and the schema CHECK pins the column
/// to exactly these five. `None` means storage corruption (or a
/// schema/enum drift), which callers surface as a Storage error —
/// the old silent `_ => Pending` catch-all (BC2) quietly reanimated
/// any garbage row as a pending step.
fn parse_step_status(s: &str) -> Option<StepStatus> {
    match s {
        "pending" => Some(StepStatus::Pending),
        "ready" => Some(StepStatus::Ready),
        "active" => Some(StepStatus::Active),
        "completed" => Some(StepStatus::Completed),
        "skipped" => Some(StepStatus::Skipped),
        _ => None,
    }
}

fn step_status_err(s: &str) -> JobsError {
    JobsError::Storage(format!(
        "steps.status holds {s:?} — outside the schema CHECK vocabulary; \
         storage corruption or a schema/enum drift"
    ))
}

pub(crate) fn step_status_str(s: StepStatus) -> &'static str {
    match s {
        StepStatus::Pending => "pending",
        StepStatus::Ready => "ready",
        StepStatus::Active => "active",
        StepStatus::Completed => "completed",
        StepStatus::Skipped => "skipped",
    }
}

pub(crate) fn blocked_by_uuids(ids: &[StepId]) -> Vec<uuid::Uuid> {
    ids.iter().map(|id| *id.inner().as_uuid()).collect()
}

/// Look up the version of the currently-active step plugin for `kind`.
/// Zero when no plugin serves this kind (the step renders through an
/// in-tree surface like repair/inspection/billing), which is the same
/// value the column defaults to on INSERT without this call.
async fn active_plugin_version(pool: &sqlx::PgPool, kind: &str) -> Result<i32, JobsError> {
    let row: Option<(i32,)> =
        sqlx::query_as("SELECT version FROM step_plugins WHERE kind = $1 AND status = 'active'")
            .bind(kind)
            .fetch_optional(pool)
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
    Ok(row.map(|(v,)| v).unwrap_or(0))
}

// ---------------------------------------------------------------------------
// Trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl JobsRepository for PgJobs {
    async fn create_job_at(
        &self,
        job: &Job,
        now: chrono::DateTime<chrono::Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        let (subj_kind, subj_ref) = subject_parts(&job.subject);
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        // Birth-by-job subject kinds (`metadata.birth = "job"` in the
        // SubjectKind registry: `workflow`, `custom`) have no domain
        // table — this Job IS the subject's birth record, so its
        // identity row is minted here, in the same transaction as the
        // job insert (Q1 write-through). Domain kinds don't match the
        // WHERE and mint nothing; their identity belongs to the domain
        // write-through, and the existence gate upstream rejects
        // ghosts. The rebuilder's `jobs.job.created` pass reproduces
        // exactly these rows from the log.
        sqlx::query(
            "INSERT INTO subjects (kind, id) \
             SELECT sk.kind, $2 FROM subject_kinds sk \
              WHERE sk.kind = $1 AND sk.retired_at IS NULL \
                AND sk.metadata->>'birth' = 'job' \
             ON CONFLICT (kind, id) DO NOTHING",
        )
        .bind(subj_kind)
        .bind(subj_ref)
        .execute(&mut *tx)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        // ON CONFLICT DO NOTHING — re-emission of an existing
        // Job (replay path, deterministic-UUID sim runs) is a
        // no-op rather than a 500. update_job_at is the path for
        // intentional changes; create_job_at represents "make
        // sure this Job exists with its initial state."
        let result = sqlx::query(
            r#"
            INSERT INTO jobs (id, kind, subject_kind, subject_id, title, owner_id,
                              status, priority, opened_on, due_on, closed_on, metadata, tags,
                              workflow_version, created_at, updated_at, simulated)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $15, $16)
            ON CONFLICT (id) DO NOTHING
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
        .bind(job.due_on)
        .bind(job.closed_on)
        .bind(&job.metadata)
        .bind(&job.tags)
        .bind(job.workflow_version)
        .bind(now)
        // Decided once, at ADMISSION (create_job in http/jobs.rs:
        // explicit body flag OR sim-chain origin) — and never
        // revisited. The adapter persists the Job it was handed so
        // the row can never disagree with the JOB_CREATED payload
        // recorded beside it; everything downstream (steps, side
        // effects, an operator poking at it later) is simulated iff
        // the Job is.
        .bind(job.simulated)
        .execute(&mut *tx)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        // OUTBOX (phase 2): the caller's events (JOB_CREATED) record
        // with the row — and only when the INSERT actually inserted.
        // The ON CONFLICT replay guard doubles as the event gate: a
        // re-emitted Job (deterministic sim runs) records nothing.
        if result.rows_affected() > 0 {
            for event in events {
                boss_events::outbox::record_event_in_tx(&mut tx, event)
                    .await
                    .map_err(JobsError::Storage)?;
            }
        }
        tx.commit()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn get_job(&self, id: &JobId) -> Result<Option<Job>, JobsError> {
        let row = sqlx::query_as::<_, JobRow>(
            "SELECT id, kind, workflow_version, subject_kind, subject_id, title, owner_id, status, priority, opened_on, due_on, closed_on, metadata, tags, simulated FROM jobs WHERE id = $1",
        )
        .bind(*id.inner().as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(row.map(row_to_job))
    }

    async fn resolve_job_id_prefix(&self, prefix: &str) -> Result<Vec<JobId>, JobsError> {
        // LIKE on the canonical text form; LIMIT 2 is all the caller
        // needs to tell one match from many, and keeps a short prefix
        // from scanning the table. `%` and `_` cannot appear in the
        // hex-and-hyphen prefix the handler admits, so no escaping is
        // needed — but the prefix is still bound, never interpolated.
        let ids = sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT id FROM jobs WHERE id::text LIKE $1 || '%' LIMIT 2",
        )
        .bind(prefix)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(ids.into_iter().map(JobId::from_uuid).collect())
    }

    async fn update_job_at(
        &self,
        job: &Job,
        now: chrono::DateTime<chrono::Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        let (subj_kind, subj_ref) = subject_parts(&job.subject);
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        // `simulated` is deliberately absent from the SET list: a
        // Job's origin is decided at admission and never revisited.
        // The storage enforces the immutability rather than trusting
        // every caller to (same rule as rebuild.rs's upsert).
        let result = sqlx::query(
            r#"
            UPDATE jobs SET kind = $2, subject_kind = $3, subject_id = $4,
                title = $5, owner_id = $6, status = $7, priority = $8,
                opened_on = $9, due_on = $10, closed_on = $11, metadata = $12,
                tags = $13, updated_at = $14
            WHERE id = $1
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
        .bind(job.due_on)
        .bind(job.closed_on)
        .bind(&job.metadata)
        .bind(&job.tags)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(JobsError::NotFound(job.id));
        }
        // OUTBOX (phase 2): the caller's events (JOB_UPDATED + status
        // markers) record with the row (the NotFound above returns
        // pre-recording).
        for event in events {
            boss_events::outbox::record_event_in_tx(&mut tx, event)
                .await
                .map_err(JobsError::Storage)?;
        }
        tx.commit()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn merge_job_metadata_at(
        &self,
        id: &JobId,
        patch: &serde_json::Map<String, serde_json::Value>,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<Job, JobsError> {
        // Split the overlay once: null values are removals, everything
        // else upserts. Top-level only — that is the whole of the
        // contract, matching the conductor's `overlay_metadata`.
        let removals: Vec<String> = patch
            .iter()
            .filter(|(_, v)| v.is_null())
            .map(|(k, _)| k.clone())
            .collect();
        let upserts: serde_json::Map<String, serde_json::Value> = patch
            .iter()
            .filter(|(_, v)| !v.is_null())
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        // ONE statement is the atomicity: the merge happens against
        // the row as it stands at write time, never against a copy a
        // caller fetched earlier. The CASE folds a non-object metadata
        // (jsonb null, or a fresh row) to `{}` so `||` concatenates
        // instead of erroring; `- text[]` is the null-removes half.
        let row = sqlx::query_as::<_, JobRow>(
            r#"
            UPDATE jobs SET
                metadata = (CASE WHEN jsonb_typeof(metadata) = 'object'
                                 THEN metadata ELSE '{}'::jsonb END
                            || $2::jsonb) - $3::text[],
                updated_at = $4
            WHERE id = $1
            RETURNING id, kind, workflow_version, subject_kind, subject_id, title, owner_id,
                      status, priority, opened_on, due_on, closed_on, metadata, tags, simulated
            "#,
        )
        .bind(*id.inner().as_uuid())
        .bind(serde_json::Value::Object(upserts))
        .bind(&removals)
        .bind(stamp.timestamp)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        let Some(row) = row else {
            return Err(JobsError::NotFound(*id));
        };
        let job = row_to_job(row);
        // OUTBOX (phase 2): the JOB_UPDATED state event is built from
        // the POST-merge row this transaction just produced — the
        // rebuild consumes it as full row state, so it must be the
        // row, not an optimistic copy — and records with it.
        let event = stamp.event(
            crate::events::JOB_UPDATED,
            serde_json::to_value(&job).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(JobsError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(job)
    }

    async fn list_estate_nodes(&self) -> Result<Vec<crate::port::EstateNode>, JobsError> {
        let rows = sqlx::query_as::<_, EstateNodeRow>(
            r#"
            SELECT id, label, address, role, cpu, memory_gb, disk_gb, notes,
                   (retired_at IS NOT NULL) AS retired
            FROM nodes
            ORDER BY role, id
            "#,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|r| crate::port::EstateNode {
                id: r.id,
                label: r.label,
                address: r.address,
                role: r.role,
                cpu: r.cpu,
                memory_gb: r.memory_gb,
                disk_gb: r.disk_gb,
                notes: r.notes,
                retired: r.retired,
            })
            .collect())
    }

    async fn recent_events_by_kind(
        &self,
        kind: &str,
        limit: i64,
    ) -> Result<Vec<serde_json::Value>, JobsError> {
        // The SQL lives in boss-events, which owns audit_log — this
        // crate already writes through its `record_event_in_tx`, and
        // reading through its helper keeps the table's ownership in
        // one place rather than growing a second copy of the query.
        let rows = boss_events::tail_http::recent_by_kind(&self.pool, kind, limit)
            .await
            .map_err(JobsError::Storage)?;
        rows.into_iter()
            .map(|r| serde_json::to_value(r).map_err(|e| JobsError::Storage(e.to_string())))
            .collect()
    }

    async fn repin_workflow_version_at(
        &self,
        id: &JobId,
        to_version: i32,
        stamp: &boss_core::publisher::EventStamp,
    ) -> Result<Job, JobsError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        // The one column update_job deliberately cannot reach, in its
        // own statement, so re-pinning is always an explicit act.
        let row = sqlx::query_as::<_, JobRow>(
            r#"
            UPDATE jobs SET workflow_version = $2, updated_at = $3
            WHERE id = $1
            RETURNING id, kind, workflow_version, subject_kind, subject_id, title, owner_id,
                      status, priority, opened_on, due_on, closed_on, metadata, tags, simulated
            "#,
        )
        .bind(*id.inner().as_uuid())
        .bind(to_version)
        .bind(stamp.timestamp)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        let Some(row) = row else {
            return Err(JobsError::NotFound(*id));
        };
        let job = row_to_job(row);
        let event = stamp.event(
            crate::events::JOB_UPDATED,
            serde_json::to_value(&job).unwrap_or_default(),
        );
        boss_events::outbox::record_event_in_tx(&mut tx, &event)
            .await
            .map_err(JobsError::Storage)?;
        tx.commit()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(job)
    }

    async fn list_jobs(
        &self,
        filter: &JobFilter,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<Job>, i64), JobsError> {
        // Short-circuit when policy says "caller sees nothing" — no
        // point taking a DB round-trip.
        if matches!(filter.scope, JobScope::None) {
            return Ok((Vec::new(), 0));
        }

        // Static query with NULL-OR predicates — sidesteps dynamic
        // query construction. `$4` is the kind_prefix pattern already
        // formatted with a trailing %; pass NULL when no prefix.
        // Policy-scope binds ($7..$9) default to NULL / empty arrays
        // when scope is `All`, which the NULL-OR guards short-circuit.
        let prefix_pattern = filter.kind_prefix.as_ref().map(|p| format!("{p}%"));

        // Translate the scope into three mutually-exclusive parameter
        // sets. Exactly one of scope_owner / scope_owners /
        // scope_accounts is non-NULL for a scoped call; all three
        // are NULL for `All`.
        let (scope_owner, scope_owners, scope_accounts): (
            Option<&str>,
            Option<Vec<String>>,
            Option<Vec<String>>,
        ) = match &filter.scope {
            JobScope::All => (None, None, None),
            JobScope::None => unreachable!("short-circuited above"),
            JobScope::OwnerIs(u) => (Some(u.as_str()), None, None),
            JobScope::OwnerIn(us) => (None, Some(us.clone()), None),
            JobScope::AccountIn(ps) => (None, None, Some(ps.clone())),
        };

        // $10 is the caller-supplied subject_id filter (asset_id
        // or account_id from the HTTP layer). Before this bind
        // existed, the SQL ignored `filter.subject_id` entirely
        // and returned the full policy-scoped set — which is why
        // /api/jobs?account_id=foo looked empty on every detail page.
        let list_sql = r#"
            SELECT id, kind, workflow_version, subject_kind, subject_id, title, owner_id, status,
                   priority, opened_on, due_on, closed_on, metadata, tags, simulated
            FROM jobs
            WHERE ($1::text IS NULL OR kind = $1)
              -- $13 is the terminal retention window. With it, $2 is
              -- no longer a plain equality: a board wants live packets
              -- OR recently-closed ones, which is an OR across two
              -- different columns and cannot be expressed by status
              -- alone.
              AND (
                CASE WHEN $13::date IS NULL
                     THEN ($2::text IS NULL OR status = $2)
                     ELSE (status NOT IN ('closed', 'cancelled')
                           OR closed_on >= $13::date)
                END
              )
              AND ($3::text IS NULL OR owner_id = $3)
              AND ($4::text IS NULL OR kind LIKE $4)
              AND ($7::text IS NULL OR owner_id = $7)
              AND ($8::text[] IS NULL OR owner_id = ANY($8))
              AND (
                $9::text[] IS NULL
                OR (subject_kind IN ('account', 'employee')
                    AND subject_id = ANY($9))
              )
              AND ($10::text IS NULL OR subject_id = $10)
              -- $11 is the BLOCKER's full id: a waiter matches on the
              -- full id or a >= 8-char prefix of it, mirroring
              -- job_edge_resolves so every accepted wait is findable.
              AND (
                $11::text IS NULL
                OR metadata->>'waiting_on' = $11
                OR (length(metadata->>'waiting_on') >= 8
                    AND $11 LIKE (metadata->>'waiting_on') || '%')
              )
              -- $12 is a JSONB containment document (a station
              -- predicate's bound `metadata_equals`): every key/value
              -- in it must be present on the packet.
              AND ($12::jsonb IS NULL OR metadata @> $12::jsonb)
              -- $14 partitions real work from the demo tenant's.
              -- Pushed into SQL, not applied to the page: 87% of
              -- packets are simulated, so a post-fetch filter returns
              -- a nearly empty page and a wrong total.
              AND ($14::bool IS NULL OR simulated = $14)
            ORDER BY opened_on DESC
            LIMIT $5 OFFSET $6
        "#;

        let rows = sqlx::query_as::<_, JobRow>(list_sql)
            .bind(filter.kind.as_deref())
            .bind(filter.status.map(job_status_str))
            .bind(filter.owner_id.as_deref())
            .bind(prefix_pattern.as_deref())
            .bind(limit)
            .bind(offset)
            .bind(scope_owner)
            .bind(scope_owners.as_deref())
            .bind(scope_accounts.as_deref())
            .bind(filter.subject_id.as_deref())
            .bind(filter.waiting_on.as_deref())
            .bind(filter.metadata_contains.as_ref())
            .bind(filter.closed_since)
            .bind(filter.simulated)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;

        let total: i64 = sqlx::query_scalar(
            r#"
            SELECT COUNT(*) FROM jobs
            WHERE ($1::text IS NULL OR kind = $1)
              AND (
                CASE WHEN $11::date IS NULL
                     THEN ($2::text IS NULL OR status = $2)
                     ELSE (status NOT IN ('closed', 'cancelled')
                           OR closed_on >= $11::date)
                END
              )
              AND ($3::text IS NULL OR owner_id = $3)
              AND ($4::text IS NULL OR kind LIKE $4)
              AND ($5::text IS NULL OR owner_id = $5)
              AND ($6::text[] IS NULL OR owner_id = ANY($6))
              AND (
                $7::text[] IS NULL
                OR (subject_kind IN ('account', 'employee')
                    AND subject_id = ANY($7))
              )
              AND ($8::text IS NULL OR subject_id = $8)
              AND (
                $9::text IS NULL
                OR metadata->>'waiting_on' = $9
                OR (length(metadata->>'waiting_on') >= 8
                    AND $9 LIKE (metadata->>'waiting_on') || '%')
              )
              AND ($10::jsonb IS NULL OR metadata @> $10::jsonb)
              -- Same partition as the list query, so `total` agrees
              -- with the rows actually returned.
              AND ($12::bool IS NULL OR simulated = $12)
            "#,
        )
        .bind(filter.kind.as_deref())
        .bind(filter.status.map(job_status_str))
        .bind(filter.owner_id.as_deref())
        .bind(prefix_pattern.as_deref())
        .bind(scope_owner)
        .bind(scope_owners.as_deref())
        .bind(scope_accounts.as_deref())
        .bind(filter.subject_id.as_deref())
        .bind(filter.waiting_on.as_deref())
        .bind(filter.metadata_contains.as_ref())
        .bind(filter.closed_since)
        .bind(filter.simulated)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;

        Ok((rows.into_iter().map(row_to_job).collect(), total))
    }

    async fn add_step_at(
        &self,
        step: &Step,
        now: chrono::DateTime<chrono::Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        // Snapshot the current active plugin version for this step kind
        // so republishing the plugin later doesn't retroactively change
        // which bundle the step is pinned against. Caller-supplied
        // non-zero values win (bulk replay seeding its own versions);
        // zero triggers the lookup.
        let version = if step.step_plugin_version != 0 {
            step.step_plugin_version
        } else {
            active_plugin_version(&self.pool, &step.kind).await?
        };

        // ON CONFLICT DO NOTHING for the same reason as
        // create_job_at: replay paths (deterministic sim runs)
        // re-emit Steps; update_step_at handles intentional
        // changes. Without this an idempotent retry 500's.
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        let result = sqlx::query(
            r#"
            INSERT INTO steps (id, job_id, kind, title, spec_slug, assignee_id, status, sort_order,
                               blocked_by, sign_offs_required, assurance_required, sign_offs, fields,
                               completed_on, metadata, notes, step_plugin_version,
                               embedded_job, created_at, updated_at, became_ready_at)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $19,
                    -- Born ready IS the ready flip: a step materialized
                    -- straight into `ready` (the open-time readiness
                    -- pass) became an obligation at this INSERT. The
                    -- queue-age lens (2a0b034e) reads this stamp.
                    CASE WHEN $7 = 'ready' THEN $19 END)
            ON CONFLICT (id) DO NOTHING
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
        .bind(version)
        .bind(step.embedded_job.map(|j| *j.inner().as_uuid()))
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        // OUTBOX (phase 2): the caller's events (STEP_CREATED) record
        // with the row — only when the INSERT actually inserted (the
        // replay guard doubles as the event gate).
        if result.rows_affected() > 0 {
            for event in events {
                boss_events::outbox::record_event_in_tx(&mut tx, event)
                    .await
                    .map_err(JobsError::Storage)?;
            }
        }
        tx.commit()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn get_step(&self, id: &StepId) -> Result<Option<Step>, JobsError> {
        let row = sqlx::query_as::<_, StepRow>(
            "SELECT id, job_id, kind, title, spec_slug, assignee_id, status, sort_order, blocked_by, sign_offs_required, assurance_required, sign_offs, fields, completed_on, metadata, notes, step_plugin_version, embedded_job FROM steps WHERE id = $1",
        )
        .bind(*id.inner().as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        row.map(row_to_step).transpose()
    }

    async fn update_step_at(
        &self,
        step: &Step,
        now: chrono::DateTime<chrono::Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        let result = sqlx::query(
            r#"
            UPDATE steps SET kind = $2, title = $3, assignee_id = $4,
                -- Terminal statuses are immutable at the row (the
                -- state-machine invariant): a write whose merge was
                -- computed against a pre-completion fetch (dispatcher
                -- assign retries, JetStream redeliveries, any racing
                -- read-modify-write) cannot demote a Completed/Skipped
                -- step back to live.
                status = CASE
                    WHEN status IN ('completed', 'skipped') THEN status
                    ELSE $5
                END,
                completed_on = CASE
                    WHEN status IN ('completed', 'skipped') THEN completed_on
                    ELSE $8
                END,
                sort_order = $6, blocked_by = $7,
                metadata = CASE
                    WHEN status IN ('completed', 'skipped') THEN metadata
                    ELSE $9
                END,
                notes = $10, embedded_job = $11, updated_at = $12,
                -- The ready stamp is written ONCE, at the write that
                -- lands the step in `ready` (a pending → ready
                -- promotion arrives here), and no later write moves it
                -- — the property `updated_at` cannot have, and the one
                -- the queue-age lens (2a0b034e) exists to read. The
                -- inner CASE reads the OLD `status`: a terminal row
                -- keeps its status above, so it must not gain a stamp
                -- here either.
                became_ready_at = COALESCE(became_ready_at, CASE
                    WHEN status NOT IN ('completed', 'skipped')
                         AND $5 = 'ready' THEN $12
                END),
                -- The authored completion contract (`fields`) takes
                -- the same freeze as metadata: live rows accept the
                -- write, terminal rows keep theirs. This column was
                -- absent from the list entirely, so a 204'd update
                -- silently dropped it and every step's contract was
                -- write-once at materialization (a07cfddd).
                fields = CASE
                    WHEN status IN ('completed', 'skipped') THEN fields
                    ELSE $13
                END
            WHERE id = $1
            "#,
        )
        .bind(*step.id.inner().as_uuid())
        .bind(&step.kind)
        .bind(&step.title)
        .bind(&step.assignee_id)
        .bind(step_status_str(step.status))
        .bind(step.sort_order)
        .bind(blocked_by_uuids(&step.blocked_by))
        .bind(step.completed_on)
        .bind(&step.metadata)
        .bind(&step.notes)
        .bind(step.embedded_job.map(|j| *j.inner().as_uuid()))
        .bind(now)
        .bind(serde_json::to_value(&step.fields).unwrap_or_default())
        .execute(&mut *tx)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;

        if result.rows_affected() == 0 {
            return Err(JobsError::StepNotFound(step.id));
        }
        // OUTBOX (phase 2): the caller's events (STEP_UPDATED +
        // completion/ready/done markers) record with the row.
        for event in events {
            boss_events::outbox::record_event_in_tx(&mut tx, event)
                .await
                .map_err(JobsError::Storage)?;
        }
        tx.commit()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn claim_step_at(
        &self,
        step_id: &StepId,
        actor: &str,
        now: chrono::DateTime<chrono::Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<Step, JobsError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        // The compare half of the CAS lives in the WHERE clause, so
        // two racing claims serialize on the row lock and exactly one
        // sees a matching predicate. Idempotent re-claim by the
        // holder matches too (ready or already active).
        let row = sqlx::query(
            r#"
            UPDATE steps SET assignee_id = $2, status = 'active', updated_at = $3
            WHERE id = $1
              AND (
                    (status = 'ready' AND (assignee_id IS NULL OR assignee_id = $2))
                 OR (status = 'active' AND assignee_id = $2)
              )
            RETURNING id
            "#,
        )
        .bind(*step_id.inner().as_uuid())
        .bind(actor)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;

        if row.is_none() {
            // Lost the race (or the step was never claimable). Read
            // the row in the same tx so the conflict names the truth
            // the claimant collided with.
            let cur: Option<(Option<String>, String)> =
                sqlx::query_as("SELECT assignee_id, status FROM steps WHERE id = $1")
                    .bind(*step_id.inner().as_uuid())
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(|e| JobsError::Storage(e.to_string()))?;
            return match cur {
                None => Err(JobsError::StepNotFound(*step_id)),
                Some((holder, status)) => Err(JobsError::ClaimConflict { holder, status }),
            };
        }

        for event in events {
            boss_events::outbox::record_event_in_tx(&mut tx, event)
                .await
                .map_err(JobsError::Storage)?;
        }
        tx.commit()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        self.get_step(step_id)
            .await?
            .ok_or(JobsError::StepNotFound(*step_id))
    }

    async fn append_sign_off(
        &self,
        step_id: &StepId,
        stamp: &boss_core::job::SignOffStamp,
        now: chrono::DateTime<chrono::Utc>,
        events: &[boss_core::event::Event],
    ) -> Result<(), JobsError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        let result = sqlx::query(
            "UPDATE steps SET sign_offs = sign_offs || $2::jsonb, updated_at = $3 \
             WHERE id = $1",
        )
        .bind(*step_id.inner().as_uuid())
        .bind(serde_json::to_value(stamp).map_err(|e| JobsError::Storage(e.to_string()))?)
        .bind(now)
        .execute(&mut *tx)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        if result.rows_affected() == 0 {
            return Err(JobsError::StepNotFound(*step_id));
        }
        // OUTBOX (phase 2): the caller's STEP_SIGNED_OFF marker
        // records with the stamp append.
        for event in events {
            boss_events::outbox::record_event_in_tx(&mut tx, event)
                .await
                .map_err(JobsError::Storage)?;
        }
        tx.commit()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn record_events(&self, events: &[boss_core::event::Event]) -> Result<(), JobsError> {
        if events.is_empty() {
            return Ok(());
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        for event in events {
            boss_events::outbox::record_event_in_tx(&mut tx, event)
                .await
                .map_err(JobsError::Storage)?;
        }
        tx.commit()
            .await
            .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn list_steps(&self, job_id: &JobId) -> Result<Vec<Step>, JobsError> {
        let rows = sqlx::query_as::<_, StepRow>(
            "SELECT id, job_id, kind, title, spec_slug, assignee_id, status, sort_order, blocked_by, sign_offs_required, assurance_required, sign_offs, fields, completed_on, metadata, notes, step_plugin_version, embedded_job FROM steps WHERE job_id = $1 ORDER BY sort_order",
        )
        .bind(*job_id.inner().as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        rows.into_iter().map(row_to_step).collect()
    }

    async fn list_assignments(
        &self,
        assignee_id: Option<&str>,
        roles: &[String],
        limit: i64,
    ) -> Result<Vec<AssignmentRow>, JobsError> {
        // One indexed JOIN: open Jobs × their workable steps, filtered
        // to (assigned-to-me) OR (unassigned with a role I hold). The
        // `authority_role` lives in step metadata JSONB. Ordered by
        // (opened_on, sort_order) for a stable executor queue.
        let rows = sqlx::query_as::<_, AssignmentRowSql>(
            "SELECT s.id, s.job_id, s.kind, s.title, s.spec_slug, s.assignee_id, s.status, \
                    s.sort_order, s.blocked_by, s.sign_offs_required, s.assurance_required, s.sign_offs, \
                    s.fields, s.completed_on, s.metadata, s.notes, \
                    s.step_plugin_version, s.embedded_job, \
                    j.title AS job_title, j.due_on, j.kind AS workflow, j.workflow_version, \
                    j.subject_kind, j.subject_id, j.priority, \
                    j.simulated, j.tags \
             FROM steps s \
             JOIN jobs j ON s.job_id = j.id \
             WHERE j.status = 'open' \
               AND s.status IN ('ready', 'active') \
               AND ( \
                     ($1::text IS NOT NULL AND s.assignee_id = $1) \
                  OR ( (s.metadata ->> 'authority_role') = ANY($2) \
                       AND (s.assignee_id IS NULL OR s.status = 'active') ) \
               ) \
             ORDER BY j.opened_on, s.sort_order \
             LIMIT $3",
        )
        .bind(assignee_id)
        .bind(roles)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        rows.into_iter()
            .map(|r| {
                Ok(AssignmentRow {
                    job_id: JobId::from_uuid(r.step.job_id),
                    job_title: r.job_title,
                    due_on: r.due_on,
                    workflow: r.workflow,
                    workflow_version: r.workflow_version,
                    subject_kind: r.subject_kind,
                    subject_id: r.subject_id,
                    priority: parse_priority(&r.priority),
                    simulated: r.simulated,
                    tags: r.tags,
                    step: row_to_step(r.step)?,
                })
            })
            .collect()
    }

    async fn list_assigned_workable(&self, limit: i64) -> Result<Vec<AssignmentRow>, JobsError> {
        // One indexed JOIN: open Jobs × their workable steps that already
        // carry an assignee — the whole assigned backlog in a single
        // round-trip. The sim workforce pulls this each pass and drives
        // every assigned step, decoupled from who assigned it.
        let rows = sqlx::query_as::<_, AssignmentRowSql>(
            "SELECT s.id, s.job_id, s.kind, s.title, s.spec_slug, s.assignee_id, s.status, \
                    s.sort_order, s.blocked_by, s.sign_offs_required, s.assurance_required, s.sign_offs, \
                    s.fields, s.completed_on, s.metadata, s.notes, \
                    s.step_plugin_version, s.embedded_job, \
                    j.title AS job_title, j.due_on, j.kind AS workflow, j.workflow_version, \
                    j.subject_kind, j.subject_id, j.priority, \
                    j.simulated, j.tags \
             FROM steps s \
             JOIN jobs j ON s.job_id = j.id \
             WHERE j.status = 'open' \
               AND s.status IN ('ready', 'active') \
               AND s.assignee_id IS NOT NULL AND s.assignee_id <> '' \
             ORDER BY j.opened_on, s.sort_order \
             LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        rows.into_iter()
            .map(|r| {
                Ok(AssignmentRow {
                    job_id: JobId::from_uuid(r.step.job_id),
                    job_title: r.job_title,
                    due_on: r.due_on,
                    workflow: r.workflow,
                    workflow_version: r.workflow_version,
                    subject_kind: r.subject_kind,
                    subject_id: r.subject_id,
                    priority: parse_priority(&r.priority),
                    simulated: r.simulated,
                    tags: r.tags,
                    step: row_to_step(r.step)?,
                })
            })
            .collect()
    }

    async fn count_in_flight_steps_by_kind(&self, step_kind: &str) -> Result<i64, JobsError> {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM steps \
             WHERE kind = $1 AND status IN ('pending', 'ready', 'active')",
        )
        .bind(step_kind)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(n)
    }

    async fn count_open_jobs_for_workflow(
        &self,
        kind: &str,
        version: i32,
    ) -> Result<i64, JobsError> {
        // Served by the `jobs_kind_version` index (03-jobs.sql).
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM jobs \
             WHERE kind = $1 AND workflow_version = $2 \
               AND status NOT IN ('closed', 'cancelled')",
        )
        .bind(kind)
        .bind(version)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(n)
    }

    async fn workflow_terminal_report(
        &self,
        kind: &str,
        since: Option<chrono::NaiveDate>,
        simulated: Option<bool>,
    ) -> Result<Vec<crate::port::VersionTerminalReport>, JobsError> {
        // One statement for the whole report — the version dimension
        // is the PINNED `workflow_version`, served by the
        // `jobs_kind_version` index, so there is no per-version
        // fan-out. Contract pinned against the port's pure
        // `terminal_report_from_jobs` by tests/terminal_report_pg.rs:
        // outcomes read `metadata->>'outcome'` over closed rows only,
        // cycle days prefer the precise `opened_at` / `closed_at`
        // metadata stamps (RFC3339 instants written at admission and
        // at the close hooks — the dates have one-day resolution by
        // construction), COALESCEd to `(closed_on - opened_on)` for
        // packets that predate the stamps, and the percentiles are
        // `percentile_cont`, which the port helper mirrors
        // formula-for-formula.
        let rows: Vec<(
            i32,
            i64,
            serde_json::Value,
            serde_json::Value,
            i64,
            i64,
            Option<f64>,
            Option<f64>,
        )> = sqlx::query_as(
            r#"
            WITH base AS (
              SELECT workflow_version,
                     status,
                     metadata->>'outcome' AS outcome,
                     COALESCE(
                       (EXTRACT(EPOCH FROM ((metadata->>'closed_at')::timestamptz
                                          - (metadata->>'opened_at')::timestamptz))
                          / 86400.0)::float8,
                       (closed_on - opened_on)::float8
                     ) AS cycle_days
              FROM jobs
              WHERE kind = $1
                AND ($2::date IS NULL OR opened_on >= $2)
                AND ($3::bool IS NULL OR simulated = $3)
            ),
            statuses AS (
              SELECT workflow_version,
                     jsonb_object_agg(status, n) AS by_status,
                     SUM(n)::BIGINT AS total
              FROM (
                SELECT workflow_version, status, COUNT(*) AS n
                FROM base GROUP BY workflow_version, status
              ) t
              GROUP BY workflow_version
            ),
            outcomes AS (
              SELECT workflow_version,
                     jsonb_object_agg(outcome, n)
                       FILTER (WHERE outcome IS NOT NULL) AS outcomes,
                     COALESCE(SUM(n) FILTER (WHERE outcome IS NULL), 0)::BIGINT
                       AS closed_without_outcome
              FROM (
                SELECT workflow_version, outcome, COUNT(*) AS n
                FROM base WHERE status = 'closed'
                GROUP BY workflow_version, outcome
              ) t
              GROUP BY workflow_version
            ),
            cycles AS (
              SELECT workflow_version,
                     COUNT(*)::BIGINT AS samples,
                     percentile_cont(0.5) WITHIN GROUP (ORDER BY cycle_days) AS median,
                     percentile_cont(0.9) WITHIN GROUP (ORDER BY cycle_days) AS p90
              FROM base
              WHERE status = 'closed' AND cycle_days IS NOT NULL
              GROUP BY workflow_version
            )
            SELECT s.workflow_version,
                   s.total,
                   s.by_status,
                   COALESCE(o.outcomes, '{}'::jsonb) AS outcomes,
                   COALESCE(o.closed_without_outcome, 0) AS closed_without_outcome,
                   COALESCE(c.samples, 0) AS samples,
                   c.median,
                   c.p90
            FROM statuses s
            LEFT JOIN outcomes o USING (workflow_version)
            LEFT JOIN cycles c USING (workflow_version)
            ORDER BY s.workflow_version DESC
            "#,
        )
        .bind(kind)
        .bind(since)
        .bind(simulated)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;

        fn counts_map(
            v: serde_json::Value,
        ) -> Result<std::collections::BTreeMap<String, i64>, JobsError> {
            match v {
                serde_json::Value::Object(map) => map
                    .into_iter()
                    .map(|(k, n)| {
                        n.as_i64().map(|n| (k, n)).ok_or_else(|| {
                            JobsError::Storage(format!("non-integer count in report: {n}"))
                        })
                    })
                    .collect(),
                other => Err(JobsError::Storage(format!(
                    "report expected a JSON object of counts, got: {other}"
                ))),
            }
        }

        rows.into_iter()
            .map(
                |(
                    version,
                    total,
                    by_status,
                    outcomes,
                    closed_without_outcome,
                    samples,
                    median,
                    p90,
                )| {
                    Ok(crate::port::VersionTerminalReport {
                        version,
                        total,
                        by_status: counts_map(by_status)?,
                        outcomes: counts_map(outcomes)?,
                        closed_without_outcome,
                        cycle_time_days: crate::port::CycleTimeDays {
                            samples,
                            median,
                            p90,
                        },
                    })
                },
            )
            .collect()
    }

    async fn queue_age(
        &self,
        scope: &JobScope,
    ) -> Result<Vec<crate::port::QueueAgeRow>, JobsError> {
        // Same short-circuit as `list_jobs`: policy said "nothing",
        // so no round trip.
        if matches!(scope, JobScope::None) {
            return Ok(Vec::new());
        }
        // The three scope binds are `list_jobs`'s $7..$9, verbatim —
        // the lens must not grow a second definition of whose packets
        // these are. Membership is the packet 2a0b034e query:
        // ready/active steps of open packets. `since` is the recorded
        // ready flip when the projection has it, else `updated_at` —
        // an honest lower bound, labelled by `exact`.
        let (scope_owner, scope_owners, scope_accounts): (
            Option<&str>,
            Option<Vec<String>>,
            Option<Vec<String>>,
        ) = match scope {
            JobScope::All => (None, None, None),
            JobScope::None => unreachable!("short-circuited above"),
            JobScope::OwnerIs(u) => (Some(u.as_str()), None, None),
            JobScope::OwnerIn(us) => (None, Some(us.clone()), None),
            JobScope::AccountIn(ps) => (None, None, Some(ps.clone())),
        };
        type Row = (
            uuid::Uuid,
            String,
            String,
            bool,
            uuid::Uuid,
            Option<String>,
            String,
            String,
            Option<String>,
            chrono::DateTime<chrono::Utc>,
            bool,
        );
        let rows: Vec<Row> = sqlx::query_as(
            r#"
            SELECT j.id, j.kind, j.title, j.simulated,
                   s.id, s.spec_slug, s.title, s.status, s.assignee_id,
                   COALESCE(s.became_ready_at, s.updated_at) AS since,
                   (s.became_ready_at IS NOT NULL) AS exact
            FROM steps s
            JOIN jobs j ON j.id = s.job_id
            WHERE s.status IN ('ready', 'active')
              AND j.status = 'open'
              AND ($1::text IS NULL OR j.owner_id = $1)
              AND ($2::text[] IS NULL OR j.owner_id = ANY($2))
              AND (
                $3::text[] IS NULL
                OR (j.subject_kind IN ('account', 'employee')
                    AND j.subject_id = ANY($3))
              )
            ORDER BY since ASC, s.id ASC
            "#,
        )
        .bind(scope_owner)
        .bind(scope_owners.as_deref())
        .bind(scope_accounts.as_deref())
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;

        rows.into_iter()
            .map(
                |(
                    job_id,
                    job_kind,
                    job_title,
                    simulated,
                    step_id,
                    spec_slug,
                    step_title,
                    status,
                    assignee_id,
                    since,
                    exact,
                )| {
                    Ok(crate::port::QueueAgeRow {
                        job_id: JobId::from_uuid(job_id),
                        job_kind,
                        job_title,
                        step_id: StepId::from_uuid(step_id),
                        spec_slug,
                        step_title,
                        status: parse_step_status(&status)
                            .ok_or_else(|| step_status_err(&status))?,
                        assignee_id,
                        simulated,
                        since,
                        exact,
                    })
                },
            )
            .collect()
    }

    async fn count_jobs_by_kind(
        &self,
        status: Option<JobStatus>,
    ) -> Result<Vec<(String, i64)>, JobsError> {
        let rows: Vec<(String, i64)> = sqlx::query_as(
            "SELECT kind, COUNT(*)::BIGINT FROM jobs \
             WHERE ($1::text IS NULL OR status = $1) \
             GROUP BY kind ORDER BY kind",
        )
        .bind(status.map(job_status_str))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(rows)
    }

    async fn jobs_tier_distribution(
        &self,
        status: Option<JobStatus>,
    ) -> Result<Vec<(String, i32, i64)>, JobsError> {
        // Per-job tier = min(sort_order) over non-done steps, or -1
        // when every step is terminal. One CTE pass so we don't walk
        // the steps table twice. Ordered so the frontend receives a
        // deterministic serialization.
        let rows: Vec<(String, i32, i64)> = sqlx::query_as(
            r#"
            WITH per_job AS (
              SELECT j.id,
                     j.kind,
                     COALESCE(
                       MIN(s.sort_order) FILTER (
                         WHERE s.status IN ('pending', 'ready', 'active')
                       ),
                       -1
                     ) AS tier
              FROM jobs j
              LEFT JOIN steps s ON s.job_id = j.id
              WHERE ($1::text IS NULL OR j.status = $1)
              GROUP BY j.id, j.kind
            )
            SELECT kind, tier, COUNT(*)::BIGINT
            FROM per_job
            GROUP BY kind, tier
            ORDER BY kind, tier
            "#,
        )
        .bind(status.map(job_status_str))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(rows)
    }

    async fn list_launch_calendar(
        &self,
        from: chrono::NaiveDate,
        to: chrono::NaiveDate,
    ) -> Result<Vec<LaunchCalendarRow>, JobsError> {
        // Every open/pending/pending-sign-off marketing-motion joined
        // to its single launch step (the one carrying launch_date). We pull the
        // launch_date + launch_channel out of step metadata in SQL so
        // the caller doesn't have to fetch the step rows separately.
        // `current_tier` mirrors the computation in
        // `jobs_tier_distribution` (min non-done sort_order; -1 when
        // everything is terminal).
        //
        // The date window is applied inclusively on both ends. Motions
        // whose launch step has no date yet (launch_date IS NULL) are
        // intentionally returned — the UI buckets them under
        // "unscheduled" at the top of the list.
        #[derive(sqlx::FromRow)]
        struct Row {
            id: uuid::Uuid,
            title: String,
            owner_id: String,
            subject_id: String,
            status: String,
            current_tier: Option<i32>,
            launch_date: Option<chrono::NaiveDate>,
            launch_channel: Option<String>,
        }
        let rows: Vec<Row> = sqlx::query_as::<_, Row>(
            r#"
            WITH launches AS (
              SELECT
                s.job_id,
                -- jsonb -> text then cast to date is tolerant of both
                -- string values ("2026-05-15") and missing keys.
                NULLIF(s.metadata ->> 'launch_date', '')::date AS launch_date,
                NULLIF(s.metadata ->> 'launch_channel', '')    AS launch_channel
              FROM steps s
              -- property, not kind: the launch step is whichever step
              -- carries a launch_date (no-step-kind-match rule)
              WHERE s.metadata ? 'launch_date'
            ),
            tiers AS (
              SELECT j.id AS job_id,
                     COALESCE(
                       MIN(s.sort_order) FILTER (
                         WHERE s.status IN ('pending','ready','active')
                       ),
                       -1
                     ) AS current_tier
              FROM jobs j
              LEFT JOIN steps s ON s.job_id = j.id
              GROUP BY j.id
            )
            SELECT j.id,
                   j.title,
                   j.owner_id,
                   j.subject_id,
                   j.status,
                   t.current_tier,
                   l.launch_date,
                   l.launch_channel
            FROM jobs j
            LEFT JOIN launches l ON l.job_id = j.id
            LEFT JOIN tiers t    ON t.job_id = j.id
            WHERE j.kind = 'marketing-motion'
              AND j.status NOT IN ('closed','cancelled')
              AND (
                l.launch_date IS NULL
                OR l.launch_date BETWEEN $1 AND $2
              )
            ORDER BY l.launch_date NULLS FIRST, j.title
            "#,
        )
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;

        Ok(rows
            .into_iter()
            .map(|r| LaunchCalendarRow {
                job_id: JobId::from_uuid(r.id),
                title: r.title,
                owner_id: Some(r.owner_id),
                subject_id: Some(r.subject_id),
                status: parse_job_status(&r.status),
                current_tier: r.current_tier,
                launch_date: r.launch_date,
                launch_channel: r.launch_channel,
            })
            .collect())
    }

    async fn resolve_blockers(
        &self,
        ids: &[StepId],
    ) -> Result<Vec<(StepId, StepStatus)>, JobsError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let uuids: Vec<uuid::Uuid> = ids.iter().map(|id| *id.inner().as_uuid()).collect();

        #[derive(sqlx::FromRow)]
        struct StatusRow {
            id: uuid::Uuid,
            status: String,
        }

        let rows =
            sqlx::query_as::<_, StatusRow>("SELECT id, status FROM steps WHERE id = ANY($1)")
                .bind(&uuids)
                .fetch_all(&self.pool)
                .await
                .map_err(|e| JobsError::Storage(e.to_string()))?;

        rows.into_iter()
            .map(|r| {
                let status =
                    parse_step_status(&r.status).ok_or_else(|| step_status_err(&r.status))?;
                Ok((StepId::from_uuid(r.id), status))
            })
            .collect()
    }

    async fn sim_clock_state(&self) -> Result<Option<crate::port::SimClockState>, JobsError> {
        // Single-row table by convention; LIMIT 1 belt + suspenders
        // in case a deploy ever has more than one row.
        let row: Option<(
            chrono::NaiveDate,
            Option<chrono::NaiveDate>,
            Option<chrono::NaiveDate>,
            bool,
            bool,
        )> = sqlx::query_as(
            "SELECT current_sim_date, epoch_start_date, epoch_end_date, paused, \
                    restart_in_progress \
             FROM sim_clock WHERE id = 1 LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(row.map(
            |(current_sim_date, epoch_start_date, epoch_end_date, paused, restart_in_progress)| {
                // Date-only Postgres adapter: synthesize midnight UTC for the
                // `now` field. The live SimClockState used by the SPA is built
                // via http::sim_clock_state_from_clock, which calls clock-api
                // directly and carries the full sub-day time; this date-only
                // projection is read by tests and any caller that doesn't need
                // sub-day precision.
                let now = current_sim_date
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight is always valid")
                    .and_utc();
                crate::port::SimClockState {
                    now,
                    current_sim_date,
                    epoch_start_date,
                    epoch_end_date,
                    paused,
                    restart_in_progress,
                }
            },
        ))
    }

    async fn set_sim_clock_paused(&self, paused: bool) -> Result<(), JobsError> {
        // Move paused_at in lockstep with paused: boss-clock's now() only
        // freezes when (paused, paused_at) = (true, Some). Setting paused
        // alone leaves the clock advancing, so the Pause button wouldn't
        // actually stop sim-time. On resume, clear it.
        sqlx::query(
            "UPDATE sim_clock \
             SET paused = $1, paused_at = CASE WHEN $1 THEN NOW() ELSE NULL END \
             WHERE id = 1",
        )
        .bind(paused)
        .execute(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn restart_sim_clock_epoch(&self) -> Result<(), JobsError> {
        // Read epoch_start + baseline cutoff before touching
        // anything destructive. epoch_start tells us where to
        // rewind to; baseline tells us which audit_log rows to
        // keep (everything <= it is canonical seed; everything
        // > it is live-tick accumulation).
        //
        // If epoch_baseline_audit_id is NULL (the canonical regen /
        // reset script's step 9 didn't complete OR the deploy
        // started from an older bundle that pre-dated the column),
        // self-heal by claiming MAX(audit_log.id) NOW as the
        // baseline. The user-facing effect: the first restart-epoch
        // click captures whatever audit_log state currently exists
        // as "the baseline" and rewinds future restarts to it. Not
        // ideal — the script should set this — but unblocks the
        // demo without forcing the user to ssh in.
        let row: Option<(Option<chrono::NaiveDate>, Option<i64>, bool)> = sqlx::query_as(
            "SELECT epoch_start_date, epoch_baseline_audit_id, \
                    COALESCE(restart_in_progress, false) \
             FROM sim_clock WHERE id = 1 LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        let Some((Some(start), baseline_opt, already_running)) = row else {
            return Err(JobsError::Storage(
                "sim_clock missing epoch_start_date — cannot restart epoch \
                 (run reset-to-baseline.sh once to capture the baseline)"
                    .to_string(),
            ));
        };
        let baseline = match baseline_opt {
            Some(b) => b,
            None => {
                // Self-heal: claim MAX(audit_log.id) as the baseline
                // and persist it. Subsequent restart-epoch calls hit
                // the Some(b) path.
                let max_id: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM audit_log")
                    .fetch_one(&self.pool)
                    .await
                    .map_err(|e| JobsError::Storage(e.to_string()))?;
                sqlx::query("UPDATE sim_clock SET epoch_baseline_audit_id = $1 WHERE id = 1")
                    .bind(max_id)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| JobsError::Storage(e.to_string()))?;
                tracing::warn!(
                    epoch_baseline_audit_id = max_id,
                    "sim_clock.epoch_baseline_audit_id was NULL; self-healed by capturing MAX(audit_log.id) at restart time. \
                     The reset/regen scripts should have set this — investigate which one skipped step 9."
                );
                max_id
            }
        };
        if already_running {
            return Err(JobsError::Storage(
                "restart already in progress".to_string(),
            ));
        }

        // Atomically claim the restart slot: paused=true,
        // restart_in_progress=true. The handler returns 202 to
        // the SPA right after this; the heavyweight work (truncate
        // + rebuild-all + clock-rewind) runs in a tokio::spawn
        // task. The SPA polls /api/jobs/live to see when
        // restart_in_progress flips back to false.
        // paused_at = NOW() is load-bearing, not cosmetic: boss-clock's
        // now() only freezes sim-time when (paused, paused_at) = (true,
        // Some) — with paused_at NULL it falls through to Utc::now() and
        // the clock keeps advancing, so the sim daemon never quiesces.
        // Without it the tick-wait below is a no-op and the audit_log
        // trim races live writes (trimming a job's create event while a
        // later-committed step event survives → orphaned steps → the
        // jobs rebuild aborts on the FK).
        sqlx::query(
            "UPDATE sim_clock \
             SET paused = true, paused_at = NOW(), \
                 restart_in_progress = true, updated_at = NOW() \
             WHERE id = 1",
        )
        .execute(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;

        let pool = self.pool.clone();
        let db_url = self
            .db_url
            .clone()
            .or_else(|| std::env::var("BOSS_POSTGRES_URL").ok())
            .unwrap_or_else(|| "postgres://boss:boss@127.0.0.1/boss".to_string());
        let nats_url = self
            .nats_url
            .clone()
            .or_else(|| std::env::var("BOSS_NATS_URL").ok());
        tokio::spawn(async move {
            if let Err(e) =
                run_restart_epoch_background(&pool, &db_url, nats_url.as_deref(), start, baseline)
                    .await
            {
                tracing::error!(error = %e, "restart_epoch background task failed");
                // Clear the flag on failure so the operator can
                // retry. Daemon stays paused; sim_clock state is
                // left in whatever state the failure produced.
                let _ =
                    sqlx::query("UPDATE sim_clock SET restart_in_progress = false WHERE id = 1")
                        .execute(&pool)
                        .await;
            }
        });

        Ok(())
    }

    async fn record_step_write_refusal_at(
        &self,
        refusal: &crate::refusals::StepWriteRefusal,
        now: DateTime<Utc>,
    ) -> Result<(), JobsError> {
        sqlx::query(
            "INSERT INTO step_write_refusals \
             (refused_at, job_id, step_id, actor_id, method, path, status_code, error_class, detail) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(now)
        .bind(refusal.job_id)
        .bind(refusal.step_id)
        .bind(&refusal.actor_id)
        .bind(&refusal.method)
        .bind(&refusal.path)
        .bind(i32::from(refusal.status_code))
        .bind(refusal.error_class.as_str())
        .bind(&refusal.detail)
        .execute(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(())
    }

    async fn step_write_refusals(
        &self,
        limit: i64,
    ) -> Result<Vec<crate::refusals::RecordedRefusal>, JobsError> {
        let rows = sqlx::query_as::<_, RefusalRow>(
            "SELECT id, refused_at, job_id, step_id, actor_id, method, path, status_code, \
             error_class, detail FROM step_write_refusals ORDER BY refused_at DESC, id DESC \
             LIMIT $1",
        )
        .bind(limit.max(0))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| JobsError::Storage(e.to_string()))?;
        Ok(rows.into_iter().map(RefusalRow::into_domain).collect())
    }
}

#[derive(sqlx::FromRow)]
struct RefusalRow {
    id: i64,
    refused_at: DateTime<Utc>,
    job_id: Option<uuid::Uuid>,
    step_id: Option<uuid::Uuid>,
    actor_id: String,
    method: String,
    path: String,
    status_code: i32,
    error_class: String,
    detail: String,
}

impl RefusalRow {
    fn into_domain(self) -> crate::refusals::RecordedRefusal {
        use crate::refusals::ErrorClass;
        // The column is CHECK-constrained to this vocabulary, so an
        // unknown value means the constraint and the enum drifted.
        // Degrade to `Other` rather than failing the read — a
        // measurement surface that 500s is worse than one row landing
        // in a coarser bucket, and the pinning test in `refusals` is
        // what catches the drift.
        let error_class = ErrorClass::ALL
            .into_iter()
            .find(|c| c.as_str() == self.error_class)
            .unwrap_or(ErrorClass::Other);
        crate::refusals::RecordedRefusal {
            id: self.id,
            refused_at: self.refused_at,
            refusal: crate::refusals::StepWriteRefusal {
                job_id: self.job_id,
                step_id: self.step_id,
                actor_id: self.actor_id,
                method: self.method,
                path: self.path,
                status_code: self.status_code as u16,
                error_class,
                detail: self.detail,
            },
        }
    }
}

/// Heavyweight reset path — runs in a tokio::spawn task off the
/// HTTP request that triggered it. Operator polls
/// `/api/jobs/live` to see when `sim_clock.restart_in_progress`
/// flips back to false.
///
/// Trim-not-truncate strategy: the seed bundle is 880k single-row
/// INSERTs (no COPY), so re-importing it costs 5+ minutes. Instead
/// we DELETE only `audit_log` rows past the canonical baseline id
/// (the live-tick accumulation), then run the per-service
/// rebuilders to replay the surviving (smaller) audit_log into
/// fresh projections. ~30s typical.
/// Trim `audit_log` back to the seed baseline for an epoch restart,
/// preserving platform meta-work.
///
/// Extracted from `run_restart_epoch_background` so the preservation
/// rule can be tested: the rest of that routine waits on quiescence,
/// talks to NATS and drives every rebuilder, none of which a test of
/// "what survives the trim" should have to stand up.
pub async fn trim_epoch_audit_log(pool: &PgPool, baseline: i64) -> Result<u64, JobsError> {
    // Trim audit_log past the seed baseline. The append-only
    // trigger (DELETE rejection per the correctness-protocol
    // invariant) has to be disabled briefly — this is the one
    // controlled exception, scoped to the demo-loop reset path.
    // The per-service rebuilders below wipe their own projection
    // tables in their replay transactions, so projections clear
    // and re-derive in one pass.
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| JobsError::Storage(format!("begin trim tx: {e}")))?;
    // Outbox first, deliberately: at restart time every outbox row is
    // the finished epoch's by definition, so pending rows must die
    // with it rather than relay into the new epoch. TRUNCATE takes
    // ACCESS EXCLUSIVE, so it queues behind any in-flight relay batch
    // (its FOR UPDATE row locks) — and any audit rows that batch
    // committed carry ids ≤ this moment, so the DELETE below removes
    // them. Ordering the truncate before the audit trim is what makes
    // a racing relay harmless.
    sqlx::query("TRUNCATE event_outbox")
        .execute(&mut *tx)
        .await
        .map_err(|e| JobsError::Storage(format!("truncate event_outbox: {e}")))?;
    sqlx::query("ALTER TABLE audit_log DISABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&mut *tx)
        .await
        .map_err(|e| JobsError::Storage(format!("disable trigger: {e}")))?;
    // Delete the simulated company; keep the real one.
    //
    // Sim-ness is a property of the JOB, decided from the origin of the
    // request that opened it and immutable thereafter. Everything
    // associated with a simulated Job is simulated — including a real
    // operator clicking around one. A fake brew order does not become
    // real because somebody looked at it.
    //
    // That framing is what makes this simple. Deciding per EVENT gave a
    // Job a mixed history, which forced the trim to preserve any Job a
    // human had touched or risk orphaning steps and aborting the
    // rebuild on `steps_job_id_fkey`. Carrying the bit on the Job
    // removes the case instead of handling it: a Job's rows all share
    // one fate, so no partial deletion is possible.
    //
    // Events with no Job fall back to their own marker — ledger
    // postings, asset receipts and the like are not Job-scoped, and
    // absence still means keep, because the conservative direction for
    // a DELETE is to keep.
    let trimmed = sqlx::query(
        "DELETE FROM audit_log a
          WHERE a.id > $1
            AND CASE
                  WHEN COALESCE(a.payload->>'job_id', a.payload->>'id') IN
                       (SELECT id::text FROM jobs)
                  THEN EXISTS (
                       SELECT 1 FROM jobs j
                        WHERE j.id::text = COALESCE(a.payload->>'job_id', a.payload->>'id')
                          AND j.simulated)
                  ELSE a.payload->>'_simulated' = 'true'
                END",
    )
    .bind(baseline)
    .execute(&mut *tx)
    .await
    .map_err(|e| JobsError::Storage(format!("trim audit_log: {e}")))?;
    sqlx::query("ALTER TABLE audit_log ENABLE TRIGGER audit_log_reject_row_mutation_trg")
        .execute(&mut *tx)
        .await
        .map_err(|e| JobsError::Storage(format!("re-enable trigger: {e}")))?;
    tx.commit()
        .await
        .map_err(|e| JobsError::Storage(format!("commit trim tx: {e}")))?;
    Ok(trimmed.rows_affected())
}

async fn run_restart_epoch_background(
    pool: &PgPool,
    db_url: &str,
    nats_url: Option<&str>,
    epoch_start: chrono::NaiveDate,
    baseline: i64,
) -> Result<(), JobsError> {
    // Wait one tick interval for the daemon to observe paused=true.
    let tick_interval: i32 =
        sqlx::query_scalar("SELECT tick_interval_seconds FROM sim_clock WHERE id = 1")
            .fetch_one(pool)
            .await
            .unwrap_or(10);
    let pause_wait = std::time::Duration::from_secs((tick_interval.max(1) + 2) as u64);
    tokio::time::sleep(pause_wait).await;

    // Wait for write-quiescence before trimming. Pausing the sim (the
    // paused_at freeze above) stops NEW work, but the dispatcher keeps
    // draining its in-flight step.done backlog — writing jobs/steps
    // (and their commerce/shipping side-effects) stamped at the frozen
    // instant. Trimming while those commit races them: the DELETE
    // removes a job's create event while a later-committed step event
    // survives → orphaned steps → the jobs rebuild aborts on
    // steps_job_id_fkey. Poll MAX(audit_log.id) until it stops growing
    // (backlog drained), then trim a settled log. The sim is paused so
    // no new step.done events arrive — the backlog is finite + drains.
    {
        let mut last_max: i64 = -1;
        let mut stable: u32 = 0;
        let mut quiesced = false;
        for _ in 0..60 {
            let cur: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM audit_log")
                .fetch_one(pool)
                .await
                .map_err(|e| JobsError::Storage(format!("quiesce poll: {e}")))?;
            if cur == last_max {
                stable += 1;
                if stable >= 4 {
                    quiesced = true;
                    break;
                }
            } else {
                stable = 0;
                last_max = cur;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        if quiesced {
            tracing::info!(
                last_audit_id = last_max,
                "restart_epoch: writers quiesced; trimming"
            );
        } else {
            tracing::warn!(
                last_audit_id = last_max,
                "restart_epoch: audit_log still changing after ~60s; trimming anyway \
                 (a writer isn't honoring the pause — investigate)"
            );
        }
    }

    let trimmed = trim_epoch_audit_log(pool, baseline).await?;
    tracing::info!(
        rows_trimmed = trimmed,
        baseline,
        "restart_epoch: audit_log trimmed past seed baseline"
    );

    // Drop the JetStream delivery buffer for the same reason the outbox
    // was truncated above: whatever is still buffered belongs to the
    // epoch just deleted, and redelivering it against the trimmed
    // database 404s through the whole redelivery budget and
    // dead-letters. Best-effort — a restart that completes with a stale
    // buffer is far better than one that aborts because NATS was
    // briefly unreachable, so this logs and carries on.
    purge_epoch_delivery_buffer(nats_url).await;

    // Clear directly-written per-epoch ledger state that no audit-log
    // rebuilder owns (see clear_epoch_payroll_state), so the new cycle
    // starts from an empty payroll ledger instead of deduping against
    // last cycle's calendar-dated runs.
    clear_epoch_payroll_state(pool).await?;

    // Spawn boss-rebuild-all WITHOUT --audit-log-seed: the
    // surviving audit_log is the source of truth; we just need
    // to re-derive projections from it.
    let rebuild_bin = std::env::var("BOSS_REBUILD_ALL_BIN")
        .unwrap_or_else(|_| "/usr/local/bin/boss-rebuild-all".to_string());
    let status = tokio::process::Command::new(&rebuild_bin)
        .arg("--database-url")
        .arg(db_url)
        .status()
        .await
        .map_err(|e| JobsError::Storage(format!("spawn {rebuild_bin}: {e}")))?;
    if !status.success() {
        return Err(JobsError::Storage(format!(
            "boss-rebuild-all exited {status}"
        )));
    }

    // Rewind the formula clock to epoch_start, unpause, clear the flag.
    // Sim-time is computed (epoch_start_date + (now − wall_anchor −
    // paused_offset) × warp), not stored — so re-anchor wall_anchor to NOW
    // with a zero pause-offset and sim_now snaps back to epoch_start_date.
    // (Writing the long-dropped current_sim_date column is what used to
    // fail this step after the formula-clock migration.) The daemon's next
    // tick resumes the new epoch against the freshly rebuilt projections.
    sqlx::query(
        "UPDATE sim_clock \
         SET epoch_start_date = $1, wall_anchor = NOW(), \
             paused_offset_seconds = 0, paused = false, paused_at = NULL, \
             restart_in_progress = false, updated_at = NOW() \
         WHERE id = 1",
    )
    .bind(epoch_start)
    .execute(pool)
    .await
    .map_err(|e| JobsError::Storage(e.to_string()))?;
    Ok(())
}

/// Purge the `BOSS_EVENTS` JetStream buffer on a demo-loop reset.
///
/// Deliberately best-effort: it opens its own short-lived connection
/// rather than holding one open across the restart, because the purge
/// is a nice-to-have-correctness step on a path whose job is to get the
/// demo into a clean new epoch. Every failure mode (no URL configured,
/// NATS down, stream absent on a first run) logs and returns.
///
/// The URL is passed in, not read from the environment: this service
/// takes its broker address from config (`nats_url` in
/// `/etc/boss-jobs-api.toml`), so an env-var lookup would find nothing
/// on a real deployment and skip the purge silently.
///
/// Reset-path only. A purge on an ordinary restart would discard the
/// in-flight events the durability layer exists to protect.
async fn purge_epoch_delivery_buffer(nats_url: Option<&str>) {
    let Some(url) = nats_url else {
        tracing::warn!(
            "restart_epoch: no NATS URL configured; skipping delivery-buffer purge \
             (stale events may dead-letter against the trimmed database)"
        );
        return;
    };
    let client = match async_nats::connect(url).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "restart_epoch: NATS connect failed; \
                 leaving the delivery buffer as-is (stale events may dead-letter)");
            return;
        }
    };
    let ctx = async_nats::jetstream::new(client);
    match boss_nats::durable::purge_stream(&ctx, boss_nats::durable::STREAM_NAME).await {
        Ok(()) => tracing::info!(
            stream = boss_nats::durable::STREAM_NAME,
            "restart_epoch: delivery buffer purged"
        ),
        Err(e) => tracing::warn!(
            error = %e,
            "restart_epoch: stream purge failed; stale events may dead-letter"
        ),
    }
}

/// Clear the directly-written per-epoch payroll projection tables on a
/// demo-loop reset.
///
/// `payroll_runs` (+ its `payroll_run_lines`, FK `ON DELETE CASCADE`) is
/// written straight by the ledger payroll-synthesize endpoint and is NOT
/// owned by any audit-log rebuilder — so the per-service replay above
/// (which wipes + re-derives `gl_journal_lines`, `financial_facts`, …)
/// leaves it untouched. Across epoch loops it accumulates prior-cycle
/// rows, and synthesize's calendar-dated idempotency key
/// (`payroll-YYYYMMDD`) then collides with last cycle's row → the new
/// cycle's payroll is deduped away and never reaches the GL. The reset
/// must clear it so each epoch starts from an empty payroll ledger.
///
/// Reset-path only (NOT the general `boss-rebuild-all` rebuilder), so an
/// ordinary projection repair never drops payroll history.
pub async fn clear_epoch_payroll_state(pool: &PgPool) -> Result<(), JobsError> {
    // Both tables in one TRUNCATE so the FK (lines → runs) is satisfied
    // without CASCADE.
    sqlx::query("TRUNCATE payroll_run_lines, payroll_runs")
        .execute(pool)
        .await
        .map_err(|e| JobsError::Storage(format!("clear epoch payroll state: {e}")))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Unit tests — parse_subject / subject_parts only (no DB required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::primitives::Subject as SubjectTrait;

    /// For every canonical kind, subject_parts(parse_subject(k, v))
    /// round-trips to the same (k, v) strings. This is the DB
    /// round-trip property expressed at the boundary functions,
    /// without needing a live Postgres connection.
    #[test]
    fn closed_kind_write_read_round_trip() {
        for (kind, ref_id) in [
            ("system", "SYS-001"),
            ("account", "prac-42"),
            ("purchase_order", "po-2026-0101"),
            ("campaign", "cmp-spring-26"),
            ("employee", "emp-001"),
            ("vendor", "ven-oem-coherent"),
        ] {
            let subject = parse_subject(kind, ref_id);
            let (out_kind, out_ref) = subject_parts(&subject);
            assert_eq!(out_kind, kind, "closed kind {kind:?} round-trip");
            assert_eq!(out_ref, ref_id, "closed kind {kind:?} ref_id round-trip");
        }
    }

    #[test]
    fn custom_kind_is_preserved_across_round_trip() {
        // Wave 7c preservation: a user-defined custom_kind survives
        // write → read → re-write with the real string intact.
        let subject = parse_subject("dose-review", "dr-2026-042");
        assert_eq!(SubjectTrait::kind(&subject), "dose-review");
        assert_eq!(SubjectTrait::id(&subject), "dr-2026-042");

        let (out_kind, out_ref) = subject_parts(&subject);
        assert_eq!(out_kind, "dose-review");
        assert_eq!(out_ref, "dr-2026-042");
    }
}
