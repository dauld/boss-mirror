//! Axum routes for the scheduling surfaces.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use boss_clock_client::ClockClient;
use boss_core::publisher::DomainPublisher;
use boss_core::roles::{ANONYMOUS_VISITOR_IDS, PLATFORM_ADMIN_ROLE, is_anonymous_visitor_role};
use boss_policy_client::{CurrentUser, User};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
use uuid::Uuid;

use super::ics::build_ics;
use super::port::{SchedulingError, SchedulingRepository};
use super::types::{AssignmentStatus, NewScheduledAssignment, NewTechAvailability};

pub struct SchedulingApiState {
    pub repo: Arc<dyn SchedulingRepository>,
    /// Audit-log + NATS publisher. `None` allowed for tests that
    /// only exercise projection writes.
    pub publisher: Option<DomainPublisher>,
    /// Authoritative clock — every event stamp resolves via clock-api
    /// so sim mode produces sim-dated audit_log rows. Same trait as
    /// the rest of the workspace.
    pub clock: Arc<dyn ClockClient>,
}

pub fn router(state: SchedulingApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route(
            "/api/scheduling/availability",
            get(list_avail).post(create_avail),
        )
        .route("/api/scheduling/availability/{id}", delete(delete_avail))
        .route(
            "/api/scheduling/assignments",
            get(list_assign).post(create_assign),
        )
        .route(
            "/api/scheduling/assignments/{id}",
            get(get_assign).delete(delete_assign),
        )
        .route(
            "/api/scheduling/assignments/{id}/status",
            post(update_assign_status),
        )
        .route(
            "/api/scheduling/shift-patterns",
            get(list_shifts).post(upsert_shift),
        )
        .route(
            "/api/scheduling/shift-patterns/materialize",
            post(materialize),
        )
        .route("/api/scheduling/week-grid", get(week_grid))
        .route(
            "/api/scheduling/techs/{emp_id}/calendar-token",
            get(get_calendar_token).post(rotate_calendar_token),
        )
        .route("/ics/{token}/calendar.ics", get(public_ics_feed))
        .with_state(shared)
}

fn err(e: SchedulingError) -> Response {
    match e {
        SchedulingError::NotFound(s) => (StatusCode::NOT_FOUND, s).into_response(),
        SchedulingError::BadRequest(s) => (StatusCode::BAD_REQUEST, s).into_response(),
        SchedulingError::Storage(s) => (StatusCode::INTERNAL_SERVER_ERROR, s).into_response(),
    }
}

// ---------------------------------------------------------------------------
// Availability
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RangeQuery {
    /// RFC 3339 timestamps; defaults to now → +7 days.
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    employee_id: Option<String>,
}

fn resolve_range(q: &RangeQuery) -> (DateTime<Utc>, DateTime<Utc>) {
    let from = q.from.unwrap_or_else(Utc::now);
    let to = q.to.unwrap_or_else(|| from + Duration::days(7));
    (from, to)
}

/// Resolve the outbox event stamp for this request. Scheduling
/// write handlers carry no CurrentUser extractor (only the two
/// calendar-token handlers do, to authorize); the publisher's
/// `default_actor` resolves the request identity from the task-local
/// context, and its clock probe settles `_simulated` — the same
/// envelope the retired post-commit emits carried (outbox phase 2).
/// The stamp mints wall-clock time itself: sim time is retired from
/// the record (David, 2026-08-22, packet a7a4cae5).
async fn event_stamp(state: &SchedulingApiState) -> boss_core::publisher::EventStamp {
    match &state.publisher {
        Some(p) => p.stamp_with_actor(p.default_actor()).await,
        None => boss_core::publisher::EventStamp::new(
            "jobs",
            boss_core::actor::ActorId::Automation("scheduling".into()),
        ),
    }
}

async fn list_avail(
    State(state): State<Arc<SchedulingApiState>>,
    Query(q): Query<RangeQuery>,
) -> Response {
    let (from, to) = resolve_range(&q);
    match state
        .repo
        .list_availability(q.employee_id.as_deref(), from, to)
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn create_avail(
    State(state): State<Arc<SchedulingApiState>>,
    Json(body): Json<NewTechAvailability>,
) -> Response {
    // OUTBOX (phase 2): the adapter records the scheduling events
    // inside the domain transaction; nothing publishes post-commit.
    let stamp = event_stamp(&state).await;
    match state.repo.create_availability(body, &stamp).await {
        Ok(row) => (StatusCode::CREATED, Json(row)).into_response(),
        Err(e) => err(e),
    }
}

async fn delete_avail(
    State(state): State<Arc<SchedulingApiState>>,
    Path(id): Path<Uuid>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    let stamp = event_stamp(&state).await;
    match state.repo.delete_availability(id, now, &stamp).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(e),
    }
}

// ---------------------------------------------------------------------------
// Assignments
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AssignQuery {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    tech_id: Option<String>,
    target_job_id: Option<Uuid>,
}

