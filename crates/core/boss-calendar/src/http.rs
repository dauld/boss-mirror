//! Axum HTTP API for the calendar service.
//!
//! Routes:
//! - `POST /api/calendar/reservations` — create one. Returns 201
//!   with `{ "id": "<uuid>" }` on success, 409 with the existing
//!   conflicting rows on a hard-overlap collision, 400 on a
//!   malformed window.
//! - `GET  /api/calendar/reservations?resource_kind=...&resource_id=...&start=...&end=...`
//!   — list every active reservation on the given resource that
//!   intersects the window. RFC3339 datetimes; both `start` + `end`
//!   required.
//! - `DELETE /api/calendar/reservations/{id}?actor=...` — soft-cancel.
//!   Idempotent. 404 if the id doesn't exist.
//! - `POST /api/calendar/cancel-by-reason` — cascade cancel by
//!   `(reason_kind, reason_ref_id)`. Body: `{kind, ref_id, actor}`.
//!   Returns `{ "cancelled": <n> }`.
//! - `POST /api/calendar/business-calendars/batch[?mode=insert-if-absent|take]`
//!   — publish
//!   business calendars, insert-if-absent by code (design e187198f: the
//!   instance is the truth; `take` replaces a held code wholesale).
//!   Body: `Vec<BusinessCalendar>`. Operator-gated
//!   (with the `x-sim-origin` bypass). Returns
//!   `{ received, inserted, kept: [{id, differs}], updated: [{id, changes}], unchanged }`.
//! - `GET  /api/calendar/business-calendars` — every business calendar
//!   with its closed-day set, sorted by code (the batch's own input
//!   shape: what `boss tenant export` writes the seed file from, backlog
//!   e618f3ac). Open read.
//! - `GET  /api/calendar/business-calendars/{code}` — fetch one business
//!   calendar with its full closed-day set, or 404. Open read.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use boss_core::calendar::{BusinessCalendar, ReservationId, ReservationRequest, TimeWindow};
use boss_core::job::Subject;
use boss_core::publish::ModeQuery;
use boss_policy_client::{AccessTier, CurrentUser};

use crate::port::{CalendarClient, CalendarError};

