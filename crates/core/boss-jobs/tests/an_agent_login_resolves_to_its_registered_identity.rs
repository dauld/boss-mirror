//! An agent login resolves to its registered identity at the door.
//!
//! Design 6fda05ae (decided 2026-09-15, from backlog adf025df): the one
//! agent doing most of the IT department's work had two spellings —
//! `claude@algedonic.dev` on 142 open-packet step assignments and 12
//! completions, `claude:opus-5[1m]` on every agent_runs row — and
//! nothing related them. A human never reaches a step as an address,
//! because boss-gateway's oidc.rs resolves the login to an `emp-*` id
//! before any write is signed. This is the same resolution for agents:
//! the jobs API's door reads `X-Boss-User.id`, and an id the
//! `actor_aliases` table maps is replaced by the registered agent's id
//! BEFORE the extractor, the ambient actor, the claim path or the
//! completion stamp ever see it.
//!
//! Three claims, each pinned here through the in-memory registry:
//!
//!  - a claim and a completion signed with the alias land on the step
//!    as `agent-claude` (assignee_id, completed_by) and on every event
//!    the write emits (`_actor`);
//!  - an address NO alias maps is still admitted — the migration window
//!    is open — and the admission is COUNTED: one
//!    `actor.login.unresolved` event naming the login, method and path;
//!  - a read under an unmatched address, and any write under a
//!    non-address id, count nothing: reads attribute nothing, and an
//!    employee id or an automation slug is not a login.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::agents::{InMemoryAgents, LoginDoor, UNRESOLVED_LOGIN, resolve_login};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

struct AdminRoster;

#[async_trait::async_trait]
impl RosterLookup for AdminRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec!["emp-bootstrap-admin".to_string()],
            _ => Vec::new(),
        })
    }

    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == "emp-bootstrap-admin")
    }
}

/// The app the binary builds, minus Postgres: the jobs router with the
/// login door layered OUTSIDE the request-context middleware, the way
/// `boss_jobs_api.rs` layers it — the door must run first so the
/// ambient actor is taken from the resolved header.
/// Returns the app, the outbox (where the handlers' domain events
/// record — the in-memory analogue of in-transaction recording) and
/// the bus (where the door's count is published).
fn app() -> (axum::Router, Arc<InMemoryJobs>, Arc<RecordingEventBus>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Create,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let kinds = Arc::new(InMemoryWorkflows::new());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus: bus.clone(),
        publisher: publisher.clone(),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: Some(Arc::new(AdminRoster)),
        clock: Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
        dispatcher_firings: None,
        agent_budget: None,
    };
    let registry = Arc::new(InMemoryAgents::new().with_agent(AGENT, [ALIAS]));
    let door = Arc::new(LoginDoor::new(registry, publisher));
    let app = router(state)
        .layer(axum::middleware::from_fn(
            boss_policy_client::request_context_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(door, resolve_login));
    (app, jobs, bus)
}

/// The header the pod's `boss-api` door builds (infra/dev/boss-api):
/// the actor id, platform-admin, operator tier.
fn user(id: &str) -> String {
    format!(
        r#"{{"id":"{id}","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":null}}"#
    )
}

const ALIAS: &str = "claude@algedonic.dev";
const AGENT: &str = "agent-claude";
const STRANGER: &str = "nobody@example.test";

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.expect("request");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            serde_json::Value::String(String::from_utf8_lossy(&bytes).to_string())
        })
    };
    (status, json)
}

fn req(method: &str, uri: &str, signed_as: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", user(signed_as))
        .body(Body::from(body.to_string()))
        .expect("request")
}

