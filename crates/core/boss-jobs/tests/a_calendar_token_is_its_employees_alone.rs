//! An employee's calendar-feed token is theirs alone (backlog 7ae9ccec).
//!
//! The token IS the authentication for the public, sessionless
//! `/ics/{token}/calendar.ics` feed — 90 days of the employee's past
//! and 180 of their future schedule. Until this car both token
//! handlers took no caller at all: a guest session (role
//! `audit-readonly`, minted by `POST /api/auth/guest`) could GET any
//! employee's token through the gateway's `/api/scheduling/*` proxy
//! and then read their whole schedule without a session, and any
//! signed-in employee could read or rotate any other's.
//!
//! The rule, from the company frame the triage was run under: a
//! read answers only the employee themself; a rotate is the employee
//! or a platform-admin (revoking a leaked feed is an operator's job),
//! and an operator's rotate never hands the operator the new token.
//!
//! The repository here is an in-memory fake holding only the token
//! table — the handlers' authorization is the thing under test, and
//! it runs before any storage is touched.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::http::StatusCode;
use boss_core::publisher::EventStamp;
use boss_jobs::scheduling::http::{SchedulingApiState, router};
use boss_jobs::scheduling::{
    NewScheduledAssignment, NewTechAvailability, ScheduledAssignment, SchedulingError,
    SchedulingRepository, TechAvailability, TechShiftPattern, WeekGridRow,
};
use boss_testing::TestRequest;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;
use uuid::Uuid;

const OWNER: &str = "emp-tech-001";
const OTHER: &str = "emp-tech-002";
const SEEDED: &str = "seeded-token-of-emp-tech-001";
const URI: &str = "/api/scheduling/techs/emp-tech-001/calendar-token";

#[derive(Default)]
struct Tokens(Mutex<HashMap<String, String>>);

impl Tokens {
    fn seeded() -> Arc<Self> {
        let t = Self::default();
        t.0.lock()
            .unwrap()
            .insert(OWNER.to_string(), SEEDED.to_string());
        Arc::new(t)
    }
    fn of(&self, emp: &str) -> Option<String> {
        self.0.lock().unwrap().get(emp).cloned()
    }
}

fn unused<T>() -> Result<T, SchedulingError> {
    Err(SchedulingError::Storage(
        "not held by this fake: only calendar tokens are".into(),
    ))
}

#[async_trait]
impl SchedulingRepository for Tokens {
    async fn create_availability(
        &self,
        _: NewTechAvailability,
        _: &EventStamp,
    ) -> Result<TechAvailability, SchedulingError> {
        unused()
    }
    async fn list_availability(
        &self,
        _: Option<&str>,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
    ) -> Result<Vec<TechAvailability>, SchedulingError> {
        unused()
    }
    async fn delete_availability(
        &self,
        _: Uuid,
        _: DateTime<Utc>,
        _: &EventStamp,
    ) -> Result<(), SchedulingError> {
        unused()
    }
    async fn create_assignment(
        &self,
        _: NewScheduledAssignment,
        _: &EventStamp,
    ) -> Result<ScheduledAssignment, SchedulingError> {
        unused()
    }
    async fn get_assignment(
        &self,
        _: Uuid,
    ) -> Result<Option<ScheduledAssignment>, SchedulingError> {
        unused()
    }
    async fn list_assignments(
        &self,
        _: Option<&str>,
        _: Option<Uuid>,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
    ) -> Result<Vec<ScheduledAssignment>, SchedulingError> {
        unused()
    }
    async fn update_assignment_status(
        &self,
        _: Uuid,
        _: boss_jobs::scheduling::AssignmentStatus,
        _: DateTime<Utc>,
        _: &EventStamp,
    ) -> Result<(), SchedulingError> {
        unused()
    }
    async fn delete_assignment(
        &self,
        _: Uuid,
        _: DateTime<Utc>,
        _: &EventStamp,
    ) -> Result<(), SchedulingError> {
        unused()
    }
    #[allow(clippy::too_many_arguments)]
    async fn upsert_shift_pattern(
        &self,
        _: &str,
        _: i16,
        _: chrono::NaiveTime,
        _: chrono::NaiveTime,
        _: &str,
        _: NaiveDate,
        _: &EventStamp,
    ) -> Result<TechShiftPattern, SchedulingError> {
        unused()
    }
    async fn list_shift_patterns(
        &self,
        _: Option<&str>,
    ) -> Result<Vec<TechShiftPattern>, SchedulingError> {
        unused()
    }
    async fn materialize_shift_patterns(
        &self,
        _: NaiveDate,
        _: NaiveDate,
    ) -> Result<i64, SchedulingError> {
        unused()
    }
    async fn week_grid(
        &self,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
        _: Option<&[String]>,
    ) -> Result<Vec<WeekGridRow>, SchedulingError> {
        unused()
    }
    async fn calendar_token_for(
        &self,
        employee_id: &str,
    ) -> Result<Option<String>, SchedulingError> {
        Ok(self.of(employee_id))
    }
    async fn rotate_calendar_token(
        &self,
        employee_id: &str,
        new_token: &str,
        _: DateTime<Utc>,
        _: &EventStamp,
    ) -> Result<(), SchedulingError> {
        self.0
            .lock()
            .unwrap()
            .insert(employee_id.to_string(), new_token.to_string());
        Ok(())
    }
    async fn employee_by_calendar_token(
        &self,
        token: &str,
    ) -> Result<Option<String>, SchedulingError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .iter()
            .find(|(_, t)| t.as_str() == token)
            .map(|(e, _)| e.clone()))
    }
}

