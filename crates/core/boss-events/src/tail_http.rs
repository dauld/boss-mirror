//! Audit-log tail HTTP endpoint — the read surface for `audit_log`.
//!
//! Writers (every service's `PgAuditWriter`) insert rows; this router
//! serves recent-first reads with filters on source, kind, actor, and
//! time window. Intended home: the CTO surface at `/cto/events`, where an
//! operator can watch the event stream flow in ~real time.
//!
//! Access: Operator tier, Auditor tier, or role ∈ {ceo, cto}.
//! Everybody else gets 403. The log carries every domain payload
//! including HR + financial events, so it's locked down harder than a
//! domain-specific admin view. Role-based fallback exists so the CTO
//! can watch the tail from a normal session without needing a
//! FIDO-elevated cookie just to glance at the feed.
//!
//! This router lives in boss-events (not a domain crate) because the
//! audit_log is cross-cutting. It's mounted in `boss-people-api` for
//! convenience — people-api already owns the Postgres pool and the
//! admin-side routers. The gateway's `/api/events/{*rest}` proxy
//! points at people-api's port.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use boss_policy_client::AccessTier;
use boss_policy_client::CurrentUser;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone)]
pub struct AuditTailState {
    pub pool: Arc<PgPool>,
}

pub fn audit_tail_router(pool: PgPool) -> Router {
    let state = AuditTailState {
        pool: Arc::new(pool),
    };
    Router::new()
        .route("/api/events/health", get(events_health))
        .route("/api/events/stats", get(stats))
        .route("/api/events/tail", get(tail))
        .route("/api/events/stream", get(stream))
        // Operator-on-demand export. Streams matching audit_log
        // rows as JSON Lines (one event per line) with a
        // Content-Disposition: attachment so the browser
        // downloads it as a file. Same filters as tail, higher
        // cap (50,000 rows), no SSE — straight HTTP body.
        .route("/api/events/export", get(export))
        // Public companion to /api/events/tail — no auth, restricted
        // to a curated demo-friendly topic set, smaller cap.
        // Powers the public landing page's right-rail event tail. The
        // gateway proxies this unauth so visitors see a window
        // into what the operating company is doing right now.
        .route("/api/events/public-tail", get(public_tail))
        .with_state(state)
}

/// One row of the audit_log returned to the client. Payload is the
/// raw JSONB — callers decide how to render it.
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct AuditEntry {
    pub event_id: Uuid,
    pub timestamp: DateTime<Utc>,
    pub source: String,
    pub kind: String,
    pub payload: serde_json::Value,
}

/// Liveness probe — used by the launcher's readiness wait + IT Monitoring
/// page to confirm the service is reachable. No auth gate.
/// How big the log is and how fast it grows (168b3f25). David,
/// 2026-09-02: "We need size and growth stats on audit log to make
/// sure it isn't growing unsustainably." Until now the only size
/// reading was `total_rows` inside the nightly integrity checker's
/// pod log — a number nobody reads is a number nobody has. One read,
/// six SQL statements, all over the log itself; the per-day buckets
/// and top kinds cover the last 30 days, which is the horizon a trend
/// needs and cheap enough to answer on demand.
///
/// Every window is anchored at the log's OWN newest row, not the wall
/// clock (no-wallclock): "last 24h" is the day before the newest event.
/// For a live log the two are the same instant; for a quiet or
/// replayed one this is the honest reading — growth measured against
/// the log's head, and an empty log has no windows at all.
#[derive(Debug, Serialize)]
pub struct AuditStats {
    pub total_rows: i64,
    /// `pg_total_relation_size('audit_log')`: heap + indexes + toast.
    pub table_bytes: i64,
    pub oldest_at: Option<DateTime<Utc>>,
    pub newest_at: Option<DateTime<Utc>>,
    pub rows_last_24h: i64,
    pub rows_last_7d: i64,
    /// One bucket per UTC day with rows in the last 30 days, oldest first.
    pub per_day: Vec<DayRows>,
    /// The kinds writing most in the last 30 days, busiest first.
    pub top_kinds: Vec<KindRows>,
}

#[derive(Debug, Serialize)]
pub struct DayRows {
    pub day: String,
    pub rows: i64,
}

#[derive(Debug, Serialize)]
pub struct KindRows {
    pub kind: String,
    pub rows: i64,
}

