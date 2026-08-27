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

/// A REBUILD MUST REPRODUCE THE PROJECTION, NOT RESTAMP IT.
///
/// `scheduling.assignment.status-changed` used to be replayed with
/// `updated_at = NOW()`, which is the clock of the REBUILD rather than
/// of the event. The row survived a rebuild — the existing test above
/// would still pass — but it came back with a different `updated_at`
/// every time, so the projection drifted a little further from the log
/// on each replay. That is a determinism failure, and it is invisible
/// to any assertion that only counts rows.
///
/// This asserts the value, not the count: `updated_at` after the
/// rebuild must equal what it was before. Mapped as one of seven
/// pre-existing divergences in d7b8158e.
#[tokio::test(flavor = "multi_thread")]
async fn a_status_change_keeps_its_event_time_through_a_rebuild() {
    let db = TestDb::new().await;
    let job_id = "22222222-2222-2222-2222-222222222222";
    seed_employee_and_target_job(&db.pool, "emp-tech-002", job_id).await;
    let app = build_app(db.pool.clone());

    TestRequest::post("/api/scheduling/assignments")
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
        .await
        .assert_status(StatusCode::CREATED);

    let (assignment_id,): (uuid::Uuid,) =
        sqlx::query_as("SELECT id FROM scheduled_assignments WHERE tech_id = 'emp-tech-002'")
            .fetch_one(&db.pool)
            .await
            .unwrap();

    TestRequest::post(format!(
        "/api/scheduling/assignments/{assignment_id}/status"
    ))
    .json(&json!({"status": "confirmed"}))
    .send(&app)
    .await
    .assert_status(StatusCode::NO_CONTENT);

    let (status_before, updated_before): (String, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("SELECT status, updated_at FROM scheduled_assignments WHERE id = $1")
            .bind(assignment_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(status_before, "confirmed");

    drain_outbox(&db.pool).await;

    // Wipe and rebuild purely from audit_log.
    sqlx::query("DELETE FROM scheduled_assignments")
        .execute(&db.pool)
        .await
        .unwrap();
    let report = rebuild_scheduling(&db.pool).await.expect("rebuild");
    assert!(report.assignment_status_changes >= 1, "{report:?}");

    let (status_after, updated_after): (String, chrono::DateTime<chrono::Utc>) =
        sqlx::query_as("SELECT status, updated_at FROM scheduled_assignments WHERE id = $1")
            .bind(assignment_id)
            .fetch_one(&db.pool)
            .await
            .unwrap();

    assert_eq!(status_after, "confirmed", "the status itself must replay");
    assert_eq!(
        updated_after, updated_before,
        "updated_at must come from the event, not from the rebuild's clock — a replay that \
         restamps it makes the projection drift further from the log on every rebuild"
    );
}
