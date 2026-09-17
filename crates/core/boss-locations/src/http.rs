//! HTTP API for the Locations registry. Reads are open; the one
//! write — `POST /api/locations/batch` — seeds the registry from a
//! tenant's `seeds/locations.toml` (backlog 1ec8312a, 2026-09-17)
//! and is gated to operator-tier callers (with the `x-sim-origin`
//! bypass), the classes batch's shape. Authoring (edit / retire)
//! lands when the admin UI does.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use boss_core::primitives::Location;
use boss_policy_client::{AccessTier, CurrentUser};
use serde::Deserialize;
use serde_json::Value;

use crate::port::LocationRepository;

#[derive(Clone)]
pub struct LocationsApiState {
    pub locations: Arc<dyn LocationRepository>,
}

pub fn router(state: LocationsApiState) -> Router {
    Router::new()
        .route("/api/locations/health", get(health))
        .route("/api/locations", get(list_locations))
        .route("/api/locations/batch", post(batch_upsert))
        .route("/api/locations/{id}", get(get_location))
        .route("/api/locations/{id}/exists", get(location_exists))
        .route("/api/locations/{id}/children", get(children_of))
        .with_state(state)
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

/// Standard health probe — see boss-classes/src/http.rs for context.
async fn health() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({
        "status": "ok",
        "service": "boss-locations-api",
        "storage": STORAGE,
    }))
}

#[derive(Deserialize)]
struct ListQuery {
    /// Filter by Class registry kind code. If absent, returns
    /// roots (Locations with no parent).
    kind: Option<String>,
    /// Filter to direct children of this Location id. If absent
    /// and `kind` is absent, returns roots.
    parent_id: Option<String>,
}