async fn list_assign(
    State(state): State<Arc<SchedulingApiState>>,
    Query(q): Query<AssignQuery>,
) -> Response {
    let from = q.from.unwrap_or_else(Utc::now);
    let to = q.to.unwrap_or_else(|| from + Duration::days(7));
    match state
        .repo
        .list_assignments(q.tech_id.as_deref(), q.target_job_id, from, to)
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

async fn create_assign(
    State(state): State<Arc<SchedulingApiState>>,
    Json(body): Json<NewScheduledAssignment>,
) -> Response {
    let stamp = event_stamp(&state).await;
    match state.repo.create_assignment(body, &stamp).await {
        Ok(row) => (StatusCode::CREATED, Json(row)).into_response(),
        Err(e) => err(e),
    }
}

async fn get_assign(
    State(state): State<Arc<SchedulingApiState>>,
    Path(id): Path<Uuid>,
) -> Response {
    match state.repo.get_assignment(id).await {
        Ok(Some(row)) => Json(row).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, id.to_string()).into_response(),
        Err(e) => err(e),
    }
}

async fn delete_assign(
    State(state): State<Arc<SchedulingApiState>>,
    Path(id): Path<Uuid>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    let stamp = event_stamp(&state).await;
    match state.repo.delete_assignment(id, now, &stamp).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct StatusBody {
    status: AssignmentStatus,
}

async fn update_assign_status(
    State(state): State<Arc<SchedulingApiState>>,
    Path(id): Path<Uuid>,
    Json(body): Json<StatusBody>,
) -> Response {
    let now = boss_clock_client::now_from(&state.clock).await;
    let stamp = event_stamp(&state).await;
    match state
        .repo
        .update_assignment_status(id, body.status, now, &stamp)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(e),
    }
}

// ---------------------------------------------------------------------------
// Shift patterns
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ShiftQuery {
    employee_id: Option<String>,
}

async fn list_shifts(
    State(state): State<Arc<SchedulingApiState>>,
    Query(q): Query<ShiftQuery>,
) -> Response {
    match state
        .repo
        .list_shift_patterns(q.employee_id.as_deref())
        .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct UpsertShiftBody {
    employee_id: String,
    day_of_week: i16,
    starts_at_time: chrono::NaiveTime,
    ends_at_time: chrono::NaiveTime,
    #[serde(default = "default_tz")]
    timezone: String,
    #[serde(default)]
    effective_from: Option<chrono::NaiveDate>,
}
fn default_tz() -> String {
    "America/Los_Angeles".to_string()
}

async fn upsert_shift(
    State(state): State<Arc<SchedulingApiState>>,
    Json(body): Json<UpsertShiftBody>,
) -> Response {
    let eff = body
        .effective_from
        .unwrap_or_else(|| Utc::now().date_naive());
    let stamp = event_stamp(&state).await;
    match state
        .repo
        .upsert_shift_pattern(
            &body.employee_id,
            body.day_of_week,
            body.starts_at_time,
            body.ends_at_time,
            &body.timezone,
            eff,
            &stamp,
        )
        .await
    {
        Ok(row) => Json(row).into_response(),
        Err(e) => err(e),
    }
}

#[derive(Deserialize)]
struct MaterializeBody {
    #[serde(default = "default_weeks_ahead")]
    weeks_ahead: i64,
}
fn default_weeks_ahead() -> i64 {
    super::materialize::DEFAULT_WEEKS_AHEAD
}

async fn materialize(
    State(state): State<Arc<SchedulingApiState>>,
    Json(body): Json<MaterializeBody>,
) -> Response {
    match super::materialize::materialize_next(state.repo.as_ref(), body.weeks_ahead).await {
        Ok(inserted) => Json(serde_json::json!({ "inserted": inserted })).into_response(),
        Err(e) => err(e),
    }
}

// ---------------------------------------------------------------------------
// Week grid projection
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct WeekGridQuery {
    from: Option<DateTime<Utc>>,
    to: Option<DateTime<Utc>>,
    /// Comma-separated employee IDs; empty means "all techs".
    employees: Option<String>,
}

// ---------------------------------------------------------------------------
// ICS calendar feed — token management + public feed endpoint
// ---------------------------------------------------------------------------

/// Window the public feed exposes. 90 days back covers "what did I do
/// last quarter", 180 days forward covers the tech's visible horizon
/// without flooding their calendar client with distant tentative work.
const ICS_PAST_DAYS: i64 = 90;
const ICS_FUTURE_DAYS: i64 = 180;

// WHO MAY TOUCH A CALENDAR TOKEN (backlog 7ae9ccec, 2026-09-25). The
// token is the whole authentication of the sessionless feed above, so
// holding it IS reading the employee's schedule. Both handlers took no
// caller until this car: a guest session (audit-readonly, which passes
// has_global_read) could GET any employee's token through the gateway's
// /api/scheduling/* proxy, and any signed-in employee could read or
// rotate anyone's. Global READ is not this token's grant — the token is
// the employee's own, like a password.
//
// - A read answers only the employee themself. Nobody else, operator
//   included, gets 403.
// - A rotate is the employee, or a platform-admin revoking a leaked
//   feed; the operator's response carries no token, so revoking a feed
//   never hands the operator the new one.
// - An anonymous visitor's id — the CurrentUser fallback for a request
//   with no x-boss-user (`anonymous`), or the guest session's fixed
//   address — owns nothing, whatever the path says, and neither a
//   read-only-floor role nor the headerless `guest` ever mints a token.
//   Both are asked of boss_core::roles rather than spelled here: the
//   first version of this car named `audit-readonly` alone, and the
//   `visitor` role joined the floor the same day (design 2830b6b7).

fn is_the_employee(user: &User, emp_id: &str) -> bool {
    !ANONYMOUS_VISITOR_IDS.contains(&user.id.as_str()) && user.id == emp_id
}

fn may_rotate(user: &User, emp_id: &str) -> bool {
    user.role == PLATFORM_ADMIN_ROLE
        || (is_the_employee(user, emp_id) && !is_anonymous_visitor_role(&user.role))
}

fn not_yours(emp_id: &str, verb: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        format!("a calendar token is its employee's alone: only {emp_id} may {verb} it"),
    )
        .into_response()
}