fn app(tokens: Arc<Tokens>) -> Router {
    router(SchedulingApiState {
        repo: tokens,
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
    })
}

/// A refusal must not carry the token in any form — not in a JSON
/// field, not in a prose error.
fn assert_refused_without_token(status: StatusCode, body: &str, who: &str) {
    assert_eq!(status, StatusCode::FORBIDDEN, "{who}: {body}");
    assert!(
        !body.contains(SEEDED),
        "{who}'s refusal leaked the token: {body}"
    );
}

#[tokio::test]
async fn a_guest_session_cannot_read_an_employees_token() {
    let tokens = Tokens::seeded();
    // The gateway's guest session: no employee id, so it identifies
    // downstream by its username, role audit-readonly (which passes
    // has_global_read — global READ is not this token's grant).
    let r = TestRequest::get(URI)
        .as_user("guest@algedonic.dev", "audit-readonly")
        .send(&app(tokens))
        .await;
    assert_refused_without_token(r.status, &r.body_text(), "guest");
}

#[tokio::test]
async fn a_request_with_no_identity_cannot_read_an_employees_token() {
    let r = TestRequest::get(URI).send(&app(Tokens::seeded())).await;
    assert_refused_without_token(r.status, &r.body_text(), "anonymous");
}

#[tokio::test]
async fn another_employee_cannot_read_an_employees_token() {
    let r = TestRequest::get(URI)
        .as_user(OTHER, "service-tech")
        .send(&app(Tokens::seeded()))
        .await;
    assert_refused_without_token(r.status, &r.body_text(), "another employee");
}

#[tokio::test]
async fn an_operator_cannot_read_an_employees_token_either() {
    // The operator may revoke a feed (rotate, below); reading it would
    // hand the operator the employee's whole schedule, which is not
    // an operator's to hold.
    let r = TestRequest::get(URI)
        .as_user("emp-david", "platform-admin")
        .send(&app(Tokens::seeded()))
        .await;
    assert_refused_without_token(r.status, &r.body_text(), "operator");
}

#[tokio::test]
async fn the_employee_reads_their_own_token() {
    let r = TestRequest::get(URI)
        .as_user(OWNER, "service-tech")
        .send(&app(Tokens::seeded()))
        .await;
    r.assert_status(StatusCode::OK);
    let body: Value = r.assert_json();
    assert_eq!(body["employee_id"], OWNER);
    assert_eq!(body["token"], SEEDED);
    assert_eq!(body["ics_url"], format!("/ics/{SEEDED}/calendar.ics"));
}

#[tokio::test]
async fn an_operators_rotate_revokes_the_feed_and_carries_no_token() {
    let tokens = Tokens::seeded();
    let r = TestRequest::post(URI)
        .as_user("emp-david", "platform-admin")
        .send(&app(tokens.clone()))
        .await;
    r.assert_status(StatusCode::OK);
    let body: Value = r.assert_json();
    assert_eq!(body["employee_id"], OWNER);
    assert!(
        body.get("token").is_none() && body.get("ics_url").is_none(),
        "an operator's rotate must not hand the operator the new feed: {body}"
    );
    let now = tokens.of(OWNER).expect("the rotate wrote a token");
    assert_ne!(now, SEEDED, "the old feed URL must stop working");
    assert!(
        !r.body_text().contains(&now),
        "the new token rode in the operator's response: {}",
        r.body_text()
    );
}

#[tokio::test]
async fn another_employee_cannot_rotate_an_employees_token() {
    let tokens = Tokens::seeded();
    let r = TestRequest::post(URI)
        .as_user(OTHER, "service-tech")
        .send(&app(tokens.clone()))
        .await;
    assert_refused_without_token(r.status, &r.body_text(), "another employee's rotate");
    assert_eq!(
        tokens.of(OWNER).as_deref(),
        Some(SEEDED),
        "a refused rotate must write nothing"
    );
}

