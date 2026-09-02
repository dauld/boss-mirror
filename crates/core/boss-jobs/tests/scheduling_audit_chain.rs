//! Regression test for the scheduling audit chain.
//!
//! Exercises the four core scheduling write paths through HTTP, drops
//! the projections, runs rebuild_scheduling, and asserts every row
//! reappears from audit_log alone — i.e. every write is audited and
//! the projections are fully rebuildable.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_core::publisher::DomainPublisher;
use boss_jobs::scheduling::PgScheduling;
use boss_jobs::scheduling::http::{SchedulingApiState, router as scheduling_router};
use boss_jobs::scheduling::rebuild_scheduling;
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use serde_json::json;
use sqlx::PgPool;

async fn seed_employee_and_target_job(pool: &PgPool, emp_id: &str, job_id: &str) {
    sqlx::query(
        "INSERT INTO locations (id, name, kind, timezone, created_at) \
         VALUES ('loc-test', 'Test Location', 'office', 'America/Chicago', NOW()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO employees (id, name, email, role, department, hire_date, location, employment_type, status, manager_id) \
         VALUES ($1, 'Test Tech', $2, 'service-tech', 'service', '2024-01-15', 'loc-test', 'full-time', 'active', NULL) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(emp_id)
    .bind(format!("{emp_id}@boss.example"))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO accounts (id, name, director, city, state, tier, customer_since, territory_rep_id, account_type) \
         VALUES ('acc-sched-test', 'Test Co', 'Director', 'Austin', 'TX', 'gold', '2025-06-01', $1, 'wholesale-distributor') \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(emp_id)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO jobs (id, kind, subject_kind, subject_id, title, owner_id, status, priority, opened_on) \
         VALUES ($1, 'service-visit', 'account', 'acc-sched-test', 'Test job', $2, 'open', 'standard', CURRENT_DATE) \
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(uuid::Uuid::parse_str(job_id).unwrap())
    .bind(emp_id)
    .execute(pool)
    .await
    .unwrap();
}

/// Drain the outbox through the relay pipeline into audit_log.
async fn drain_outbox(pool: &PgPool) {
    let bus = RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(pool, &(bus as Arc<dyn boss_core::port::EventBus>), 100)
        .await
        .expect("relay drain");
}

fn build_app(pool: PgPool) -> Router {
    // No direct audit writer: events record on the outbox in the
    // domain write; the drain moves them to audit_log.
    let publisher = DomainPublisher::new(RecordingEventBus::new(), "scheduling");
    scheduling_router(SchedulingApiState {
        repo: Arc::new(PgScheduling::new(pool)),
        publisher: Some(publisher),
        clock: Arc::new(boss_clock_client::WallClockClient),
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn scheduling_writes_survive_rebuild() {
    let db = TestDb::new().await;
    let job_id = "11111111-1111-1111-1111-111111111111";
    seed_employee_and_target_job(&db.pool, "emp-tech-001", job_id).await;
    let app = build_app(db.pool.clone());

    // 1. Availability (PTO slot).
    TestRequest::post("/api/scheduling/availability")
        .json(&json!({
            "employee_id": "emp-tech-001",
            "kind": "pto",
            "starts_at": "2026-06-01T00:00:00Z",
            "ends_at": "2026-06-05T23:59:59Z",
            "notes": "Beach week",
            "source": "manual",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // 2. Assignment (tech booked against a job).
    TestRequest::post("/api/scheduling/assignments")
        .json(&json!({
            "tech_id": "emp-tech-001",
            "target_job_id": job_id,
            "kind": "wo",
            "starts_at": "2026-05-10T09:00:00Z",
            "ends_at": "2026-05-10T12:00:00Z",
            "status": "tentative",
            "notes": null,
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // 3. Shift pattern (recurring weekly template).
    TestRequest::post("/api/scheduling/shift-patterns")
        .json(&json!({
            "employee_id": "emp-tech-001",
            "day_of_week": 1,
            "starts_at_time": "08:00:00",
            "ends_at_time": "17:00:00",
            "timezone": "America/Los_Angeles",
            "effective_from": "2026-01-01",
        }))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    // 4. Calendar token rotation.
    TestRequest::post("/api/scheduling/techs/emp-tech-001/calendar-token")
        .json(&json!({}))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    let pre_avail: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM tech_availability WHERE employee_id = 'emp-tech-001'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let pre_assign: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM scheduled_assignments WHERE tech_id = 'emp-tech-001'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let pre_shift: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM tech_shift_patterns WHERE employee_id = 'emp-tech-001'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let pre_token: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM tech_calendar_tokens WHERE employee_id = 'emp-tech-001'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(pre_avail.0, 1);
    assert_eq!(pre_assign.0, 1);
    assert_eq!(pre_shift.0, 1);
    assert_eq!(pre_token.0, 1);

    // Drain the outbox into audit_log, then rebuild.
    drain_outbox(&db.pool).await;
    let report = rebuild_scheduling(&db.pool).await.expect("rebuild");
    assert!(report.availability_upserted >= 1, "{report:?}");
    assert!(report.assignments_upserted >= 1, "{report:?}");
    assert!(report.shift_patterns_upserted >= 1, "{report:?}");
    assert!(report.calendar_tokens_rotated >= 1, "{report:?}");

    let post_avail: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM tech_availability WHERE employee_id = 'emp-tech-001'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let post_assign: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM scheduled_assignments WHERE tech_id = 'emp-tech-001'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
    let post_shift: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM tech_shift_patterns WHERE employee_id = 'emp-tech-001'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let post_token: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM tech_calendar_tokens WHERE employee_id = 'emp-tech-001'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(post_avail, pre_avail);
    assert_eq!(post_assign, pre_assign);
    assert_eq!(post_shift, pre_shift);
    assert_eq!(post_token, pre_token);
}

/// Row shape for the byte-identity check: every column of
/// `scheduled_assignments`, timestamps included. A rebuild that
/// stamps replay-time NOW() instead of the event's recorded time
/// fails here (packet d7b8158e).
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct AssignmentRow {
    id: uuid::Uuid,
    tech_id: String,
    target_job_id: uuid::Uuid,
    kind: String,
    starts_at: chrono::DateTime<chrono::Utc>,
    ends_at: chrono::DateTime<chrono::Utc>,
    status: String,
    notes: Option<String>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct TokenRow {
    employee_id: String,
    token: String,
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn snapshot_assignments(pool: &PgPool) -> Vec<AssignmentRow> {
    sqlx::query_as(
        "SELECT id, tech_id, target_job_id, kind, starts_at, ends_at, status, notes, \
                created_at, updated_at \
         FROM scheduled_assignments ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn snapshot_tokens(pool: &PgPool) -> Vec<TokenRow> {
    sqlx::query_as(
        "SELECT employee_id, token, created_at FROM tech_calendar_tokens ORDER BY employee_id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Determinism: replaying the log must reproduce the projections
/// byte-for-byte — including `scheduled_assignments.updated_at` after
/// a status change and `tech_calendar_tokens.created_at` after a
/// rotation. Both used to stamp NOW() (live at apply, and again at
/// replay), so live and rebuilt rows disagreed by wall-clock drift.
#[tokio::test(flavor = "multi_thread")]
async fn scheduling_rebuild_is_byte_identical() {
    let db = TestDb::new().await;
    let job_id = "22222222-2222-2222-2222-222222222222";
    seed_employee_and_target_job(&db.pool, "emp-tech-002", job_id).await;
    let app = build_app(db.pool.clone());

    // Assignment created, then its status advanced (the advance is
    // the path that used to stamp NOW() into updated_at).
    let created = TestRequest::post("/api/scheduling/assignments")
        .json(&json!({
            "tech_id": "emp-tech-002",
            "target_job_id": job_id,
            "kind": "wo",
            "starts_at": "2026-05-11T09:00:00Z",
            "ends_at": "2026-05-11T12:00:00Z",
            "status": "tentative",
            "notes": null,
        }))
        .send(&app)
        .await;
    created.assert_status(StatusCode::CREATED);
    let assign_id = created.assert_json::<serde_json::Value>()["id"]
        .as_str()
        .unwrap()
        .to_string();

    TestRequest::post(format!("/api/scheduling/assignments/{assign_id}/status"))
        .json(&json!({"status": "confirmed"}))
        .send(&app)
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // Calendar token minted (the mint used to stamp NOW() into
    // created_at on both the live insert and the replay).
    TestRequest::post("/api/scheduling/techs/emp-tech-002/calendar-token")
        .json(&json!({}))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    drain_outbox(&db.pool).await;
    let assignments_before = snapshot_assignments(&db.pool).await;
    let tokens_before = snapshot_tokens(&db.pool).await;
    assert_eq!(assignments_before.len(), 1);
    assert_eq!(assignments_before[0].status, "confirmed");
    assert_eq!(tokens_before.len(), 1);

    // Rebuild wipes the projections and replays audit_log.
    rebuild_scheduling(&db.pool).await.expect("rebuild");

    let assignments_after = snapshot_assignments(&db.pool).await;
    let tokens_after = snapshot_tokens(&db.pool).await;
    assert_eq!(
        assignments_before, assignments_after,
        "scheduled_assignments must replay byte-identical (updated_at included)"
    );
    assert_eq!(
        tokens_before, tokens_after,
        "tech_calendar_tokens must replay byte-identical (created_at included)"
    );
}
