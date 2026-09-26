//! Axum HTTP handlers for the assets API.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use boss_catalog_client::CatalogClient;
use boss_classes_client::ClassesClient;
use boss_core::port::EventBus;
use boss_core::primitives::ClassRef;
use boss_core::publisher::DomainPublisher;
use boss_inventory_client::InventoryClient;

use boss_people_client::{PeopleClient, PeopleClientError};
use boss_policy::{Action, Decision, Resource};
use boss_policy_client::{CurrentUser, PolicyClient};
use serde::{Deserialize, Serialize};

use crate::asset_insights::build_asset_insights;
use crate::bridge::asset_event_to_core;
use crate::port::AssetsRepository;
use crate::sse::SseHub;
use crate::types::{AssetCurrentState, AssetEvent, AssetEventKind, AssetId};

const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 1000;

#[derive(Deserialize)]
struct Pagination {
    limit: Option<i64>,
    offset: Option<i64>,
    /// Scope the page to one account. Powers the unified
    /// account-detail view's Devices panel.
    /// Account-scoped filter: matches assets whose HOLDER is this
    /// account (holder_kind='account'). Location-held equipment never
    /// matches — by design (Q5).
    account_id: Option<String>,
}

impl Pagination {
    fn limit(&self) -> i64 {
        self.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
    }

    fn offset(&self) -> i64 {
        self.offset.unwrap_or(0).max(0)
    }
}

#[derive(Serialize)]
struct PaginatedResponse<T: Serialize> {
    data: Vec<T>,
    total: i64,
    limit: i64,
    offset: i64,
}

/// Cross-service clients the device-insights projection fans out to.
/// Bundled so the binary constructs once and passes a single Option;
/// tests that don't exercise the endpoint leave `insights_clients =
/// None` and the route responds 503.
pub struct InsightsClients {
    pub catalog: Arc<dyn CatalogClient>,
    pub inventory: Arc<dyn InventoryClient>,
}

