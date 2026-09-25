//! A sim header alone opens no door (backlog 85e7f10f, 2026-09-25).
//!
//! `SimBypassPolicyClient` answered `Allow { scope: All }` to any
//! request carrying `x-sim-origin: true`, and five service binaries
//! wrapped their policy client in it unconditionally. The gateway strips
//! the header, but the LAN machine door (10.20.0.34:7900) and every
//! in-cluster caller do not pass the gateway — so any caller that
//! reached the jobs API directly passed every policy-gated read and
//! write by adding one header. The same header also admitted real work
//! `Simulated` (`http/jobs.rs`, the partition decided at admission),
//! the set the cutover trims.
//!
//! The app here is the binary's: the jobs router under the
//! request-context middleware, its policy client built through
//! `SimBypassPolicyClient::from_env`, which reads `BOSS_SIM_ENABLED`
//! the way every service does. Two instances are built in turn:
//!
//!  - SIM OFF (prod — boss.yaml sets `"false"`, and unset is off): the
//!    header is ignored. An anonymous caller and even the sim's own
//!    identity are refused a step write and read no packet; an
//!    operator's packet sent with the header is admitted REAL.
//!  - SIM ON (the playground): an anonymous caller and an ordinary
//!    employee are refused exactly as before; the sim's identity
//!    (`automation:sim`, role `system-sim`) still writes, and its
//!    packets are still admitted simulated.
//!
//! ONE test, on a runtime it builds itself AFTER setting the variable:
//! the environment is process-wide and Rust gives one process per file
//! under `tests/`, so nothing else in this binary reads it concurrently.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{
    Action, FakePolicyClient, PolicyClient, Resource, SIM_ENABLED_ENV, Scope, SimBypassPolicyClient,
};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

fn header(id: &str, role: &str, tier: &str) -> String {
    json!({
        "id": id,
        "role": role,
        "access_tier": tier,
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": null,
    })
    .to_string()
}

/// The one caller the policy grants: an operator-tier CEO.
fn ceo() -> String {
    header("emp-ceo", "ceo", "operator")
}

/// What boss-sim's LiveApiOutput signs every call with.
fn the_sim() -> String {
    header("automation:sim", "system-sim", "operator")
}

/// An ordinary employee who has learned the header.
fn an_employee() -> String {
    header("emp-042", "clerk", "user")
}

fn kind() -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        "sim-door-kind",
        "Sim door kind",
        "test",
        vec!["custom".into()],
        vec![StepSpec {
            title: "work".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: "Do the work".into(),
            ..Default::default()
        }],
    )
}

/// The binary's shape: policy through `from_env`, the router under the
/// request-context middleware.
fn app() -> axum::Router {
    bare_router().layer(axum::middleware::from_fn(
        boss_policy_client::request_context_middleware,
    ))
}

/// The same router WITHOUT the middleware, so a test can hold a sim
/// chain open around a request the way the dispatcher does around the
/// event it reacts to — what reaches the policy client when the header
/// is not the source of the chain.
fn bare_router() -> axum::Router {
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds.seed(kind()).unwrap();
    let inner: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Create, Resource::job(), Scope::All)
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .allow("ceo", Action::Update, Resource::job(), Scope::All)
            .allow("ceo", Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        ..JobsApiState::minimal(
            Arc::new(InMemoryJobs::new()),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            SimBypassPolicyClient::from_env(inner),
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    router(state)
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    user: Option<&str>,
    sim_header: bool,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(u) = user {
        b = b.header("x-boss-user", u);
    }
    if sim_header {
        b = b.header("x-sim-origin", "true");
    }
    let req = b
        .body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let v = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into_owned()));
    (status, v)
}

fn job_body() -> Value {
    json!({
        "kind": "sim-door-kind",
        "subject": { "subject_kind": "custom", "id": "doc-1" },
        "title": "A packet",
        "owner_id": "emp-ceo",
        "status": "open",
        "priority": "standard",
        "metadata": {},
        "tags": [],
    })
}

/// Opens a packet as `user` (with or without the header) and returns
/// `(job_id, simulated, step_id)`, read back as the CEO.
async fn open(app: &axum::Router, user: &str, sim_header: bool) -> (String, Value, String) {
    let (status, job) = send(
        app,
        "POST",
        "/api/jobs",
        Some(user),
        sim_header,
        Some(job_body()),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "open: {job}");
    let id = job["id"].as_str().unwrap().to_string();
    let (status, full) = send(
        app,
        "GET",
        &format!("/api/jobs/{id}"),
        Some(&ceo()),
        false,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "read back: {full}");
    let step = full["steps"][0]["id"].as_str().expect("a step").to_string();
    (id, full["simulated"].clone(), step)
}

