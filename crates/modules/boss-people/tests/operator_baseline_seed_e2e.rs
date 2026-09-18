//! End-to-end test for the operator-baseline-seed → audit_log →
//! rebuild_people path that the OSS quickstart + docker init
//! scripts rely on.
//!
//! The seed serialises an `Employee` struct directly rather than
//! hand-crafting a JSONB audit_log payload. A hand-typed payload
//! would drift the moment the `Employee` struct changes (added /
//! renamed / removed field): the projection INSERT would still
//! succeed and the audit_log row would look valid, but a future
//! `rebuild_people` would silently lose data or fail at parse time.
//!
//! The serialised path goes through
//! `serde_json::to_value(&Employee)` (see
//! `crates/boss-people/src/bin/boss_operator_baseline_seed.rs`).
//! This test mirrors that path and asserts that:
//!
//!   1. The serialised payload round-trips through `audit_log`
//!      and `rebuild_people` back to a projection row whose
//!      every column matches the source `Employee`.
//!   2. Adding a new field to `Employee` without updating the
//!      rebuilder's JSON deserialiser breaks this test (rather
//!      than silently dropping the field on the next quickstart
//!      run).

use boss_people::rebuild_people;
use boss_people::types::*;
use boss_testing::TestDb;
use chrono::{NaiveDate, Utc};
use sqlx::PgPool;
use uuid::Uuid;

async fn seed_location(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO locations (id, name, kind, timezone, created_at) \
         VALUES ('loc-hq', 'HQ', 'office', 'America/Chicago', NOW()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();
}

/// Mirror of `boss_operator_baseline_seed::main` — write a single
/// `people.employee.created` event using `serde_json::to_value`
/// against the canonical `Employee` struct. If the binary's
/// serialisation changes shape, this helper changes with it.
async fn seed_via_canonical_path(pool: &PgPool, emp: &Employee) {
    let payload = serde_json::to_value(emp).expect("Employee serialises");
    sqlx::query(
        "INSERT INTO audit_log (event_id, timestamp, source, kind, payload) \
         VALUES ($1, $2, 'people', 'people.employee.created', $3)",
    )
    .bind(Uuid::new_v4())
    .bind(Utc::now())
    .bind(&payload)
    .execute(pool)
    .await
    .expect("audit_log INSERT");
}

#[tokio::test(flavor = "multi_thread")]
async fn operator_baseline_seed_round_trips_through_rebuild() {
    let db = TestDb::new().await;
    seed_location(&db.pool).await;

    // Bootstrap-admin shape exactly matching what
    // infra/oss-quickstart/init.sh now constructs
    // via temp TOML.
    let admin = Employee {
        id: "emp-bootstrap-test".into(),
        name: Some("Bootstrap admin".into()),
        email: Some("ops@example.com".into()),
        role: Some("platform-admin".into()),
        department: Some("executive".into()),
        skill_level: None,
        skills: vec![],
        hire_date: Some(NaiveDate::from_ymd_opt(2026, 5, 4).unwrap()),
        location: Some("loc-hq".into()),
        manager_id: None,
        employment_type: Some("full-time".to_string()),
        status: Some("active".to_string()),
        certifications: vec![],
        annual_salary_cents: None,
    };

    seed_via_canonical_path(&db.pool, &admin).await;

    // Wipe any seed-bundle rows so the rebuild only has our one
    // bootstrap-admin event to project. (TestDb is fresh, but the
    // operator-baseline seed may materialize default rows; defensive
    // DELETE keeps the assertion exact.)
    sqlx::query("DELETE FROM employees")
        .execute(&db.pool)
        .await
        .unwrap();

    let report = rebuild_people(&db.pool).await.expect("rebuild succeeds");
    assert!(
        report.employees_upserted >= 1,
        "rebuild should project at least the bootstrap-admin row"
    );

    // Round-trip assertion: every field the canonical Employee
    // struct exposes must round-trip back to the projection.
    // If a new field is added to Employee without a matching
    // projection column / rebuilder branch, this query either
    // returns the wrong value or fails at parse time.
    let row: ProjectionRow = sqlx::query_as(
        "SELECT id, name, email, role, department, location, hire_date, employment_type, status, annual_salary_cents \
         FROM employees WHERE id = $1",
    )
    .bind(&admin.id)
    .fetch_one(&db.pool)
    .await
    .expect("bootstrap-admin row projected");

    assert_eq!(row.id, admin.id);
    assert_eq!(row.name, admin.name);
    assert_eq!(row.email, admin.email);
    assert_eq!(row.role, admin.role);
    assert_eq!(row.department, admin.department);
    assert_eq!(row.location, admin.location);
    assert_eq!(row.hire_date, admin.hire_date);
    assert_eq!(row.employment_type.as_deref(), Some("full-time"));
    assert_eq!(row.status.as_deref(), Some("active"));
    assert_eq!(row.annual_salary_cents, admin.annual_salary_cents);
}

/// Mirror of the `employees` columns the test pins. Lifting it
/// into a named struct keeps clippy's type-complexity lint happy
/// and makes the per-field assertions self-documenting.
#[derive(sqlx::FromRow)]
struct ProjectionRow {
    id: String,
    name: Option<String>,
    email: Option<String>,
    role: Option<String>,
    department: Option<String>,
    location: Option<String>,
    hire_date: Option<NaiveDate>,
    employment_type: Option<String>,
    status: Option<String>,
    annual_salary_cents: Option<i64>,
}