pub struct AssetsApiState<R: AssetsRepository, B: EventBus> {
    pub assets: Arc<R>,
    pub bus: Arc<B>,
    pub publisher: DomainPublisher,
    pub people_client: Arc<dyn PeopleClient>,
    /// Optional Class registry for asset-event taxonomy validation.
    /// When configured, every ingested event has its taxonomy field(s)
    /// — `source` on `Received`, `coverage` on `WarrantyStarted`,
    /// `condition` on `Sold` — checked against the active Class set
    /// under `(subject_kind='asset')`. When `None`, the gate is
    /// permissive (test path; production always wires it, see the
    /// binary). Kept `Option` so tests pass `None`.
    pub classes_client: Option<Arc<dyn ClassesClient>>,
    pub hub: SseHub,
    /// Row-level authorization. None in tests that don't exercise
    /// the policy path — those handlers skip the gate and allow the
    /// request, preserving the existing test surface.
    pub policy: Option<Arc<dyn PolicyClient>>,
    pub insights_clients: Option<InsightsClients>,
    /// Authoritative clock. See `boss-clock-client`.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

pub fn router<R: AssetsRepository + 'static, B: EventBus + 'static>(
    state: AssetsApiState<R, B>,
) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/assets/health", get(health))
        .route("/api/assets/summary", get(assets_summary::<R, B>))
        .route("/api/assets", get(list_assets::<R, B>))
        .route("/api/assets/ids", get(list_asset_ids::<R, B>))
        .route(
            "/api/assets/accounts/{account_id}/open-tickets/count",
            get(open_ticket_count_for_account::<R, B>),
        )
        .route(
            "/api/assets/kb/{sku}/active-count",
            get(active_asset_count_for_sku::<R, B>),
        )
        .route(
            "/api/assets/{asset_id}/insights",
            get(asset_insights::<R, B>),
        )
        .route("/api/assets/{asset_id}", get(get_asset::<R, B>))
        .route("/api/assets/events", post(post_event::<R, B>))
        .route("/api/assets/events/batch", post(batch_events::<R, B>))
        .route("/api/assets/events/stream", get(event_stream::<R, B>))
        .with_state(shared)
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-assets-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

async fn assets_summary<R: AssetsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<AssetsApiState<R, B>>>,
) -> Response {
    // Source `today` from ClockClient so the warranty-expiring-30d
    // count respects sim-time.
    let today = state.clock.now().await.now.date_naive();
    match state.assets.assets_summary(today).await {
        Ok(summary) => Json(summary).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn list_asset_ids<R: AssetsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<AssetsApiState<R, B>>>,
    Query(pagination): Query<Pagination>,
) -> Response {
    let limit = pagination.limit();
    let offset = pagination.offset();
    match state.assets.list_asset_ids(limit, offset).await {
        Ok((data, total)) => Json(PaginatedResponse {
            data,
            total,
            limit,
            offset,
        })
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn list_assets<R: AssetsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<AssetsApiState<R, B>>>,
    Query(pagination): Query<Pagination>,
) -> Response {
    let limit = pagination.limit();
    let offset = pagination.offset();
    match state
        .assets
        .list_assets(limit, offset, pagination.account_id.as_deref())
        .await
    {
        Ok((data, total)) => Json(PaginatedResponse {
            data,
            total,
            limit,
            offset,
        })
        .into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Serialize)]
struct AssetResponse {
    current_state: Option<AssetCurrentState>,
    events: Vec<AssetEvent>,
}

#[derive(Serialize)]
struct OpenTicketCountResponse {
    account_id: String,
    count: u64,
}

async fn open_ticket_count_for_account<R: AssetsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<AssetsApiState<R, B>>>,
    Path(account_id): Path<String>,
) -> Response {
    match state
        .assets
        .open_ticket_count_for_account(&account_id)
        .await
    {
        Ok(count) => Json(OpenTicketCountResponse { account_id, count }).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Serialize)]
struct ActiveAssetCountResponse {
    sku: String,
    count: u64,
}

async fn active_asset_count_for_sku<R: AssetsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<AssetsApiState<R, B>>>,
    Path(sku): Path<String>,
) -> Response {
    match state.assets.active_asset_count_for_sku(&sku).await {
        Ok(count) => Json(ActiveAssetCountResponse { sku, count }).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// `GET /api/assets/{asset_id}/insights` — backs the SR 360 + Device
/// 360 Insights sections. Fans out to catalog (failure modes + spare
/// parts BOM) and inventory (stock for high-usage parts). See
/// [`crate::asset_insights`] for the shape and aggregator rules.
async fn asset_insights<R: AssetsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<AssetsApiState<R, B>>>,
    Path(serial): Path<String>,
) -> Response {
    let Some(clients) = &state.insights_clients else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "device-insights requires catalog/inventory clients — not configured",
        )
            .into_response();
    };

    let asset_id = AssetId::new(serial.clone());
    let current_state = match state.assets.current_state(&asset_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return (StatusCode::NOT_FOUND, format!("no asset with id {serial}")).into_response();
        }
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };

    // Fetch the catalog model first so we know which SKUs to ask
    // inventory about. An unidentified asset (no sku yet — it was
    // Registered but not Identified) has no catalog model, so skip
    // the catalog call entirely; the model-derived panels render empty
    // until it's identified.
    let model_res = match &current_state.sku {
        Some(s) => clients.catalog.model_summary_by_sku(s).await,
        None => Ok(None),
    };
    let model = match model_res {
        Ok(m) => m,
        Err(e) => return (StatusCode::BAD_GATEWAY, format!("catalog: {e}")).into_response(),
    };

    let stock = if let Some(m) = &model {
        let skus: Vec<String> = m
            .spare_parts
            .iter()
            .filter(|p| p.high_usage)
            .map(|p| p.part_sku.clone())
            .collect();
        match clients.inventory.parts_stock_by_skus(&skus).await {
            Ok(s) => s,
            Err(e) => {
                return (StatusCode::BAD_GATEWAY, format!("inventory: {e}")).into_response();
            }
        }
    } else {
        Vec::new()
    };

    let insights = build_asset_insights(
        serial,
        model,
        stock,
        boss_clock_client::now_from(&state.clock).await,
    );
    Json(insights).into_response()
}

