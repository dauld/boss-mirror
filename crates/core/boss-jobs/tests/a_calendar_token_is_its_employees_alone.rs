//! An employee's calendar-feed token is theirs alone (backlog 7ae9ccec),
//! and the server keeps only its digest (backlog 4aaff4dc).
//!
//! The token IS the authentication for the public, sessionless
//! `/ics/{token}/calendar.ics` feed — 90 days of the employee's past
//! and 180 of their future schedule. Until car f46d9df2 both token
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
//! Since design 3101c506 (2026-09-26) the server holds the SHA-256 of a
//! token and never the token: the read says only THAT a feed exists and
//! when it was made, the employee sees the URL once in their own
//! rotate's response, an operator's rotate is a revocation that refuses
//! 404 when there is no feed to revoke, and the feeds minted before
//! digests — whose tokens the audit log holds in the clear — are revoked
//! by one bounded verb.
//!
//! The repository here is an in-memory fake holding only the token
//! table, and it holds it the way the Postgres adapter does: digests
//! (a_calendar_token_rests_as_its_hash_pg.rs proves the adapter).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::http::StatusCode;
use boss_core::publisher::EventStamp;
use boss_jobs::scheduling::http::{SchedulingApiState, router};
use boss_jobs::scheduling::{
    CalendarTokenSha256, NewScheduledAssignment, NewTechAvailability, ScheduledAssignment,
    SchedulingError, SchedulingRepository, TechAvailability, TechShiftPattern, WeekGridRow,
};
use boss_testing::TestRequest;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::Value;
use uuid::Uuid;

const OWNER: &str = "emp-tech-001";
const OTHER: &str = "emp-tech-002";
const SEEDED: &str = "seeded-token-of-emp-tech-001";
const URI: &str = "/api/scheduling/techs/emp-tech-001/calendar-token";
const LOGGED_RAW: &str = "/api/scheduling/calendar-tokens/logged-raw";
const REVOKE_LOGGED_RAW: &str = "/api/scheduling/calendar-tokens/logged-raw/revoke";

#[derive(Clone)]
struct Feed {
    sha: CalendarTokenSha256,
    created_at: DateTime<Utc>,
    logged_raw: bool,
}

/// The token table as digests, plus a count of the events a write
/// would record — one per row written, none for a refusal.
#[derive(Default)]
struct Tokens {
    feeds: Mutex<HashMap<String, Feed>>,
    events: Mutex<u64>,
}

