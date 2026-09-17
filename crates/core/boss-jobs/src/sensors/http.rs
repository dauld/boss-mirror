//! Axum routes for sensors: the registry's list and its one write (the
//! tenant batch), and the poller's four doors — record readings, read
//! what still owes a packet, stamp the packet, mark the poll — plus the
//! retention sweep. Dispatcher handlers own no database (the census-door
//! precedent), so every write the `sensor.poll` handler makes lands
//! here, signed as the sensor's own actor.
//!
//! **Reads admit `crate::trust::can_read`** — operator machinery and
//! the auditor tier, the recorded-probe reader (`boss-sor-read
//! /api/sensors` is how the car for this surface is proved). **Writes
//! admit `crate::trust::is_trusted`** — operator tier or a trusted
//! internal sibling. Nothing here is a session's surface; the registry
//! page, when there is one, reads through the same door.
//!
//! `POST /api/sensors/batch` validates every row with the same
//! `validate_sensor` the TOML loader ran, and refuses the whole batch
//! (422, deterministic) naming the row: a registry with a half-admitted
//! tenant is worse than a refusal.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;

use boss_policy_client::CurrentUser;

use crate::trust::{can_read, is_trusted};

use super::port::{Sensors, SensorsError, sweep_retention};
use super::types::{NewReading, PollStamp, SensorBatch, validate_sensor};

pub struct SensorsApiState {
    pub repo: Arc<dyn Sensors>,
}

pub fn router(state: SensorsApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/sensors", get(list))
        .route("/api/sensors/batch", post(publish))
        .route("/api/sensors/sweep", post(sweep))
        .route("/api/sensors/{id}/readings", get(unstamped).post(record))
        .route(
            "/api/sensors/{id}/readings/{external_id}/packet",
            put(stamp),
        )
        .route("/api/sensors/{id}/polled", post(polled))
        .with_state(shared)
}

fn err_response(e: SensorsError) -> Response {
    match e {
        SensorsError::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
        SensorsError::UnknownSensor(m) => {
            (StatusCode::NOT_FOUND, format!("no sensor {m}")).into_response()
        }
        SensorsError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

async fn list(
    State(state): State<Arc<SensorsApiState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.repo.list().await {
        Ok(rows) => Json(serde_json::json!({ "data": rows, "total": rows.len() })).into_response(),
        Err(e) => err_response(e),
    }
}

async fn publish(
    State(state): State<Arc<SensorsApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<SensorBatch>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if body.tenant_id.trim().is_empty() {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            "tenant_id is required: the publishing tenant's [meta] tenant_id",
        )
            .into_response();
    }
    if let Some(why) = body.sensors.iter().find_map(|s| validate_sensor(s).err()) {
        return (StatusCode::UNPROCESSABLE_ENTITY, why).into_response();
    }
    match state.repo.publish(&body.tenant_id, &body.sensors).await {
        Ok(out) => Json(out).into_response(),
        Err(e) => err_response(e),
    }
}

#[derive(Debug, Deserialize)]
struct ReadingsQuery {
    /// The one filter the poller needs; the door answers nothing else
    /// so a full readings listing does not exist to be paged.
    #[serde(default)]
    unstamped: Option<bool>,
}

async fn unstamped(
    State(state): State<Arc<SensorsApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Query(q): Query<ReadingsQuery>,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if q.unstamped != Some(true) {
        return (
            StatusCode::BAD_REQUEST,
            "the readings door answers ?unstamped=true only — the readings that still owe a packet",
        )
            .into_response();
    }
    match state.repo.unstamped(&id).await {
        Ok(rows) => Json(serde_json::json!({ "data": rows, "total": rows.len() })).into_response(),
        Err(e) => err_response(e),
    }
}

async fn record(
    State(state): State<Arc<SensorsApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<Vec<NewReading>>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(r) = body.iter().find(|r| r.external_id.trim().is_empty()) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "a reading needs the source's own id (external_id); one observed at {} has none",
                r.observed_at
            ),
        )
            .into_response();
    }
    match state.repo.record(&id, &body).await {
        Ok(out) => Json(out).into_response(),
        Err(e) => err_response(e),
    }
}

#[derive(Debug, Deserialize)]
struct StampBody {
    packet_id: String,
}

