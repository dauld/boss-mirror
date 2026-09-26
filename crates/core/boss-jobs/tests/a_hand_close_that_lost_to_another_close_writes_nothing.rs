//! A hand close that lost to another close writes nothing (backlog
//! 29a7ea09).
//!
//! A packet closes three ways: a declared terminal completing, every
//! step reaching a terminal state (the catch-all), and a job PUT that
//! sets `status: closed` (the hand close). 29a7ea09 measured the first
//! two racing on car 6b23d135 at 2026-09-24T22:18:10Z — each a whole-row
//! read-modify-write, the second erasing the outcome the first stamped —
//! and moved both onto `close_job_at`, a merge of only the keys a close
//! owns that lands only on a row still open. The hand close stayed a
//! whole-row write: the PUT reads the row, judges it open, and writes
//! its body back closed. A step-driven close committing between that
//! read and that write left a row the store let through, because a
//! finished row accepted any write that KEPT its finished status — so
//! the PUT's body landed over the close, the outcome the terminal had
//! stamped was gone, and JOB_CLOSED was recorded a second time.
//!
//! The store now holds the write to the status the writer READ: a row
//! that finished after the read is not written.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{JobId, JobStatus};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const KIND: &str = "closes-by-terminal-or-hand";

/// One piece of work that is a declared terminal: completing it closes
/// the packet `disproved`, as the terminal did on car 6b23d135.
fn spec() -> WorkflowSpec {
    let mut spec = WorkflowSpec::platform_seed(
        KIND,
        KIND,
        "platform",
        vec!["custom".into()],
        vec![StepSpec {
            title: "work".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: "The only work".into(),
            authority_role: Some("platform-admin".into()),
            terminal: Some(Terminal {
                outcome: "disproved".into(),
            }),
            ..Default::default()
        }],
    );
    spec.metadata = json!({ "owner_role": "platform-admin" });
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
    json!({
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
    kinds.seed(spec()).expect("seed the kind");
    let jobs = Arc::new(InMemoryJobs::new());
    let mut policy = FakePolicyClient::builder();
    for action in [Action::Create, Action::Read, Action::Update, Action::Close] {
        policy = policy.allow("platform-admin", action, Resource::job(), Scope::All);
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(
        policy
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

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", admin_header())
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn get_job(app: &axum::Router, job_id: &str) -> Value {
    let (status, body) = send(app, "GET", &format!("/api/jobs/{job_id}"), None).await;
    assert_eq!(status, StatusCode::OK, "read failed: {body}");
    serde_json::from_str(&body).expect("job json")
}

async fn open_job(app: &axum::Router) -> String {
    let (status, body) = send(
        app,
        "POST",
        "/api/jobs",
        Some(json!({
            "kind": KIND,
            "subject": { "subject_kind": "custom", "id": "/system/flow" },
            "title": "A packet two closers reach",
            "owner_id": "emp-bootstrap-admin",
            "priority": "standard",
            "status": "open",
            "metadata": {},
            "tags": ["test"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {body}");
    let job: Value = serde_json::from_str(&body).expect("job json");
    job["id"].as_str().expect("job id").to_string()
}

fn closes(jobs: &InMemoryJobs) -> usize {
    jobs.recorded_events()
        .iter()
        .filter(|e| e.kind == boss_jobs::events::JOB_CLOSED)
        .count()
}

#[tokio::test]
async fn a_hand_close_landing_after_a_terminal_close_does_not_erase_its_outcome() {
    let (app, jobs) = app();
    let job_id = open_job(&app).await;
    let id = JobId::from_uuid(uuid::Uuid::parse_str(&job_id).expect("uuid"));

    // The hand close's body, built while the packet was open.
    let mut body = get_job(&app, &job_id).await;
    body.as_object_mut().expect("job object").remove("steps");
    body["status"] = json!("closed");

    // The terminal close commits the moment the PUT has read the row:
    // the row the PUT judged open is closed `disproved` by the time it
    // writes. (The other writer records its own JOB_CLOSED; the stand-in
    // writes only the row, so every JOB_CLOSED counted below is the PUT's.)
    jobs.change_job_after_next_read(&id, |row| {
        row.status = JobStatus::Closed;
        row.closed_on = Some(chrono::Utc::now().date_naive());
        row.metadata["outcome"] = json!("disproved");
    });
    let (status, answer) = send(&app, "PUT", &format!("/api/jobs/{job_id}"), Some(body)).await;

    let after = get_job(&app, &job_id).await;
    assert_eq!(
        after["metadata"]["outcome"], "disproved",
        "the outcome the terminal close stamped survives the hand close that lost: {after:#}"
    );
    assert_eq!(
        closes(&jobs),
        0,
        "the losing hand close records no second JOB_CLOSED"
    );
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a close that lost to another close is refused out loud, not a 204: {answer}"
    );
    assert!(
        answer.contains("closed"),
        "the refusal names the status the packet is in: {answer}"
    );
}

/// Control: a hand close with nothing racing it still lands, and a
/// retitle of a packet read closed still lands on the closed row.
#[tokio::test]
async fn a_hand_close_and_a_retitle_after_it_still_land() {
    let (app, jobs) = app();
    let job_id = open_job(&app).await;

    let mut body = get_job(&app, &job_id).await;
    body.as_object_mut().expect("job object").remove("steps");
    body["status"] = json!("closed");
    let (status, answer) = send(&app, "PUT", &format!("/api/jobs/{job_id}"), Some(body)).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the hand close lands: {answer}"
    );
    assert_eq!(closes(&jobs), 1);

    let mut body = get_job(&app, &job_id).await;
    body.as_object_mut().expect("job object").remove("steps");
    body["title"] = json!("Retitled after the close");
    let (status, answer) = send(&app, "PUT", &format!("/api/jobs/{job_id}"), Some(body)).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the retitle lands: {answer}"
    );
    let after = get_job(&app, &job_id).await;
    assert_eq!(after["title"], "Retitled after the close");
    assert_eq!(after["status"], "closed");
    assert_eq!(closes(&jobs), 1, "a retitle records no close");
}
