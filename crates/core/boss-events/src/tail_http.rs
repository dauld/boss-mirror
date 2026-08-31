//! Audit-log tail HTTP endpoint — the read surface for `audit_log`.
//!
//! Writers (every service's `PgAuditWriter`) insert rows; this router
//! serves recent-first reads with filters on source, kind, and time
//! window. Intended home: the CTO surface at `/cto/events`, where an
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

/// Liveness probe — used by deploy-services.sh + IT Monitoring
/// page to confirm the service is reachable. No auth gate.
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
pub async fn recent_by_kind(
    pool: &PgPool,
    kind: &str,
    limit: i64,
) -> Result<Vec<AuditEntry>, String> {
    sqlx::query_as::<_, AuditEntry>(
        "SELECT event_id, timestamp, source, kind, payload FROM audit_log \
         WHERE kind = $1 ORDER BY timestamp DESC LIMIT $2",
    )
    .bind(kind)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())
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
    // No bind: the two spellings are a closed set decided here, never
    // caller text reaching SQL. An unrecognised value filters nothing,
    // which keeps a typo in a URL from silently hiding the log.
    match q.simulated.as_deref() {
        Some("real") => {
            sql.push_str(" AND COALESCE(payload->>'_simulated', 'false') <> 'true'");
        }
        Some("sim") => sql.push_str(" AND payload->>'_simulated' = 'true'"),
        _ => {}
    }
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
/// Filters mirror /api/events/tail (source, kind, since, until).
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
}

/// SSE companion to `/api/events/tail`. Pushes new audit_log rows
/// as they land, keyed off the table's monotonic id column. Filters
/// (source, kind) match the tail endpoint's shape.
///
/// Server-side polls the audit_log every 2s for `id > last_seen`,
/// dedupes by id, pushes each new row as one SSE `data` frame.
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

    let stream = async_stream::stream! {
        // First: anchor the cursor at the current MAX(id). The
        // operator gets rows arriving AFTER they connect, not a
        // history dump (the tail endpoint is the right tool for
        // history). MAX is constant-time on the audit_log_id_pk
        // index.
        let mut cursor: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(id), 0) FROM audit_log")
            .fetch_one(pool.as_ref())
            .await
            .unwrap_or(0);

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
                Err(_) => continue,
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
