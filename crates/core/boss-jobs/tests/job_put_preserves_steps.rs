//! Does `PUT /api/jobs/{id}` clobber the Job's steps?
//!
//! Defect b490a82b says yes: "GET a job, change one metadata field,
//! PUT it back — and every step is re-materialised: statuses reset to
//! their initial values and step metadata is lost. Reproduced on
//! 2026-08-17 while setting `action_needed` on maintenance-sweep
//! packets: an inspect step that read `completed` with findings and
//! measured recorded came back `ready` with neither."
//!
//! I filed that report, off nine sweeps driven from a script. Reading
//! the handler afterwards, no path in it touches a step: `update_job`
//! UPDATEs the `jobs` row and then calls `reevaluate_and_persist`,
//! which only promotes `Pending` steps to `Ready` or `Skipped` and
//! writes back nothing else. Neither adapter's `update_job_at` names
//! the steps table.
//!
//! So either the report is wrong or the mechanism is somewhere the
//! reading missed, and the way to tell is to run the sequence rather
//! than to reason about it again. This file runs it — GET, edit one
//! metadata key, PUT, read the steps back — and asserts on what
//! survives.
//!
//! It earns its place either way. If the clobbering is real this
//! reproduces it; if it is not, this is the regression test for a
//! GET-then-PUT round trip, which is the obvious way to change one
//! field and had no coverage at all. The absence of that test is why
//! a wrong report was plausible enough to file.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

