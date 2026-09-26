//! A schedule is read by its employee, or by a grant whose scope
//! covers them (backlog a621d091).
//!
//! The five scheduling reads — availability, assignments (list and
//! single), shift-patterns and the week grid — took no caller until
//! this car. The gateway forwards `/api/scheduling/*` for any session,
//! so the guest session (`audit-readonly`, minted by
//! `POST /api/auth/guest`) read every employee's week, and a request
//! with no `x-boss-user` at all — the `/ics` traversal (1d9b7db7), or
//! anything reaching the jobs port directly — read the same.
//!
//! The rule is the jobs list's idiom: `CurrentUser`, then Read on a
//! resource (`schedule`) whose granted scope names the employees the
//! caller may see. An employee sees their own schedule without any
//! grant. `schedule` is not a shipped resource, so the read-only roles
//! — which the anonymous visitor carries — inherit nothing: the
//! platform-admin default reads all of it, and a tenant grants its
//! managers in its own seed.
//!
//! A headerless request is `anonymous` / `guest` in `CurrentUser`.
//! `trust::is_trusted` admits that caller on the operator-machinery
//! doors, and is deliberately untouched here (e84de48e); these reads
//! never ask it, so a headerless caller holds no grant and is nobody's
//! self — it is refused.
//!
//! The repository is an in-memory fake holding two employees' rows:
//! authorization is the thing under test, not storage.

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::http::StatusCode;
use boss_core::publisher::EventStamp;
use boss_jobs::scheduling::http::{SchedulingApiState, router};
use boss_jobs::scheduling::{
    AssignmentKind, AssignmentStatus, AvailabilityKind, AvailabilitySource, CalendarTokenSha256,
    NewScheduledAssignment, NewTechAvailability, ScheduledAssignment, SchedulingError,
    SchedulingRepository, TechAvailability, TechShiftPattern, WeekGridBlock, WeekGridRow,
};
use boss_policy_client::{Action, FakePolicyClient, Resource, Scope};
use boss_testing::TestRequest;
use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

const MINE: &str = "emp-tech-001";
const THEIRS: &str = "emp-tech-002";
const MANAGER: &str = "emp-mgr-001";

fn at(h: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 9, 28, h, 0, 0).unwrap()
}

/// A stable id per employee, so a test can ask for one by path.
fn assignment_id(emp: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, emp.as_bytes())
}

fn availability(emp: &str) -> TechAvailability {
    TechAvailability {
        id: Uuid::new_v5(&Uuid::NAMESPACE_URL, emp.as_bytes()),
        employee_id: emp.to_string(),
        kind: AvailabilityKind::Available,
        starts_at: at(8),
        ends_at: at(17),
        notes: None,
        source: AvailabilitySource::Manual,
        created_at: at(0),
    }
}

fn assignment(emp: &str) -> ScheduledAssignment {
    ScheduledAssignment {
        id: assignment_id(emp),
        tech_id: emp.to_string(),
        target_job_id: Uuid::nil(),
        kind: AssignmentKind::Wo,
        starts_at: at(10),
        ends_at: at(12),
        status: AssignmentStatus::Confirmed,
        notes: None,
        created_at: at(0),
        updated_at: at(0),
    }
}

fn shift(emp: &str) -> TechShiftPattern {
    TechShiftPattern {
        id: Uuid::new_v5(&Uuid::NAMESPACE_DNS, emp.as_bytes()),
        employee_id: emp.to_string(),
        day_of_week: 1,
        starts_at_time: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
        ends_at_time: NaiveTime::from_hms_opt(17, 0, 0).unwrap(),
        timezone: "UTC".into(),
        effective_from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        effective_to: None,
        created_at: at(0),
    }
}

/// Two employees' schedules. Each read filters the way the Postgres
/// adapter does: an employee named narrows to them, none named is all.
struct TwoSchedules;

const EVERYONE: [&str; 2] = [MINE, THEIRS];

