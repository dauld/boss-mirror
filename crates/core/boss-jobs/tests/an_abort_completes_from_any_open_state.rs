//! An aborted terminal completes from any open state.
//!
//! The blocker gate in the step PUT refuses `status=completed` while
//! any step in `blocked_by` is still open — invariant I-4, the rule
//! that keeps a machine's steps in order. The gate is right for every
//! step whose meaning is "the work up to here is done". It is wrong
//! for exactly one: a terminal whose materialised `outcome_kind` is
//! `aborted`, whose meaning is "stop here, wherever here is".
//!
//! Measured 2026-09-15 (backlog fd0f92ae): all 37 aborted terminals
//! in `infra/platform/workflows/*.toml` carry a `ready_when` that
//! references an upstream step (`steps.scope.done AND
//! job.metadata.abandoned = "true"`, `steps.triage.done AND … =
//! "decline"`), so every one of them is Pending with a live blocker
//! for most of its Job's life, and the step API answered 409 "step
//! has unresolved blockers" to the abort the accepted design allows
//! whenever the Job is open. Abandonment is not forward progress; a
//! surface that offers the control while its server refuses it is the
//! defect.
//!
//! What stays: the required fields (`reason`), the actor's authority,
//! the frozen-terminal refusals, and the gate for every OTHER step.
//! The terminal's `ready_when` stays as data too — it still describes
//! the machine's own path to the terminal.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
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

const KIND: &str = "aborts-from-anywhere";
const ABORT_READY_WHEN: &str = "steps.work.done AND job.metadata.abandoned = \"true\"";