#[derive(Clone)]
pub struct CalendarApiState {
    pub calendar: Arc<dyn CalendarClient>,
    /// Domain event publisher. Optional so test setups that don't
    /// wire NATS keep working; production binaries always attach a
    /// `PgAuditWriter` so reservation events land in `audit_log`.
    pub publisher: Option<boss_core::publisher::DomainPublisher>,
    /// Authoritative clock. See `boss-clock-client`.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

pub fn router(state: CalendarApiState) -> Router {
    Router::new()
        .route("/api/calendar/health", get(health))
        .route(
            "/api/calendar/reservations",
            post(create_reservation).get(list_reservations),
        )
        .route(
            "/api/calendar/reservations/{id}",
            delete(cancel_reservation),
        )
        .route("/api/calendar/cancel-by-reason", post(cancel_by_reason))
        .route(
            "/api/calendar/business-calendars",
            get(list_business_calendars),
        )
        .route(
            "/api/calendar/business-calendars/batch",
            post(batch_business_calendars),
        )
        .route(
            "/api/calendar/business-calendars/{code}",
            get(get_business_calendar),
        )
        .with_state(state)
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> axum::Json<boss_core::startup::HealthResponse> {
    axum::Json(boss_core::startup::health_response(
        "boss-calendar-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

async fn create_reservation(
    State(state): State<CalendarApiState>,
    Json(req): Json<ReservationRequest>,
) -> Response {
    // OUTBOX (phase 2): the adapter records the reserved event (full
    // post-INSERT row state) inside the domain transaction; nothing
    // publishes post-commit and no fetch-back read is needed. The
    // stamp mints wall time; the row binds the same instant so live,
    // payload, and replay agree.
    let stamp = event_stamp(&state).await;
    match state
        .calendar
        .reserve_at(req, stamp.timestamp, &stamp)
        .await
    {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => calendar_error_response(e),
    }
}

/// Resolve the outbox event stamp for this request. Calendar write
/// handlers carry no CurrentUser extractor; the publisher's
/// `default_actor` resolves the request identity from the task-local
/// context (else `automation:calendar`), and its clock probe settles
/// `_simulated` — the same envelope the retired post-commit emits
/// carried (outbox phase 2).
async fn event_stamp(state: &CalendarApiState) -> boss_core::publisher::EventStamp {
    match &state.publisher {
        Some(p) => p.stamp_with_actor(p.default_actor()).await,
        None => boss_core::publisher::EventStamp::new(
            "calendar",
            boss_core::actor::ActorId::Automation("calendar".into()),
        ),
    }
}

#[derive(Deserialize)]
struct ListQuery {
    resource_kind: String,
    resource_id: String,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
}

async fn list_reservations(
    State(state): State<CalendarApiState>,
    Query(q): Query<ListQuery>,
) -> Response {
    // The kind is open (registry-validated); a reservation is on a
    // Subject. The `resource_kind`/`resource_id` query params are the
    // calendar's I/O label for that subject.
    let subject = Subject::new(q.resource_kind, q.resource_id);
    let window = match TimeWindow::new(q.start, q.end) {
        Ok(w) => w,
        Err(msg) => return (StatusCode::BAD_REQUEST, msg).into_response(),
    };
    match state.calendar.list(&subject, window).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => calendar_error_response(e),
    }
}

#[derive(Deserialize)]
struct CancelQuery {
    /// Audit-trail tag — recorded in the cancellation log. Defaults
    /// to "unknown" when callers forget; we don't 400 since the
    /// cancel itself is idempotent.
    #[serde(default = "default_actor")]
    actor: String,
}

fn default_actor() -> String {
    "unknown".to_string()
}

async fn cancel_reservation(
    State(state): State<CalendarApiState>,
    Path(id): Path<String>,
    Query(q): Query<CancelQuery>,
) -> Response {
    let uuid = match uuid::Uuid::parse_str(&id) {
        Ok(u) => u,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "id must be a UUID").into_response();
        }
    };
    let res_id = ReservationId::from_uuid(uuid);
    // OUTBOX (phase 2): the adapter records the cancelled event (full
    // post-cancel row state) with the flip — and only on an actual
    // flip, so a repeat cancel no longer publishes a duplicate event.
    let stamp = event_stamp(&state).await;
    match state
        .calendar
        .cancel_at(res_id, &q.actor, stamp.timestamp, &stamp)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => calendar_error_response(e),
    }
}

#[derive(Deserialize)]
struct CancelByReasonBody {
    kind: String,
    ref_id: String,
    #[serde(default = "default_actor")]
    actor: String,
}

async fn cancel_by_reason(
    State(state): State<CalendarApiState>,
    Json(body): Json<CancelByReasonBody>,
) -> Response {
    // OUTBOX (phase 2): the adapter enumerates the flipped rows via
    // UPDATE ... RETURNING and records one cancelled event per row in
    // the SAME transaction — the racy list-then-emit snapshot this
    // handler used to take around the UPDATE is gone.
    let stamp = event_stamp(&state).await;
    match state
        .calendar
        .cancel_by_reason_at(
            &body.kind,
            &body.ref_id,
            &body.actor,
            stamp.timestamp,
            &stamp,
        )
        .await
    {
        Ok(n) => Json(serde_json::json!({ "cancelled": n })).into_response(),
        Err(e) => calendar_error_response(e),
    }
}

/// Batch-upsert business calendars — the seed surface, used to load
/// the `us-banking` / `us-tax` reference calendars from JSON instead of
/// `psql -f`. Each calendar is upserted by `code`; its closed-day set is
/// replaced wholesale.
///
/// Gated to operator-tier callers, with the `x-sim-origin` bypass that
/// every seed path honors (the trusted simulator/seeder masquerades as
/// operators; its requests carry `x-sim-origin: true`, which the
/// request-context middleware scopes into `is_in_sim_chain`). Reads stay
/// open; only this write is privileged.
async fn batch_business_calendars(
    State(state): State<CalendarApiState>,
    CurrentUser(user): CurrentUser,
    Query(ModeQuery { mode }): Query<ModeQuery>,
    Json(calendars): Json<Vec<BusinessCalendar>>,
) -> Response {
    let sim = boss_core::sim_origin::is_in_sim_chain();
    let tier_ok = matches!(user.access_tier, AccessTier::Operator);
    if !(sim || tier_ok) {
        return (StatusCode::FORBIDDEN, "operator tier required").into_response();
    }

    match state
        .calendar
        .publish_business_calendars(&calendars, mode)
        .await
    {
        Ok(out) => Json(out).into_response(),
        Err(e) => calendar_error_response(e),
    }
}

/// Every business calendar, sorted by code, each with its closed-day
/// set. Open read, like the per-code GET.
async fn list_business_calendars(State(state): State<CalendarApiState>) -> Response {
    match state.calendar.list_business_calendars().await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => calendar_error_response(e),
    }
}

