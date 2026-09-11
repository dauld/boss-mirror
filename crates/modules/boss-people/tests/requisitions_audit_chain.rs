//! Audit-chain test for `requisitions`.
//!
//! The requisitions handler must emit an audit_log event for every
//! write. Without it a `boss-rebuild-all` cycle would lose every
//! requisition (TRUNCATE employees CASCADE wipes requisitions via the
//! FK on hiring_manager_id, with nothing to replay).
//!
//! This test exercises POST /api/people/requisitions, including a
//! status update via the ON CONFLICT DO UPDATE re-emit, then
//! drops the projection and asserts the row reappears with the
//! latest status intact.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_people::rebuild_people;
use boss_people::requisitions::{Requisition, requisitions_router};
use boss_testing::{TestDb, TestRequest};
use chrono::NaiveDate;
use sqlx::PgPool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
struct RequisitionRow {
    id: String,
    role: String,
    department: String,
    status: String,
    headcount: i16,
    hiring_manager_id: String,
    // Byte-identity includes the timestamp: before packet d7b8158e
    // `created_at` fell to the DEFAULT NOW() on both the live insert
    // and the replay, so a rebuild silently re-dated every
    // requisition.
    created_at: chrono::DateTime<chrono::Utc>,
}

async fn snapshot_requisitions(pool: &PgPool) -> Vec<RequisitionRow> {
    sqlx::query_as(
        "SELECT id, role, department, status, headcount, hiring_manager_id, created_at \
         FROM requisitions ORDER BY id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn build_app(pool: PgPool) -> Router {
    // No publisher, no direct audit writer: events reach audit_log
    // only via the outbox -> relay drain.
    requisitions_router(
        pool,
        None,
        std::sync::Arc::new(boss_clock_client::WallClockClient),
    )
}

/// Drain the outbox through the relay pipeline into audit_log.
async fn drain_outbox(pool: &PgPool) {
    let bus = boss_testing::RecordingEventBus::new();
    boss_events::outbox::drain_outbox_once(pool, &(bus as Arc<dyn boss_core::port::EventBus>), 100)
        .await
        .expect("relay drain");
}

async fn seed_employee_and_location(pool: &PgPool) {
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
         VALUES ('emp-mgr-001', 'Hiring Mgr', 'mgr@boss.example', 'service-manager', 'service', '2024-01-15', 'loc-test', 'full-time', 'active', NULL) \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();

    // Mirror the manager event into audit_log so rebuild_people
    // (which TRUNCATEs employees CASCADE) can reproduce the FK
    // target before replaying the requisition event.
    let payload = serde_json::json!({
        "id": "emp-mgr-001",
        "name": "Hiring Mgr",
        "email": "mgr@boss.example",
        "role": "service-manager",
        "department": "service",
        "skills": [],
        "hire_date": "2024-01-15",
        "location": "loc-test",
        "employment_type": "full-time",
        "status": "active",
        "certifications": [],
    });
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES (gen_random_uuid(), NOW(), 'people', 'people.employee.created', $1)",
    )
    .bind(&payload)
    .execute(pool)
    .await
    .unwrap();
}

fn req_body(id: &str, status: &str) -> Requisition {
    Requisition {
        id: id.to_string(),
        role: "service-tech".into(),
        department: "service".into(),
        status: status.into(),
        opened_on: NaiveDate::from_ymd_opt(2026, 5, 1).unwrap(),
        target_fill_date: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
        location: "loc-test".into(),
        headcount: 2,
        hiring_manager_id: "emp-mgr-001".into(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn requisitions_survive_rebuild() {
    let db = TestDb::new().await;
    seed_employee_and_location(&db.pool).await;
    let app = build_app(db.pool.clone()).await;

    // Open at status=open.
    TestRequest::post("/api/people/requisitions")
        .json(&req_body("req-001", "open"))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    // Re-POST with status=interviewing — the ON CONFLICT DO UPDATE
    // path mutates status. We want this transition to also survive
    // rebuild.
    TestRequest::post("/api/people/requisitions")
        .json(&req_body("req-001", "interviewing"))
        .send(&app)
        .await
        .assert_status(StatusCode::CREATED);

    let pre = snapshot_requisitions(&db.pool).await;
    assert_eq!(pre.len(), 1);
    assert_eq!(pre[0].status, "interviewing");

    drain_outbox(&db.pool).await;
    let report = rebuild_people(&db.pool).await.expect("rebuild");
    assert!(
        report.requisitions_upserted >= 2,
        "rebuild should replay both status events, got {report:?}"
    );

    let post = snapshot_requisitions(&db.pool).await;
    assert_eq!(
        pre, post,
        "requisition row must round-trip exactly through audit_log"
    );
    assert_eq!(post[0].status, "interviewing", "latest status must win");
}
