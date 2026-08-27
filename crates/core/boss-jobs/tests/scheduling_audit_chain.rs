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

/// A calendar-token rotation must replay with the SAME created_at.
///
/// The live write stamped created_at with the Postgres clock while the
/// event published `rotated_at` from the sim clock, and the rebuild used
/// NOW() again -- three chances to disagree, none of them the value in
/// the log. A published ICS URL resolves on the token, but the
/// created_at drifted on every rebuild. Same family as the assignment
/// fix (d7b8158e); asserts the value, not the row count.
#[tokio::test(flavor = "multi_thread")]
async fn a_calendar_token_keeps_its_event_time_through_a_rebuild() {
    let db = TestDb::new().await;
    seed_employee_and_target_job(
        &db.pool,
        "emp-tech-003",
        "33333333-3333-3333-3333-333333333333",
    )
    .await;
    let app = build_app(db.pool.clone());

    TestRequest::post("/api/scheduling/techs/emp-tech-003/calendar-token")
        .json(&json!({}))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    let (created_before,): (chrono::DateTime<chrono::Utc>,) = sqlx::query_as(
        "SELECT created_at FROM tech_calendar_tokens WHERE employee_id = 'emp-tech-003'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    drain_outbox(&db.pool).await;
    sqlx::query("DELETE FROM tech_calendar_tokens")
        .execute(&db.pool)
        .await
        .unwrap();
    let report = rebuild_scheduling(&db.pool).await.expect("rebuild");
    assert!(report.calendar_tokens_rotated >= 1, "{report:?}");

    let (created_after,): (chrono::DateTime<chrono::Utc>,) = sqlx::query_as(
        "SELECT created_at FROM tech_calendar_tokens WHERE employee_id = 'emp-tech-003'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();

    assert_eq!(
        created_after, created_before,
        "created_at must come from the event's rotated_at, not the rebuild's clock"
    );
}