pub async fn audit_stats(pool: &PgPool) -> Result<AuditStats, String> {
    let (total_rows, oldest_at, newest_at): (i64, Option<DateTime<Utc>>, Option<DateTime<Utc>>) =
        sqlx::query_as("SELECT COUNT(*)::BIGINT, MIN(timestamp), MAX(timestamp) FROM audit_log")
            .fetch_one(pool)
            .await
            .map_err(|e| e.to_string())?;
    let (table_bytes,): (i64,) =
        sqlx::query_as("SELECT pg_total_relation_size('audit_log')::BIGINT")
            .fetch_one(pool)
            .await
            .map_err(|e| e.to_string())?;
    let Some(head) = newest_at else {
        return Ok(AuditStats {
            total_rows,
            table_bytes,
            oldest_at,
            newest_at,
            rows_last_24h: 0,
            rows_last_7d: 0,
            per_day: Vec::new(),
            top_kinds: Vec::new(),
        });
    };
    let (rows_last_24h,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::BIGINT FROM audit_log \
         WHERE timestamp >= $1::timestamptz - INTERVAL '24 hours'",
    )
    .bind(head)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;
    let (rows_last_7d,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*)::BIGINT FROM audit_log \
         WHERE timestamp >= $1::timestamptz - INTERVAL '7 days'",
    )
    .bind(head)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;
    let per_day: Vec<(String, i64)> = sqlx::query_as(
        "SELECT to_char(date_trunc('day', timestamp AT TIME ZONE 'UTC'), 'YYYY-MM-DD') AS day, \
         COUNT(*)::BIGINT FROM audit_log \
         WHERE timestamp >= $1::timestamptz - INTERVAL '30 days' GROUP BY 1 ORDER BY 1",
    )
    .bind(head)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;
    let top_kinds: Vec<(String, i64)> = sqlx::query_as(
        "SELECT kind, COUNT(*)::BIGINT FROM audit_log \
         WHERE timestamp >= $1::timestamptz - INTERVAL '30 days' GROUP BY kind ORDER BY 2 DESC LIMIT 12",
    )
    .bind(head)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(AuditStats {
        total_rows,
        table_bytes,
        oldest_at,
        newest_at,
        rows_last_24h,
        rows_last_7d,
        per_day: per_day
            .into_iter()
            .map(|(day, rows)| DayRows { day, rows })
            .collect(),
        top_kinds: top_kinds
            .into_iter()
            .map(|(kind, rows)| KindRows { kind, rows })
            .collect(),
    })
}

