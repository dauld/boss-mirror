//! End-to-end proof that the StepPlugin registry HTTP surface
//! works: create → publish → retire, plus policy gating (a guest
//! with Create but not Publish is blocked from publishing).

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{
    InMemoryJobs, InMemoryStepPlugins, StepPluginRegistry, StepPluginSpec, WorkflowStatus,
};
use boss_policy_client::{AccessTier, Action, Resource, Scope, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn cto() -> User {
    User {
        id: "emp-cto".into(),
        role: "cto".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn guest() -> User {
    User {
        id: "anonymous".into(),
        role: "guest".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn user_header(u: &User) -> String {
    serde_json::to_string(u).unwrap()
}

fn draft(kind: &str) -> StepPluginSpec {
    StepPluginSpec::draft(
        kind,
        format!("Test {kind}"),
        "qa",
        format!("{kind}.js"),
        serde_json::json!({}),
    )
}

fn build_app(registry: Arc<dyn StepPluginRegistry>, policy: Arc<dyn PolicyClient>) -> Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let step_registry = Arc::new(StepRegistry::v1());
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs,
        bus,
        publisher,
        step_registry,
        policy,
        kind_registry: None,
        plugin_registry: Some(registry),
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
    };
    router(state)
}

async fn send_json(
    app: Router,
    method: &str,
    uri: &str,
    user: &User,
    body: Option<serde_json::Value>,
) -> axum::http::Response<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("x-boss-user", user_header(user));
    let body = match body {
        Some(v) => {
            builder = builder.header("content-type", "application/json");
            Body::from(serde_json::to_vec(&v).unwrap())
        }
        None => Body::empty(),
    };
    app.oneshot(builder.body(body).unwrap()).await.unwrap()
}

#[tokio::test]
async fn full_plugin_lifecycle() {
    let registry: Arc<dyn StepPluginRegistry> = Arc::new(InMemoryStepPlugins::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("cto", Action::Read, Resource::step_plugin(), Scope::All)
            .allow("cto", Action::Create, Resource::step_plugin(), Scope::All)
            .allow("cto", Action::Update, Resource::step_plugin(), Scope::All)
            .allow("cto", Action::Publish, Resource::step_plugin(), Scope::All)
            .allow("cto", Action::Retire, Resource::step_plugin(), Scope::All)
            .build(),
    );
    let app = build_app(registry, policy);

    // 1. Create draft.
    let body = serde_json::to_value(draft("emerald-inspection")).unwrap();
    let resp = send_json(
        app.clone(),
        "POST",
        "/api/jobs/step-plugins",
        &cto(),
        Some(body),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let stored: StepPluginSpec = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(stored.version, 1);
    assert_eq!(stored.status, WorkflowStatus::Draft);

    // 2. Active GET 404s (no active yet).
    let resp = send_json(
        app.clone(),
        "GET",
        "/api/jobs/step-plugins/emerald-inspection",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // 3. Publish.
    let resp = send_json(
        app.clone(),
        "POST",
        "/api/jobs/step-plugins/emerald-inspection/publish",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    // 4. Active now visible.
    let resp = send_json(
        app.clone(),
        "GET",
        "/api/jobs/step-plugins/emerald-inspection",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let active: StepPluginSpec = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(active.status, WorkflowStatus::Active);

    // 5. Retire.
    let resp = send_json(
        app.clone(),
        "POST",
        "/api/jobs/step-plugins/emerald-inspection/retire",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    // 6. Active GET 404s again after retire.
    let resp = send_json(
        app,
        "GET",
        "/api/jobs/step-plugins/emerald-inspection",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn guest_with_create_cannot_publish() {
    // Publish is independently policy-gated from Create (Q7: manager-
    // tier install with policy). A guest who can draft can't publish.
    let registry: Arc<dyn StepPluginRegistry> = Arc::new(InMemoryStepPlugins::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("guest", Action::Read, Resource::step_plugin(), Scope::All)
            .allow("guest", Action::Create, Resource::step_plugin(), Scope::All)
            .build(),
    );
    let app = build_app(registry, policy);

    // Guest drafts — allowed.
    let resp = send_json(
        app.clone(),
        "POST",
        "/api/jobs/step-plugins",
        &guest(),
        Some(serde_json::to_value(draft("guest-plugin")).unwrap()),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Guest publishes — denied.
    let resp = send_json(
        app,
        "POST",
        "/api/jobs/step-plugins/guest-plugin/publish",
        &guest(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}