impl Tokens {
    /// OWNER holds a feed minted before digests: its token is in the
    /// log in the clear, so it is marked `logged_raw`, as the migration
    /// marks every row it re-hashed.
    fn seeded() -> Arc<Self> {
        let t = Self::default();
        t.feeds.lock().unwrap().insert(
            OWNER.to_string(),
            Feed {
                sha: CalendarTokenSha256::of(SEEDED),
                created_at: Utc::now(),
                logged_raw: true,
            },
        );
        Arc::new(t)
    }
    fn of(&self, emp: &str) -> Option<CalendarTokenSha256> {
        self.feeds.lock().unwrap().get(emp).map(|f| f.sha.clone())
    }
    fn events(&self) -> u64 {
        *self.events.lock().unwrap()
    }
    fn write(&self, emp: &str, sha: &CalendarTokenSha256, at: DateTime<Utc>) {
        self.feeds.lock().unwrap().insert(
            emp.to_string(),
            Feed {
                sha: sha.clone(),
                created_at: at,
                logged_raw: false,
            },
        );
        *self.events.lock().unwrap() += 1;
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
    /// The feed's schedule is empty here: the fake holds no schedule,
    /// only the tokens that open one.
    async fn list_availability(
        &self,
        _: Option<&str>,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
    ) -> Result<Vec<TechAvailability>, SchedulingError> {
        Ok(vec![])
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
        Ok(vec![])
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
    async fn calendar_feed_created_at(
        &self,
        employee_id: &str,
    ) -> Result<Option<DateTime<Utc>>, SchedulingError> {
        Ok(self
            .feeds
            .lock()
            .unwrap()
            .get(employee_id)
            .map(|f| f.created_at))
    }
    async fn rotate_calendar_token(
        &self,
        employee_id: &str,
        token_sha256: &CalendarTokenSha256,
        now: DateTime<Utc>,
        _: &EventStamp,
    ) -> Result<(), SchedulingError> {
        self.write(employee_id, token_sha256, now);
        Ok(())
    }
    async fn revoke_calendar_token(
        &self,
        employee_id: &str,
        token_sha256: &CalendarTokenSha256,
        now: DateTime<Utc>,
        _: &EventStamp,
    ) -> Result<(), SchedulingError> {
        if self.of(employee_id).is_none() {
            return Err(SchedulingError::NotFound(format!(
                "no calendar feed for {employee_id}"
            )));
        }
        self.write(employee_id, token_sha256, now);
        Ok(())
    }
    async fn count_calendar_tokens_logged_raw(&self) -> Result<i64, SchedulingError> {
        Ok(self
            .feeds
            .lock()
            .unwrap()
            .values()
            .filter(|f| f.logged_raw)
            .count() as i64)
    }
    async fn revoke_calendar_tokens_logged_raw(
        &self,
        now: DateTime<Utc>,
        _: &EventStamp,
    ) -> Result<u64, SchedulingError> {
        let logged: Vec<String> = self
            .feeds
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, f)| f.logged_raw)
            .map(|(e, _)| e.clone())
            .collect();
        for emp in &logged {
            self.write(emp, &CalendarTokenSha256::of_a_discarded_token(), now);
        }
        Ok(logged.len() as u64)
    }
    async fn employee_by_calendar_token(
        &self,
        token_sha256: &CalendarTokenSha256,
    ) -> Result<Option<String>, SchedulingError> {
        Ok(self
            .feeds
            .lock()
            .unwrap()
            .iter()
            .find(|(_, f)| &f.sha == token_sha256)
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

/// Whether `token` opens a feed, asked the way a calendar app asks.
async fn opens(tokens: &Arc<Tokens>, token: &str) -> bool {
    let r = TestRequest::get(format!("/ics/{token}/calendar.ics"))
        .send(&app(tokens.clone()))
        .await;
    match r.status {
        StatusCode::OK => true,
        StatusCode::NOT_FOUND => false,
        other => panic!("the feed answered {other}: {}", r.body_text()),
    }
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

/// The server holds a digest, so even the employee's own read answers
/// only that a feed exists and when it was made — the URL was theirs
/// once, in the response to the rotate that made it.
#[tokio::test]
async fn the_employee_reads_that_their_feed_exists_and_never_its_url() {
    let r = TestRequest::get(URI)
        .as_user(OWNER, "service-tech")
        .send(&app(Tokens::seeded()))
        .await;
    r.assert_status(StatusCode::OK);
    let body: Value = r.assert_json();
    assert_eq!(body["employee_id"], OWNER);
    assert!(body["created_at"].is_string(), "{body}");
    for key in ["token", "ics_url", "token_sha256"] {
        assert!(body.get(key).is_none(), "the read carried {key}: {body}");
    }
    assert!(!r.body_text().contains(SEEDED));
}

#[tokio::test]
async fn an_employee_with_no_feed_reads_404() {
    let r = TestRequest::get("/api/scheduling/techs/emp-tech-002/calendar-token")
        .as_user(OTHER, "service-tech")
        .send(&app(Tokens::seeded()))
        .await;
    r.assert_status(StatusCode::NOT_FOUND);
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
    assert_eq!(body["revoked"], true, "{body}");
    assert!(
        body.get("token").is_none() && body.get("ics_url").is_none(),
        "an operator's rotate must not hand the operator the new feed: {body}"
    );
    let now = tokens.of(OWNER).expect("the revoke wrote a digest");
    assert_ne!(
        now,
        CalendarTokenSha256::of(SEEDED),
        "the old feed URL must stop working"
    );
    assert!(!opens(&tokens, SEEDED).await, "the revoked URL still opens");
    assert!(
        !r.body_text().contains(now.as_str()),
        "the new digest rode in the operator's response: {}",
        r.body_text()
    );
}

/// Round-2 finding 3 on car f46d9df2: an operator's rotate on an id
/// nobody checked minted an orphan row and event. An operator's rotate
/// is a revocation, so with no feed there is nothing to revoke.
#[tokio::test]
async fn an_operators_revoke_of_an_employee_with_no_feed_is_404_and_writes_nothing() {
    let tokens = Tokens::seeded();
    let r = TestRequest::post("/api/scheduling/techs/emp-made-up/calendar-token")
        .as_user("emp-david", "platform-admin")
        .send(&app(tokens.clone()))
        .await;
    r.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(tokens.of("emp-made-up"), None, "an orphan row was minted");
    assert_eq!(tokens.events(), 0, "a refused revoke recorded an event");
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
        tokens.of(OWNER),
        Some(CalendarTokenSha256::of(SEEDED)),
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
    assert_eq!(tokens.of(OWNER), Some(CalendarTokenSha256::of(SEEDED)));
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
    assert_eq!(tokens.of(OWNER), Some(CalendarTokenSha256::of(SEEDED)));
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
        assert_eq!(
            tokens.of(OWNER),
            Some(CalendarTokenSha256::of(SEEDED)),
            "{role} rotated"
        );
    }
}

/// The employee's own rotate is the one place the token is shown — once
/// — and what the server kept is its digest, not it.
#[tokio::test]
async fn the_employee_rotates_their_own_token_and_receives_it_once() {
    let tokens = Tokens::seeded();
    let r = TestRequest::post(URI)
        .as_user(OWNER, "service-tech")
        .send(&app(tokens.clone()))
        .await;
    r.assert_status(StatusCode::OK);
    let body: Value = r.assert_json();
    let token = body["token"]
        .as_str()
        .expect("the owner is handed the token");
    assert_eq!(body["employee_id"], OWNER);
    assert_eq!(body["ics_url"], format!("/ics/{token}/calendar.ics"));
    let kept = tokens.of(OWNER).expect("the rotate wrote a digest");
    assert_eq!(kept, CalendarTokenSha256::of(token));
    assert_ne!(kept.as_str(), token, "the server kept the token itself");
    assert!(opens(&tokens, token).await, "the new URL must open");
    assert!(!opens(&tokens, SEEDED).await, "the old URL still opens");
}

/// What the log and the table hold is the digest, so the digest must
/// not open the feed — or holding the log would still be holding the
/// feed.
#[tokio::test]
async fn the_feed_opens_with_its_token_and_never_with_its_digest() {
    let tokens = Tokens::seeded();
    assert!(opens(&tokens, SEEDED).await);
    let digest = CalendarTokenSha256::of(SEEDED);
    assert!(
        !opens(&tokens, digest.as_str()).await,
        "the stored digest opened the feed"
    );
}

/// The feeds minted before digests have their tokens in the log, which
/// cannot be edited — so they are revoked, by one bounded verb run after
/// the deploy. Only a platform-admin runs it; it revokes exactly the
/// rows whose tokens were logged, so a second run revokes nothing and a
/// feed re-subscribed since keeps working.
#[tokio::test]
async fn the_logged_tokens_are_revoked_by_an_operator_and_only_once() {
    let tokens = Tokens::seeded();
    // OTHER mints a feed under digests before the revoke runs.
    let other = TestRequest::post("/api/scheduling/techs/emp-tech-002/calendar-token")
        .as_user(OTHER, "service-tech")
        .send(&app(tokens.clone()))
        .await;
    other.assert_status(StatusCode::OK);
    let other_token = other.assert_json::<Value>()["token"]
        .as_str()
        .unwrap()
        .to_string();

    let count = TestRequest::get(LOGGED_RAW)
        .as_user("guest@algedonic.dev", "audit-readonly")
        .send(&app(tokens.clone()))
        .await;
    count.assert_status(StatusCode::OK);
    assert_eq!(count.assert_json::<Value>()["logged_raw"], 1);

    for (id, role) in [
        (OWNER, "service-tech"),
        ("guest@algedonic.dev", "audit-readonly"),
        ("anonymous", "guest"),
    ] {
        let r = TestRequest::post(REVOKE_LOGGED_RAW)
            .as_user(id, role)
            .send(&app(tokens.clone()))
            .await;
        r.assert_status(StatusCode::FORBIDDEN);
    }
    assert!(opens(&tokens, SEEDED).await, "a refused revoke revoked");

    let r = TestRequest::post(REVOKE_LOGGED_RAW)
        .as_user("emp-david", "platform-admin")
        .send(&app(tokens.clone()))
        .await;
    r.assert_status(StatusCode::OK);
    assert_eq!(r.assert_json::<Value>()["revoked"], 1);
    assert!(!opens(&tokens, SEEDED).await, "a logged token still opens");
    assert!(
        opens(&tokens, &other_token).await,
        "a digest-era feed broke"
    );
    assert!(
        tokens.of(OWNER).is_some(),
        "the revoke keeps the row, with a digest no URL matches"
    );

    let again = TestRequest::post(REVOKE_LOGGED_RAW)
        .as_user("emp-david", "platform-admin")
        .send(&app(tokens.clone()))
        .await;
    again.assert_status(StatusCode::OK);
    assert_eq!(again.assert_json::<Value>()["revoked"], 0);
    let after = TestRequest::get(LOGGED_RAW)
        .send(&app(tokens.clone()))
        .await;
    assert_eq!(after.assert_json::<Value>()["logged_raw"], 0);
}