/// `GET /api/events/stats` — the same door as the tail: operator or
/// auditor tier, or a role with global read.
async fn stats(State(state): State<AuditTailState>, CurrentUser(user): CurrentUser) -> Response {
    let tier_ok = matches!(user.access_tier, AccessTier::Operator | AccessTier::Auditor);
    let role_ok = boss_core::roles::has_global_read(&user.role);
    if !(tier_ok || role_ok) {
        return (
            StatusCode::FORBIDDEN,
            "operator tier or executive role required",
        )
            .into_response();
    }
    match audit_stats(&state.pool).await {
        Ok(stats) => Json(stats).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn events_health() -> Response {
    Json(serde_json::json!({"status": "ok"})).into_response()
}

/// Recent rows of ONE exact kind, newest first — the crate-level read
/// other services call so the SQL against `audit_log` stays in the
/// crate that owns the table (this router's own header names that
/// ownership). First consumer: boss-jobs' estate readers (d471a8ce) —
/// the estate loop's events were observable only through an in-pod
/// port-forward, which left two satisfied proven arbiters with no
/// surface a recorded probe could re-run against.
///
/// EXACT kind, deliberately not the tail's ILIKE substring: a reader
/// serving one declared kind must not grow neighbours when a new kind
/// shares a prefix.
///
/// `scope` narrows to one series within that kind, and does it IN THE
/// WHERE CLAUSE — the rule [`TailQuery::simulated`] states for the
/// same reason. One kind carries series at wildly different cadences
/// (the estate observer records `kubernetes-nodes` every 15 minutes,
/// `codebase` once a night), so a LIMIT taken across all of them is
/// spent entirely by the fastest and the slow series is unreadable
/// through its own reader. Filtering the returned page in Rust would
/// not fix that: by then the slow rows are already gone.
///
/// `since` (inclusive) and `until` (exclusive) bound the series in time
/// — the half-open window [`TailQuery`] states — and sit in the same
/// WHERE clause for the same reason (backlog bf362f25). A reader that
/// could only ever see the newest page could not answer a post-mortem:
/// thirty hours after an incident the oldest reachable estate row was
/// already past the window it needed, while every row was still in the
/// log. `until` is the before-cursor: the oldest timestamp on one page
/// is the `until` of the next.
///
/// `total` is the count of the WINDOW, not of the page, so a caller can
/// tell a whole answer (rows == total) from the head of a longer one —
/// the comparison a bare `limit=` read never makes (e7cf78c6). It is a
/// second statement, not a transaction with the first: a row appended
/// between them can make `total` one ahead of an unbounded page, never
/// behind, and an `until`-bounded window is closed and cannot move.
///
/// One statement per question with nullable binds rather than the
/// tail's dynamic composition — `$n IS NULL` says "no filter" without
/// building SQL by hand.
pub async fn recent_by_kind(
    pool: &PgPool,
    kind: &str,
    window: &KindWindow<'_>,
    limit: i64,
) -> Result<KindPage, String> {
    const WHERE: &str = "WHERE kind = $1 \
         AND ($2::text IS NULL OR payload->>'scope' = $2) \
         AND ($3::timestamptz IS NULL OR timestamp >= $3) \
         AND ($4::timestamptz IS NULL OR timestamp < $4)";
    if let Some(key) = window.latest_per {
        return newest_per_key(pool, kind, window, key, WHERE, limit).await;
    }
    let rows = sqlx::query_as::<_, AuditEntry>(&format!(
        "SELECT event_id, timestamp, source, kind, payload FROM audit_log {WHERE} \
         ORDER BY timestamp DESC LIMIT $5"
    ))
    .bind(kind)
    .bind(window.scope)
    .bind(window.since)
    .bind(window.until)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;
    let (total,): (i64,) =
        sqlx::query_as(&format!("SELECT COUNT(*)::BIGINT FROM audit_log {WHERE}"))
            .bind(kind)
            .bind(window.scope)
            .bind(window.since)
            .bind(window.until)
            .fetch_one(pool)
            .await
            .map_err(|e| e.to_string())?;
    Ok(KindPage { rows, total })
}

/// [`recent_by_kind`] with `latest_per` set: the newest row of each
/// distinct `payload->>key` inside the window, newest first, and
/// `total` the number of those groups (backlog 725532ab).
///
/// The grouping is `DISTINCT ON` in the same statement as the WHERE
/// and BEFORE the LIMIT, for the reason the scope filter is: measured
/// 2026-09-25, `scope=host&limit=50` held 50 of 768 host rows, all
/// forge's, because forge compares every fifteen minutes and boss-gcp
/// once a day. Collapsing that page per host in the reader still has
/// no boss-gcp row to collapse. `event_id` breaks a timestamp tie so
/// the same log always answers the same row (determinism).
///
/// `COUNT(*)` over `SELECT DISTINCT` counts a row with no such key as
/// one group, exactly as `DISTINCT ON` returns one row for it — so
/// rows == total is a whole answer and rows < total a truncated one.
async fn newest_per_key(
    pool: &PgPool,
    kind: &str,
    window: &KindWindow<'_>,
    key: &str,
    filter: &str,
    limit: i64,
) -> Result<KindPage, String> {
    let rows = sqlx::query_as::<_, AuditEntry>(&format!(
        "SELECT event_id, timestamp, source, kind, payload FROM ( \
           SELECT DISTINCT ON (payload->>$6::text) event_id, timestamp, source, kind, payload \
           FROM audit_log {filter} \
           ORDER BY payload->>$6::text, timestamp DESC, event_id DESC \
         ) newest ORDER BY timestamp DESC, event_id DESC LIMIT $5"
    ))
    .bind(kind)
    .bind(window.scope)
    .bind(window.since)
    .bind(window.until)
    .bind(limit)
    .bind(key)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;
    let (total,): (i64,) = sqlx::query_as(&format!(
        "SELECT COUNT(*)::BIGINT FROM (SELECT DISTINCT payload->>$5::text FROM audit_log {filter}) groups"
    ))
    .bind(kind)
    .bind(window.scope)
    .bind(window.since)
    .bind(window.until)
    .bind(key)
    .fetch_one(pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(KindPage { rows, total })
}

/// Which rows of one kind [`recent_by_kind`] reads: an exact payload
/// `scope`, and a half-open `[since, until)` window on `timestamp`.
/// Every field absent reads the whole kind. `latest_per`, a top-level
/// payload key, reduces the window to the newest row per distinct
/// value of that key (backlog 725532ab).
#[derive(Debug, Clone, Copy, Default)]
pub struct KindWindow<'a> {
    pub scope: Option<&'a str>,
    pub since: Option<DateTime<Utc>>,
    pub until: Option<DateTime<Utc>>,
    pub latest_per: Option<&'a str>,
}

/// One page of a kind's rows, newest first, and how many rows its
/// window holds in all.
#[derive(Debug, Clone)]
pub struct KindPage {
    pub rows: Vec<AuditEntry>,
    pub total: i64,
}

/// One `(job kind, step kind, spec slug, authority role)` cell of the
/// station flow cube: how many obligations of that exact shape became
/// READY and how many were COMPLETED inside a wall-clock window.
///
/// Why this read exists at all: `GET /api/stations/load` answers a
/// station's depth, and its own header says depth is close to
/// meaningless without a drain rate. The rate needs no new stamp —
/// `step.ready.<kind>` and `step.done.<kind>` are already the two
/// transitions a step-waiting station's membership turns on. This is
/// the read that counts them; `boss-jobs`' `station_flow` module maps
/// the cells onto stations using each station's own predicate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, sqlx::FromRow)]
pub struct StepFlowRow {
    pub job_kind: String,
    pub step_kind: String,
    pub spec_slug: String,
    pub authority_role: String,
    pub arrived: i64,
    pub served: i64,
}

/// The flow cube over `[since, now]` — wall clock, deliberately.
///
/// `created_at`, NEVER `timestamp`. Event time is sim-authoritative on
/// a demo deployment, where the epoch runs 366 sim-days in about nine
/// real hours; a ten-minute triage measured on it reads as a week.
/// `boss-views/src/flow.rs` owns the doctrine and the incident behind
/// it, and this obeys it. `created_at` is a plain write instant nothing
/// overwrites, and the epoch trim DELETEs simulated rows rather than
/// rewriting survivors, so a real packet's wall-clock history is intact
/// across laps.
///
/// The join into `steps` / `jobs` is not decoration: the log's
/// `step.ready` payload carries neither the Job's kind nor the step's
/// `spec_slug`, so the two coordinates a station predicate needs most
/// have to come from the projection. DISTINCT on the step id, because
/// a step re-promoted to ready records a second event and is still one
/// obligation.
pub async fn step_flow_cube(
    pool: &PgPool,
    since: DateTime<Utc>,
) -> Result<Vec<StepFlowRow>, String> {
    sqlx::query_as::<_, StepFlowRow>(
        "WITH win AS (
             SELECT payload->>'step_id' AS sid,
                    kind LIKE 'step.ready.%' AS is_arrival
             FROM audit_log
             WHERE created_at > $1
               AND (kind LIKE 'step.ready.%' OR kind LIKE 'step.done.%')
               AND payload->>'step_id' IS NOT NULL
         )
         SELECT j.kind AS job_kind,
                s.kind AS step_kind,
                COALESCE(NULLIF(s.spec_slug, ''), '') AS spec_slug,
                COALESCE(s.metadata->>'authority_role', '') AS authority_role,
                COUNT(DISTINCT win.sid) FILTER (WHERE win.is_arrival)::BIGINT AS arrived,
                COUNT(DISTINCT win.sid) FILTER (WHERE NOT win.is_arrival)::BIGINT AS served
         FROM win
         JOIN steps s ON s.id::text = win.sid
         JOIN jobs j ON j.id = s.job_id
         GROUP BY 1, 2, 3, 4",
    )
    .bind(since)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())
}