async fn stamp(
    State(state): State<Arc<SensorsApiState>>,
    CurrentUser(user): CurrentUser,
    Path((id, external_id)): Path<(String, String)>,
    Json(body): Json<StampBody>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if body.packet_id.trim().is_empty() {
        return (StatusCode::UNPROCESSABLE_ENTITY, "packet_id is required").into_response();
    }
    match state.repo.stamp(&id, &external_id, &body.packet_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err_response(e),
    }
}

async fn polled(
    State(state): State<Arc<SensorsApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<PollStamp>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.repo.mark_polled(&id, &body).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err_response(e),
    }
}

async fn sweep(
    State(state): State<Arc<SensorsApiState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match sweep_retention(state.repo.as_ref(), boss_clock_client::wall_now()).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => err_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use boss_policy_client::{AccessTier, User};
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::sensors::InMemorySensors;

    fn header(id: &str, role: &str, tier: AccessTier) -> String {
        serde_json::to_string(&User {
            id: id.into(),
            role: role.into(),
            access_tier: tier,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: None,
        })
        .expect("the user header serializes")
    }

    fn seed() -> Option<String> {
        Some(header(
            "automation:tenant-seed",
            "platform-admin",
            AccessTier::Operator,
        ))
    }

    fn sensor_actor() -> Option<String> {
        Some(header(
            "automation:sensor:stripe-sponsorships",
            "platform-admin",
            AccessTier::Operator,
        ))
    }

    fn probe_reader() -> Option<String> {
        Some(header(
            "audit-readonly",
            "audit-readonly",
            AccessTier::Auditor,
        ))
    }

    fn guest_session() -> Option<String> {
        Some(header(
            "guest@algedonic.dev",
            "audit-readonly",
            AccessTier::User,
        ))
    }

    async fn send(
        app: Router,
        method: &str,
        path: &str,
        body: Option<Value>,
        user: Option<String>,
    ) -> (StatusCode, String) {
        let mut req = Request::builder().method(method).uri(path);
        if body.is_some() {
            req = req.header("content-type", "application/json");
        }
        if let Some(u) = user {
            req = req.header("x-boss-user", u);
        }
        let body = body.map(|b| Body::from(b.to_string())).unwrap_or_default();
        let resp = app
            .oneshot(req.body(body).expect("request builds"))
            .await
            .expect("the router answers");
        let status = resp.status();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("body collects")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn app(repo: &Arc<InMemorySensors>) -> Router {
        router(SensorsApiState {
            repo: repo.clone() as Arc<dyn Sensors>,
        })
    }

    fn batch() -> Value {
        json!({
            "tenant_id": "acme",
            "sensors": [{
                "id": "stripe-sponsorships", "source": "stripe",
                "credential": "stripe-restricted-read", "every_minutes": 15,
                "opens": "receive-a-sponsorship", "subject_kind": "custom"
            }]
        })
    }

    /// The whole mechanism in one pass: the tenant batch lands
    /// (insert-if-absent), the poller records readings, reads what is
    /// owed, stamps the packet, marks the poll — and the registry
    /// listing carries the cursor.
    #[tokio::test]
    async fn a_tenant_publishes_and_a_poll_records_stamps_and_marks() {
        let repo = Arc::new(InMemorySensors::new());
        let (status, body) = send(
            app(&repo),
            "POST",
            "/api/sensors/batch",
            Some(batch()),
            seed(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["inserted"], 1);
        let (_, body) = send(
            app(&repo),
            "POST",
            "/api/sensors/batch",
            Some(batch()),
            seed(),
        )
        .await;
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["inserted"], 0, "a second publish inserts nothing");

        let readings = json!([
            {"external_id": "ch_1", "observed_at": "2026-09-17T10:00:00Z", "payload": {"amount": 100}},
            {"external_id": "ch_2", "observed_at": "2026-09-17T10:01:00Z", "payload": {"amount": 200}}
        ]);
        let (status, body) = send(
            app(&repo),
            "POST",
            "/api/sensors/stripe-sponsorships/readings",
            Some(readings.clone()),
            sensor_actor(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let (status, body) = send(
            app(&repo),
            "POST",
            "/api/sensors/stripe-sponsorships/readings",
            Some(readings),
            sensor_actor(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let out: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(out["inserted"], 0, "a redelivery inserts nothing");

        let (status, body) = send(
            app(&repo),
            "GET",
            "/api/sensors/stripe-sponsorships/readings?unstamped=true",
            None,
            sensor_actor(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let owed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(owed["total"], 2);
        assert_eq!(owed["data"][0]["external_id"], "ch_1", "oldest first");

        let (status, body) = send(
            app(&repo),
            "PUT",
            "/api/sensors/stripe-sponsorships/readings/ch_1/packet",
            Some(json!({"packet_id": "job-1"})),
            sensor_actor(),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let (_, body) = send(
            app(&repo),
            "GET",
            "/api/sensors/stripe-sponsorships/readings?unstamped=true",
            None,
            sensor_actor(),
        )
        .await;
        let owed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(owed["total"], 1);

        let (status, body) = send(
            app(&repo),
            "POST",
            "/api/sensors/stripe-sponsorships/polled",
            Some(json!({"polled_at": "2026-09-17T10:05:00Z", "cursor_at": "2026-09-17T10:01:00Z"})),
            sensor_actor(),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        let (status, body) = send(app(&repo), "GET", "/api/sensors", None, probe_reader()).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let listing: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(listing["total"], 1);
        assert_eq!(listing["data"][0]["id"], "stripe-sponsorships");
        assert_eq!(listing["data"][0]["opens_kind"], "receive-a-sponsorship");
        assert_eq!(listing["data"][0]["cursor_at"], "2026-09-17T10:01:00Z");
        assert_eq!(listing["data"][0]["last_polled_at"], "2026-09-17T10:05:00Z");
    }

    /// A bad declaration refuses the WHOLE batch, 422, naming the row;
    /// nothing lands.
    #[tokio::test]
    async fn a_bad_declaration_refuses_the_batch_by_name() {
        let repo = Arc::new(InMemorySensors::new());
        let mut b = batch();
        b["sensors"].as_array_mut().unwrap().push(json!({
            "id": "Bad Id", "source": "stripe", "credential": "c", "every_minutes": 5,
            "opens": "k", "subject_kind": "custom"
        }));
        let (status, body) = send(app(&repo), "POST", "/api/sensors/batch", Some(b), seed()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body.contains("Bad Id"), "{body}");
        assert!(repo.list().await.unwrap().is_empty());

        let mut b = batch();
        b["tenant_id"] = json!("");
        let (status, body) = send(app(&repo), "POST", "/api/sensors/batch", Some(b), seed()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert!(body.contains("tenant_id"), "{body}");
    }

    /// The probe reader and a header-less sibling read; the gateway's
    /// guest session does not; a user-tier session writes nothing.
    #[tokio::test]
    async fn reads_admit_the_probe_reader_and_writes_are_operator_machinery() {
        let repo = Arc::new(InMemorySensors::new());
        for (user, want) in [
            (probe_reader(), StatusCode::OK),
            (None, StatusCode::OK),
            (guest_session(), StatusCode::FORBIDDEN),
        ] {
            let (status, _) = send(app(&repo), "GET", "/api/sensors", None, user.clone()).await;
            assert_eq!(status, want, "{user:?}");
        }
        let david = Some(header("emp-david", "platform-admin", AccessTier::User));
        let (status, _) = send(
            app(&repo),
            "POST",
            "/api/sensors/batch",
            Some(batch()),
            david,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = send(
            app(&repo),
            "POST",
            "/api/sensors/batch",
            Some(batch()),
            probe_reader(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "the auditor reads and cannot write"
        );
    }

    /// A reading against an undeclared sensor is a 404 that names it,
    /// never a silent row.
    #[tokio::test]
    async fn a_reading_for_an_undeclared_sensor_is_a_named_404() {
        let repo = Arc::new(InMemorySensors::new());
        let (status, body) = send(
            app(&repo),
            "POST",
            "/api/sensors/nobody/readings",
            Some(
                json!([{"external_id": "x", "observed_at": "2026-09-17T10:00:00Z", "payload": {}}]),
            ),
            sensor_actor(),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
        assert!(body.contains("nobody"), "{body}");
        let (status, body) = send(
            app(&repo),
            "GET",
            "/api/sensors/nobody/readings",
            None,
            sensor_actor(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    /// The sweep is operator machinery and reports what it did.
    #[tokio::test]
    async fn the_sweep_reports_its_cutoff_and_retention() {
        let repo = Arc::new(InMemorySensors::new());
        let (status, _) = send(
            app(&repo),
            "POST",
            "/api/sensors/sweep",
            None,
            Some(header("emp-david", "platform-admin", AccessTier::User)),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, body) = send(
            app(&repo),
            "POST",
            "/api/sensors/sweep",
            None,
            sensor_actor(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let s: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(s["deleted"], 0);
        assert_eq!(s["retention_days"], super::super::RETENTION_DAYS);
    }
}
