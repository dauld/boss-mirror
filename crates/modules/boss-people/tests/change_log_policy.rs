//! The change log read through policy, against Postgres (backlog
//! 8cdad84c, 2026-09-23).
//!
//! `GET /api/people/changes` and `GET /api/people/{id}/changes` made no
//! policy call, and a change row can carry pay in `from_value` /
//! `to_value`. The unit tests pin the refusals, which happen before any
//! query; this pins the reads that DO query — the department join a
//! `department:<d>` grant is judged on, and the redaction of a
//! pay-bearing row — on the real schema.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use boss_people::PgPeople;
use boss_people::employee_changes::employee_changes_router;
use boss_people::http::{PeopleApiState, router as people_router};
use boss_people::types::*;
use boss_people::workflows::workflow_router;
use boss_policy::{Action, Resource, Scope};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::{TestDb, TestRequest};
use chrono::NaiveDate;
use serde_json::{Value, json};
use sqlx::PgPool;

fn build_app(pool: PgPool, policy: Option<Arc<dyn PolicyClient>>) -> Router {
    let people = Arc::new(PgPeople::new(pool.clone()));
    let clock = Arc::new(boss_clock_client::WallClockClient);
    workflow_router(
        pool.clone(),
        people.clone(),
        None,
        clock.clone(),
        policy.clone(),
    )
    .merge(employee_changes_router(
        pool.clone(),
        None,
        clock.clone(),
        policy.clone(),
    ))
    .merge(people_router(PeopleApiState {
        people,
        publisher: None,
        policy,
        subject_kinds: None,
        clock,
    }))
}

fn employee(id: &str, department: &str) -> Employee {
    Employee {
        id: id.into(),
        name: Some(format!("Change Log {id}")),
        email: Some(format!("{id}@boss.example")),
        role: Some("service-tech".into()),
        department: Some(department.into()),
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

/// Two employees in two departments, each with a promotion carrying
/// pay, written on the test path (no policy wired).
async fn seed(pool: &PgPool) {
    sqlx::query(
        "INSERT INTO locations (id, name, kind, timezone, created_at) \
         VALUES ('loc-test', 'Test Location', 'office', 'America/Chicago', NOW()) \
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(pool)
    .await
    .unwrap();
    let writer = build_app(pool.clone(), None);
    for (id, department) in [("emp-cl-svc", "service"), ("emp-cl-wh", "warehouse")] {
        TestRequest::post("/api/people")
            .json(&employee(id, department))
            .send(&writer)
            .await
            .assert_status(StatusCode::CREATED);
        TestRequest::post(format!("/api/people/{id}/changes"))
            .json(&json!({
                "kind": "promotion",
                "from_value": "8500000",
                "to_value": "9500000",
                "notes": "raise to 95k",
                "effective_date": "2026-09-23",
            }))
            .send(&writer)
            .await
            .assert_status(StatusCode::CREATED);
    }
}

fn reader_policy(pay: bool) -> Arc<dyn PolicyClient> {
    let b = FakePolicyClient::builder().allow(
        "svc-mgr",
        Action::Read,
        Resource::employee(),
        Scope::Department("service".into()),
    );
    let b = if pay {
        b.allow(
            "svc-mgr",
            Action::Read,
            Resource::compensation(),
            Scope::All,
        )
    } else {
        b
    };
    Arc::new(b.build())
}

async fn read(app: &Router, uri: &str) -> Vec<Value> {
    let resp = TestRequest::get(uri)
        .as_user("emp-cl-mgr", "svc-mgr")
        .send(app)
        .await;
    resp.assert_status(StatusCode::OK);
    resp.assert_json()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_change_log_shows_the_department_it_is_granted_without_the_pay() {
    let db = TestDb::new().await;
    seed(&db.pool).await;
    let app = build_app(db.pool.clone(), Some(reader_policy(false)));

    let all = read(&app, "/api/people/changes").await;
    let ids: Vec<&str> = all
        .iter()
        .map(|r| r["employee_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["emp-cl-svc"], "only the granted department");
    let one = read(&app, "/api/people/emp-cl-svc/changes").await;
    assert_eq!(one.len(), 1);
    for row in all.iter().chain(one.iter()) {
        assert_eq!(row["kind"], "promotion");
        for key in ["from_value", "to_value", "notes"] {
            assert!(row.get(key).is_none(), "{key} leaked: {row}");
        }
    }
    assert!(
        read(&app, "/api/people/emp-cl-wh/changes").await.is_empty(),
        "another department's history is not this reader's"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_compensation_grant_shows_the_pay_on_the_change_log() {
    let db = TestDb::new().await;
    seed(&db.pool).await;
    let app = build_app(db.pool.clone(), Some(reader_policy(true)));
    let one = read(&app, "/api/people/emp-cl-svc/changes").await;
    assert_eq!(one.len(), 1);
    assert_eq!(one[0]["to_value"], "9500000");
    assert_eq!(one[0]["notes"], "raise to 95k");
    let all = read(&app, "/api/people/changes").await;
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["from_value"], "8500000");
}