async fn list_locations(
    State(state): State<LocationsApiState>,
    Query(q): Query<ListQuery>,
) -> Response {
    let result = match (q.kind.as_deref(), q.parent_id.as_deref()) {
        (Some(k), None) => state.locations.list_for_kind(k).await,
        (None, parent) => state.locations.children_of(parent).await,
        (Some(_), Some(_)) => {
            return (
                StatusCode::BAD_REQUEST,
                "use either `kind` or `parent_id`, not both",
            )
                .into_response();
        }
    };
    match result {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_location(State(state): State<LocationsApiState>, Path(id): Path<String>) -> Response {
    match state.locations.get(&id).await {
        Ok(Some(l)) => Json(l).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "no such location").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn location_exists(
    State(state): State<LocationsApiState>,
    Path(id): Path<String>,
) -> Response {
    match state.locations.exists_active(&id).await {
        Ok(b) => Json(serde_json::json!({ "exists": b })).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn children_of(State(state): State<LocationsApiState>, Path(id): Path<String>) -> Response {
    match state.locations.children_of(Some(&id)).await {
        Ok(rows) => Json(rows).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// One row in a `POST /api/locations/batch` body. Mirrors the
/// `locations` table's authorable columns; `retired_at` /
/// `created_at` / `updated_at` are the table's own (seeded rows
/// arrive active). Optional fields default, so a minimal row is
/// `{"id","name","kind","timezone"}` — the four NOT NULL columns.
///
/// Public because `boss tenant check` parses a tenant's
/// `seeds/locations.toml` rows with THIS type (backlog 1ec8312a,
/// 2026-09-17), so the check and the door cannot disagree about a
/// row — the same rule the classes door's `ClassInput` follows.
#[derive(Debug, Deserialize)]
pub struct LocationInput {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub timezone: String,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub latitude: Option<f64>,
    #[serde(default)]
    pub longitude: Option<f64>,
    #[serde(default)]
    pub address: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default = "empty_object")]
    pub metadata: Value,
}

fn empty_object() -> Value {
    serde_json::json!({})
}

impl From<LocationInput> for Location {
    fn from(i: LocationInput) -> Self {
        Location {
            id: i.id,
            name: i.name,
            kind: i.kind,
            parent_id: i.parent_id,
            timezone: i.timezone,
            latitude: i.latitude,
            longitude: i.longitude,
            address: i.address,
            account_id: i.account_id,
            metadata: i.metadata,
            retired_at: None,
        }
    }
}

/// Batch-insert Location rows, insert-if-absent by id — the single
/// write surface, the door a tenant's `seeds/locations.toml` goes
/// through. Before it (backlog 1ec8312a, measured 2026-09-16) the
/// only rows were the schema's, so the people door refused an
/// employee at any site a tenant declared. Gated like the classes
/// batch: operator tier, or the `x-sim-origin` chain the engines'
/// seed paths carry. Reads stay open.
async fn batch_upsert(
    State(state): State<LocationsApiState>,
    CurrentUser(user): CurrentUser,
    Json(rows): Json<Vec<LocationInput>>,
) -> Response {
    let sim = boss_core::sim_origin::is_in_sim_chain();
    let tier_ok = matches!(user.access_tier, AccessTier::Operator);
    if !(sim || tier_ok) {
        return (StatusCode::FORBIDDEN, "operator tier required").into_response();
    }
    let locations: Vec<Location> = rows.into_iter().map(Into::into).collect();
    // The fact each inserted row leaves is stamped with the actor the
    // request signed with; a sim-chain caller with no identity is
    // this service's own automation, never anonymous. Publisher-less
    // stamp (the classes door's shape): the adapter stages the event
    // on the outbox inside the insert's transaction and the relay
    // moves it on, so this service needs no bus of its own.
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("locations".into()));
    let stamp = boss_core::publisher::EventStamp::new("locations", actor);
    match state.locations.batch_upsert(&locations, &stamp).await {
        Ok(inserted) => Json(serde_json::json!({
            "received": locations.len(),
            "inserted": inserted,
        }))
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory::InMemoryLocations;
    use axum::body::to_bytes;
    use axum::http::Request;
    use boss_core::primitives::Location;
    use serde_json::{Value, json};
    use tower::ServiceExt;

    fn loc(id: &str, name: &str, kind: &str, parent_id: Option<&str>) -> Location {
        Location {
            id: id.into(),
            name: name.into(),
            kind: kind.into(),
            parent_id: parent_id.map(String::from),
            timezone: "America/Los_Angeles".into(),
            latitude: None,
            longitude: None,
            address: None,
            account_id: None,
            metadata: json!({}),
            retired_at: None,
        }
    }

    fn build_app(rows: Vec<Location>) -> Router {
        let state = LocationsApiState {
            locations: Arc::new(InMemoryLocations::new(rows)),
        };
        router(state)
    }

    #[tokio::test]
    async fn list_by_kind_returns_filtered_rows() {
        let app = build_app(vec![
            loc("loc-hq", "HQ", "hq", None),
            loc("loc-mission", "Mission", "storefront", None),
            loc("loc-bay", "Bay", "storefront", None),
        ]);
        let req = Request::builder()
            .uri("/api/locations?kind=storefront")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v.as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn list_with_no_args_returns_roots() {
        let app = build_app(vec![
            loc("loc-hq", "HQ", "hq", None),
            loc("loc-zone-a", "Zone A", "warehouse-zone", Some("loc-hq")),
        ]);
        let req = Request::builder()
            .uri("/api/locations")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        let arr = v.as_array().unwrap();
        assert_eq!(arr.len(), 1, "only loc-hq is a root");
        assert_eq!(arr[0]["id"], "loc-hq");
    }

    #[tokio::test]
    async fn list_rejects_kind_and_parent_id_together() {
        let app = build_app(vec![]);
        let req = Request::builder()
            .uri("/api/locations?kind=hq&parent_id=loc-hq")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn get_returns_404_for_missing_location() {
        let app = build_app(vec![loc("loc-hq", "HQ", "hq", None)]);
        let req = Request::builder()
            .uri("/api/locations/loc-bogus")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn exists_returns_boolean_envelope() {
        let app = build_app(vec![loc("loc-hq", "HQ", "hq", None)]);
        let req = Request::builder()
            .uri("/api/locations/loc-hq/exists")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["exists"], json!(true));
    }

    #[tokio::test]
    async fn children_endpoint_returns_direct_kids() {
        let app = build_app(vec![
            loc("loc-hq", "HQ", "hq", None),
            loc("loc-zone-a", "Zone A", "warehouse-zone", Some("loc-hq")),
            loc("loc-zone-b", "Zone B", "warehouse-zone", Some("loc-hq")),
            loc("loc-bin-a01", "A01", "warehouse-zone", Some("loc-zone-a")),
        ]);
        let req = Request::builder()
            .uri("/api/locations/loc-hq/children")
            .body(axum::body::Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        let arr = v.as_array().unwrap();
        let ids: Vec<&str> = arr.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(
            ids,
            vec!["loc-zone-a", "loc-zone-b"],
            "only direct children of loc-hq"
        );
    }

    // ------------------------------------------------------------------
    // POST /api/locations/batch — the one write (backlog 1ec8312a,
    // 2026-09-17): the tenant's `seeds/locations.toml` through the
    // classes batch shape.
    // ------------------------------------------------------------------

    /// `x-boss-user` JSON for an operator-tier caller — the header
    /// `boss tenant publish` signs with.
    fn operator_header() -> String {
        json!({
            "id": "automation:tenant-seed",
            "role": "platform-admin",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
        })
        .to_string()
    }

    fn batch_request(user_header: Option<&str>, body: Value) -> Request<axum::body::Body> {
        let mut b = Request::builder()
            .method("POST")
            .uri("/api/locations/batch")
            .header("content-type", "application/json");
        if let Some(h) = user_header {
            b = b.header("x-boss-user", h);
        }
        b.body(axum::body::Body::from(body.to_string())).unwrap()
    }

    fn hq_rows() -> Value {
        json!([
            {"id": "loc-t-hq", "name": "HQ (remote)", "kind": "office", "timezone": "America/Los_Angeles"},
            {"id": "loc-t-lab", "name": "Lab", "kind": "office", "timezone": "UTC",
             "parent_id": "loc-t-hq", "metadata": {"floor": 2}}
        ])
    }

    #[tokio::test]
    async fn batch_inserts_rows_for_operator_and_a_minimal_row_needs_four_fields() {
        let repo = Arc::new(InMemoryLocations::new(vec![]));
        let app = router(LocationsApiState {
            locations: repo.clone(),
        });
        let resp = app
            .oneshot(batch_request(Some(&operator_header()), hq_rows()))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["received"], json!(2));
        assert_eq!(v["inserted"], json!(2));
        let lab = repo.get("loc-t-lab").await.unwrap().expect("lab landed");
        assert_eq!(lab.parent_id.as_deref(), Some("loc-t-hq"));
        assert_eq!(lab.metadata, json!({"floor": 2}));
        assert!(lab.retired_at.is_none(), "seeded rows arrive active");
        let hq = repo.get("loc-t-hq").await.unwrap().expect("hq landed");
        assert_eq!(
            hq.metadata,
            json!({}),
            "metadata defaults to an empty object"
        );
    }

    #[tokio::test]
    async fn batch_is_idempotent_on_conflict() {
        let repo = Arc::new(InMemoryLocations::new(vec![loc(
            "loc-t-hq", "HQ", "office", None,
        )]));
        let app = router(LocationsApiState {
            locations: repo.clone(),
        });
        let resp = app
            .oneshot(batch_request(Some(&operator_header()), hq_rows()))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["received"], json!(2));
        assert_eq!(v["inserted"], json!(1), "the existing id is left untouched");
        assert_eq!(repo.get("loc-t-hq").await.unwrap().unwrap().name, "HQ");
    }

    /// The fact a declaration leaves (backlog d9409039, 2026-09-17):
    /// one `location.declared` per row INSERTED — the row as inserted
    /// plus `declared_by`, the actor the request signed with — and
    /// nothing for the row the registry already held, nothing for the
    /// batch.
    #[tokio::test]
    async fn batch_records_one_declared_event_per_inserted_row_and_none_for_a_kept_row() {
        let repo = Arc::new(InMemoryLocations::new(vec![loc(
            "loc-t-hq", "HQ", "office", None,
        )]));
        let app = router(LocationsApiState {
            locations: repo.clone(),
        });
        let mut rows = hq_rows();
        rows.as_array_mut().unwrap().push(json!(
            {"id": "loc-t-yard", "name": "Yard", "kind": "office", "timezone": "UTC"}
        ));
        let resp = app
            .oneshot(batch_request(Some(&operator_header()), rows))
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let events = repo.recorded_events();
        assert_eq!(
            events.len(),
            2,
            "one event per inserted row, none for the kept loc-t-hq"
        );
        assert!(
            events
                .iter()
                .all(|e| e.kind == crate::port::LOCATION_DECLARED),
            "{events:?}"
        );
        assert!(events.iter().all(|e| e.source == "locations"), "{events:?}");
        let ids: Vec<&str> = events
            .iter()
            .map(|e| e.payload["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["loc-t-lab", "loc-t-yard"]);
        assert_eq!(events[0].payload["parent_id"], json!("loc-t-hq"));
        assert_eq!(events[0].payload["timezone"], json!("UTC"));
        assert_eq!(events[0].payload["metadata"], json!({"floor": 2}));
        assert_eq!(
            events[0].payload["declared_by"],
            json!("automation:tenant-seed"),
            "the actor the request signed with, named on the fact"
        );
        assert_eq!(
            events[0].payload["_actor"], events[0].payload["declared_by"],
            "declared_by and the stamp's actor are one value"
        );
    }

    #[tokio::test]
    async fn batch_is_forbidden_for_non_operator() {
        // No header → anonymous, AccessTier::User. Reads stay open;
        // the write is the one privileged door.
        let app = build_app(vec![]);
        let resp = app.oneshot(batch_request(None, hq_rows())).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn batch_is_bypassed_by_sim_origin() {
        // The engines' seed paths carry `x-sim-origin: true`, which
        // the request-context middleware scopes into `is_in_sim_chain`;
        // the router under test omits that middleware, so the
        // task-local is set directly.
        let repo = Arc::new(InMemoryLocations::new(vec![]));
        let app = router(LocationsApiState {
            locations: repo.clone(),
        });
        let resp = boss_core::sim_origin::with_sim_chain(
            true,
            app.oneshot(batch_request(None, hq_rows())),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(repo.exists_active("loc-t-hq").await.unwrap());
    }

    #[tokio::test]
    async fn a_row_missing_a_not_null_column_is_refused_by_name() {
        let app = build_app(vec![]);
        let resp = app
            .oneshot(batch_request(
                Some(&operator_header()),
                json!([{"id": "loc-t-hq", "name": "HQ", "kind": "office"}]),
            ))
            .await
            .unwrap();
        // axum's Json rejection: 422 naming the missing field.
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let body = to_bytes(resp.into_body(), 64 * 1024).await.unwrap();
        assert!(
            String::from_utf8_lossy(&body).contains("timezone"),
            "{}",
            String::from_utf8_lossy(&body)
        );
    }
}
