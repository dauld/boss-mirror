//! A close the store refused is not a 204 (backlog 29a7ea09).
//!
//! A step write can close its packet two ways — a declared terminal
//! completing, and every step reaching a terminal state (the
//! catch-all). The step row commits first; the close is a second write.
//! Until 29a7ea09 the catch-all discarded that second write's error
//! with `let _ =` and the terminal close logged a warning, and both
//! answered 204: the caller was told its completion had landed whole
//! while the packet stayed open with every step terminal, and nothing
//! anywhere said so. Silence is the one failure mode the correctness
//! protocol forbids, so a close that did not land answers 500 and
//! names the packet.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::JobId;
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// One step, which is a declared terminal when `outcome` is `Some`
/// and leaves the close to the catch-all when it is `None`.
fn one_step_spec(kind: &str, outcome: Option<&str>) -> WorkflowSpec {
    let mut spec = WorkflowSpec::platform_seed(
        kind,
        kind,
        "platform",
        vec!["custom".into()],
        vec![StepSpec {
            title: "work".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: "The only work".into(),
            authority_role: Some("platform-admin".into()),
            terminal: outcome.map(|o| Terminal { outcome: o.into() }),
            ..Default::default()
        }],
    );
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
    spec
}

struct AdminRoster;

#[async_trait]
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

fn admin_header() -> String {
    serde_json::json!({
        "id": "emp-bootstrap-admin",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds
        .seed(one_step_spec("closes-by-catch-all", None))
        .expect("seed the catch-all kind");
    kinds
        .seed(one_step_spec("closes-by-terminal", Some("done")))
        .expect("seed the terminal kind");
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
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn get_job(app: &axum::Router, job_id: &str) -> serde_json::Value {
    let (status, body) = send(
        app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/jobs/{job_id}"))
            .header("x-boss-user", admin_header())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "read failed: {body}");
    serde_json::from_str(&body).expect("job json")
}

async fn open_job(app: &axum::Router, kind: &str) -> String {
    let (status, body) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({
                    "kind": kind,
                    "subject": { "subject_kind": "custom", "id": "/system/flow" },
                    "title": format!("A {kind} packet"),
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "metadata": {},
                    "tags": ["test"],
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {body}");
    let job: serde_json::Value = serde_json::from_str(&body).expect("job json");
    job["id"].as_str().expect("job id").to_string()
}

/// Complete the packet's one step with the close made to fail, and
/// return what the step write answered.
async fn complete_with_a_failing_close(kind: &str) -> (StatusCode, String, serde_json::Value) {
    let (app, jobs) = app();
    let job_id = open_job(&app, kind).await;
    jobs.fail_job_close(&JobId::from_uuid(
        uuid::Uuid::parse_str(&job_id).expect("uuid"),
    ));
    let job = get_job(&app, &job_id).await;
    let step = &job["steps"][0];
    let step_id = step["id"].as_str().expect("step id");
    let (status, body) = send(
        &app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}/steps/{step_id}"))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({ "status": "completed", "metadata": step["metadata"] })
                    .to_string(),
            ))
            .unwrap(),
    )
    .await;
    (status, body, get_job(&app, &job_id).await)
}

#[tokio::test]
async fn a_catch_all_close_that_did_not_land_answers_500() {
    let (status, body, job) = complete_with_a_failing_close("closes-by-catch-all").await;
    assert_eq!(
        job["status"], "open",
        "precondition: the close did not land"
    );
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a completion whose close was refused must not answer success: {body}"
    );
    assert!(
        body.contains("close") && body.contains(job["id"].as_str().unwrap_or("?")),
        "the answer names the packet whose close failed: {body}"
    );
}

#[tokio::test]
async fn a_declared_terminal_close_that_did_not_land_answers_500() {
    let (status, body, job) = complete_with_a_failing_close("closes-by-terminal").await;
    assert_eq!(
        job["status"], "open",
        "precondition: the close did not land"
    );
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "a completion whose close was refused must not answer success: {body}"
    );
    assert!(
        body.contains("close") && body.contains(job["id"].as_str().unwrap_or("?")),
        "the answer names the packet whose close failed: {body}"
    );
}