#[tokio::test]
async fn a_guest_session_cannot_rotate_an_employees_token() {
    let tokens = Tokens::seeded();
    let r = TestRequest::post(URI)
        .as_user("guest@algedonic.dev", "audit-readonly")
        .send(&app(tokens.clone()))
        .await;
    assert_refused_without_token(r.status, &r.body_text(), "guest rotate");
    assert_eq!(tokens.of(OWNER).as_deref(), Some(SEEDED));
}

/// The path is the caller's to choose, so "you are the employee" must
/// not be satisfiable by naming yourself: the no-identity fallback's
/// id and a guest's username own no token.
#[tokio::test]
async fn naming_yourself_in_the_path_is_not_owning_a_token() {
    let tokens = Tokens::seeded();
    let anon = TestRequest::get("/api/scheduling/techs/anonymous/calendar-token")
        .send(&app(tokens.clone()))
        .await;
    anon.assert_status(StatusCode::FORBIDDEN);
    let anon_rotate = TestRequest::post("/api/scheduling/techs/anonymous/calendar-token")
        .send(&app(tokens.clone()))
        .await;
    anon_rotate.assert_status(StatusCode::FORBIDDEN);
    let guest = TestRequest::post("/api/scheduling/techs/guest@algedonic.dev/calendar-token")
        .as_user("guest@algedonic.dev", "audit-readonly")
        .send(&app(tokens.clone()))
        .await;
    guest.assert_status(StatusCode::FORBIDDEN);
    assert_eq!(tokens.of("anonymous"), None, "no token minted for no one");
    assert_eq!(
        tokens.of("guest@algedonic.dev"),
        None,
        "a read-only role mints nothing"
    );
}

/// A BASIC guest carries `visitor`, the second role on the read-only
/// floor (design 2830b6b7, landed beside this car on 2026-09-25). The
/// first version of this car named `audit-readonly` alone, so a visitor
/// session naming its own id in the path minted itself a token — the
/// drift `boss_core::roles::is_read_only_floor` exists to stop. The
/// guest's fixed address is an anonymous visitor's id whatever role it
/// rides with, so it owns no token to read either.
#[tokio::test]
async fn a_visitor_session_neither_reads_nor_mints_a_token() {
    let tokens = Tokens::seeded();
    let path = "/api/scheduling/techs/guest@algedonic.dev/calendar-token";
    let read = TestRequest::get(path)
        .as_user("guest@algedonic.dev", "visitor")
        .send(&app(tokens.clone()))
        .await;
    assert_refused_without_token(read.status, &read.body_text(), "visitor read");
    let rotate = TestRequest::post(path)
        .as_user("guest@algedonic.dev", "visitor")
        .send(&app(tokens.clone()))
        .await;
    assert_refused_without_token(rotate.status, &rotate.body_text(), "visitor rotate");
    let foreign = TestRequest::post(URI)
        .as_user("guest@algedonic.dev", "visitor")
        .send(&app(tokens.clone()))
        .await;
    assert_refused_without_token(foreign.status, &foreign.body_text(), "visitor rotate");
    assert_eq!(
        tokens.of("guest@algedonic.dev"),
        None,
        "a visitor mints nothing"
    );
    assert_eq!(tokens.of(OWNER).as_deref(), Some(SEEDED));
}

/// A read-only role never rotates, even on a path that names the
/// session's own id: a signed-in employee whose role is on the floor
/// (the seeded auditor shares `audit-readonly`; a visitor could be
/// bound to an employee id) reads, and never writes.
#[tokio::test]
async fn a_read_only_role_does_not_rotate_its_own_token() {
    for role in ["audit-readonly", "visitor"] {
        let tokens = Tokens::seeded();
        let r = TestRequest::post(URI)
            .as_user(OWNER, role)
            .send(&app(tokens.clone()))
            .await;
        assert_refused_without_token(r.status, &r.body_text(), role);
        assert_eq!(tokens.of(OWNER).as_deref(), Some(SEEDED), "{role} rotated");
    }
}

#[tokio::test]
async fn the_employee_rotates_their_own_token_and_receives_it() {
    let tokens = Tokens::seeded();
    let r = TestRequest::post(URI)
        .as_user(OWNER, "service-tech")
        .send(&app(tokens.clone()))
        .await;
    r.assert_status(StatusCode::OK);
    let body: Value = r.assert_json();
    let now = tokens.of(OWNER).expect("the rotate wrote a token");
    assert_ne!(now, SEEDED);
    assert_eq!(body["employee_id"], OWNER);
    assert_eq!(body["token"], now.as_str());
    assert_eq!(body["ics_url"], format!("/ics/{now}/calendar.ics"));
}