async fn get_calendar_token(
    State(state): State<Arc<SchedulingApiState>>,
    CurrentUser(user): CurrentUser,
    Path(emp_id): Path<String>,
) -> Response {
    if !is_the_employee(&user, &emp_id) {
        return not_yours(&emp_id, "read");
    }
    match state.repo.calendar_token_for(&emp_id).await {
        Ok(Some(t)) => Json(serde_json::json!({
            "employee_id": emp_id,
            "token": t,
            "ics_url": format!("/ics/{t}/calendar.ics"),
        }))
        .into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            format!("no calendar token for {emp_id}"),
        )
            .into_response(),
        Err(e) => err(e),
    }
}

async fn rotate_calendar_token(
    State(state): State<Arc<SchedulingApiState>>,
    CurrentUser(user): CurrentUser,
    Path(emp_id): Path<String>,
) -> Response {
    if !may_rotate(&user, &emp_id) {
        return not_yours(&emp_id, "rotate");
    }
    // Two v4 UUIDs concatenated = 256 bits of randomness. `simple()`
    // format emits 32 hex chars per UUID, so the token is a 64-char
    // URL-safe string.
    let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let now = boss_clock_client::now_from(&state.clock).await;
    let stamp = event_stamp(&state).await;
    match state
        .repo
        .rotate_calendar_token(&emp_id, &token, now, &stamp)
        .await
    {
        // Only the employee is handed the new feed. An operator's
        // rotate is a revocation: the old URL stops working, and the
        // employee reads the new one themself.
        Ok(()) if is_the_employee(&user, &emp_id) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "employee_id": emp_id,
                "token": token,
                "ics_url": format!("/ics/{token}/calendar.ics"),
            })),
        )
            .into_response(),
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "employee_id": emp_id,
                "rotated": true,
            })),
        )
            .into_response(),
        Err(e) => err(e),
    }
}

async fn public_ics_feed(
    State(state): State<Arc<SchedulingApiState>>,
    Path(token): Path<String>,
) -> Response {
    let emp_id = match state.repo.employee_by_calendar_token(&token).await {
        Ok(Some(e)) => e,
        Ok(None) => return (StatusCode::NOT_FOUND, "unknown calendar token").into_response(),
        Err(e) => return err(e),
    };

    let now = Utc::now();
    let from = now - Duration::days(ICS_PAST_DAYS);
    let to = now + Duration::days(ICS_FUTURE_DAYS);

    let assignments = match state
        .repo
        .list_assignments(Some(&emp_id), None, from, to)
        .await
    {
        Ok(rows) => rows,
        Err(e) => return err(e),
    };
    let availability = match state.repo.list_availability(Some(&emp_id), from, to).await {
        Ok(rows) => rows,
        Err(e) => return err(e),
    };

    let body = build_ics(&emp_id, &assignments, &availability, now);
    (
        StatusCode::OK,
        [
            (
                axum::http::header::CONTENT_TYPE,
                "text/calendar; charset=utf-8",
            ),
            (axum::http::header::CACHE_CONTROL, "private, max-age=300"),
        ],
        body,
    )
        .into_response()
}

async fn week_grid(
    State(state): State<Arc<SchedulingApiState>>,
    Query(q): Query<WeekGridQuery>,
) -> Response {
    let from = q.from.unwrap_or_else(Utc::now);
    let to = q.to.unwrap_or_else(|| from + Duration::days(7));
    let emp_vec: Option<Vec<String>> = q.employees.as_deref().map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(ToOwned::to_owned)
            .collect()
    });
    let slice: Option<&[String]> = emp_vec.as_deref();
    match state.repo.week_grid(from, to, slice).await {
        Ok(rows) => Json(serde_json::json!({
            "from": from,
            "to": to,
            "rows": rows,
        }))
        .into_response(),
        Err(e) => err(e),
    }
}