async fn get_asset<R: AssetsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<AssetsApiState<R, B>>>,
    Path(asset_id): Path<String>,
) -> Response {
    let asset_id = AssetId::new(asset_id);
    let events = match state.assets.events_for(&asset_id).await {
        Ok(e) => e,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    if events.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            format!("no asset with id {}", asset_id.as_str()),
        )
            .into_response();
    }
    let current_state = match state.assets.current_state(&asset_id).await {
        Ok(s) => s,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    Json(AssetResponse {
        current_state,
        events,
    })
    .into_response()
}

async fn validate_actor_ids(
    people: &dyn PeopleClient,
    events: &[AssetEvent],
) -> Result<(), Response> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for event in events {
        // Only Human actors need existence-checking against the
        // employees table. The machine classes — Automation and Agent
        // — are named but not FK-backed (automation slugs and
        // `<mode>:<model>` agent ids are both free-form for now), so
        // they skip the check rather than 422 as an unknown employee.
        let boss_core::actor::ActorId::Human(id) = &event.actor_id else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        let id = id.as_str();
        {
            match people.employee_exists(id).await {
                Ok(true) => {}
                Ok(false) => {
                    return Err((
                        StatusCode::UNPROCESSABLE_ENTITY,
                        Json(serde_json::json!({
                            "error": "unknown actor",
                            "actor_id": id,
                        })),
                    )
                        .into_response());
                }
                Err(PeopleClientError::Unreachable(msg)) => {
                    return Err((
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({
                            "error": "people service unreachable",
                            "detail": msg,
                        })),
                    )
                        .into_response());
                }
                Err(e) => {
                    return Err((
                        StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({
                            "error": "actor existence check failed",
                            "detail": e.to_string(),
                        })),
                    )
                        .into_response());
                }
            }
        }
    }
    Ok(())
}

/// Validate a single asset taxonomy code against the Class registry.
///
/// `attribute` names the event field the code came from (e.g.
/// `"source"`) so the 400 message points the caller at the right
/// place. All three asset taxonomies key on `subject_kind='asset'`;
/// the attribute is metadata-only narration, not part of the
/// `ClassRef` key (`ClassRef` keys on `(subject_kind, code)`).
async fn check_asset_class(
    client: &Arc<dyn ClassesClient>,
    attribute: &str,
    code: &str,
) -> Result<(), Response> {
    let class_ref = ClassRef::new("asset", code);
    match client.class_exists(&class_ref).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown asset {attribute} `{code}` — register it as a Class first \
                 (subject_kind='asset')"
            ),
        )
            .into_response()),
        Err(e) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            format!("classes registry unreachable: {e}"),
        )
            .into_response()),
    }
}

/// Gate an inbound `AssetEvent` against the Class registry before it is
/// persisted. The three asset taxonomies — `IntakeSource`,
/// `WarrantyCoverage`, `AssetCondition` — live inside `AssetEvent`
/// payload variants (JSONB), not flat columns, so there is no schema
/// CHECK to backstop them; this is the only enforcement point.
///
/// Inspects the event's variant and validates whichever taxonomy
/// field(s) it carries:
/// - `Received { source, .. }`   → `source`
/// - `WarrantyStarted { coverage, .. }` → `coverage`
/// - `Sold { condition, .. }`    → `condition`
///
/// Every other variant carries no taxonomy and passes straight
/// through. Permissive when `classes_client` is `None` (test path);
/// fail-closed 503 when the registry is unreachable; 400 on an
/// unregistered code. Called at the top of both `post_event` and
/// `batch_events` — there is no unvalidated ingest path.
async fn check_asset_classes(
    classes_client: Option<&Arc<dyn ClassesClient>>,
    event: &AssetEvent,
) -> Result<(), Response> {
    let Some(client) = classes_client else {
        return Ok(());
    };
    match &event.kind {
        AssetEventKind::Received { source, .. } => {
            check_asset_class(client, "source", source.as_str()).await
        }
        AssetEventKind::WarrantyStarted { coverage, .. } => {
            check_asset_class(client, "coverage", coverage.as_str()).await
        }
        AssetEventKind::Sold { condition, .. } => {
            check_asset_class(client, "condition", condition.as_str()).await
        }
        _ => Ok(()),
    }
}