/// A step write under `user` with the header: the status it answers.
async fn step_write(app: &axum::Router, job: &str, step: &str, user: Option<&str>) -> StatusCode {
    send(
        app,
        "PUT",
        &format!("/api/jobs/{job}/steps/{step}"),
        user,
        true,
        Some(json!({ "assignee_id": "emp-ceo" })),
    )
    .await
    .0
}

/// How many packets a list read under `user` with the header sees.
async fn packets_seen(app: &axum::Router, user: Option<&str>) -> Value {
    let (status, v) = send(app, "GET", "/api/jobs", user, true, None).await;
    assert_eq!(status, StatusCode::OK, "list: {v}");
    v["total"].clone()
}

async fn with_the_sim_off() {
    let app = app();
    let (job, simulated, step) = open(&app, &ceo(), true).await;
    assert_eq!(
        simulated, false,
        "with no sim, a forged header must not admit real work Simulated"
    );

    assert_eq!(
        step_write(&app, &job, &step, None).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        step_write(&app, &job, &step, Some(&an_employee())).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        step_write(&app, &job, &step, Some(&the_sim())).await,
        StatusCode::FORBIDDEN,
        "no sim instance, no bypass — not even for the sim's own identity"
    );
    assert_eq!(packets_seen(&app, None).await, 0);
    // Control: the same read by the granted caller sees the packet, so
    // the zero above is a refusal, not an empty store.
    assert_eq!(packets_seen(&app, Some(&ceo())).await, 1);

    // Two locks, each tested alone. The middleware above ignored the
    // header; here the chain is held open by hand, around a router with
    // no middleware, and the sim's own identity is STILL refused —
    // because `from_env` did not install the bypass on this instance.
    let bare = bare_router();
    let (job, _, step) = open(&bare, &ceo(), false).await;
    let on_a_chain = boss_core::sim_origin::with_sim_chain(
        true,
        step_write(&bare, &job, &step, Some(&the_sim())),
    )
    .await;
    assert_eq!(
        on_a_chain,
        StatusCode::FORBIDDEN,
        "an instance without a sim carries no bypass at all"
    );
}

async fn with_the_sim_on() {
    let app = app();
    let (job, _, step) = open(&app, &ceo(), false).await;

    assert_eq!(
        step_write(&app, &job, &step, None).await,
        StatusCode::FORBIDDEN,
        "a header is not a caller"
    );
    assert_eq!(
        step_write(&app, &job, &step, Some(&an_employee())).await,
        StatusCode::FORBIDDEN,
        "an ordinary employee with the header is still an ordinary employee"
    );
    assert_eq!(packets_seen(&app, None).await, 0);
    assert_eq!(packets_seen(&app, Some(&an_employee())).await, 0);

    // The playground keeps working: the sim writes and reads, and what
    // it admits is simulated.
    let by_the_sim = step_write(&app, &job, &step, Some(&the_sim())).await;
    assert!(
        by_the_sim.is_success(),
        "the sim writes on a sim instance: {by_the_sim}"
    );
    let (_, simulated, _) = open(&app, &the_sim(), true).await;
    assert_eq!(simulated, true, "the sim's packets are still simulated");
    assert_eq!(packets_seen(&app, Some(&the_sim())).await, 2);

    // The control for the sim-off half's hand-held chain: the same shape
    // on a sim instance lets the sim through, so that refusal was the
    // missing bypass and not the shape.
    let bare = bare_router();
    let (job, _, step) = open(&bare, &ceo(), false).await;
    let on_a_chain = boss_core::sim_origin::with_sim_chain(
        true,
        step_write(&bare, &job, &step, Some(&the_sim())),
    )
    .await;
    assert!(on_a_chain.is_success(), "{on_a_chain}");
}

#[test]
fn a_sim_header_alone_opens_no_door_with_the_sim_off_or_on() {
    let rt = || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    };
    // SAFETY: the only test in this binary; no other thread reads the
    // environment while it is written (see the module doc).
    unsafe { std::env::remove_var(SIM_ENABLED_ENV) };
    rt().block_on(with_the_sim_off());
    unsafe { std::env::set_var(SIM_ENABLED_ENV, "false") };
    rt().block_on(with_the_sim_off());
    unsafe { std::env::set_var(SIM_ENABLED_ENV, "true") };
    rt().block_on(with_the_sim_on());
    unsafe { std::env::remove_var(SIM_ENABLED_ENV) };
}
