//! Integration tests for `audit_tail_router` — the `/api/events/tail`
//! read surface over `audit_log`.
//!
//! Covers: authz (operator tier + ceo/cto role allow, plain user
//! denies), source/kind filtering, limit clamping, descending order —
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
