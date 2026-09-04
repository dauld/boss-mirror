//! The publish gate — an unviable WorkflowSpec can never become the
//! ACTIVE row.
//!
//! Incident 2026-08-13: an agent published a `protocol-retro`
//! Workflow whose five steps carried no terminal. `POST
//! /api/workflows` and `POST /api/workflows/{kind}/publish` both
//! accepted it silently, and the row lay latent until the next pod
//! roll — at which point `boss-jobs-api`'s boot viability lint
//! refused to start and took the cluster API + the human door down
//! for ~11 minutes. `POST /api/workflows/_validate` had been able to
//! name the exact problem the whole time; publish simply never asked
//! it.
//!
//! The rule these tests pin: a DRAFT may be saved unviable (drafts
//! are work in progress), but no path that sets `status = active`
//! may accept a spec the viability lint rejects.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::JobId;
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::events::WORKFLOW_PUBLISHED;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{
    InMemoryWorkflows, StepSpec, Terminal, WorkflowError, WorkflowRegistry, WorkflowSpec,
    WorkflowStatus,
};
use boss_jobs::step_registry::StepRegistry;
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

fn user_header(u: &User) -> String {
    serde_json::to_string(u).unwrap()
}

fn build_app(registry: Arc<dyn WorkflowRegistry>) -> Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let step_registry = Arc::new(StepRegistry::v1());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("cto", Action::Read, Resource::workflow(), Scope::All)
            .allow("cto", Action::Create, Resource::workflow(), Scope::All)
            .allow("cto", Action::Update, Resource::workflow(), Scope::All)
            .allow("cto", Action::Publish, Resource::workflow(), Scope::All)
            .allow("cto", Action::Retire, Resource::workflow(), Scope::All)
            .build(),
    );
    let state = JobsApiState {
        // Station registry not exercised here; the gate under test is the
        // Workflow publish path (the landed idiom for unused registries).
        stations: None,
        job_edges: None,
        jobs,
        bus,
        publisher,
        step_registry,
        policy,
        kind_registry: Some(registry),
        plugin_registry: None,
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

async fn json_body(resp: axum::http::Response<Body>) -> serde_json::Value {
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
}

/// The shape that took the cluster down: five chained steps, every
/// one of them ordinary work, not one of them carrying an outcome.
/// The Job could be opened and worked but could never close.
fn protocol_retro_v1() -> WorkflowSpec {
    let chain = [
        ("collect-signals", "true"),
        ("draft-findings", "steps.collect-signals.done"),
        ("circulate", "steps.draft-findings.done"),
        ("discuss", "steps.circulate.done"),
        ("record-actions", "steps.discuss.done"),
    ];
    WorkflowSpec::platform_seed(
        "protocol-retro",
        "Protocol Retro",
        "platform",
        vec!["custom".into()],
        chain
            .iter()
            .map(|(title, ready_when)| StepSpec {
                title: (*title).into(),
                kind: "task".into(),
                ready_when: (*ready_when).into(),
                // No `terminal` anywhere — this is the bug.
                ..Default::default()
            })
            .collect(),
    )
}

fn viable_spec(kind: &str) -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        kind,
        "Viable",
        "platform",
        vec!["custom".into()],
        vec![
            StepSpec {
                title: "start".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                ..Default::default()
            },
            StepSpec {
                title: "finish".into(),
                kind: "task".into(),
                ready_when: "steps.start.done".into(),
                terminal: Some(Terminal {
                    outcome: "done".into(),
                }),
                ..Default::default()
            },
        ],
    )
}

#[tokio::test]
async fn publish_refuses_the_unviable_protocol_retro_spec() {
    let registry = Arc::new(InMemoryWorkflows::new());
    let app = build_app(registry.clone());

    // A draft of the broken spec saves fine — drafts are work in
    // progress.
    let resp = send_json(
        app.clone(),
        "POST",
        "/api/workflows",
        &cto(),
        Some(serde_json::to_value(protocol_retro_v1()).unwrap()),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Publishing it must not.
    let resp = send_json(
        app.clone(),
        "POST",
        "/api/workflows/protocol-retro/publish",
        &cto(),
        None,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "an unviable spec must be refused at publish, not at the next pod roll"
    );

    // The body carries the same `{ok, problems}` shape `_validate`
    // returns, so the editor renders one thing either way.
    let body = json_body(resp).await;
    assert_eq!(body["ok"].as_bool(), Some(false), "body: {body}");
    let joined: String = body["problems"]
        .as_array()
        .expect("problems array")
        .iter()
        .filter_map(|p| p["message"].as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        joined.contains("no terminal"),
        "the refusal must name the real problem, got: {joined}"
    );

    // Nothing went active, and no publish event was recorded.
    assert!(
        registry.get_active("protocol-retro").await.is_err(),
        "refused publish must leave no active row"
    );
    assert!(
        registry
            .recorded_events()
            .iter()
            .all(|e| e.kind != WORKFLOW_PUBLISHED),
        "a refused publish records no jobs.kind.published"
    );

    // The draft survives — the author can fix it and try again.
    let versions = registry.list_versions("protocol-retro").await.unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].status, WorkflowStatus::Draft);
}

