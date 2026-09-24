//! Axum HTTP handlers for the knowledge-base API.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use boss_assets_client::AssetsClient;
use boss_classes_client::ClassesClient;
use boss_clock_client::ClockClient;
use boss_core::primitives::ClassRef;
use boss_core::publisher::DomainPublisher;

use crate::port::{KbError, KbRepository};
use crate::types::{AssetModel, DeviceCategory, Document};

/// Shared state for knowledge-base API handlers.
pub struct KbApiState<R: KbRepository> {
    pub catalog: Arc<R>,
    pub publisher: Option<DomainPublisher>,
    pub assets_client: Arc<dyn AssetsClient>,
    /// Optional Class registry for `DeviceCategory` validation. When
    /// configured, every model write checks that the incoming
    /// category code exists under
    /// `(subject_kind='asset', member_attribute='category')` in the
    /// Class registry. When `None`, the API is permissive (matches
    /// `boss-jobs::http::check_custom_subject`).
    pub classes_client: Option<Arc<dyn ClassesClient>>,
    /// Authoritative clock — every emit stamps from here so sim
    /// mode produces sim-dated audit_log rows.
    pub clock: Arc<dyn ClockClient>,
}

/// Build the knowledge-base API router.
pub fn router<R: KbRepository + 'static>(state: KbApiState<R>) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/catalog/health", get(health))
        .route("/api/catalog/models", get(list_models::<R>))
        .route(
            "/api/catalog/models/{sku}",
            get(get_model::<R>)
                .put(update_model::<R>)
                .delete(delete_model::<R>),
        )
        .route("/api/catalog/models", post(create_model::<R>))
        .route("/api/catalog/parts", get(list_parts::<R>))
        .route("/api/catalog/documents", get(list_documents))
        .with_state(shared)
}

/// Flat list of every row in the `parts` table — independent
/// of any asset_model linkage. The brewery's PartsList page
/// (and any other tenant whose parts aren't satellites of a
/// device install base) reads from here.
async fn list_parts<R: KbRepository + 'static>(
    State(state): State<Arc<KbApiState<R>>>,
) -> Response {
    match state.catalog.all_parts().await {
        Ok(parts) => Json(parts).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Deserialize)]
struct DocumentsQuery {
    entity_kind: Option<String>,
    entity_id: Option<String>,
}