/// Everything the log holds about ONE job, oldest first — the
/// per-packet audit read behind boss-jobs' `GET /api/jobs/{id}/events`
/// (c17871fe). Lives here for the same reason [`recent_by_kind`] does:
/// the SQL against `audit_log` stays in the crate that owns the table.
///
/// A job's slice is every row whose payload names it: step events
/// carry the job under `job_id`, the job's own lifecycle events carry
/// it as `id`. Both are expression-indexed (migration 202609081700).
///
/// `limit` is applied to the NEWEST rows (`ORDER BY id DESC LIMIT`)
/// and the page is then reversed, so a packet with a long history
/// answers with its most recent `limit` events in the order they
/// happened — not its first `limit`, which would hide the completion
/// the reader came for.
pub async fn recent_for_job(
    pool: &PgPool,
    job_id: &str,
    limit: i64,
) -> Result<Vec<AuditEntry>, String> {
    let mut rows = sqlx::query_as::<_, AuditEntry>(
        "SELECT event_id, timestamp, source, kind, payload FROM audit_log \
         WHERE payload->>'job_id' = $1 OR payload->>'id' = $1 \
         ORDER BY id DESC LIMIT $2",
    )
    .bind(job_id)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;
    rows.reverse();
    Ok(rows)
}

#[derive(Debug, Deserialize, Default)]
pub struct TailQuery {
    /// Exact-match filter on the `source` column (e.g. "jobs").
    pub source: Option<String>,
    /// Case-insensitive substring match on `kind` (e.g. "step" matches
    /// both `job.step.created` and `job.step.updated`).
    pub kind: Option<String>,
    /// Only rows with `timestamp >= since`.
    pub since: Option<DateTime<Utc>>,
    /// Only rows with `timestamp < until`.
    pub until: Option<DateTime<Utc>>,
    /// Max rows to return. Clamped to [1, 500]; default 100.
    pub limit: Option<i64>,
    /// Provenance filter: `real` hides simulated traffic, `sim` shows
    /// only it, absent shows everything.
    ///
    /// This exists because the audit log is overwhelmingly synthetic —
    /// 328,255 of 370,033 rows carried `_simulated: true` when this
    /// was written, 89%. Filtering client-side would have meant
    /// fetching a page and discarding nine rows in ten, so a
    /// 100-row page showed about eleven real events. The filter has to
    /// be where the LIMIT is applied or it does not really filter.
    ///
    /// The flag lives in the payload rather than a column
    /// (`payload->>'_simulated'`), so this reads it there. `real`
    /// COALESCEs: a row predating the flag is real, because the
    /// simulator did not exist to have written it.
    pub simulated: Option<String>,
    /// Exact match on who acted — `payload->>'_actor'`, the stamp every
    /// writer puts on its payload (backlog 03f79eca). park-a-job,
    /// rotate-a-credential and ship-a-change each state that the log
    /// answers "who and when"; until this, the tail could answer only
    /// "when", and who acted was visible only by opening a row's JSON.
    /// Exact, not the kind filter's substring: `agent-claude` must not
    /// also return `agent-claude-2`. A row predating the stamp has no
    /// actor and matches no actor filter.
    pub actor: Option<String>,
}