#[tokio::test]
async fn publish_accepts_a_viable_spec() {
    let registry = Arc::new(InMemoryWorkflows::new());
    let app = build_app(registry.clone());

    let resp = send_json(
        app.clone(),
        "POST",
        "/api/workflows",
        &cto(),
        Some(serde_json::to_value(viable_spec("nightly-brew")).unwrap()),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = send_json(
        app.clone(),
        "POST",
        "/api/workflows/nightly-brew/publish",
        &cto(),
        None,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK, "the gate must not overreach");

    let active = registry.get_active("nightly-brew").await.expect("active");
    assert_eq!(active.status, WorkflowStatus::Active);
    assert_eq!(active.version, 1);
}

#[tokio::test]
async fn draft_save_still_accepts_an_unviable_spec() {
    let registry = Arc::new(InMemoryWorkflows::new());
    let app = build_app(registry.clone());

    // Half-authored: one step, no terminal, no trigger.
    let mut spec = protocol_retro_v1();
    spec.kind = "half-authored".into();
    spec.steps.truncate(1);
    spec.steps[0].ready_when = "steps.nowhere.done".into();

    let resp = send_json(
        app.clone(),
        "POST",
        "/api/workflows",
        &cto(),
        Some(serde_json::to_value(&spec).unwrap()),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "a draft is work in progress — saving it unviable is legal"
    );
    let stored: WorkflowSpec = serde_json::from_value(json_body(resp).await).unwrap();
    assert_eq!(stored.status, WorkflowStatus::Draft);

    // And the author-time dry run still names the problems.
    let resp = send_json(
        app,
        "POST",
        "/api/workflows/_validate",
        &cto(),
        Some(serde_json::json!({ "kind": spec.kind, "steps": spec.steps })),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(json_body(resp).await["ok"].as_bool(), Some(false));
}

#[tokio::test]
async fn publish_authored_refuses_an_unviable_spec() {
    // The workflow-publish Step's dispatch path writes through
    // `publish_authored`, which sets a row active WITHOUT a draft
    // ever existing — same gate, same refusal.
    let registry = InMemoryWorkflows::new();
    let actor = boss_core::actor::ActorId::Automation("test".into());
    let now = chrono::Utc::now();

    let err = registry
        .publish_authored(protocol_retro_v1(), JobId::new(), &actor, now)
        .await
        .expect_err("publish_authored must refuse an unviable spec");
    match &err {
        WorkflowError::Unviable(problems) => {
            assert!(
                problems.iter().any(|p| p.reason.contains("no terminal")),
                "refusal must name the problem: {problems:?}"
            );
        }
        other => panic!("expected Unviable, got {other:?}"),
    }

    assert!(registry.get_active("protocol-retro").await.is_err());
    assert!(
        registry
            .recorded_events()
            .iter()
            .all(|e| e.kind != WORKFLOW_PUBLISHED)
    );
}

#[tokio::test]
async fn bootstrap_reconcile_refuses_to_seed_an_unviable_default() {
    // Platform seeding is a third path to `status = active`. A
    // shipped default that fails the lint is a code bug; it must not
    // reach the registry, and the reconcile must say so rather than
    // seed it and let boot quarantine clean up after itself.
    let registry = InMemoryWorkflows::new();
    let actor = boss_core::actor::ActorId::Automation("bootstrap-reconciler".into());
    let now = chrono::Utc::now();

    let stats = registry
        .bootstrap_reconcile(
            &[viable_spec("good-seed"), protocol_retro_v1()],
            &actor,
            now,
        )
        .await
        .expect("reconcile completes — one bad default doesn't abort the rest");

    assert_eq!(stats.inserted, 1, "the viable default seeds");
    assert_eq!(stats.rejected, 1, "the unviable default is refused");
    assert!(registry.get_active("good-seed").await.is_ok());
    assert!(
        registry.get_active("protocol-retro").await.is_err(),
        "an unviable default must never reach the active slot"
    );
}

// The gate above is only safe if the platform's own seeds pass it.
// That check already exists and stays where it is — the lib test
// `registry::tests::platform_workflows_passes_validate_all` — rather
// than being restated here. Tenant seed bundles get the same lint at
// load time (`seed_loader::load_workflows_*`).