/// List generic entity-keyed documents (vendor datasheets,
/// ingredient COA scans, account agreements, system manuals).
/// Both `entity_kind` and `entity_id` are required; missing
/// either returns an empty array (the SPA KB view renders an
/// empty Documents section, no console noise). The backing
/// `documents` table is seeded per-tenant; the catalog
/// service surfaces what's there without prescribing schemas.
async fn list_documents<R: KbRepository + 'static>(
    State(state): State<Arc<KbApiState<R>>>,
    Query(q): Query<DocumentsQuery>,
) -> Response {
    let (Some(kind), Some(id)) = (q.entity_kind, q.entity_id) else {
        return Json::<Vec<serde_json::Value>>(vec![]).into_response();
    };
    match state.catalog.documents_for(&kind, &id).await {
        Ok(docs) => Json(docs).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-catalog-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

async fn list_models<R: KbRepository + 'static>(
    State(state): State<Arc<KbApiState<R>>>,
) -> Response {
    match state.catalog.all_models().await {
        Ok(models) => Json(models).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_model<R: KbRepository + 'static>(
    State(state): State<Arc<KbApiState<R>>>,
    Path(sku): Path<String>,
) -> Response {
    match state.catalog.model_by_sku(&sku).await {
        Ok(Some(model)) => Json(model).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, format!("no model with SKU {sku}")).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

/// Validate an incoming `DeviceCategory` against the Class registry.
///
/// When `classes_client` is `None`, the function is permissive — the
/// service is running without registry validation. When configured,
/// the category code must exist as an active Class under
/// `(subject_kind='asset', member_attribute='category')`.
/// `DeviceCategory` is a free-text wrapper; this gate is what makes a
/// given category string actually mean something.
///
/// Returns `Ok(())` on success, or a 400 response on rejection /
/// 503 when the registry is unreachable (fail-closed: an unreachable
/// registry shouldn't accept arbitrary category strings).
async fn check_category(
    classes_client: Option<&Arc<dyn ClassesClient>>,
    category: &DeviceCategory,
) -> Result<(), Response> {
    let Some(client) = classes_client else {
        return Ok(());
    };
    let class_ref = ClassRef::new("asset", category.as_str());
    match client.class_exists(&class_ref).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown asset category `{}` — register it as a Class first \
                 (subject_kind='asset', member_attribute='category')",
                category.as_str()
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

/// Validate each document's `kind` and `audience` against the Class
/// registry under `subject_kind='asset'` (kinds carry
/// `member_attribute='document-kind'`, audiences 'document-audience').
/// Same contract as `check_category`: permissive when no registry is
/// wired, fail-closed (503) when it's unreachable, 400 on an
/// unregistered code.
async fn check_document_kinds(
    classes_client: Option<&Arc<dyn ClassesClient>>,
    documents: &[Document],
) -> Result<(), Response> {
    let Some(client) = classes_client else {
        return Ok(());
    };
    for doc in documents {
        for (code, attribute) in [
            (doc.kind.as_str(), "document-kind"),
            (doc.audience.as_str(), "document-audience"),
        ] {
            let class_ref = ClassRef::new("asset", code);
            match client.class_exists(&class_ref).await {
                Ok(true) => {}
                Ok(false) => {
                    return Err((
                        StatusCode::BAD_REQUEST,
                        format!(
                            "unknown document {attribute} `{code}` — register it as a \
                             Class first (subject_kind='asset', \
                             member_attribute='{attribute}')"
                        ),
                    )
                        .into_response());
                }
                Err(e) => {
                    return Err((
                        StatusCode::SERVICE_UNAVAILABLE,
                        format!("classes registry unreachable: {e}"),
                    )
                        .into_response());
                }
            }
        }
    }
    Ok(())
}

async fn create_model<R: KbRepository + 'static>(
    State(state): State<Arc<KbApiState<R>>>,
    Json(model): Json<AssetModel>,
) -> Response {
    if let Err(resp) = check_category(state.classes_client.as_ref(), &model.category).await {
        return resp;
    }
    if let Err(resp) = check_document_kinds(state.classes_client.as_ref(), &model.documents).await {
        return resp;
    }
    // OUTBOX (phase 2): the adapter records kb.model.created (full
    // row state) inside the domain transaction; nothing publishes
    // post-commit.
    let stamp = event_stamp(&state).await;
    match state
        .catalog
        .create_model_at(&model, stamp.timestamp, &stamp)
        .await
    {
        Ok(sku) => (StatusCode::CREATED, Json(serde_json::json!({ "sku": sku }))).into_response(),
        Err(e) => kb_error_response(e),
    }
}

/// Resolve the outbox event stamp for this request. Kb handlers
/// carry no CurrentUser extractor; the publisher's `default_actor`
/// resolves the request identity from the task-local context (else
/// `automation:kb`), and its clock probe settles `_simulated` — the
/// same envelope the retired post-commit emits carried (outbox
/// phase 2).
async fn event_stamp<R: KbRepository>(state: &KbApiState<R>) -> boss_core::publisher::EventStamp {
    match &state.publisher {
        Some(p) => p.stamp_with_actor(p.default_actor()).await,
        None => boss_core::publisher::EventStamp::new(
            "kb",
            boss_core::actor::ActorId::Automation("kb".into()),
        ),
    }
}

async fn update_model<R: KbRepository + 'static>(
    State(state): State<Arc<KbApiState<R>>>,
    Path(sku): Path<String>,
    Json(model): Json<AssetModel>,
) -> Response {
    if let Err(resp) = check_category(state.classes_client.as_ref(), &model.category).await {
        return resp;
    }
    if let Err(resp) = check_document_kinds(state.classes_client.as_ref(), &model.documents).await {
        return resp;
    }
    let stamp = event_stamp(&state).await;
    match state
        .catalog
        .update_model_at(&sku, &model, stamp.timestamp, &stamp)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => kb_error_response(e),
    }
}

async fn delete_model<R: KbRepository + 'static>(
    State(state): State<Arc<KbApiState<R>>>,
    Path(sku): Path<String>,
) -> Response {
    // Cross-service guard: ask assets whether any active devices
    // (anything not decommissioned) still reference this SKU.
    // Fail closed on assets unreachable — a kb model delete
    // is rare and destructive, and quietly deleting without
    // verifying safety is worse than a loud 503. See the account
    // delete guard in boss-people for the same pattern.
    match state.assets_client.active_asset_count_for_sku(&sku).await {
        Ok(0) => {}
        Ok(n) => {
            return (
                StatusCode::CONFLICT,
                format!(
                    "cannot delete kb model {sku}: {n} active device(s) \
                     still reference it; decommission them first"
                ),
            )
                .into_response();
        }
        Err(boss_assets_client::AssetsClientError::Unreachable(msg)) => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                format!(
                    "cannot verify kb model {sku} is safe to delete: \
                     assets unreachable ({msg})"
                ),
            )
                .into_response();
        }
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    }

    let stamp = event_stamp(&state).await;
    match state
        .catalog
        .delete_model_at(&sku, stamp.timestamp, &stamp)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => kb_error_response(e),
    }
}