/// The actor clause, shared by the three reads that take one — tail,
/// export and stream — so the lens a page sets is the lens all three
/// apply. That the three reads each composed their own WHERE is how
/// the provenance lens came to be honoured by one of them and ignored
/// by two (34ea2ae0); a new filter does not repeat it.
fn push_actor(actor: Option<&String>, sql: &mut String, binds: &mut Vec<Bind>) {
    if let Some(actor) = actor {
        binds.push(Bind::Str(actor.clone()));
        sql.push_str(&format!(" AND payload->>'_actor' = ${}", binds.len()));
    }
}

/// The provenance clause — [`TailQuery::simulated`] — shared by tail,
/// export and stream for the reason [`push_actor`] states. Until
/// 2026-09-24 only the tail applied it: the stream declared no
/// `simulated` and the export's WHERE never read the one it parsed, so
/// live mode (the page's default) and Save .jsonl both served synthetic
/// rows under a select reading "Real only" (backlog 34ea2ae0).
///
/// No bind: the two spellings are a closed set decided here, never
/// caller text reaching SQL. An unrecognised value filters nothing,
/// which keeps a typo in a URL from silently hiding the log.
fn push_simulated(simulated: Option<&str>, sql: &mut String) {
    match simulated {
        Some("real") => {
            sql.push_str(" AND COALESCE(payload->>'_simulated', 'false') <> 'true'");
        }
        Some("sim") => sql.push_str(" AND payload->>'_simulated' = 'true'"),
        _ => {}
    }
}