/// Policy: appending an asset event mutates an asset row, so both
/// event doors ask Update on `Resource::asset()` — one question, asked
/// one way. Until backlog 6c0f6547 (2026-09-26) only the single-event
/// door asked; its batch twin asked nothing. A Deny is 403; a policy
/// service that cannot answer refuses with the client error's own
/// response (503 + Retry-After), never a pass. `None` allows: the
/// test path the unit surface relies on; the binary wires a client.
async fn require_asset_update<R: AssetsRepository, B: EventBus>(
    state: &AssetsApiState<R, B>,
    user: &boss_policy_client::User,
) -> Result<(), Response> {
    let Some(policy) = state.policy.as_ref() else {
        return Ok(());
    };
    match policy.check(user, Action::Update, Resource::asset()).await {
        Ok(Decision::Allow { .. }) => Ok(()),
        Ok(Decision::Deny { reason }) => Err((StatusCode::FORBIDDEN, reason).into_response()),
        Err(e) => Err(e.into_response()),
    }
}

async fn post_event<R: AssetsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<AssetsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(event): Json<AssetEvent>,
) -> Response {
    // Class-registry gate, before any persistence. The event's
    // taxonomy field(s) (source/coverage/condition) live in the JSONB
    // payload with no schema CHECK — this is the only enforcement
    // point. Permissive when no registry is wired (test path).
    if let Err(resp) = check_asset_classes(state.classes_client.as_ref(), &event).await {
        return resp;
    }
    if let Err(resp) = require_asset_update(&state, &user).await {
        return resp;
    }
    if let Err(resp) =
        validate_actor_ids(state.people_client.as_ref(), std::slice::from_ref(&event)).await
    {
        return resp;
    }
    if let Err(e) = state.assets.append(event.clone()).await {
        return (StatusCode::CONFLICT, e.to_string()).into_response();
    }
    // The record stamp is wall time — sim time is retired from the
    // record (David, 2026-08-22, packet a7a4cae5). The AssetEvent's
    // `ts` stays in the payload as the business date, which is where
    // the sim timeline lives.
    let event_time = boss_clock_client::wall_now();
    let core_event = asset_event_to_core(&event, event_time);
    state.publisher.publish(core_event).await;
    (StatusCode::CREATED, Json(serde_json::json!({"ok": true}))).into_response()
}

