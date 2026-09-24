//! Integration tests for `audit_tail_router` — the `/api/events/tail`
//! read surface over `audit_log`.
//!
//! Covers: authz (operator tier + ceo/cto role allow, plain user
//! denies), source/kind/actor filtering, limit clamping, descending
//! order, and the actor filter on the export and the live stream —
//! and, since 2026-09-24, the three reads the /it/operate/audit page
//! makes that had no server test at all (backlog 0398c4d0, from page
//! audit 65a273d5): the tail's `simulated` provenance lens, the JSONL
//! export, and the SSE stream. The page's browser half is pinned in
//! apps/web/tests/mocked/audit-log-page.mocked.spec.ts; this is the
//! half a mocked browser cannot reach — what the SQL actually selects.

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode, header};
use boss_core::audit::AuditWriter;
use boss_core::event::Event;
use boss_events::{PgAuditWriter, audit_tail_router};
use boss_testing::TestDb;
use chrono::{DateTime, Duration, TimeZone, Utc};
use futures::StreamExt;
use tower::ServiceExt;
use uuid::Uuid;

fn operator_user() -> String {
    serde_json::json!({
        "id": "emp-op",
        "role": "service-tech",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": null,
    })
    .to_string()
}

fn cto_user() -> String {
    serde_json::json!({
        "id": "emp-001",
        "role": "cto",
        "access_tier": "user",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "executive",
    })
    .to_string()
}

fn plain_user() -> String {
    serde_json::json!({
        "id": "emp-2",
        "role": "service-tech",
        "access_tier": "user",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": null,
    })
    .to_string()
}

async fn seed(writer: &PgAuditWriter, source: &str, kind: &str, mins_ago: i64) -> Uuid {
    let id = Uuid::new_v4();
    let ts = Utc::now() - Duration::minutes(mins_ago);
    writer
        .write(&Event {
            id,
            timestamp: ts,
            source: source.into(),
            kind: kind.into(),
            payload: serde_json::json!({"mins_ago": mins_ago}),
        })
        .await
        .unwrap();
    id
}

