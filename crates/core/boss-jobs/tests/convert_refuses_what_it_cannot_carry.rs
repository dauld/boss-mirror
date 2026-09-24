//! `POST /api/jobs/{id}/convert` refuses a move it cannot make true of
//! the packet.
//!
//! MEASURED (backlog 1e973965, the draft-design analyst on 4347a1af,
//! 2026-09-23). The door answered `converted: true` after changing only
//! `jobs.workflow_version`. Each step row keeps what materialisation
//! copied onto it from the admission version — on page-audit c0d2caf0
//! (pinned v1, active v3) `measure` and `file` still held v1 procedure
//! text after v3 changed them — and a step the target inserts is never
//! created. The safety verdict (`convertibility_for_packet`) called both
//! moves harmless, correctly: the MOVE is safe, the DOOR cannot carry it.
//!
//! Design 7cf202a9 Q2 decided the interim: until a re-pin re-projects
//! pending steps and materialises inserted ones, the door refuses any
//! move whose target changes a pending step's procedure or inserts a
//! step, and the refusal names what would change. The route had no test
//! at all; these are its first, one per refusal and one for the safe case.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{WorkflowSpec, WorkflowStatus, seedable_platform_workflows};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

const KIND: &str = "ship-a-change";

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

/// The platform's active ship-a-change, as seeded.
fn active_spec() -> WorkflowSpec {
    seedable_platform_workflows()
        .into_iter()
        .find(|s| s.kind == KIND)
        .expect("ship-a-change is a platform workflow")
}

/// A later version of it, edited by `edit` — seeded as a draft so the
/// active row the packet opens under stays the one `get_active` finds,
/// and the conversion names its target with `to_version`.
fn next_version(kinds: &InMemoryWorkflows, bump: i32, edit: impl FnOnce(&mut WorkflowSpec)) -> i32 {
    let mut spec = active_spec();
    spec.version += bump;
    spec.status = WorkflowStatus::Draft;
    edit(&mut spec);
    let version = spec.version;
    kinds.seed(spec).expect("seed target version");
    version
}

fn set_procedure(spec: &mut WorkflowSpec, slug: &str, text: &str) {
    let step = spec
        .steps
        .iter_mut()
        .find(|s| s.title == slug)
        .unwrap_or_else(|| panic!("step {slug} in {KIND}"));
    let mut defaults = match &step.metadata_defaults {
        serde_json::Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    };
    defaults.insert("procedure".into(), serde_json::Value::String(text.into()));
    step.metadata_defaults = serde_json::Value::Object(defaults);
}

fn app() -> (axum::Router, Arc<InMemoryWorkflows>) {
    let kinds = Arc::new(InMemoryWorkflows::new());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
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
                Resource::job(),
                Scope::All,
            )
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
    let state = JobsApiState {
        kind_registry: Some(kinds.clone() as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs,
            bus.clone(),
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), kinds)
}

const ADMIN: &str = r#"{"id":"emp-bootstrap-admin","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

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

fn req(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", ADMIN)
        .body(Body::from(body.to_string()))
        .expect("request")
}

async fn get_job(app: &axum::Router, id: &str) -> serde_json::Value {
    let (status, full) = send(
        app,
        req("GET", &format!("/api/jobs/{id}"), serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    full
}

/// A packet opened under the active version, standing at `build`.
async fn open_at_build(app: &axum::Router) -> String {
    let (status, job) = send(
        app,
        req(
            "POST",
            "/api/jobs",
            serde_json::json!({
                "kind": KIND,
                "subject": {"subject_kind": "custom", "id": "feat/x"},
                "title": "t", "owner_id": "emp-bootstrap-admin",
                "status": "open", "priority": "standard",
                "metadata": {}, "tags": [],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "job create: {job}");
    let id = job["id"].as_str().expect("job id").to_string();
    let full = get_job(app, &id).await;
    let scope = full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["spec_slug"] == "scope")
        .expect("scope step")
        .clone();
    let scope_id = scope["id"].as_str().expect("step id");
    let (status, body) = send(
        app,
        req(
            "PUT",
            &format!("/api/jobs/{id}/steps/{scope_id}"),
            serde_json::json!({
                "status": "completed",
                "metadata": {"summary": "s", "excludes": "e",
                             "authority_role": "platform-admin"},
            }),
        ),
    )
    .await;
    assert!(status.is_success(), "complete scope: {status}: {body}");
    id
}

async fn convert(app: &axum::Router, id: &str, to: i32) -> (StatusCode, serde_json::Value) {
    send(
        app,
        req(
            "POST",
            &format!("/api/jobs/{id}/convert"),
            serde_json::json!({ "to_version": to }),
        ),
    )
    .await
}

fn obstacle_for<'a>(body: &'a serde_json::Value, step: &str) -> &'a serde_json::Value {
    body["obstacles"]
        .as_array()
        .unwrap_or_else(|| panic!("a refusal carries its obstacles: {body}"))
        .iter()
        .find(|o| o["step"] == step)
        .unwrap_or_else(|| panic!("an obstacle naming {step}: {body}"))
}

/// THE MEASURED CASE: the target changes the procedure of a step the
/// packet has not reached. Before this car: 200, `converted: true`, and
/// the `build` row still carrying the admission text.
#[tokio::test]
async fn a_move_that_changes_a_pending_procedure_is_refused_and_says_which() {
    let (app, kinds) = app();
    let id = open_at_build(&app).await;
    let pinned = get_job(&app, &id).await["workflow_version"].clone();
    let to = next_version(&kinds, 1, |s| {
        set_procedure(s, "build", "Build it, and say what you measured.")
    });

    let (status, body) = convert(&app, &id, to).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["converted"], false);
    let reason = obstacle_for(&body, "build")["reason"]
        .as_str()
        .expect("reason");
    assert!(
        reason.contains("`procedure`"),
        "names what would change: {reason}"
    );
    assert_eq!(
        get_job(&app, &id).await["workflow_version"],
        pinned,
        "a refusal leaves the pin where it was"
    );
}

/// A step the target inserts ahead of the packet: harmless to the
/// safety verdict, but the door creates no row for it.
#[tokio::test]
async fn a_move_that_inserts_a_step_is_refused_and_names_it() {
    let (app, kinds) = app();
    let id = open_at_build(&app).await;
    let to = next_version(&kinds, 2, |s| {
        let mut extra = s
            .steps
            .iter()
            .find(|st| st.title == "settled")
            .expect("settled step")
            .clone();
        extra.title = "archived".to_string();
        s.steps.push(extra);
    });

    let (status, body) = convert(&app, &id, to).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let reason = obstacle_for(&body, "archived")["reason"]
        .as_str()
        .expect("reason");
    assert!(reason.contains("inserted"), "{reason}");
}

/// THE ONE SAFE CASE: the target changes only a step the packet has
/// already completed. That step ran under the text it holds, which is
/// what it must keep — so the door can carry this move, and does.
#[tokio::test]
async fn a_move_that_changes_only_a_completed_step_converts() {
    let (app, kinds) = app();
    let id = open_at_build(&app).await;
    let to = next_version(&kinds, 3, |s| {
        set_procedure(s, "scope", "Scope it, in writing.")
    });

    let (status, body) = convert(&app, &id, to).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["converted"], true, "{body}");
    assert_eq!(get_job(&app, &id).await["workflow_version"], to);
}