async fn batch_events<R: AssetsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<AssetsApiState<R, B>>>,
    CurrentUser(user): CurrentUser,
    Json(events): Json<Vec<AssetEvent>>,
) -> Response {
    // Class-registry gate, per event, before any persistence. The
    // batch is all-or-nothing (`batch_append`), so one unregistered
    // taxonomy code rejects the whole batch — there is no unvalidated
    // ingest path. Permissive when no registry is wired (test path).
    for event in &events {
        if let Err(resp) = check_asset_classes(state.classes_client.as_ref(), event).await {
            return resp;
        }
    }
    // The single-event door's question, once for the whole batch, in
    // the same place in the order (after the registry gate, before the
    // actor check and any write).
    if let Err(resp) = require_asset_update(&state, &user).await {
        return resp;
    }
    if let Err(resp) = validate_actor_ids(state.people_client.as_ref(), &events).await {
        return resp;
    }
    let to_publish: Vec<AssetEvent> = events.clone();
    let stats = match state.assets.batch_append(events).await {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    // One wall-clock record stamp for the whole batch (see
    // post_event); the client's per-event `ts` stays payload data.
    let event_time = boss_clock_client::wall_now();
    let core_events: Vec<_> = to_publish
        .iter()
        .map(|e| asset_event_to_core(e, event_time))
        .collect();
    state.publisher.publish_batch(core_events).await;
    let inserted = stats.inserted;
    let duplicates = stats.duplicates;
    (
        StatusCode::OK,
        Json(serde_json::json!({"inserted": inserted, "duplicates": duplicates})),
    )
        .into_response()
}

async fn event_stream<R: AssetsRepository + 'static, B: EventBus + 'static>(
    State(state): State<Arc<AssetsApiState<R, B>>>,
) -> Response {
    state.hub.sse_response().into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory::InMemoryAssets;
    use crate::types::{AssetEventId, AssetEventKind, IntakeSource};
    use axum::body::Body;
    use axum::http::Request;
    use boss_classes_client::FakeClassesClient;
    use boss_people_client::{AlwaysExistsPeople, FakePeopleClient};
    use boss_testing::RecordingEventBus;
    use chrono::NaiveDate;
    use tower::ServiceExt;

    /// Build an assets router with the given (optional) Class registry.
    /// No policy (None → allow), permissive people client, recording
    /// bus, wall clock. Exercises the `check_asset_classes` ingest gate.
    fn app_with_classes(classes_client: Option<Arc<dyn ClassesClient>>) -> Router {
        app_with(classes_client, Arc::new(AlwaysExistsPeople))
    }

    fn app_with(
        classes_client: Option<Arc<dyn ClassesClient>>,
        people_client: Arc<dyn PeopleClient>,
    ) -> Router {
        app_over(classes_client, people_client, None).0
    }

    /// The router plus the store behind it, so a test can read what a
    /// request left in it.
    fn app_over(
        classes_client: Option<Arc<dyn ClassesClient>>,
        people_client: Arc<dyn PeopleClient>,
        policy: Option<Arc<dyn PolicyClient>>,
    ) -> (Router, Arc<InMemoryAssets>) {
        let assets = Arc::new(InMemoryAssets::new());
        let bus = RecordingEventBus::new();
        let publisher =
            DomainPublisher::new(bus.clone() as Arc<dyn boss_core::port::EventBus>, "assets");
        let state = AssetsApiState {
            assets: assets.clone(),
            bus,
            publisher,
            people_client,
            classes_client,
            hub: SseHub::new(),
            policy,
            insights_clients: None,
            clock: Arc::new(boss_clock_client::WallClockClient),
        };
        (router(state), assets)
    }

    /// Backlog 6c0f6547 (2026-09-26): the single-event door asked
    /// policy for Update on asset and its batch twin asked nothing, so
    /// a caller refused one event at a time could append any number of
    /// them at once. The batch now asks the same question, before it
    /// writes anything.
    #[tokio::test]
    async fn a_denied_caller_cannot_append_asset_events_through_the_batch_door() {
        let (app, assets) = app_over(
            None,
            Arc::new(AlwaysExistsPeople),
            Some(Arc::new(boss_policy_client::FakePolicyClient::deny_all())),
        );
        let event = received_event_with_source("evt-batch-denied", "oem-new");
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/assets/events/batch")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&vec![event.clone()]).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        assert!(
            assets.events_for(&event.asset_id).await.unwrap().is_empty(),
            "a refused batch must append nothing"
        );
    }

    /// A `Received` event whose intake `source` is the given code.
    fn received_event_with_source(id: &str, source: &str) -> AssetEvent {
        AssetEvent {
            id: AssetEventId::new(id),
            asset_id: AssetId::new("SN-GATE-1"),
            ts: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
            actor_id: boss_core::actor::ActorId::human("emp-gate"),
            kind: AssetEventKind::Received {
                sku: None,
                source: IntakeSource::new(source),
                oem_serial: None,
            },
        }
    }

    async fn post_event_req(app: Router, event: &AssetEvent) -> Response {
        app.oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/assets/events")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(event).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn post_event_rejects_unregistered_intake_source() {
        // Registry only knows `oem-new`; an event carrying a bogus
        // source is rejected at the ingest boundary with an actionable
        // 400 naming the attribute + subject_kind.
        let classes = Arc::new(FakeClassesClient::with(vec![ClassRef::new(
            "asset", "oem-new",
        )])) as Arc<dyn ClassesClient>;
        let app = app_with_classes(Some(classes));
        let resp =
            post_event_req(app, &received_event_with_source("evt-bad", "not-a-source")).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(
            body.contains("source") && body.contains("subject_kind='asset'"),
            "error must name the attribute and the registry shape, got: {body}"
        );
    }

    #[tokio::test]
    async fn post_event_accepts_registered_intake_source() {
        let classes = Arc::new(FakeClassesClient::with(vec![ClassRef::new(
            "asset", "oem-new",
        )])) as Arc<dyn ClassesClient>;
        let app = app_with_classes(Some(classes));
        let resp = post_event_req(app, &received_event_with_source("evt-ok", "oem-new")).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn post_event_permissive_when_no_classes_client() {
        // No registry wired → the gate is a no-op; even a junk source
        // lands (the permissive path). Production always wires it.
        let app = app_with_classes(None);
        let resp = post_event_req(
            app,
            &received_event_with_source("evt-perm", "totally-bogus"),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    /// The actor guard existence-checks Humans against the employees
    /// table. An agent id (`claude:fable`) used to parse as a Human
    /// and get 422'd as an "unknown actor"; as the third CPU class it
    /// is not FK-backed and must pass, exactly like an automation.
    #[tokio::test]
    async fn agent_and_automation_actors_are_exempt_from_the_employee_guard() {
        for (id, actor, wire) in [
            (
                "evt-agent",
                boss_core::actor::ActorId::agent("claude", "fable"),
                "claude:fable",
            ),
            (
                "evt-agent-2",
                boss_core::actor::ActorId::agent("claude", "opus-5"),
                "claude:opus-5",
            ),
            (
                "evt-automation",
                boss_core::actor::ActorId::automation("asset-intake"),
                "automation:asset-intake",
            ),
        ] {
            let event = AssetEvent {
                id: AssetEventId::new(id),
                asset_id: AssetId::new("SN-AGENT-1"),
                ts: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
                actor_id: actor,
                kind: AssetEventKind::PutAway { bin: "A-1".into() },
            };
            // The guard reads what came off the wire, so pin the wire
            // form too: the agent arrives as a bare `<mode>:<model>`.
            assert_eq!(serde_json::to_value(&event).unwrap()["actor_id"], wire);
            // Empty roster: any id the guard actually looks up 422s.
            let app = app_with(None, Arc::new(FakePeopleClient::new()));
            let resp = post_event_req(app, &event).await;
            assert_eq!(resp.status(), StatusCode::CREATED, "actor {wire}");
        }
    }

    /// The guard still bites for a human id that is not on the roster.
    #[tokio::test]
    async fn an_unknown_employee_actor_is_still_rejected() {
        let app = app_with(None, Arc::new(FakePeopleClient::new()));
        let event = AssetEvent {
            id: AssetEventId::new("evt-ghost"),
            asset_id: AssetId::new("SN-AGENT-2"),
            ts: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
            actor_id: boss_core::actor::ActorId::human("emp-ghost"),
            kind: AssetEventKind::PutAway { bin: "A-1".into() },
        };
        let resp = post_event_req(app, &event).await;
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn post_event_skips_gate_for_variants_without_taxonomy() {
        // A `PutAway` event carries no taxonomy field; even a
        // gate-to-empty registry must let it through.
        let classes = Arc::new(FakeClassesClient::with(vec![])) as Arc<dyn ClassesClient>;
        let app = app_with_classes(Some(classes));
        let event = AssetEvent {
            id: AssetEventId::new("evt-putaway"),
            asset_id: AssetId::new("SN-GATE-2"),
            ts: NaiveDate::from_ymd_opt(2026, 4, 1).unwrap(),
            actor_id: boss_core::actor::ActorId::human("emp-gate"),
            kind: AssetEventKind::PutAway { bin: "A-1".into() },
        };
        let resp = post_event_req(app, &event).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
    }
}