async fn body_json(resp: axum::response::Response) -> serde_json::Value {
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn get_req(uri: &str, user_header: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-boss-user", user_header)
        .header(header::ACCEPT, "application/json")
        .body(Body::empty())
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn stats_report_the_logs_size_and_growth() {
    // David, 2026-09-02 (168b3f25): "We need size and growth stats on
    // audit log to make sure it isn't growing unsustainably." The
    // number lived in a nightly pod log; now it is a read.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    // Three of kind `a` (two today, one 40h ago) and one `b` today.
    seed(&writer, "jobs", "growth.a", 10).await;
    seed(&writer, "jobs", "growth.a", 60).await;
    seed(&writer, "jobs", "growth.a", 40 * 60).await;
    seed(&writer, "ledger", "growth.b", 5).await;
    let app: Router = audit_tail_router(db.pool.clone());

    let resp = app
        .clone()
        .oneshot(get_req("/api/events/stats", &operator_user()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let j = body_json(resp).await;
    assert!(j["total_rows"].as_i64().unwrap() >= 4, "{j}");
    assert!(
        j["table_bytes"].as_i64().unwrap() > 0,
        "size on disk is a number"
    );
    assert!(j["rows_last_24h"].as_i64().unwrap() >= 3, "{j}");
    assert!(j["rows_last_7d"].as_i64().unwrap() >= 4, "{j}");
    let days = j["per_day"].as_array().unwrap();
    let summed: i64 = days.iter().map(|d| d["rows"].as_i64().unwrap()).sum();
    assert!(
        summed >= 4,
        "per-day buckets carry every row of the window: {j}"
    );
    assert!(days.iter().all(|d| d["day"].as_str().is_some()));
    let top = j["top_kinds"].as_array().unwrap();
    let a = top
        .iter()
        .find(|k| k["kind"] == "growth.a")
        .expect("growth.a is counted");
    assert_eq!(a["rows"], 3, "{j}");
    assert!(j["oldest_at"].as_str().is_some() && j["newest_at"].as_str().is_some());

    // The same door as the tail: a plain user is refused.
    let resp = app
        .oneshot(get_req("/api/events/stats", &plain_user()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_user_is_forbidden() {
    let db = TestDb::new().await;
    let app: Router = audit_tail_router(db.pool.clone());

    let resp = app
        .oneshot(get_req("/api/events/tail", &plain_user()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn cto_role_is_allowed_even_on_user_tier() {
    // Executive roles get global read access. In production the
    // executive set is registered at startup from the Class registry;
    // register it here so `has_global_read("cto")` resolves.
    boss_core::roles::init_executive_roles(
        ["ceo", "coo", "cto", "cfo"].into_iter().map(String::from),
    );
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed(&writer, "jobs", "job.created", 1).await;

    let app: Router = audit_tail_router(db.pool.clone());
    let resp = app
        .oneshot(get_req("/api/events/tail", &cto_user()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_tier_is_allowed() {
    let db = TestDb::new().await;
    let app: Router = audit_tail_router(db.pool.clone());
    let resp = app
        .oneshot(get_req("/api/events/tail", &operator_user()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn filters_by_source_and_returns_newest_first() {
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());

    // 3 rows across 2 sources, at 3 distinct timestamps (newest last
    // in insert order so we can verify DESC on read).
    seed(&writer, "jobs", "job.created", 30).await;
    seed(&writer, "assets", "asset.updated", 20).await;
    let newest_jobs = seed(&writer, "jobs", "job.step.updated", 5).await;

    let app: Router = audit_tail_router(db.pool.clone());
    let resp = app
        .oneshot(get_req("/api/events/tail?source=jobs", &operator_user()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 2, "assets row should be filtered out");
    // Newest first → the 5-min-ago row leads.
    assert_eq!(
        rows[0]["event_id"].as_str().unwrap(),
        newest_jobs.to_string()
    );
    assert_eq!(rows[0]["source"].as_str().unwrap(), "jobs");
}

#[tokio::test(flavor = "multi_thread")]
async fn filters_by_kind_substring_case_insensitive() {
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    seed(&writer, "jobs", "job.created", 10).await;
    seed(&writer, "jobs", "job.step.updated", 5).await;
    seed(&writer, "assets", "asset.updated", 1).await;

    let app: Router = audit_tail_router(db.pool.clone());
    let resp = app
        .oneshot(get_req("/api/events/tail?kind=STEP", &operator_user()))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["kind"].as_str().unwrap(), "job.step.updated");
}

/// One row whose payload names who acted, the way every writer stamps
/// it (`_actor`). `None` writes a row predating the stamp.
async fn seed_by(writer: &PgAuditWriter, kind: &str, actor: Option<&str>, mins_ago: i64) -> Uuid {
    let id = Uuid::new_v4();
    let payload = match actor {
        Some(a) => serde_json::json!({"_actor": a, "mins_ago": mins_ago}),
        None => serde_json::json!({"mins_ago": mins_ago}),
    };
    writer
        .write(&Event {
            id,
            timestamp: Utc::now() - Duration::minutes(mins_ago),
            source: "jobs".into(),
            kind: kind.into(),
            payload,
        })
        .await
        .unwrap();
    id
}

// Backlog 03f79eca: park-a-job, rotate-a-credential and ship-a-change
// each say the log answers "who and when", and the tail could answer
// only "when" — who acted rode inside the payload with no parameter
// that reached it. EXACT match, deliberately not the kind filter's
// substring: `agent-claude` must not also return `agent-claude-2`.
// In the WHERE clause, beside the LIMIT, for the reason
// `TailQuery::simulated` states: a filter applied to a returned page
// does not filter.
#[tokio::test(flavor = "multi_thread")]
async fn filters_by_actor_exactly() {
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    let older = seed_by(&writer, "job.opened", Some("agent-claude"), 30).await;
    seed_by(&writer, "job.opened", Some("agent-claude-2"), 20).await;
    seed_by(&writer, "job.opened", Some("emp-david"), 10).await;
    seed_by(&writer, "job.opened", None, 8).await;
    let newer = seed_by(&writer, "job.closed", Some("agent-claude"), 5).await;

    let app: Router = audit_tail_router(db.pool.clone());
    let resp = app
        .clone()
        .oneshot(get_req(
            "/api/events/tail?actor=agent-claude",
            &operator_user(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_json(resp).await;
    let ids: Vec<&str> = body
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["event_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec![newer.to_string(), older.to_string()], "{body}");

    // Composes with the other filters rather than replacing them.
    let resp = app
        .clone()
        .oneshot(get_req(
            "/api/events/tail?actor=agent-claude&kind=closed",
            &operator_user(),
        ))
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 1, "{body}");

    // A prefix of an actor is not that actor.
    let resp = app
        .oneshot(get_req("/api/events/tail?actor=agent", &operator_user()))
        .await
        .unwrap();
    let body = body_json(resp).await;
    assert_eq!(body.as_array().unwrap().len(), 0, "{body}");
}

// The export must honour the same actor lens the table shows — a
// download that disagreed with the view above it is gap 2 of the same
// audit (34ea2ae0) in a new place.
#[tokio::test(flavor = "multi_thread")]
async fn export_honours_the_actor_filter() {
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    let mine = seed_by(&writer, "job.opened", Some("agent-claude"), 30).await;
    seed_by(&writer, "job.opened", Some("emp-david"), 10).await;

    let app: Router = audit_tail_router(db.pool.clone());
    let resp = app
        .oneshot(get_req(
            "/api/events/export?actor=agent-claude",
            &operator_user(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0]["event_id"].as_str().unwrap(), mine.to_string());
}

// Live mode is the page's DEFAULT, so an actor filter the stream did
// not apply would paint every other actor's rows into a view labelled
// with one — the shape gap 1 (34ea2ae0) found for provenance.
//
// The stream anchors at MAX(id) on its first poll and pushes rows
// after it, so the rows are written in a loop until a frame arrives:
// whichever write the anchor lands behind, each round writes the
// OTHER actor's row first, so an unfiltered stream's first frame is
// always the wrong one.
#[tokio::test(flavor = "multi_thread")]
async fn stream_honours_the_actor_filter() {
    use futures::StreamExt;

    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    let app: Router = audit_tail_router(db.pool.clone());
    let resp = app
        .oneshot(get_req(
            "/api/events/stream?actor=agent-claude",
            &operator_user(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut body = resp.into_body().into_data_stream();

    let first_frame = async {
        let mut buf = String::new();
        loop {
            let chunk = body.next().await.expect("stream ended").unwrap();
            buf.push_str(std::str::from_utf8(&chunk).unwrap());
            if let Some(line) = buf.lines().find(|l| l.starts_with("data:")) {
                return serde_json::from_str::<serde_json::Value>(line["data:".len()..].trim())
                    .unwrap();
            }
        }
    };
    let writes = async {
        loop {
            seed_by(&writer, "job.opened", Some("emp-david"), 0).await;
            seed_by(&writer, "job.opened", Some("agent-claude"), 0).await;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    };
    let frame = tokio::time::timeout(std::time::Duration::from_secs(20), async {
        tokio::select! {
            f = first_frame => f,
            _ = writes => unreachable!("the write loop never ends"),
        }
    })
    .await
    .expect("a frame within 20s");
    assert_eq!(frame["payload"]["_actor"], "agent-claude", "{frame}");
}

#[tokio::test(flavor = "multi_thread")]
async fn limit_clamps_and_payload_roundtrips() {
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());

    let id = Uuid::new_v4();
    writer
        .write(&Event {
            id,
            timestamp: Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap(),
            source: "jobs".into(),
            kind: "job.created".into(),
            payload: serde_json::json!({"hello": "world", "n": 42}),
        })
        .await
        .unwrap();

    let app: Router = audit_tail_router(db.pool.clone());
    // limit=0 should clamp up to 1; the one row we wrote still appears.
    let resp = app
        .oneshot(get_req("/api/events/tail?limit=0", &operator_user()))
        .await
        .unwrap();
    let body = body_json(resp).await;
    let rows = body.as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["payload"]["hello"].as_str().unwrap(), "world");
    assert_eq!(rows[0]["payload"]["n"].as_i64().unwrap(), 42);
}

/// One row at an exact instant with an exact payload — the provenance
/// flag and the export's filename both turn on values `seed` does not
/// let a test choose.
async fn seed_at(
    writer: &PgAuditWriter,
    source: &str,
    kind: &str,
    at: DateTime<Utc>,
    payload: serde_json::Value,
) -> Uuid {
    let id = Uuid::new_v4();
    writer
        .write(&Event {
            id,
            timestamp: at,
            source: source.into(),
            kind: kind.into(),
            payload,
        })
        .await
        .unwrap();
    id
}

fn ids(rows: &serde_json::Value) -> Vec<String> {
    rows.as_array()
        .unwrap()
        .iter()
        .map(|r| r["event_id"].as_str().unwrap().to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn simulated_lens_filters_where_the_limit_is_applied() {
    // The page's "Real only" / "Simulated only" lens is `?simulated=`.
    // Three provenances: a row predating the flag (no key — real,
    // because the simulator did not exist to write it), a row saying
    // `false`, and a synthetic row saying `true`.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    let base = Utc.with_ymd_and_hms(2026, 9, 20, 12, 0, 0).unwrap();
    let unflagged = seed_at(&writer, "jobs", "job.created", base, serde_json::json!({})).await;
    let real = seed_at(
        &writer,
        "jobs",
        "job.closed",
        base + Duration::minutes(1),
        serde_json::json!({"_simulated": false}),
    )
    .await;
    // Five synthetic rows, all NEWER than both real ones — the shape
    // of a log that is 89% synthetic (TailQuery::simulated's header).
    let mut sim = Vec::new();
    for n in 0..5 {
        sim.push(
            seed_at(
                &writer,
                "sim",
                "job.created",
                base + Duration::minutes(10 + n),
                serde_json::json!({"_simulated": true}),
            )
            .await
            .to_string(),
        );
    }
    let app: Router = audit_tail_router(db.pool.clone());
    let read = |uri: &'static str| {
        let app = app.clone();
        async move {
            let resp = app.oneshot(get_req(uri, &operator_user())).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK, "{uri}");
            body_json(resp).await
        }
    };

    assert_eq!(
        ids(&read("/api/events/tail?simulated=real").await),
        vec![real.to_string(), unflagged.to_string()],
        "real = the unflagged row and the false one, newest first"
    );
    let only_sim = ids(&read("/api/events/tail?simulated=sim").await);
    assert_eq!(only_sim.len(), 5);
    assert!(only_sim.iter().all(|id| sim.contains(id)), "{only_sim:?}");
    assert_eq!(ids(&read("/api/events/tail").await).len(), 7, "no lens");
    // An unrecognised spelling filters nothing — a typo in a URL must
    // not silently hide the log (the closed set in `tail`).
    assert_eq!(
        ids(&read("/api/events/tail?simulated=REAL").await).len(),
        7,
        "an unknown value is no lens"
    );
    // THE POINT OF THE LENS: it sits in the WHERE clause, under the
    // LIMIT. A one-row page of real traffic is the newest REAL row,
    // though five synthetic rows are newer — filtering a fetched page
    // client-side would have answered an empty page here.
    assert_eq!(
        ids(&read("/api/events/tail?simulated=real&limit=1").await),
        vec![real.to_string()]
    );
}

async fn body_text(resp: axum::response::Response) -> String {
    let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn export_streams_json_lines_oldest_first_as_a_named_attachment() {
    // The page's Export button. JSON Lines, one event per line, the
    // tail's filters, OLDEST first (a file is read top-down, the tail
    // newest-first), and a filename that names the window chosen.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    let day = Utc.with_ymd_and_hms(2026, 4, 20, 10, 0, 0).unwrap();
    let early = seed_at(
        &writer,
        "jobs",
        "job.created",
        day,
        serde_json::json!({"n": 1}),
    )
    .await;
    seed_at(
        &writer,
        "assets",
        "asset.updated",
        day + Duration::hours(1),
        serde_json::json!({}),
    )
    .await;
    let late = seed_at(
        &writer,
        "jobs",
        "job.closed",
        day + Duration::hours(2),
        serde_json::json!({"n": 2}),
    )
    .await;
    // Outside the window on both sides: `until` is exclusive.
    seed_at(
        &writer,
        "jobs",
        "job.created",
        day - Duration::days(1),
        serde_json::json!({}),
    )
    .await;
    seed_at(
        &writer,
        "jobs",
        "job.created",
        Utc.with_ymd_and_hms(2026, 4, 21, 0, 0, 0).unwrap(),
        serde_json::json!({}),
    )
    .await;
    let app: Router = audit_tail_router(db.pool.clone());

    let resp = app
        .clone()
        .oneshot(get_req(
            "/api/events/export?source=jobs&since=2026-04-20T00:00:00Z&until=2026-04-21T00:00:00Z",
            &operator_user(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "application/x-ndjson");
    assert_eq!(
        resp.headers()[header::CONTENT_DISPOSITION],
        "attachment; filename=\"audit-log-20260420-20260421.jsonl\""
    );
    let text = body_text(resp).await;
    assert!(text.ends_with('\n'), "every line is terminated: {text:?}");
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).expect("each line is one JSON event"))
        .collect();
    assert_eq!(
        ids(&serde_json::Value::Array(lines.clone())),
        vec![early.to_string(), late.to_string()],
        "the window's jobs rows, oldest first: {text}"
    );
    assert_eq!(lines[1]["payload"]["n"], 2, "the payload rides intact");

    // No window pinned: the filename says so rather than inventing
    // dates (the handler reads no wall clock).
    let resp = app
        .clone()
        .oneshot(get_req("/api/events/export?kind=CLOSED", &operator_user()))
        .await
        .unwrap();
    assert_eq!(
        resp.headers()[header::CONTENT_DISPOSITION],
        "attachment; filename=\"audit-log-all-now.jsonl\""
    );
    assert_eq!(body_text(resp).await.lines().count(), 1, "kind is ILIKE");

    // The same door as the tail: the log carries every domain payload.
    let resp = app
        .oneshot(get_req("/api/events/export", &plain_user()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test(flavor = "multi_thread")]
async fn export_ignores_the_provenance_lens_today() {
    // PINNED AS IT IS, GAP INCLUDED — the convention the page's mocked
    // spec keeps: the export reads TailQuery, which carries
    // `simulated`, and never applies it, so "Real only" + Export
    // downloads the synthetic rows too (gap 34ea2ae0). The car that
    // closes that gap turns this red and must rewrite it to expect one
    // line.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    let at = Utc.with_ymd_and_hms(2026, 9, 20, 12, 0, 0).unwrap();
    seed_at(&writer, "jobs", "job.created", at, serde_json::json!({})).await;
    seed_at(
        &writer,
        "sim",
        "job.created",
        at + Duration::minutes(1),
        serde_json::json!({"_simulated": true}),
    )
    .await;
    let app: Router = audit_tail_router(db.pool.clone());
    let resp = app
        .oneshot(get_req(
            "/api/events/export?simulated=real",
            &operator_user(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(body_text(resp).await.lines().count(), 2);
}

/// The next SSE `data:` payload off a live stream, parsed, skipping
/// keep-alive comments; `None` when `wait` passes first. A frame is
/// one event today, but the split on the blank line keeps this honest
/// if a chunk ever carries two.
async fn next_data<S>(frames: &mut S, wait: std::time::Duration) -> Option<serde_json::Value>
where
    S: futures::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin,
{
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let chunk = tokio::time::timeout_at(deadline, frames.next())
            .await
            .ok()?;
        let bytes = chunk
            .expect("the stream does not end while the client holds it")
            .expect("a frame, not a body error");
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        let data = text
            .split("\n\n")
            .flat_map(|event| event.lines())
            .find_map(|line| line.strip_prefix("data:"));
        if let Some(json) = data {
            return Some(serde_json::from_str(json.trim()).expect("a data frame is one event"));
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn stream_pushes_rows_landing_after_connect_that_match_its_filter() {
    // The page's live view. The stream anchors at MAX(id) when it is
    // first polled and then pushes, every 2s, rows past that cursor
    // that match `source` (exact) and `kind` (ILIKE) — history is the
    // tail's job, not the stream's.
    let db = TestDb::new().await;
    let writer = PgAuditWriter::new(db.pool.clone());
    let history = seed(&writer, "jobs", "job.step.updated", 1).await;
    let app: Router = audit_tail_router(db.pool.clone());

    let refused = app
        .clone()
        .oneshot(get_req("/api/events/stream", &plain_user()))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);

    let resp = app
        .oneshot(get_req(
            "/api/events/stream?source=jobs&kind=STEP",
            &operator_user(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.headers()[header::CONTENT_TYPE], "text/event-stream");
    let mut frames = resp.into_body().into_data_stream();

    // The first poll anchors the cursor and runs the immediate first
    // tick. A stream that replayed history would hand back the row
    // seeded above here, at once.
    let replay = next_data(&mut frames, std::time::Duration::from_millis(700)).await;
    assert!(replay.is_none(), "history is not replayed: {replay:?}");

    // Each round writes two rows the filter must drop BEFORE the one it
    // must keep, so a broken filter surfaces first (the stream orders
    // by id). Rounds, not one write, because the anchor is taken
    // inside the stream and nothing outside can observe it: a row
    // written before it is correctly never sent, and a later round's
    // row is past it for certain.
    let mut kept: Vec<String> = Vec::new();
    let mut pushed = None;
    for _ in 0..3 {
        seed(&writer, "assets", "asset.step.moved", 0).await; // wrong source
        seed(&writer, "jobs", "job.created", 0).await; // wrong kind
        kept.push(
            seed(&writer, "jobs", "job.step.updated", 0)
                .await
                .to_string(),
        );
        pushed = next_data(&mut frames, std::time::Duration::from_secs(5)).await;
        if pushed.is_some() {
            break;
        }
    }
    let pushed = pushed.expect("a matching row written after connect is pushed");
    let id = pushed["event_id"].as_str().unwrap().to_string();
    assert_ne!(id, history.to_string(), "history is not replayed");
    assert!(
        kept.contains(&id),
        "the first push is a row the filter keeps, not one it drops: {pushed}"
    );
    assert_eq!(pushed["source"], "jobs");
    assert_eq!(pushed["kind"], "job.step.updated");
}

/// The next SSE event off a live stream as `(event, data)` — `event`
/// is `message` when the frame names none, the SSE default — or `None`
/// when the stream ENDS. Keep-alive comments are skipped. Unlike
/// [`next_data`], the end of the stream is an answer here, not a
/// panic: a stream whose read failed is expected to say so and end.
async fn next_event<S>(frames: &mut S, wait: std::time::Duration) -> Option<(String, String)>
where
    S: futures::Stream<Item = Result<axum::body::Bytes, axum::Error>> + Unpin,
{
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let chunk = tokio::time::timeout_at(deadline, frames.next())
            .await
            .unwrap_or_else(|_| panic!("a frame or the end of the stream within {wait:?}"))?;
        let bytes = chunk.expect("a frame, not a body error");
        let text = String::from_utf8(bytes.to_vec()).unwrap();
        for event in text.split("\n\n") {
            let field = |name: &str| {
                event
                    .lines()
                    .find_map(|l| l.strip_prefix(name))
                    .map(|v| v.trim().to_string())
            };
            if let Some(data) = field("data:") {
                let name = field("event:").unwrap_or_else(|| "message".into());
                return Some((name, data));
            }
        }
    }
}

/// Make every later read of `audit_log` on this test's own database
/// fail the way a lost table, a revoked grant or a dropped connection
/// would: the query errors. The database is the test's alone (TestDb
/// copies a template per test), so nothing else sees it.
async fn break_the_log(db: &TestDb) {
    sqlx::query("ALTER TABLE audit_log RENAME TO audit_log_gone")
        .execute(&db.pool)
        .await
        .unwrap();
}

// A dead stream must not look like a quiet log (backlog 260879f5, page
// audit 65a273d5). The poll loop answered a failed read with
// `Err(_) => continue`: the connection stayed OPEN, keep-alives kept
// flowing, and no frame ever came — to the page, exactly the shape of
// a log where nothing is happening. A failed read now sends ONE frame,
// `event: failed`, naming the error, and ENDS the stream, so the page
// can say the stream is down and why.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_read_is_a_named_frame_and_the_stream_ends() {
    let db = TestDb::new().await;
    let app: Router = audit_tail_router(db.pool.clone());
    let resp = app
        .oneshot(get_req("/api/events/stream", &operator_user()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut frames = resp.into_body().into_data_stream();

    // The first poll anchors the cursor and runs the immediate first
    // tick against a healthy log: nothing to send, and no failure.
    // Only frames and the end are observable, so the wait is the proof
    // the anchor ran (the stream is lazy — it reads nothing unpolled).
    let quiet = tokio::time::timeout(
        std::time::Duration::from_millis(700),
        next_event(&mut frames, std::time::Duration::from_secs(60)),
    )
    .await;
    assert!(
        quiet.is_err(),
        "a healthy, quiet log sends nothing: {quiet:?}"
    );

    break_the_log(&db).await;
    let (event, data) = next_event(&mut frames, std::time::Duration::from_secs(10))
        .await
        .expect("a failed read is a frame, not an ended or silent stream");
    assert_eq!(
        event, "failed",
        "the frame is named, so no row parser eats it: {data}"
    );
    let body: serde_json::Value = serde_json::from_str(&data).expect("the frame is JSON");
    let error = body["error"].as_str().expect("the frame carries `error`");
    assert!(
        error.contains("audit_log"),
        "the frame names what failed, in the database's own words: {error}"
    );
    assert_eq!(
        next_event(&mut frames, std::time::Duration::from_secs(10)).await,
        None,
        "after the failure the stream ENDS; it does not go on answering silence"
    );
}

// The anchor read had the same swallow in a worse shape:
// `.unwrap_or(0)` turned a failed `MAX(id)` into cursor 0, so a read
// that recovered on the next tick replayed the log from its first row,
// 500 at a time, as if they were landing now. The anchor's failure is
// the same named frame, and the stream ends before any tick.
#[tokio::test(flavor = "multi_thread")]
async fn a_failed_anchor_is_a_named_frame_not_a_replay_from_row_zero() {
    let db = TestDb::new().await;
    break_the_log(&db).await;
    let app: Router = audit_tail_router(db.pool.clone());
    let resp = app
        .oneshot(get_req("/api/events/stream", &operator_user()))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let mut frames = resp.into_body().into_data_stream();
    let (event, data) = next_event(&mut frames, std::time::Duration::from_secs(10))
        .await
        .expect("a failed anchor is a frame");
    assert_eq!(event, "failed", "{data}");
    assert!(data.contains("audit_log"), "{data}");
    assert_eq!(
        next_event(&mut frames, std::time::Duration::from_secs(10)).await,
        None,
        "a stream with no anchor ends"
    );
}