/// One trigger, one completed terminal behind it, one aborted terminal
/// behind it — the shape every one of the 37 measured rows has.
fn spec() -> WorkflowSpec {
    let mut spec = WorkflowSpec::platform_seed(
        KIND,
        "Aborts from anywhere",
        "platform",
        vec!["custom".into()],
        vec![
            StepSpec {
                title: "work".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                title_template: "The work".into(),
                authority_role: Some("platform-admin".into()),
                ..Default::default()
            },
            StepSpec {
                title: "done".into(),
                kind: "outcome".into(),
                ready_when: "steps.work.done".into(),
                title_template: "Done".into(),
                authority_role: Some("platform-admin".into()),
                metadata_defaults: serde_json::json!({ "outcome_kind": "completed" }),
                terminal: Some(Terminal {
                    outcome: "done".into(),
                }),
                ..Default::default()
            },
            StepSpec {
                title: "aborted".into(),
                kind: "outcome".into(),
                ready_when: ABORT_READY_WHEN.into(),
                title_template: "Aborted".into(),
                authority_role: Some("platform-admin".into()),
                metadata_defaults: serde_json::json!({ "outcome_kind": "aborted" }),
                terminal: Some(Terminal {
                    outcome: "aborted".into(),
                }),
                fields: vec![boss_core::job::StepField {
                    name: "reason".into(),
                    field_type: "string".into(),
                    required: true,
                    filled_by: Default::default(),
                    item_keys: Vec::new(),
                    covers: None,
                    binds: None,
                    item_value_max_bytes: None,
                    item_one_of: Vec::new(),
                }],
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

struct Harness {
    app: axum::Router,
    jobs: Arc<InMemoryJobs>,
    kinds: Arc<InMemoryWorkflows>,
}

fn harness() -> Harness {
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds.seed(spec()).expect("seed the kind");
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
    let state = JobsApiState {
        kind_registry: Some(kinds.clone() as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    Harness {
        app: router(state),
        jobs,
        kinds,
    }
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

async fn get_job(app: &axum::Router, job_id: &str) -> serde_json::Value {
    let (status, job) = send(
        app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/jobs/{job_id}"))
            .header("x-boss-user", admin_header())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "read failed: {job}");
    job
}

async fn open_job(app: &axum::Router) -> String {
    let (status, job) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({
                    "kind": KIND,
                    "subject": { "subject_kind": "custom", "id": "/system/flow" },
                    "title": "A packet that will be abandoned",
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
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    job["id"].as_str().expect("job id").to_string()
}

fn step_by_slug(job: &serde_json::Value, slug: &str) -> serde_json::Value {
    job["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["spec_slug"] == slug)
        .cloned()
        .unwrap_or_else(|| panic!("no step with slug `{slug}` on {job:#}"))
}

/// PUT `status=completed` on one step, merging `extra` into the step's
/// stored metadata (never replacing: `outcome_kind` shares the object).
async fn complete(
    app: &axum::Router,
    job_id: &str,
    step: &serde_json::Value,
    extra: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let mut metadata = step["metadata"].clone();
    if let (Some(dst), Some(src)) = (metadata.as_object_mut(), extra.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    let step_id = step["id"].as_str().expect("step id");
    send(
        app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}/steps/{step_id}"))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({ "status": "completed", "metadata": metadata }).to_string(),
            ))
            .unwrap(),
    )
    .await
}

/// The claim itself: a Pending aborted terminal, its blocker still
/// open, completes with its reason — and the Job closes on it, with
/// every other open step skipped and the record saying where the abort
/// fired from.
#[tokio::test]
async fn a_pending_aborted_terminal_completes_with_a_reason_and_closes_the_job() {
    let h = harness();
    let job_id = open_job(&h.app).await;

    let job = get_job(&h.app, &job_id).await;
    let aborted = step_by_slug(&job, "aborted");
    assert_eq!(
        aborted["status"], "pending",
        "precondition: the abort is behind its blocker at open: {aborted:#}"
    );
    assert_eq!(step_by_slug(&job, "work")["status"], "ready");

    let (status, body) = complete(
        &h.app,
        &job_id,
        &aborted,
        serde_json::json!({ "reason": "the customer withdrew the order" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "a pending aborted terminal with its reason must complete: {body}"
    );

    let closed = get_job(&h.app, &job_id).await;
    assert_eq!(
        closed["status"], "closed",
        "the abort closes the Job: {closed:#}"
    );
    assert_eq!(
        closed["metadata"]["outcome"], "aborted",
        "the Job's outcome records the abort: {closed:#}"
    );
    // Closure holds: no step is left dangling, and none is skipped
    // silently — each skip is its own STEP_UPDATED, and the terminal
    // names the steps it fired past.
    assert_eq!(step_by_slug(&closed, "work")["status"], "skipped");
    assert_eq!(step_by_slug(&closed, "done")["status"], "skipped");
    let fired = step_by_slug(&closed, "aborted");
    assert_eq!(fired["status"], "completed");
    assert_eq!(
        fired["metadata"]["aborted_from"],
        serde_json::json!(["work", "done"]),
        "the abort records which steps were still open when it fired: {fired:#}"
    );
    assert_eq!(
        fired["metadata"]["reason"],
        "the customer withdrew the order"
    );

    // Provenance: the completion is evented as any other — the step's
    // done marker and the Job's close marker both name the outcome.
    let events = h.jobs.recorded_events();
    let done_marker = events
        .iter()
        .find(|e| e.kind == "step.done.outcome")
        .unwrap_or_else(|| panic!("no step.done.outcome marker among {events:#?}"));
    assert_eq!(done_marker.payload["spec_slug"], "aborted");
    assert_eq!(
        done_marker.payload["metadata"]["aborted_from"],
        serde_json::json!(["work", "done"])
    );
    let close_marker = events
        .iter()
        .find(|e| e.kind == "jobs.job.closed")
        .expect("a close marker was recorded");
    assert_eq!(close_marker.payload["outcome"], "aborted");

    // The terminal's ready_when is untouched: it still describes the
    // machine's own path to the abort, as data.
    let pinned = h.kinds.get_active(KIND).await.expect("the kind is active");
    assert_eq!(pinned.steps[2].ready_when, ABORT_READY_WHEN);
}

/// Every OTHER step keeps the gate: the completed-kind terminal behind
/// the same blocker still answers 409.
#[tokio::test]
async fn a_pending_non_aborted_step_still_answers_409() {
    let h = harness();
    let job_id = open_job(&h.app).await;
    let job = get_job(&h.app, &job_id).await;
    let done = step_by_slug(&job, "done");
    assert_eq!(done["status"], "pending");

    let (status, body) = complete(&h.app, &job_id, &done, serde_json::json!({})).await;
    assert_eq!(status, StatusCode::CONFLICT, "the gate must hold: {body}");
    assert_eq!(body["error"], "step has unresolved blockers");
    assert_eq!(get_job(&h.app, &job_id).await["status"], "open");
}

/// The exemption reads the MATERIALISED `outcome_kind`, not the body's:
/// a caller cannot claim `aborted` on the way in to slip past the gate.
#[tokio::test]
async fn a_body_cannot_claim_aborted_to_pass_the_gate() {
    let h = harness();
    let job_id = open_job(&h.app).await;
    let job = get_job(&h.app, &job_id).await;
    let done = step_by_slug(&job, "done");

    let (status, body) = complete(
        &h.app,
        &job_id,
        &done,
        serde_json::json!({ "outcome_kind": "aborted", "reason": "smuggled" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a body-claimed outcome_kind must not open the gate: {body}"
    );
    assert_eq!(body["error"], "step has unresolved blockers");
}

/// The abort's own contract is still enforced: no reason, no abort.
#[tokio::test]
async fn an_aborted_terminal_without_its_reason_still_answers_400() {
    let h = harness();
    let job_id = open_job(&h.app).await;
    let job = get_job(&h.app, &job_id).await;
    let aborted = step_by_slug(&job, "aborted");

    let (status, body) = complete(&h.app, &job_id, &aborted, serde_json::json!({})).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an abort without its required reason must be refused: {body}"
    );
    assert!(
        body.as_str().is_some_and(|s| s.contains("reason")),
        "the refusal names the missing field: {body}"
    );
    let still_open = get_job(&h.app, &job_id).await;
    assert_eq!(still_open["status"], "open");
    assert_eq!(step_by_slug(&still_open, "aborted")["status"], "pending");
}
