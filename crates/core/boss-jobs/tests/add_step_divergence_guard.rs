//! Adding a step to a live Job is refused, because it would freeze it.
//!
//! Step predicates are not stored on steps — the `steps` table has no
//! `ready_when` column — so readiness is recomputed by pairing spec
//! steps with job steps POSITIONALLY. One appended step misaligns every
//! pair after it, `registry::reevaluate` correctly refuses to evaluate
//! anything, and from that moment the Job is frozen: no step advances
//! again, including its terminal, so it never closes and never leaves
//! its owner's queue.
//!
//! Design review 32a4e70d is the worked example. It gained a
//! per-question step on 2026-08-13 and sat in David's queue with its
//! review step COMPLETED and its terminal pending, producing feedback
//! 55c92985: "I finished the top design review in the table and it
//! still shows the same metadata and is in the same queue."
//!
//! The divergence WAS logged the whole time — `reevaluate_and_persist`
//! warns with the job id and both counts. That is the lesson worth
//! keeping: a warn in a log nobody reads is not a signal. The fix is
//! refusing at the door, where the caller is told, rather than
//! recording the damage after it is done.
//!
//! A new step is a change to the WORKFLOW. The registry is where that
//! belongs: publish a new version and admit new packets under it.
//! In-flight packets stay pinned to the version they were admitted
//! under, which is the entire point of the versioning.

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
            .allow("platform-admin", Action::Close, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs,
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
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

async fn open_a_job(app: &axum::Router) -> String {
    let (status, job) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({
                    "kind": "ship-a-change",
                    "subject": { "subject_kind": "custom", "id": "divergence-guard" },
                    "title": "a packet someone will try to append a step to",
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "metadata": {},
                    "tags": ["test"],
                    "opened_on": "2026-08-14"
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "job not opened: {job}");
    job["id"].as_str().expect("job id").to_string()
}

#[tokio::test]
async fn appending_a_step_to_a_live_job_is_refused() {
    let app = app();
    let job_id = open_a_job(&app).await;

    let before = send(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/jobs/{job_id}/steps"))
            .header("x-boss-user", admin_header())
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .1;
    let count_before = before.as_array().expect("steps array").len();
    assert!(count_before > 0, "the workflow materialized steps");

    let (status, body) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri(format!("/api/jobs/{job_id}/steps"))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({
                    "id": uuid::Uuid::new_v4(),
                    "job_id": job_id,
                    "kind": "task",
                    "title": "Q: a question captured as a step",
                    "status": "pending",
                    "blocked_by": [],
                    "sign_offs_required": [],
                    "sign_offs": [],
                    "fields": [],
                    "metadata": {}
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "appending must be refused, not accepted: {body}"
    );
    let msg = body.as_str().unwrap_or_default();
    assert!(
        msg.contains("freeze") || msg.contains("diverge"),
        "the refusal must say WHY, so the caller does not just retry: {msg}"
    );

    // The decisive assertion: the job is untouched. A guard that
    // refused but appended anyway would be worse than none.
    let after = send(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/jobs/{job_id}/steps"))
            .header("x-boss-user", admin_header())
            .body(Body::empty())
            .unwrap(),
    )
    .await
    .1;
    assert_eq!(
        after.as_array().expect("steps array").len(),
        count_before,
        "a refused add must not have written the step"
    );
}