/// Fetch one business calendar by `code` with its full closed-day set,
/// or 404 if no such calendar exists. Open read — callers run the
/// business-day math locally via `boss_core::calendar::BusinessCalendar`.
async fn get_business_calendar(
    State(state): State<CalendarApiState>,
    Path(code): Path<String>,
) -> Response {
    match state.calendar.get_business_calendar(&code).await {
        Ok(Some(cal)) => Json(cal).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such business calendar").into_response(),
        Err(e) => calendar_error_response(e),
    }
}

fn calendar_error_response(err: CalendarError) -> Response {
    match err {
        CalendarError::Conflict { existing } => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "conflict",
                "existing": existing,
            })),
        )
            .into_response(),
        CalendarError::NotFound(id) => (
            StatusCode::NOT_FOUND,
            format!("reservation not found: {id}"),
        )
            .into_response(),
        CalendarError::Invalid(msg) => (StatusCode::BAD_REQUEST, msg).into_response(),
        CalendarError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use chrono::TimeZone;
    use tower::ServiceExt;

    use crate::in_memory::InMemoryCalendar;

    fn t(h: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 4, 27, h, 0, 0).unwrap()
    }

    fn app() -> Router {
        let cal: Arc<dyn CalendarClient> = Arc::new(InMemoryCalendar::new());
        router(CalendarApiState {
            calendar: cal,
            publisher: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        })
    }

    #[tokio::test]
    async fn health_ok() {
        let resp = app()
            .oneshot(
                Request::builder()
                    .uri("/api/calendar/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn create_then_list_round_trips() {
        let app = app();
        let req_body = serde_json::json!({
            "subject": {"subject_kind": "employee", "id": "emp-1"},
            "window": {"start": t(10), "end": t(12)},
            "reason_kind": "job-step",
            "reason_ref_id": "stp-1",
            "strength": "hard",
            "created_by": "test",
        });
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/calendar/reservations")
                    .header("content-type", "application/json")
                    .body(Body::from(req_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        // RFC3339 contains `+00:00` for UTC offset; URL-encode the
        // `+` so axum's Query extractor doesn't read it as a space.
        let start = t(9).to_rfc3339().replace('+', "%2B");
        let end = t(13).to_rfc3339().replace('+', "%2B");
        let resp = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/calendar/reservations?resource_kind=employee&resource_id=emp-1&start={start}&end={end}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&body).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["reason_ref_id"], "stp-1");
    }

    #[tokio::test]
    async fn hard_overlap_returns_409_with_existing() {
        let app = app();
        let req_a = serde_json::json!({
            "subject": {"subject_kind": "employee", "id": "emp-1"},
            "window": {"start": t(10), "end": t(12)},
            "reason_kind": "job-step",
            "reason_ref_id": "stp-1",
            "strength": "hard",
            "created_by": "test",
        });
        let _ = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/calendar/reservations")
                    .header("content-type", "application/json")
                    .body(Body::from(req_a.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        let req_b = serde_json::json!({
            "subject": {"subject_kind": "employee", "id": "emp-1"},
            "window": {"start": t(11), "end": t(13)},
            "reason_kind": "job-step",
            "reason_ref_id": "stp-2",
            "strength": "hard",
            "created_by": "test",
        });
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/calendar/reservations")
                    .header("content-type", "application/json")
                    .body(Body::from(req_b.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "conflict");
        assert_eq!(json["existing"].as_array().unwrap().len(), 1);
        assert_eq!(json["existing"][0]["reason_ref_id"], "stp-1");
    }

    // --- business-calendar batch (operator-gated) + get round-trip ---

    /// `x-boss-user` JSON for an operator-tier caller — mirrors the
    /// header the gateway injects + the seed binaries send.
    fn operator_header() -> String {
        serde_json::json!({
            "id": "automation:test-seed",
            "role": "platform-admin",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
        })
        .to_string()
    }

    fn batch_request(user_header: Option<&str>, body: serde_json::Value) -> Request<Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri("/api/calendar/business-calendars/batch")
            .header("content-type", "application/json");
        if let Some(h) = user_header {
            b = b.header("x-boss-user", h);
        }
        b.body(Body::from(body.to_string())).unwrap()
    }

    fn one_calendar(closed: &[&str]) -> serde_json::Value {
        serde_json::json!([{
            "code": "us-banking",
            "name": "US Banking",
            "weekend": [5, 6],
            "closed": closed,
        }])
    }

    async fn get_calendar(app: &Router, code: &str) -> (StatusCode, Option<serde_json::Value>) {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!("/api/calendar/business-calendars/{code}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        if status != StatusCode::OK {
            return (status, None);
        }
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, Some(serde_json::from_slice(&bytes).unwrap()))
    }

    /// `boss tenant export` writes the instance's calendars back into
    /// `seeds/business_calendars.json` (design e187198f car 3, backlog
    /// e618f3ac): until 2026-09-18 the only read was per code, so an
    /// export had no way to learn WHICH codes the instance held. The
    /// list is every calendar with its closed set, sorted by code —
    /// the batch's own input shape, so export and publish are inverses.
    #[tokio::test]
    async fn list_business_calendars_answers_every_code_sorted_in_the_batch_shape() {
        let app = app();
        let body = serde_json::json!([
            {"code": "us-tax", "name": "US Tax", "weekend": [5, 6], "closed": ["2026-04-15"]},
            {"code": "us-banking", "name": "US Banking", "weekend": [5, 6], "closed": ["2026-01-01"]},
        ]);
        let resp = app
            .clone()
            .oneshot(batch_request(Some(&operator_header()), body))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/calendar/business-calendars")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let rows: Vec<BusinessCalendar> = serde_json::from_slice(&bytes).unwrap();
        let codes: Vec<&str> = rows.iter().map(|c| c.code.as_str()).collect();
        assert_eq!(codes, ["us-banking", "us-tax"], "sorted by code");
        assert_eq!(
            rows[1]
                .closed
                .iter()
                .map(|d| d.to_string())
                .collect::<Vec<_>>(),
            ["2026-04-15"],
            "each row carries its full closed set, as the per-code GET does"
        );
    }

    #[tokio::test]
    async fn get_business_calendar_404_for_missing() {
        let (status, _) = get_calendar(&app(), "no-such-calendar").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn batch_upsert_inserts_for_operator_then_get_round_trips() {
        let app = app();
        let resp = app
            .clone()
            .oneshot(batch_request(
                Some(&operator_header()),
                one_calendar(&["2026-01-01", "2026-07-03"]),
            ))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["received"], serde_json::json!(1));
        assert_eq!(v["inserted"], serde_json::json!(1));

        let (status, cal) = get_calendar(&app, "us-banking").await;
        assert_eq!(status, StatusCode::OK);
        let cal = cal.unwrap();
        assert_eq!(cal["code"], "us-banking");
        assert_eq!(
            cal["closed"],
            serde_json::json!(["2026-01-01", "2026-07-03"])
        );
    }

    /// THE INSTANCE IS THE TRUTH (design e187198f, 2026-09-18): a
    /// re-seed of a held code KEEPS it by default and names the field
    /// that differs; only `?mode=take` replaces the closed set
    /// wholesale (no merge), naming the change from → to.
    #[tokio::test]
    async fn batch_keeps_a_held_code_by_default_and_take_replaces_the_closed_set_wholesale() {
        let app = app();
        // v1: one closed day.
        let r1 = app
            .clone()
            .oneshot(batch_request(
                Some(&operator_header()),
                one_calendar(&["2026-01-01"]),
            ))
            .await
            .unwrap();
        assert_eq!(r1.status(), StatusCode::OK);
        // v2 (same code), by default: kept, and the answer says on what.
        let r2 = app
            .clone()
            .oneshot(batch_request(
                Some(&operator_header()),
                one_calendar(&["2026-07-03", "2026-12-25"]),
            ))
            .await
            .unwrap();
        assert_eq!(r2.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(r2.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["inserted"], serde_json::json!(0));
        assert_eq!(v["kept"][0]["id"], "us-banking");
        assert_eq!(v["kept"][0]["differs"], serde_json::json!(["closed"]));
        assert_eq!(v["updated"], serde_json::json!([]));
        let (_, cal) = get_calendar(&app, "us-banking").await;
        assert_eq!(
            cal.unwrap()["closed"],
            serde_json::json!(["2026-01-01"]),
            "the instance's closed set is kept under the default"
        );

        // v2 under take: replaces, does NOT merge, and names the change.
        let r3 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/calendar/business-calendars/batch?mode=take")
                    .header("content-type", "application/json")
                    .header("x-boss-user", operator_header())
                    .body(Body::from(
                        one_calendar(&["2026-07-03", "2026-12-25"]).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r3.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(r3.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["kept"], serde_json::json!([]));
        assert_eq!(v["updated"][0]["id"], "us-banking");
        assert_eq!(v["updated"][0]["changes"][0]["field"], "closed");
        assert_eq!(
            v["updated"][0]["changes"][0]["from"],
            serde_json::json!(["2026-01-01"])
        );
        let (_, cal) = get_calendar(&app, "us-banking").await;
        assert_eq!(
            cal.unwrap()["closed"],
            serde_json::json!(["2026-07-03", "2026-12-25"]),
            "take replaces the closed set wholesale (no merge with v1)"
        );
    }

    #[tokio::test]
    async fn batch_upsert_forbidden_for_non_operator() {
        // No `x-boss-user` header → anonymous, AccessTier::User.
        let resp = app()
            .oneshot(batch_request(None, one_calendar(&["2026-01-01"])))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn batch_upsert_bypassed_by_sim_origin() {
        // Sim traffic carries `x-sim-origin: true`, scoped into
        // `is_in_sim_chain`. The router omits that middleware, so set the
        // task-local directly to exercise the bypass with an anonymous caller.
        let app = app();
        let resp = boss_core::sim_origin::with_sim_chain(
            true,
            app.clone()
                .oneshot(batch_request(None, one_calendar(&["2026-01-01"]))),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let (status, _) = get_calendar(&app, "us-banking").await;
        assert_eq!(status, StatusCode::OK);
    }
}
