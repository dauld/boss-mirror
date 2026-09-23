//! Audit-chain test for workflow status updates.
//!
//! The `update_status` + `start_offboarding` handlers must emit an
//! `audit_log` event for both the status flip and the
//! `employee_changes` row. Without it a `boss-rebuild-all` cycle
//! would lose both: the rebuilder TRUNCATEs `employees` (CASCADE wipes
//! employee_changes) and replays from `audit_log`, which would carry
//! no record of either change.
//!
//! This test exercises both write paths through the publisher,
//! drops the projection, runs `rebuild_people`, and asserts both
//! the status change AND the `employee_changes` row reappear from
//! `audit_log` alone.

use std::sync::Arc;

/// Drain the outbox through the relay pipeline into audit_log.
async fn drain_outbox(pool: &PgPool) {
    let bus = boss_testing::RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(pool, &(bus as Arc<dyn boss_core::port::EventBus>), 100)
        .await
        .expect("relay drain");
}

use axum::Router;
use axum::http::StatusCode;
use boss_people::PgPeople;
use boss_people::http::{PeopleApiState, router as people_router};
use boss_people::rebuild_people;
use boss_people::types::*;
use boss_people::workflows::workflow_router;
use boss_testing::{TestDb, TestRequest};
use chrono::NaiveDate;
use serde_json::json;
use sqlx::PgPool;

async fn seed_location(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO locations (id, name, kind, timezone, created_at) \
         VALUES ('loc-test', 'Test Location', 'office', 'America/Chicago', NOW()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();
}

fn build_app(pool: PgPool) -> Router {
    // No publisher, no direct audit writer: events reach audit_log
    // only via the outbox -> relay drain.
    let people = Arc::new(PgPeople::new(pool.clone()));
    let crud_state = PeopleApiState {
        people: people.clone(),
        publisher: None,
        policy: None,
        subject_kinds: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    };
    workflow_router(
        pool.clone(),
        people,
        None,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
        None,
    )
    .merge(people_router(crud_state))
}

fn fixture(id: &str) -> Employee {
    Employee {
        id: id.into(),
        name: Some("Workflow Test".into()),
        email: Some(format!("{id}@boss.example")),
        role: Some("service-tech".into()),
        department: Some("service".into()),
        skill_level: Some(3),
        skills: vec![],
        hire_date: Some(NaiveDate::from_ymd_opt(2025, 6, 15).unwrap()),
        location: Some("loc-test".into()),
        manager_id: None,
        employment_type: Some("full-time".to_string()),
        status: Some("active".to_string()),
        certifications: vec![],
        annual_salary_cents: Some(8_500_000),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn status_change_survives_rebuild() {
    let db = TestDb::new().await;
    seed_location(&db.pool).await;
    let app = build_app(db.pool.clone());

    // Onboard via POST /api/people, then flip status to on-leave
    // via PUT /api/people/{id}/status.
    let emp = fixture("emp-wf-001");
    TestRequest::post("/api/people")
        .json(&emp)
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    TestRequest::put(format!("/api/people/{}/status", emp.id))
        .json(&json!({"status": "on-leave", "notes": "personal leave"}))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    // Sanity: projection reflects the status flip + change row.
    let pre_status: (String,) = sqlx::query_as("SELECT status FROM employees WHERE id = $1")
        .bind(&emp.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(pre_status.0, "on-leave");
    let pre_changes: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM employee_changes WHERE employee_id = $1")
            .bind(&emp.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(pre_changes.0, 1, "one change-log row for the on-leave flip");

    // Audit-chain assertion: drop the projection rows, run the
    // rebuilder, expect the same shape to come back from
    // audit_log alone.
    drain_outbox(&db.pool).await;
    let report = rebuild_people(&db.pool).await.expect("rebuild succeeds");
    assert!(report.employees_upserted >= 1);
    assert!(
        report.change_records_inserted >= 1,
        "change-recorded events should project into employee_changes"
    );

    let post_status: (String,) = sqlx::query_as("SELECT status FROM employees WHERE id = $1")
        .bind(&emp.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(
        post_status.0, "on-leave",
        "status flip must survive rebuild via the EMPLOYEE_UPDATED event"
    );

    let post_changes: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM employee_changes WHERE employee_id = $1")
            .bind(&emp.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
    assert_eq!(
        post_changes.0, 1,
        "change-log row must survive rebuild via EMPLOYEE_CHANGE_RECORDED"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn offboard_survives_rebuild() {
    let db = TestDb::new().await;
    seed_location(&db.pool).await;
    let app = build_app(db.pool.clone());

    let emp = fixture("emp-wf-002");
    TestRequest::post("/api/people")
        .json(&emp)
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // Offboard helper — formerly two raw queries with no audit_log emit.
    TestRequest::post(format!("/api/people/{}/offboard", emp.id))
        .json(&serde_json::json!({}))
        .send(&app)
        .await
        .assert_status(StatusCode::OK);

    let pre_status: (String,) = sqlx::query_as("SELECT status FROM employees WHERE id = $1")
        .bind(&emp.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(pre_status.0, "terminated");

    drain_outbox(&db.pool).await;
    rebuild_people(&db.pool).await.expect("rebuild succeeds");

    let post_status: (String,) = sqlx::query_as("SELECT status FROM employees WHERE id = $1")
        .bind(&emp.id)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(post_status.0, "terminated");

    let kinds: Vec<(String,)> = sqlx::query_as(
        "SELECT kind FROM employee_changes WHERE employee_id = $1 ORDER BY created_at",
    )
    .bind(&emp.id)
    .fetch_all(&db.pool)
    .await
    .unwrap();
    assert!(
        kinds.iter().any(|(k,)| k == "offboard"),
        "expected an 'offboard' change-log row after rebuild, got {kinds:?}"
    );
}