async fn tail(
    State(state): State<AuditTailState>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<TailQuery>,
) -> Response {
    let tier_ok = matches!(user.access_tier, AccessTier::Operator | AccessTier::Auditor);
    let role_ok = boss_core::roles::has_global_read(&user.role);
    if !(tier_ok || role_ok) {
        return (
            StatusCode::FORBIDDEN,
            "operator tier or executive role required",
        )
            .into_response();
    }

    let limit = q.limit.unwrap_or(100).clamp(1, 500);

    // Dynamic WHERE composition — each filter contributes one AND
    // clause + one bind. Postgres' query planner handles the
    // timestamp DESC index via `audit_log_timestamp`.
    let mut sql =
        String::from("SELECT event_id, timestamp, source, kind, payload FROM audit_log WHERE 1=1");
    let mut binds: Vec<Bind> = Vec::new();
    if let Some(source) = &q.source {
        binds.push(Bind::Str(source.clone()));
        sql.push_str(&format!(" AND source = ${}", binds.len()));
    }
    if let Some(kind) = &q.kind {
        // Case-insensitive substring — ILIKE with wrapped %.
        binds.push(Bind::Str(format!("%{kind}%")));
        sql.push_str(&format!(" AND kind ILIKE ${}", binds.len()));
    }
    if let Some(since) = q.since {
        binds.push(Bind::Ts(since));
        sql.push_str(&format!(" AND timestamp >= ${}", binds.len()));
    }
    if let Some(until) = q.until {
        binds.push(Bind::Ts(until));
        sql.push_str(&format!(" AND timestamp < ${}", binds.len()));
    }
    push_actor(q.actor.as_ref(), &mut sql, &mut binds);
    push_simulated(q.simulated.as_deref(), &mut sql);
    binds.push(Bind::Int(limit));
    sql.push_str(&format!(" ORDER BY timestamp DESC LIMIT ${}", binds.len()));

    let mut query = sqlx::query_as::<_, AuditEntry>(&sql);
    for bind in binds {
        query = match bind {
            Bind::Str(s) => query.bind(s),
            Bind::Ts(t) => query.bind(t),
            Bind::Int(i) => query.bind(i),
        };
    }

    match query.fetch_all(state.pool.as_ref()).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Operator-on-demand audit_log export. Streams matching rows as
/// JSON Lines (one event per line) with
/// `Content-Disposition: attachment` so the browser saves the
/// response as a `.jsonl` file the operator can grep / jq / load
/// into a log tool.
///
/// JSON Lines (rather than CSV or a wrapped JSON array) was the
/// natural choice for audit_log:
/// - Each row is self-contained — parseable line-by-line by
///   standard tooling (jq, fluentbit, awk, splunk forwarder).
/// - Nested payloads stay intact; CSV would force lossy escaping
///   of the JSONB column.
/// - Append-only-friendly — the same shape the audit_log table
///   has on the writer side.
///
/// Filters mirror /api/events/tail (source, kind, since, until, actor,
/// simulated).
/// Cap is higher (50,000 rows) and the response is streamed so a
/// long-range export doesn't pin server memory.
///
/// Same auth gate as `tail` — operator/auditor tier or
/// has_global_read role. The audit_log carries every domain
/// payload (HR + financial + operational), so downloads are
/// privileged.
async fn export(
    State(state): State<AuditTailState>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<TailQuery>,
) -> Response {
    use axum::body::Body;
    use axum::http::header;
    use futures::stream::StreamExt;

    let tier_ok = matches!(user.access_tier, AccessTier::Operator | AccessTier::Auditor);
    let role_ok = boss_core::roles::has_global_read(&user.role);
    if !(tier_ok || role_ok) {
        return (
            StatusCode::FORBIDDEN,
            "operator tier or executive role required",
        )
            .into_response();
    }

    let limit = q.limit.unwrap_or(50_000).clamp(1, 50_000);

    let mut sql =
        String::from("SELECT event_id, timestamp, source, kind, payload FROM audit_log WHERE 1=1");
    let mut binds: Vec<Bind> = Vec::new();
    if let Some(source) = &q.source {
        binds.push(Bind::Str(source.clone()));
        sql.push_str(&format!(" AND source = ${}", binds.len()));
    }
    if let Some(kind) = &q.kind {
        binds.push(Bind::Str(format!("%{kind}%")));
        sql.push_str(&format!(" AND kind ILIKE ${}", binds.len()));
    }
    if let Some(since) = q.since {
        binds.push(Bind::Ts(since));
        sql.push_str(&format!(" AND timestamp >= ${}", binds.len()));
    }
    if let Some(until) = q.until {
        binds.push(Bind::Ts(until));
        sql.push_str(&format!(" AND timestamp < ${}", binds.len()));
    }
    push_actor(q.actor.as_ref(), &mut sql, &mut binds);
    push_simulated(q.simulated.as_deref(), &mut sql);
    binds.push(Bind::Int(limit));
    sql.push_str(&format!(" ORDER BY timestamp ASC LIMIT ${}", binds.len()));

    // Streaming query — sqlx fetches in pages under the hood so we
    // don't materialize all 50k rows at once. Each row gets
    // serialized to JSONL + pushed through a channel; the response
    // body is a ReceiverStream draining that channel. Errors land
    // as one-shot text frames (an aborted download is rare enough
    // not to warrant a structured error envelope).
    let pool = state.pool.clone();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(32);
    tokio::spawn(async move {
        let mut q = sqlx::query_as::<_, AuditEntry>(&sql);
        for bind in &binds {
            q = match bind {
                Bind::Str(s) => q.bind(s.clone()),
                Bind::Ts(t) => q.bind(*t),
                Bind::Int(i) => q.bind(*i),
            };
        }
        let mut rows = q.fetch(pool.as_ref());
        while let Some(row) = rows.next().await {
            let frame = match row {
                Ok(entry) => match serde_json::to_string(&entry) {
                    Ok(mut s) => {
                        s.push('\n');
                        Ok(axum::body::Bytes::from(s))
                    }
                    Err(e) => Err(std::io::Error::other(e.to_string())),
                },
                Err(e) => Err(std::io::Error::other(e.to_string())),
            };
            if tx.send(frame).await.is_err() {
                // Client hung up.
                return;
            }
        }
    });
    let body_stream = tokio_stream::wrappers::ReceiverStream::new(rx);

    // Filename hint reflects the window the operator selected,
    // falling back to "all" + current UTC instant when not pinned.
    let from_label = q
        .since
        .map(|t| t.format("%Y%m%d").to_string())
        .unwrap_or_else(|| "all".to_string());
    // `now` label avoids calling Utc::now() at the SQL handler
    // layer (which the no-wallclock lint catches). Filename ends
    // up as audit-log-{since}-now.jsonl when the operator
    // didn't pin an until — fine, the bundled rows still carry
    // their original timestamps in payload.
    let to_label = q
        .until
        .map(|t| t.format("%Y%m%d").to_string())
        .unwrap_or_else(|| "now".to_string());
    let filename = format!("audit-log-{from_label}-{to_label}.jsonl");

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/x-ndjson")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())
}

/// Public companion to [`tail`] — unauth, restricted to a curated
/// demo-friendly topic set, capped at 50 rows. The landing page
/// polls this every ~2-3s to render a right-rail event tail of
/// "what the operating company just did" alongside its existing
/// `/api/jobs/live` snapshot.
///
/// **Curated topic set** (allow-list, anything else is filtered
/// out so visitors don't see internal noise):
/// - `jobs.job.opened` / `jobs.job.closed` — Job lifecycle.
/// - `jobs.step.completed` — Step transitions (the load-bearing
///   "coordination event between people" signal).
/// - `commerce.invoice.issued` / `commerce.invoice.paid` —
///   the commercial heartbeat.
/// - `inventory.item_received` / `inventory.item_consumed` —
///   physical-flow signals.
/// - `delivery.tracking_*` — the courier counterparty chain.
/// - `accounts.account.created` — new customer signals.
///
/// **Sanitization**: payload is returned as-is for now; the
/// curated topic set is the privacy gate. Future revisions may
/// strip per-row sensitive fields (e.g. customer email on
/// commerce.invoice.issued) — captured as a follow-up if a
/// real tenant wires sensitive payloads through these topics.
async fn public_tail(
    State(state): State<AuditTailState>,
    Query(q): Query<PublicTailQuery>,
) -> Response {
    const PUBLIC_TOPICS: &[&str] = &[
        "jobs.job.opened",
        "jobs.job.closed",
        "jobs.step.completed",
        "commerce.invoice.issued",
        "commerce.invoice.paid",
        "inventory.item_received",
        "inventory.item_consumed",
        "delivery.tracking_in_transit",
        "delivery.tracking_out_for_delivery",
        "delivery.tracking_delivered",
        "accounts.account.created",
        "asset.received",
        "shipping.shipment.created",
    ];
    let limit = q.limit.unwrap_or(30).clamp(1, 50);

    // ANY($1) makes the topic allow-list a single bind — postgres
    // expands the array efficiently against the (timestamp DESC)
    // index. Adding new topics is one line above; no SQL change.
    let topics: Vec<String> = PUBLIC_TOPICS.iter().map(|s| s.to_string()).collect();
    let sql = "SELECT event_id, timestamp, source, kind, payload \
               FROM audit_log \
               WHERE kind = ANY($1) \
               ORDER BY timestamp DESC \
               LIMIT $2";
    let rows = sqlx::query_as::<_, AuditEntry>(sql)
        .bind(&topics)
        .bind(limit)
        .fetch_all(state.pool.as_ref())
        .await;
    match rows {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct PublicTailQuery {
    /// Max rows to return. Clamped to [1, 50]; default 30.
    pub limit: Option<i64>,
}

/// Small sum-type to carry the heterogeneous bind values through the
/// dynamic-SQL loop. Keeps the typing honest without needing a macro.
enum Bind {
    Str(String),
    Ts(DateTime<Utc>),
    Int(i64),
}

#[derive(Debug, Deserialize, Default)]
pub struct StreamQuery {
    /// Exact-match filter on the `source` column.
    pub source: Option<String>,
    /// Case-insensitive substring match on `kind`.
    pub kind: Option<String>,
    /// Exact match on `payload->>'_actor'` — [`TailQuery::actor`].
    pub actor: Option<String>,
    /// Provenance lens — [`TailQuery::simulated`]. Absent until
    /// 2026-09-24, so the page's default live mode painted synthetic
    /// rows into a "Real only" view (backlog 34ea2ae0).
    pub simulated: Option<String>,
}

/// SSE companion to `/api/events/tail`. Pushes new audit_log rows
/// as they land, keyed off the table's monotonic id column. Filters
/// (source, kind, actor, simulated) match the tail endpoint's shape.
///
/// Server-side polls the audit_log every 2s for `id > last_seen`,
/// dedupes by id, pushes each new row as one SSE `data` frame. A read
/// that fails sends one `event: failed` frame, `{"error": "..."}`, and
/// ends the stream — silence means a quiet log and nothing else.
/// Same auth gate as `tail` — operator/auditor tier or ceo/cto
/// role. Per the SSE policy doc (docs/design/sse-policy.md) this
/// view is "every event matters" → SSE-push, since the 5s poll
/// loses ordering guarantees a stream preserves.
async fn stream(
    State(state): State<AuditTailState>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<StreamQuery>,
) -> Response {
    use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
    use std::convert::Infallible;
    use std::time::Duration;

    let tier_ok = matches!(user.access_tier, AccessTier::Operator | AccessTier::Auditor);
    let role_ok = boss_core::roles::has_global_read(&user.role);
    if !(tier_ok || role_ok) {
        return (
            StatusCode::FORBIDDEN,
            "operator tier or executive role required",
        )
            .into_response();
    }

    let pool = state.pool.clone();
    let source_filter = q.source;
    let kind_filter = q.kind;
    let actor_filter = q.actor;
    let simulated_filter = q.simulated;

    // A failed read of the log is ONE named frame, and then the stream
    // ends (backlog 260879f5, page audit 65a273d5). It used to be
    // `Err(_) => continue`: the connection stayed open, keep-alives
    // kept flowing and no frame ever came, which to the page is exactly
    // a log where nothing is happening. Named `failed` rather than
    // sent as a plain `data:` frame so the page's row handler never
    // reads it as a row; ended rather than retried so a reconnect is
    // the page's decision, made in words it can show.
    fn read_failed(read: &str, e: &sqlx::Error) -> Result<SseEvent, Infallible> {
        let body = serde_json::json!({ "error": format!("{read}: {e}") });
        Ok(SseEvent::default().event("failed").data(body.to_string()))
    }

    let stream = async_stream::stream! {
        // First: anchor the cursor at the current MAX(id). The
        // operator gets rows arriving AFTER they connect, not a
        // history dump (the tail endpoint is the right tool for
        // history). MAX is constant-time on the audit_log_id_pk
        // index. A failed anchor used to read as cursor 0, so a read
        // that recovered on the next tick replayed the whole log as
        // if it were landing now.
        let anchor = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM audit_log")
            .fetch_one(pool.as_ref())
            .await;
        let mut cursor: i64 = match anchor {
            Ok(id) => id,
            Err(e) => {
                yield read_failed("anchoring the stream at the log's newest row", &e);
                return;
            }
        };

        let mut tick = tokio::time::interval(Duration::from_secs(2));
        tick.set_missed_tick_behavior(
            tokio::time::MissedTickBehavior::Delay,
        );
        loop {
            tick.tick().await;

            // Dynamic WHERE: id > cursor + optional source/kind
            // filters. Cap at 500 rows per tick — a wider gap
            // means the stream client missed a window; serving
            // the next 500 and updating the cursor catches up
            // naturally on the next tick.
            let mut sql = String::from(
                "SELECT event_id, timestamp, source, kind, payload, id \
                 FROM audit_log WHERE id > $1",
            );
            let mut binds: Vec<Bind> = vec![Bind::Int(cursor)];
            if let Some(source) = &source_filter {
                binds.push(Bind::Str(source.clone()));
                sql.push_str(&format!(" AND source = ${}", binds.len()));
            }
            if let Some(kind) = &kind_filter {
                binds.push(Bind::Str(format!("%{kind}%")));
                sql.push_str(&format!(" AND kind ILIKE ${}", binds.len()));
            }
            push_actor(actor_filter.as_ref(), &mut sql, &mut binds);
            push_simulated(simulated_filter.as_deref(), &mut sql);
            sql.push_str(" ORDER BY id ASC LIMIT 500");

            #[derive(sqlx::FromRow)]
            struct Row {
                event_id: Uuid,
                timestamp: DateTime<Utc>,
                source: String,
                kind: String,
                payload: serde_json::Value,
                id: i64,
            }
            let mut q = sqlx::query_as::<_, Row>(&sql);
            for bind in binds {
                q = match bind {
                    Bind::Str(s) => q.bind(s),
                    Bind::Ts(t) => q.bind(t),
                    Bind::Int(i) => q.bind(i),
                };
            }
            let rows = match q.fetch_all(pool.as_ref()).await {
                Ok(r) => r,
                Err(e) => {
                    yield read_failed("reading rows past the stream's cursor", &e);
                    return;
                }
            };
            for row in rows {
                cursor = row.id;
                let entry = AuditEntry {
                    event_id: row.event_id,
                    timestamp: row.timestamp,
                    source: row.source,
                    kind: row.kind,
                    payload: row.payload,
                };
                if let Ok(json) = serde_json::to_string(&entry) {
                    yield Ok::<_, Infallible>(SseEvent::default().data(json));
                }
            }
        }
    };

    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}