fn kb_error_response(e: KbError) -> Response {
    match e {
        KbError::NotFound(msg) => (StatusCode::NOT_FOUND, msg).into_response(),
        KbError::Conflict(msg) => (StatusCode::CONFLICT, msg).into_response(),
        KbError::BadRequest(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg).into_response(),
        KbError::Storage(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use crate::in_memory::InMemoryKb;
    use crate::types::*;

    fn test_model(sku: &str) -> AssetModel {
        AssetModel {
            sku: sku.to_string(),
            name: "Test Device".to_string(),
            manufacturer: "TestCo".to_string(),
            model_year: 2024,
            category: DeviceCategory::new("router"),
            extras: serde_json::json!({"port_count": 24}),
            physical: Physical {
                width_cm: 50.0,
                depth_cm: 50.0,
                height_cm: 100.0,
                weight_kg: 80.0,
                power_requirements: "120V".to_string(),
            },
            regulatory: Regulatory {
                clearance_id: None,
                clearance_date: None,
                regulator_device_class: 2,
            },
            commerce: Commerce {
                list_price_new_cents: 5_000_000,
                typical_refurb_price_cents: None,
                currency: "USD".to_string(),
                lead_time_days: None,
                tagline: "A test device".to_string(),
                description: "For testing only".to_string(),
                use_cases: vec!["primary-use".to_string()],
                hero_image: None,
            },
            service: ServiceProfile {
                preventive_maintenance_hours: 2.0,
                preventive_maintenance_interval_months: 6,
                calibration_interval_months: 12,
                required_skill_level: 3,
                depot_required: false,
                common_failure_modes: vec![],
                pm_checklist: vec![],
            },
            spare_parts: vec![],
            consumables: vec![],
            documents: vec![],
            end_of_support: None,
            current_firmware: None,
        }
    }

    // A AssetsClient stub for inline unit tests that don't care about
    // the delete guard path — it just reports zero active devices for
    // every sku so the guard is always a no-op.
    struct StubAssetsClient;

    #[async_trait::async_trait]
    impl boss_assets_client::AssetsClient for StubAssetsClient {
        async fn open_ticket_count_for_account(
            &self,
            _account_id: &str,
        ) -> Result<u64, boss_assets_client::AssetsClientError> {
            Ok(0)
        }
        async fn active_asset_count_for_sku(
            &self,
            _sku: &str,
        ) -> Result<u64, boss_assets_client::AssetsClientError> {
            Ok(0)
        }
    }

    fn test_app() -> Router {
        let catalog = Arc::new(InMemoryKb::new(vec![
            test_model("SKU-A"),
            test_model("SKU-B"),
        ]));
        router(KbApiState {
            catalog,
            publisher: None,
            assets_client: Arc::new(StubAssetsClient),
            classes_client: None,
            clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        })
    }

    #[tokio::test]
    async fn health_returns_ok() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/catalog/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn list_models_returns_all() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/catalog/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let models: Vec<AssetModel> = serde_json::from_slice(&body).unwrap();
        assert_eq!(models.len(), 2);
    }

    #[tokio::test]
    async fn get_model_found() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/catalog/models/SKU-A")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let model: AssetModel = serde_json::from_slice(&body).unwrap();
        assert_eq!(model.sku, "SKU-A");
    }

    #[tokio::test]
    async fn get_model_not_found() {
        let app = test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/api/catalog/models/NOPE")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    fn app_with_classes_client(classes: Arc<dyn boss_classes_client::ClassesClient>) -> Router {
        let catalog = Arc::new(InMemoryKb::new(vec![]));
        router(KbApiState {
            catalog,
            publisher: None,
            assets_client: Arc::new(StubAssetsClient),
            classes_client: Some(classes),
            clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        })
    }

    #[tokio::test]
    async fn create_model_rejected_when_category_unknown() {
        use boss_classes_client::FakeClassesClient;
        use boss_core::primitives::ClassRef;
        // Registry only knows `switch`; the test_model uses `router`
        // → 400 with the actionable error message.
        let classes = Arc::new(FakeClassesClient::with(vec![ClassRef::new(
            "asset", "switch",
        )])) as Arc<dyn boss_classes_client::ClassesClient>;
        let app = app_with_classes_client(classes);
        let model = test_model("SKU-NEW");
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/catalog/models")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&model).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(
            body.contains("router") && body.contains("subject_kind='asset'"),
            "error message must name both the rejected code and the registry shape, got: {body}"
        );
    }

    #[tokio::test]
    async fn create_model_accepted_when_category_registered() {
        use boss_classes_client::FakeClassesClient;
        let classes = Arc::new(FakeClassesClient::permissive())
            as Arc<dyn boss_classes_client::ClassesClient>;
        let app = app_with_classes_client(classes);
        let model = test_model("SKU-NEW");
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/catalog/models")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&model).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn create_model_skips_validation_when_classes_client_unset() {
        // No Class registry configured → permissive. Even an
        // obviously-junk category lands.
        let catalog = Arc::new(InMemoryKb::new(vec![]));
        let app = router(KbApiState {
            catalog,
            publisher: None,
            assets_client: Arc::new(StubAssetsClient),
            classes_client: None,
            clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        });
        let mut model = test_model("SKU-NEW");
        model.category = DeviceCategory::new("definitely-not-real-category");
        let resp = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/catalog/models")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&model).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }
}