/// A two-step Workflow shaped like `maintenance-sweep`: a step that
/// records what it found, and a terminal gated on a JOB metadata key.
///
/// The job-metadata gate is the part that matters. It is why a sweep
/// is driven by a PUT to the job at all — the fork reads
/// `job.metadata.action_needed`, not a step field — and therefore why
/// the GET-then-PUT round trip happens on a job that already has
/// completed steps carrying results.
fn sweep_shaped_spec() -> boss_jobs::registry::WorkflowSpec {
    let mut spec = boss_jobs::registry::WorkflowSpec::platform_seed(
        "sweep-shaped",
        "Sweep-shaped",
        "platform",
        vec!["custom".into()],
        vec![
            boss_jobs::registry::StepSpec {
                title: "inspect".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                title_template: "Inspect".into(),
                authority_role: Some("platform-admin".into()),
                ..Default::default()
            },
            boss_jobs::registry::StepSpec {
                title: "clear".into(),
                kind: "task".into(),
                // The fork: opens only once the job carries the marker.
                ready_when: "job.metadata.action_needed = false".into(),
                title_template: "Clear".into(),
                authority_role: Some("platform-admin".into()),
                ..Default::default()
            },
        ],
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

fn app() -> axum::Router {
    let kinds = Arc::new(InMemoryWorkflows::new());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    kinds.seed(sweep_shaped_spec()).expect("seed sweep kind");
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
            .allow("platform-admin", Action::Close, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs: Arc::new(InMemoryJobs::new()),
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: Some(Arc::new(AdminRoster)),
        clock: Arc::new(boss_clock_client::WallClockClient),
    };
    router(state)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

async fn get_job(app: &axum::Router, id: &str) -> serde_json::Value {
    let (status, job) = send(
        app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/jobs/{id}"))
            .header("x-boss-user", admin_header())
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "GET job: {job}");
    job
}

async fn steps_of(app: &axum::Router, id: &str) -> Vec<serde_json::Value> {
    let (status, body) = send(
        app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/jobs/{id}/steps"))
            .header("x-boss-user", admin_header())
            .body(Body::empty())
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "GET steps: {body}");
    body.as_array()
        .cloned()
        .or_else(|| body["data"].as_array().cloned())
        .unwrap_or_default()
}

/// Open a sweep-shaped job and complete its inspect step with results
/// on it — the state the report says a later PUT destroys.
async fn job_with_a_completed_first_step(app: &axum::Router) -> String {
    let (status, created) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({
                    "kind": "sweep-shaped",
                    "subject": { "subject_kind": "custom", "id": "disk-headroom" },
                    "title": "Disk headroom sweep",
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "metadata": { "target": "disk-headroom", "area": "infra" },
                    "tags": [],
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create: {created}");
    let job_id = created["id"].as_str().expect("job id").to_string();

    let steps = steps_of(app, &job_id).await;
    let inspect = steps.first().expect("inspect step").clone();
    let step_id = inspect["id"].as_str().expect("step id");

    let (status, body) = send(
        app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}/steps/{step_id}"))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({
                    "id": step_id,
                    "job_id": job_id,
                    "kind": "task",
                    "title": "Inspect",
                    "sort_order": 0,
                    "status": "completed",
                    "metadata": {
                        "summary": "38% used across three volumes",
                        "excludes": "none",
                        "test": "df -h on each node",
                        "gates": "none",
                        "verified": true,
                        "findings": "nothing above the floor",
                        "measured": "38%",
                    },
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;
    // ALWAYS ASSERT THE CODE. Swallowing this exact response — a 400
    // on a step kind's required fields — is what made the original
    // report look true: the completion never happened, so the step was
    // still `ready` with no metadata when it was read back, and the
    // PUT that came between got the blame.
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "completing the inspect step: {body}"
    );

    let after = steps_of(app, &job_id).await;
    assert_eq!(
        after[0]["status"], "completed",
        "precondition: the step completed"
    );
    assert_eq!(
        after[0]["metadata"]["measured"], "38%",
        "precondition: its results are stored"
    );
    job_id
}

/// The reported sequence, run.
#[tokio::test]
async fn get_then_put_leaves_completed_steps_and_their_metadata_alone() {
    let app = app();
    let job_id = job_with_a_completed_first_step(&app).await;

    // GET the job, change one metadata key, PUT the whole body back —
    // exactly what the report describes, including round-tripping the
    // `steps` array the GET response carries.
    let mut job = get_job(&app, &job_id).await;
    assert!(
        job.get("steps").is_some(),
        "the GET response carries `steps`, which is what invites this round trip"
    );
    job["metadata"]["action_needed"] = serde_json::json!(false);

    let (status, body) = send(
        &app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}"))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(job.to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "PUT job: {body}");

    let steps = steps_of(&app, &job_id).await;
    assert_eq!(
        steps[0]["status"], "completed",
        "b490a82b: the completed inspect step must not revert to ready"
    );
    assert_eq!(
        steps[0]["metadata"]["measured"], "38%",
        "b490a82b: its recorded results must survive a job PUT"
    );
    assert_eq!(steps[0]["metadata"]["findings"], "nothing above the floor");

    // And the edit itself landed, including the re-evaluation it is
    // made for: writing the marker opens the gated terminal.
    let job = get_job(&app, &job_id).await;
    assert_eq!(job["metadata"]["action_needed"], serde_json::json!(false));
    assert_eq!(
        job["metadata"]["target"], "disk-headroom",
        "the other metadata keys are still there"
    );
    assert_eq!(
        steps[1]["status"], "ready",
        "the job-metadata gate opened — this is what the PUT was for"
    );
}

/// A PUT whose body carries steps does not let them through.
///
/// The report's suggested fix was to refuse such a body. The stronger
/// property is that it cannot matter: `Job` has no `steps` field, so a
/// client round-tripping the GET response is writing the job row and
/// nothing else, and no ordering rule needs to be known by anyone.
#[tokio::test]
async fn steps_in_the_put_body_are_inert() {
    let app = app();
    let job_id = job_with_a_completed_first_step(&app).await;

    let mut job = get_job(&app, &job_id).await;
    // Hostile body: claim the completed step is pending again, with
    // its results erased. If the handler read `steps` at all, this is
    // the shape that would do the damage the report describes.
    job["steps"] = serde_json::json!([
        { "status": "pending", "metadata": {} },
        { "status": "pending", "metadata": {} },
    ]);
    job["metadata"]["action_needed"] = serde_json::json!(false);

    let (status, body) = send(
        &app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}"))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(job.to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "PUT job: {body}");

    let steps = steps_of(&app, &job_id).await;
    assert_eq!(
        steps[0]["status"], "completed",
        "a body cannot un-complete a step"
    );
    assert_eq!(steps[0]["metadata"]["measured"], "38%");
}