fn named(filter: Option<&str>) -> Vec<&'static str> {
    EVERYONE
        .into_iter()
        .filter(|e| filter.is_none_or(|f| f == *e))
        .collect()
}

fn unused<T>() -> Result<T, SchedulingError> {
    Err(SchedulingError::Storage(
        "not held by this fake: it serves the reads only".into(),
    ))
}

#[async_trait]
impl SchedulingRepository for TwoSchedules {
    async fn create_availability(
        &self,
        _: NewTechAvailability,
        _: &EventStamp,
    ) -> Result<TechAvailability, SchedulingError> {
        unused()
    }
    async fn list_availability(
        &self,
        employee_id: Option<&str>,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
    ) -> Result<Vec<TechAvailability>, SchedulingError> {
        Ok(named(employee_id).into_iter().map(availability).collect())
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
        id: Uuid,
    ) -> Result<Option<ScheduledAssignment>, SchedulingError> {
        Ok(EVERYONE.into_iter().map(assignment).find(|a| a.id == id))
    }
    async fn list_assignments(
        &self,
        tech_id: Option<&str>,
        _: Option<Uuid>,
        _: DateTime<Utc>,
        _: DateTime<Utc>,
    ) -> Result<Vec<ScheduledAssignment>, SchedulingError> {
        Ok(named(tech_id).into_iter().map(assignment).collect())
    }
    async fn update_assignment_status(
        &self,
        _: Uuid,
        _: AssignmentStatus,
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
        _: NaiveTime,
        _: NaiveTime,
        _: &str,
        _: NaiveDate,
        _: &EventStamp,
    ) -> Result<TechShiftPattern, SchedulingError> {
        unused()
    }
    async fn list_shift_patterns(
        &self,
        employee_id: Option<&str>,
    ) -> Result<Vec<TechShiftPattern>, SchedulingError> {
        Ok(named(employee_id).into_iter().map(shift).collect())
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
        employee_ids: Option<&[String]>,
    ) -> Result<Vec<WeekGridRow>, SchedulingError> {
        Ok(EVERYONE
            .into_iter()
            .filter(|e| employee_ids.is_none_or(|ids| ids.iter().any(|i| i == e)))
            .map(|e| {
                let a = availability(e);
                WeekGridRow {
                    employee_id: e.to_string(),
                    blocks: vec![WeekGridBlock::Availability {
                        id: a.id,
                        kind: a.kind,
                        starts_at: a.starts_at,
                        ends_at: a.ends_at,
                        notes: None,
                    }],
                }
            })
            .collect())
    }
    async fn calendar_feed_created_at(
        &self,
        _: &str,
    ) -> Result<Option<DateTime<Utc>>, SchedulingError> {
        unused()
    }
    async fn rotate_calendar_token(
        &self,
        _: &str,
        _: &CalendarTokenSha256,
        _: DateTime<Utc>,
        _: &EventStamp,
    ) -> Result<(), SchedulingError> {
        unused()
    }
    async fn revoke_calendar_token(
        &self,
        _: &str,
        _: &CalendarTokenSha256,
        _: DateTime<Utc>,
        _: &EventStamp,
    ) -> Result<(), SchedulingError> {
        unused()
    }
    async fn count_calendar_tokens_logged_raw(&self) -> Result<i64, SchedulingError> {
        unused()
    }
    async fn revoke_calendar_tokens_logged_raw(
        &self,
        _: DateTime<Utc>,
        _: &EventStamp,
    ) -> Result<u64, SchedulingError> {
        unused()
    }
    async fn employee_by_calendar_token(
        &self,
        _: &CalendarTokenSha256,
    ) -> Result<Option<String>, SchedulingError> {
        unused()
    }
}

