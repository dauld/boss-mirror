//! The people scope routes refuse an anonymous guest and still answer
//! the readers they exist for (backlog b3a1772d, design 2830b6b7).
//!
//! `GET /api/people/by-email/{email}/bootstrap` turns an email into an
//! employee's id, role, department, territory and direct reports, and
//! `GET /api/people/{id}/scope` answers the same by id. Until design
//! 2830b6b7 every anonymous visitor carried `audit-readonly`, which is
//! in `has_global_read`, so a stranger could look up any address on the
//! roster. The fix landed as the `visitor` role — the OSS default
//! guest, deliberately outside `has_global_read` — but the refusal at
//! THESE routes rested on that indirect property and nothing drove the
//! routes themselves. This drives them: a `visitor` session, the
//! identity-less request, and a `visitor` asking for its own guest id
//! are each refused; the gateway's own login lookup (operator tier), a
//! platform-admin and an employee reading their own scope are answered,
//! so a refusal here is the guard's and not a broken route.
//!
//! Not pinned here, by decision: an `audit-readonly` guest on an
//! instance that opts in to the system-audit read (the playground) is
//! answered — David, 2026-09-25, recorded on b3a1772d.

use axum::http::StatusCode;
use boss_people::PeopleRepository;
use boss_people::postgres::PgPeople;
use boss_people::scope::scope_router;
use boss_testing::{TestDb, TestRequest};

mod common;

const EMP: &str = "emp-roster-probe";
const EMAIL: &str = "emp-roster-probe@boss.io";

async fn app() -> (TestDb, axum::Router) {
    let db = TestDb::new().await;
    PgPeople::new(db.pool.clone())
        .create_employee(&common::employee_fixture(EMP))
        .await
        .expect("seed the employee the lookups ask for");
    let router = scope_router(db.pool.clone());
    (db, router)
}

fn by_email() -> String {
    format!("/api/people/by-email/{EMAIL}/bootstrap")
}

fn by_id() -> String {
    format!("/api/people/{EMP}/scope")
}

/// The header the gateway's `build_user_json` forwards for a guest
/// session on a Basic install: the guest identity, role `visitor`, user
/// tier, no scope.
fn as_basic_guest(req: TestRequest) -> TestRequest {
    req.as_user(
        boss_core::roles::GUEST_EMAIL,
        boss_core::roles::VISITOR_ROLE,
    )
}

fn as_operator_tier(req: TestRequest) -> TestRequest {
    let json = serde_json::json!({
        "id": "boss-gateway",
        "role": "service",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": null,
    })
    .to_string();
    req.header("x-boss-user", json)
}

#[tokio::test]
async fn a_basic_guest_is_refused_the_roster_by_email_and_by_id() {
    let (_db, router) = app().await;
    for path in [by_email(), by_id()] {
        let resp = as_basic_guest(TestRequest::get(path.clone()))
            .send(&router)
            .await;
        resp.assert_status(StatusCode::FORBIDDEN);
        assert!(
            !resp.body_text().contains(EMP),
            "a refusal must not carry the row: {path} -> {}",
            resp.body_text()
        );
    }
}

#[tokio::test]
async fn a_request_with_no_identity_is_refused_the_roster() {
    let (_db, router) = app().await;
    for path in [by_email(), by_id()] {
        TestRequest::get(path)
            .send(&router)
            .await
            .assert_status(StatusCode::FORBIDDEN);
    }
}

/// The by-id route admits a self-lookup. A guest's id is its fixed
/// email, which names no employee: asking for it answers nothing about
/// anyone on the roster.
#[tokio::test]
async fn a_guest_asking_for_its_own_id_learns_of_no_employee() {
    let (_db, router) = app().await;
    let own = format!("/api/people/{}/scope", boss_core::roles::GUEST_EMAIL);
    as_basic_guest(TestRequest::get(own))
        .send(&router)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

/// Controls: the readers these routes exist for are still answered, so
/// the refusals above are the guard's.
#[tokio::test]
async fn the_gateway_an_admin_and_the_employee_themself_are_answered() {
    let (_db, router) = app().await;

    let resp = as_operator_tier(TestRequest::get(by_email()))
        .send(&router)
        .await;
    resp.assert_status(StatusCode::OK);
    let scope: serde_json::Value = resp.assert_json();
    assert_eq!(scope["id"], EMP, "the login lookup resolves the email");

    for path in [by_email(), by_id()] {
        TestRequest::get(path)
            .as_user("emp-founder", boss_core::roles::PLATFORM_ADMIN_ROLE)
            .send(&router)
            .await
            .assert_status(StatusCode::OK);
    }

    TestRequest::get(by_id())
        .as_user(EMP, "service-tech")
        .send(&router)
        .await
        .assert_status(StatusCode::OK);
}