/// Opens a `ship-a-change` packet as the alias and returns
/// `(job_id, scope_step_id)` — `scope` is ready at admission.
async fn open_packet(app: &axum::Router, signed_as: &str) -> (String, String) {
    let (status, job) = send(
        app,
        req(
            "POST",
            "/api/jobs",
            signed_as,
            serde_json::json!({
                "kind": "ship-a-change",
                "subject": {"subject_kind": "custom", "id": "feat/x"},
                "title": "t", "owner_id": "emp-bootstrap-admin",
                "status": "open", "priority": "standard",
                "metadata": {}, "tags": [],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "job create failed: {job}");
    let job_id = job["id"].as_str().expect("job id").to_string();
    let scope = scope_step(app, &job_id).await;
    assert_eq!(scope["status"], "ready", "precondition: scope is ready");
    (job_id, scope["id"].as_str().expect("step id").to_string())
}

async fn scope_step(app: &axum::Router, job_id: &str) -> serde_json::Value {
    let (status, full) = send(
        app,
        req(
            "GET",
            &format!("/api/jobs/{job_id}"),
            ALIAS,
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["spec_slug"] == "scope")
        .expect("scope step present")
        .clone()
}

fn unresolved_events(bus: &RecordingEventBus) -> Vec<boss_core::event::Event> {
    bus.events()
        .into_iter()
        .filter(|e| e.kind == UNRESOLVED_LOGIN)
        .collect()
}

/// The whole claim: the address goes in, the registered id comes out
/// on the step and on the log, and nothing the agent wrote carries the
/// address.
#[tokio::test]
async fn a_claim_and_a_completion_signed_with_the_alias_carry_the_agent_id() {
    let (app, jobs, bus) = app();
    let (job_id, scope_id) = open_packet(&app, ALIAS).await;

    let (status, claimed) = send(
        &app,
        req(
            "POST",
            &format!("/api/jobs/{job_id}/steps/{scope_id}/claim"),
            ALIAS,
            serde_json::json!({}),
        ),
    )
    .await;
    assert!(status.is_success(), "claim: {status}: {claimed}");
    assert_eq!(
        claimed["assignee_id"], AGENT,
        "the claim path writes user.id, so the id must already be resolved: {claimed}"
    );

    let (status, body) = send(
        &app,
        req(
            "PUT",
            &format!("/api/jobs/{job_id}/steps/{scope_id}"),
            ALIAS,
            serde_json::json!({
                "status": "completed",
                "metadata": {"summary": "s", "excludes": "e",
                             "authority_role": "platform-admin"},
            }),
        ),
    )
    .await;
    assert!(status.is_success(), "completing scope: {status}: {body}");

    let scope = scope_step(&app, &job_id).await;
    assert_eq!(scope["status"], "completed", "{scope}");
    assert_eq!(scope["completed_by"], AGENT, "{scope}");
    assert_eq!(scope["assignee_id"], AGENT, "{scope}");

    // Every event this session emitted is attributed to the agent, and
    // none to the alias — the log is what the alias table exists to
    // keep readable, so no NEW row may carry the old spelling.
    let events = jobs.recorded_events();
    assert!(
        !events.is_empty(),
        "the create, claim and completion each recorded"
    );
    let actors: Vec<String> = events
        .iter()
        .map(|e| e.payload["_actor"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        actors.iter().all(|a| a == AGENT),
        "every emitted event is signed by the registered agent, got {actors:?}"
    );
    assert!(
        unresolved_events(&bus).is_empty(),
        "a resolved login is not an unresolved one"
    );
}

/// The window is open: an address no alias maps is admitted exactly as
/// it was before the door existed — and the admission leaves a
/// countable fact naming what arrived and where.
#[tokio::test]
async fn an_unmatched_address_is_admitted_and_counted_on_a_write() {
    let (app, _, bus) = app();
    let (job_id, scope_id) = open_packet(&app, STRANGER).await;

    let (status, claimed) = send(
        &app,
        req(
            "POST",
            &format!("/api/jobs/{job_id}/steps/{scope_id}/claim"),
            STRANGER,
            serde_json::json!({}),
        ),
    )
    .await;
    assert!(
        status.is_success(),
        "the window admits the stranger: {status}: {claimed}"
    );
    assert_eq!(
        claimed["assignee_id"], STRANGER,
        "unresolved means unchanged — the address survives, which is the defect the count measures"
    );

    let counted = unresolved_events(&bus);
    assert_eq!(
        counted.len(),
        2,
        "one count per admitted write (the create and the claim): {counted:?}"
    );
    let claim = counted.last().expect("the claim's count");
    assert_eq!(claim.payload["login"], STRANGER);
    assert_eq!(claim.payload["method"], "POST");
    assert_eq!(
        claim.payload["path"],
        format!("/api/jobs/{job_id}/steps/{scope_id}/claim")
    );
    assert_eq!(
        claim.payload["_actor"], "automation:jobs",
        "the door observed it; the unresolved login is the SUBJECT, never the actor: {claim:?}"
    );
}

/// Reads attribute nothing, and an id that is not address-shaped is
/// not a login — neither is counted.
#[tokio::test]
async fn reads_and_non_address_ids_count_nothing() {
    let (app, jobs, bus) = app();
    let (job_id, _) = open_packet(&app, ALIAS).await;

    let (status, _) = send(
        &app,
        req(
            "GET",
            &format!("/api/jobs/{job_id}"),
            STRANGER,
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(unresolved_events(&bus).is_empty(), "a read is not counted");

    let (_, _) = open_packet(&app, "emp-bootstrap-admin").await;
    let (_, _) = open_packet(&app, "automation:train-conductor").await;
    assert!(
        unresolved_events(&bus).is_empty(),
        "an employee id and an automation slug are identities, not logins"
    );
    let actors: Vec<String> = jobs
        .recorded_events()
        .iter()
        .map(|e| e.payload["_actor"].as_str().unwrap_or("").to_string())
        .collect();
    assert!(
        actors.contains(&"emp-bootstrap-admin".to_string())
            && actors.contains(&"automation:train-conductor".to_string()),
        "non-alias ids pass through the door untouched: {actors:?}"
    );
}