/// The platform's shipped defaults, plus one tenant-style grant: a
/// service manager reads the schedules of their team — the shape a
/// tenant seed writes for its managers.
fn app() -> Router {
    let policy = boss_policy_client::defaults::default_rules()
        .into_iter()
        .fold(FakePolicyClient::builder(), |b, r| {
            b.allow(r.role, r.action, r.resource, r.scope)
        })
        .allow(
            "service-mgr",
            Action::Read,
            Resource::schedule(),
            Scope::Team,
        )
        .build();
    router(SchedulingApiState {
        repo: Arc::new(TwoSchedules),
        publisher: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        policy: Arc::new(policy),
    })
}

/// Who is asking — as the gateway's `build_user_json` spells it.
#[derive(Clone, Copy)]
enum Caller {
    /// No `x-boss-user` at all.
    Headerless,
    /// The gateway's guest session: no employee id, so it identifies
    /// as the guest address, at `audit-readonly`.
    GuestSession,
    /// The same session at `visitor`, the Basic guest role design
    /// 2830b6b7 added to the read-only floor — the role a session
    /// that names none now falls back to.
    VisitorSession,
    Employee(&'static str),
    /// A manager whose one direct report is `MINE`.
    Manager,
    /// The deploy operator.
    Operator,
}

fn user_header(c: Caller) -> Option<Value> {
    let (id, role, tier, reports): (&str, &str, &str, Vec<&str>) = match c {
        Caller::Headerless => return None,
        Caller::GuestSession => ("guest@algedonic.dev", "audit-readonly", "user", vec![]),
        Caller::VisitorSession => ("guest@algedonic.dev", "visitor", "user", vec![]),
        Caller::Employee(id) => (id, "service-tech", "user", vec![]),
        Caller::Manager => (MANAGER, "service-mgr", "user", vec![MINE]),
        Caller::Operator => ("emp-david", "platform-admin", "operator", vec![]),
    };
    Some(json!({
        "id": id,
        "role": role,
        "access_tier": tier,
        "territory_account_ids": [],
        "direct_report_ids": reports,
        "department": null,
    }))
}

async fn get(app: &Router, who: Caller, uri: &str) -> (StatusCode, Value) {
    let req = TestRequest::get(uri);
    let req = match user_header(who) {
        Some(u) => req.header("x-boss-user", u.to_string()),
        None => req,
    };
    let resp = req.send(app).await;
    let body = serde_json::from_slice(&resp.body_bytes).unwrap_or(Value::Null);
    (resp.status, body)
}

/// The employee ids a read's answer carries, whatever its shape.
fn employees_in(body: &Value) -> Vec<String> {
    let rows = match body {
        Value::Array(rows) => rows.clone(),
        Value::Object(o) if o.contains_key("rows") => {
            o["rows"].as_array().cloned().unwrap_or_default()
        }
        Value::Object(_) => vec![body.clone()],
        _ => vec![],
    };
    let mut ids: Vec<String> = rows
        .iter()
        .filter_map(|r| {
            r.get("employee_id")
                .or_else(|| r.get("tech_id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    ids.sort();
    ids
}

/// Every read, naming `emp` where the route can name one.
fn reads_naming(emp: &str) -> Vec<String> {
    vec![
        format!("/api/scheduling/availability?employee_id={emp}"),
        format!("/api/scheduling/assignments?tech_id={emp}"),
        format!("/api/scheduling/assignments/{}", assignment_id(emp)),
        format!("/api/scheduling/shift-patterns?employee_id={emp}"),
        format!("/api/scheduling/week-grid?employees={emp}"),
    ]
}

/// Every list read, naming nobody — "everyone I may see".
fn reads_naming_nobody() -> Vec<&'static str> {
    vec![
        "/api/scheduling/availability",
        "/api/scheduling/assignments",
        "/api/scheduling/shift-patterns",
        "/api/scheduling/week-grid",
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn a_headerless_request_reads_no_schedule() {
    let app = app();
    for uri in reads_naming(MINE)
        .iter()
        .map(String::as_str)
        .chain(reads_naming_nobody())
    {
        let (status, body) = get(&app, Caller::Headerless, uri).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri} answered {body}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_guest_session_reads_no_schedule() {
    let app = app();
    for who in [Caller::GuestSession, Caller::VisitorSession] {
        for uri in reads_naming(MINE)
            .iter()
            .map(String::as_str)
            .chain(reads_naming_nobody())
        {
            let (status, body) = get(&app, who, uri).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri} answered {body}");
        }
    }
}

/// A read by id names nobody, so its refusal must not name the row's
/// employee either: answering "emp-x's schedule is outside your scope"
/// to a guessed id tells the caller whose assignment it is — which is
/// the thing being refused (adversarial review of this car,
/// 2026-09-25, SF3).
#[tokio::test(flavor = "multi_thread")]
async fn a_refused_read_by_id_names_no_employee() {
    let app = app();
    let uri = format!("/api/scheduling/assignments/{}", assignment_id(MINE));
    let mut req = TestRequest::get(&uri);
    if let Some(u) = user_header(Caller::Employee(THEIRS)) {
        req = req.header("x-boss-user", u.to_string());
    }
    let resp = req.send(&app).await;
    let text = String::from_utf8_lossy(&resp.body_bytes);
    assert_eq!(resp.status, StatusCode::FORBIDDEN, "{uri} answered {text}");
    assert!(
        !text.contains(MINE),
        "the refusal named the assignment's employee: {text}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_employee_reads_their_own_schedule() {
    let app = app();
    for uri in reads_naming(MINE) {
        let (status, body) = get(&app, Caller::Employee(MINE), &uri).await;
        assert_eq!(status, StatusCode::OK, "{uri} answered {body}");
        assert_eq!(employees_in(&body), vec![MINE], "{uri} answered {body}");
    }
    // Naming nobody is "everyone I may see" — for an employee with no
    // grant, that is themself, and never a colleague.
    for uri in reads_naming_nobody() {
        let (status, body) = get(&app, Caller::Employee(MINE), uri).await;
        assert_eq!(status, StatusCode::OK, "{uri} answered {body}");
        assert_eq!(employees_in(&body), vec![MINE], "{uri} answered {body}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn another_employee_without_scope_is_refused() {
    let app = app();
    for uri in reads_naming(MINE) {
        let (status, body) = get(&app, Caller::Employee(THEIRS), &uri).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri} answered {body}");
    }
    // Naming two where one is not yours refuses the whole read rather
    // than quietly answering half of it.
    let (status, body) = get(
        &app,
        Caller::Employee(THEIRS),
        &format!("/api/scheduling/week-grid?employees={THEIRS},{MINE}"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "week-grid answered {body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_manager_reads_their_team_by_scope_and_no_further() {
    let app = app();
    for uri in reads_naming(MINE) {
        let (status, body) = get(&app, Caller::Manager, &uri).await;
        assert_eq!(status, StatusCode::OK, "{uri} answered {body}");
        assert_eq!(employees_in(&body), vec![MINE], "{uri} answered {body}");
    }
    for uri in reads_naming(THEIRS) {
        let (status, body) = get(&app, Caller::Manager, &uri).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{uri} answered {body}");
    }
    for uri in reads_naming_nobody() {
        let (status, body) = get(&app, Caller::Manager, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri} answered {body}");
        assert_eq!(employees_in(&body), vec![MINE], "{uri} answered {body}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_operator_reads_every_schedule() {
    // The control: the platform-admin default grant reads all of it,
    // so the refusals above are the gate and not a broken fixture.
    let app = app();
    for emp in EVERYONE {
        for uri in reads_naming(emp) {
            let (status, body) = get(&app, Caller::Operator, &uri).await;
            assert_eq!(status, StatusCode::OK, "{uri} answered {body}");
            assert_eq!(employees_in(&body), vec![emp], "{uri} answered {body}");
        }
    }
    for uri in reads_naming_nobody() {
        let (status, body) = get(&app, Caller::Operator, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri} answered {body}");
        assert_eq!(
            employees_in(&body),
            vec![MINE, THEIRS],
            "{uri} answered {body}"
        );
    }
}
