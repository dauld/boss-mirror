//! A packet closes only through work its protocol saw done.
//!
//! Backlog 570e72bd — the review of car d4d49a00 (which judged a hand
//! promotion out of Pending by the blocker gate, 36352452) named five
//! roads left with the same impact, each a way to close a packet through
//! a terminal whose work was never done, or to rewrite how it ended:
//!
//! - (1) A hand PUT of a step to `skipped` went through no gate at all:
//!   no blockers, no required-at-done fields. The blocker gate reads a
//!   Skipped blocker as resolved, so skipping the work opened the way
//!   to the terminal behind it.
//! - (4) The gate read the step's `blocked_by` edge list, never its
//!   `ready_when`. A terminal waiting on `steps.review.done AND
//!   job.metadata.merged = "true"` was completed by hand the moment
//!   review was done, without the marker.
//! - (2) Admission took a body with `status: closed` and any outcome.
//! - (3) A closed packet could be moved to cancelled and back, and the
//!   move back re-emitted JOB_CLOSED, re-firing every close rule; and
//!   a cancel — an end state — needed only the Update authority.
//! - (5) The reopen refusal was a read, then a whole-row write: a close
//!   landing between them was overwritten (the storage half is pinned
//!   in `a_finished_packets_status_does_not_move_pg.rs`).
//!
//! Each road is one test, refused out loud; the controls hold the
//! callers that are legitimate — the abort, a step outside the
//! protocol, the marker-then-complete order every in-tree closer uses.

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
use serde_json::{Value, json};
use tower::ServiceExt;

const KIND: &str = "closes-through-work";
/// A kind the registry has no protocol for.
const LOOSE_KIND: &str = "no-protocol-here";

fn required(name: &str) -> boss_core::job::StepField {
    boss_core::job::StepField {
        name: name.into(),
        field_type: "string".into(),
        required: true,
        filled_by: Default::default(),
        item_keys: Vec::new(),
        covers: None,
        binds: None,
        item_value_max_bytes: None,
        item_one_of: Vec::new(),
    }
}

/// The ship-a-change shape in miniature: work, a review behind it, a
/// terminal that waits on the review AND a job-metadata marker (the
/// `merged` terminal, ship-a-change.toml), and an abort behind a flag.
fn spec() -> WorkflowSpec {
    let mut spec = WorkflowSpec::platform_seed(
        KIND,
        "Closes through work",
        "platform",
        vec!["custom".into()],
        vec![
            StepSpec {
                title: "work".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                title_template: "The work".into(),
                authority_role: Some("platform-admin".into()),
                fields: vec![required("evidence")],
                ..Default::default()
            },
            StepSpec {
                title: "review".into(),
                kind: "task".into(),
                ready_when: "steps.work.done".into(),
                title_template: "Review the work".into(),
                authority_role: Some("platform-admin".into()),
                ..Default::default()
            },
            StepSpec {
                title: "merged".into(),
                kind: "outcome".into(),
                ready_when: "steps.review.done AND job.metadata.merged = \"true\"".into(),
                title_template: "Merged".into(),
                authority_role: Some("platform-admin".into()),
                metadata_defaults: json!({ "outcome_kind": "completed" }),
                terminal: Some(Terminal {
                    outcome: "merged".into(),
                }),
                ..Default::default()
            },
            StepSpec {
                title: "aborted".into(),
                kind: "outcome".into(),
                ready_when: "steps.work.done AND job.metadata.abandoned = \"true\"".into(),
                title_template: "Aborted".into(),
                authority_role: Some("platform-admin".into()),
                metadata_defaults: json!({ "outcome_kind": "aborted" }),
                terminal: Some(Terminal {
                    outcome: "aborted".into(),
                }),
                ..Default::default()
            },
        ],
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
        Ok(matches!(id, "emp-bootstrap-admin" | "emp-editor"))
    }
}

fn header(id: &str, role: &str) -> String {
    json!({
        "id": id,
        "role": role,
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

fn admin_header() -> String {
    header("emp-bootstrap-admin", "platform-admin")
}

/// A role that may edit a packet but not end it — `update` without
/// `close`, a shape the demo tenant's policy seeds grant to two roles.
fn editor_header() -> String {
    header("emp-editor", "job-editor")
}

fn app() -> axum::Router {
    app_with(true)
}

/// `protocols: false` plumbs no Workflow registry: every kind is
/// admitted, steps are added by hand, and nothing re-evaluates them.
fn app_with(protocols: bool) -> axum::Router {
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds.seed(spec()).expect("seed the kind");
    let jobs = Arc::new(InMemoryJobs::new());
    let mut policy = FakePolicyClient::builder();
    for action in [Action::Create, Action::Read, Action::Update, Action::Close] {
        policy = policy.allow("platform-admin", action, Resource::job(), Scope::All);
    }
    for action in [Action::Create, Action::Update] {
        policy = policy.allow("platform-admin", action, Resource::step(), Scope::All);
    }
    for action in [Action::Read, Action::Update] {
        policy = policy.allow("job-editor", action, Resource::job(), Scope::All);
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: protocols.then_some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    router(state)
}

async fn send_as(
    app: &axum::Router,
    user: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", user);
    let req = match body {
        Some(b) => builder.body(Body::from(b.to_string())),
        None => builder.body(Body::empty()),
    }
    .expect("request builds");
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

async fn send(
    app: &axum::Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    send_as(app, &admin_header(), method, uri, body).await
}

async fn get_job(app: &axum::Router, job_id: &str) -> Value {
    let (status, job) = send(app, "GET", &format!("/api/jobs/{job_id}"), None).await;
    assert_eq!(status, StatusCode::OK, "read failed: {job}");
    job
}

fn packet_body(kind: &str, status: &str, metadata: Value) -> Value {
    json!({
        "kind": kind,
        "subject": { "subject_kind": "custom", "id": "/system/flow" },
        "title": "A packet someone tries to close without its work",
        "owner_id": "emp-bootstrap-admin",
        "priority": "standard",
        "status": status,
        "metadata": metadata,
        "tags": ["test"],
    })
}

async fn open_job_of(app: &axum::Router, kind: &str) -> String {
    let (status, job) = send(
        app,
        "POST",
        "/api/jobs",
        Some(packet_body(kind, "open", json!({}))),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    job["id"].as_str().expect("job id").to_string()
}

async fn open_job(app: &axum::Router) -> String {
    open_job_of(app, KIND).await
}

fn step_by_slug(job: &Value, slug: &str) -> Value {
    job["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["spec_slug"] == slug)
        .cloned()
        .unwrap_or_else(|| panic!("no step with slug `{slug}` on {job:#}"))
}

async fn put_step(
    app: &axum::Router,
    job_id: &str,
    step: &Value,
    body: Value,
) -> (StatusCode, Value) {
    let step_id = step["id"].as_str().expect("step id");
    send(
        app,
        "PUT",
        &format!("/api/jobs/{job_id}/steps/{step_id}"),
        Some(body),
    )
    .await
}

async fn put_job_as(
    app: &axum::Router,
    user: &str,
    job_id: &str,
    body: Value,
) -> (StatusCode, Value) {
    send_as(app, user, "PUT", &format!("/api/jobs/{job_id}"), Some(body)).await
}

async fn put_job(app: &axum::Router, job_id: &str, body: Value) -> (StatusCode, Value) {
    put_job_as(app, &admin_header(), job_id, body).await
}

/// The job body a read-modify-write caller sends back: the packet read,
/// minus its steps.
fn job_body(job: &Value) -> Value {
    let mut body = job.clone();
    body.as_object_mut()
        .expect("job is an object")
        .remove("steps");
    body
}

/// Complete `work` with its evidence, then `review`: `merged` is left
/// Pending, waiting on the marker only.
async fn do_the_work_and_review_it(app: &axum::Router, job_id: &str) -> Value {
    let job = get_job(app, job_id).await;
    let work = step_by_slug(&job, "work");
    let mut metadata = work["metadata"].clone();
    metadata["evidence"] = json!("measured");
    let (status, body) = put_step(
        app,
        job_id,
        &work,
        json!({ "status": "completed", "metadata": metadata }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "the work lands: {body}");
    let job = get_job(app, job_id).await;
    let review = step_by_slug(&job, "review");
    assert_eq!(review["status"], "ready", "{job:#}");
    let (status, body) = put_step(app, job_id, &review, json!({ "status": "completed" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "the review lands: {body}");
    let job = get_job(app, job_id).await;
    assert_eq!(
        step_by_slug(&job, "merged")["status"],
        "pending",
        "precondition: the terminal waits on its marker: {job:#}"
    );
    job
}

/// The honest road to `merged`: the work, the review, the marker (the
/// conductor's order — marker first, then the completion), then the
/// terminal the marker readied.
async fn close_through_merged(app: &axum::Router, job_id: &str) -> Value {
    do_the_work_and_review_it(app, job_id).await;
    let (status, answer) = send(
        app,
        "PATCH",
        &format!("/api/jobs/{job_id}/metadata"),
        Some(json!({ "merged": "true" })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{answer}");
    let job = get_job(app, job_id).await;
    let merged = step_by_slug(&job, "merged");
    assert_eq!(merged["status"], "ready", "the marker readied it: {job:#}");
    let (status, answer) = put_step(app, job_id, &merged, json!({ "status": "completed" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{answer}");
    let closed = get_job(app, job_id).await;
    assert_eq!(closed["status"], "closed", "{closed:#}");
    assert_eq!(closed["metadata"]["outcome"], "merged", "{closed:#}");
    closed
}

fn assert_still_open(job: &Value) {
    assert_eq!(job["status"], "open", "nothing closed: {job:#}");
    assert!(job["metadata"].get("outcome").is_none(), "{job:#}");
}

// ---------------------------------------------------------------------------
// (1) a hand skip of a protocol step
// ---------------------------------------------------------------------------

/// The review's road: skip the work by hand, and the terminal behind it
/// sees a Skipped blocker as resolved. Refused at the skip, and the
/// terminal stays behind the work.
#[tokio::test]
async fn a_hand_skip_of_a_protocol_step_is_refused() {
    let app = app();
    let job_id = open_job(&app).await;
    let job = get_job(&app, &job_id).await;
    let work = step_by_slug(&job, "work");
    let review = step_by_slug(&job, "review");
    assert_eq!(work["status"], "ready", "precondition: {job:#}");
    assert_eq!(review["status"], "pending", "precondition: {job:#}");

    for (step, slug) in [(&work, "work"), (&review, "review")] {
        let (status, answer) = put_step(&app, &job_id, step, json!({ "status": "skipped" })).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "a hand skip of `{slug}` must be refused: {answer}"
        );
        assert_eq!(
            answer["error"], "a protocol step is skipped by its protocol, not by hand",
            "{answer}"
        );
        assert!(
            answer["hint"].as_str().is_some_and(|h| h.contains("abort")),
            "the refusal names the door that stops work: {answer}"
        );
    }

    // And the terminal behind the work is still behind it.
    let merged = step_by_slug(&job, "merged");
    let (status, answer) = put_step(&app, &job_id, &merged, json!({ "status": "completed" })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    let still = get_job(&app, &job_id).await;
    assert_still_open(&still);
    assert_eq!(step_by_slug(&still, "work")["status"], "ready");
    assert_eq!(step_by_slug(&still, "review")["status"], "pending");
}

/// Nor is a terminal skipped by hand: that closes nothing with an
/// outcome, but every step terminal closes the packet through the
/// catch-all with no terminal done.
#[tokio::test]
async fn a_hand_skip_of_a_terminal_is_refused() {
    let app = app();
    let job_id = open_job(&app).await;
    let job = do_the_work_and_review_it(&app, &job_id).await;
    for slug in ["merged", "aborted"] {
        let step = step_by_slug(&job, slug);
        let (status, answer) = put_step(&app, &job_id, &step, json!({ "status": "skipped" })).await;
        assert_eq!(status, StatusCode::CONFLICT, "`{slug}`: {answer}");
    }
    assert_still_open(&get_job(&app, &job_id).await);
}

/// Control: a step no protocol describes — on a kind the registry has
/// no protocol for, where steps are added by hand — has no predicate to
/// judge and nothing downstream reads it, so a hand skip still lands,
/// as before. (A step cannot be added to a protocol packet at all: its
/// steps are fixed at admission, so every step of one is the
/// protocol's.)
#[tokio::test]
async fn a_step_outside_any_protocol_is_still_skipped_by_hand() {
    let app = app_with(false);
    let job_id = open_job_of(&app, LOOSE_KIND).await;
    let (status, step) = send(
        &app,
        "POST",
        &format!("/api/jobs/{job_id}/steps"),
        Some(json!({ "kind": "task", "title": "Ad-hoc chore", "status": "ready" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add rejected: {step}");
    let (status, answer) = put_step(&app, &job_id, &step, json!({ "status": "skipped" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{answer}");
}

/// Control: the abort — the protocol's own door for stopping work — still
/// completes from any open state, with the work never done, and closes
/// the packet `aborted`, which is what happened.
#[tokio::test]
async fn the_abort_still_completes_from_any_open_state() {
    let app = app();
    let job_id = open_job(&app).await;
    let job = get_job(&app, &job_id).await;
    let aborted = step_by_slug(&job, "aborted");
    assert_eq!(aborted["status"], "pending", "precondition: {job:#}");
    let (status, answer) =
        put_step(&app, &job_id, &aborted, json!({ "status": "completed" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{answer}");
    let closed = get_job(&app, &job_id).await;
    assert_eq!(closed["status"], "closed", "{closed:#}");
    assert_eq!(closed["metadata"]["outcome"], "aborted", "{closed:#}");
}

// ---------------------------------------------------------------------------
// (4) the gate evaluates the step's own ready_when
// ---------------------------------------------------------------------------

/// The review's road: every blocker done, the marker absent. The edge
/// list is satisfied and the predicate is not; the predicate decides.
#[tokio::test]
async fn a_terminal_waiting_on_a_job_marker_is_not_completed_without_it() {
    let app = app();
    let job_id = open_job(&app).await;
    let job = do_the_work_and_review_it(&app, &job_id).await;
    let merged = step_by_slug(&job, "merged");

    for moved in ["completed", "active", "ready"] {
        let (status, answer) = put_step(&app, &job_id, &merged, json!({ "status": moved })).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "`merged` to {moved} without the marker must be refused: {answer}"
        );
        assert_eq!(
            answer["error"], "step's ready_when does not hold",
            "{answer}"
        );
        assert_eq!(
            answer["ready_when"], "steps.review.done AND job.metadata.merged = \"true\"",
            "the refusal names the predicate it read: {answer}"
        );
    }
    let still = get_job(&app, &job_id).await;
    assert_still_open(&still);
    assert_eq!(step_by_slug(&still, "merged")["status"], "pending");
}

/// Control: the marker-then-complete order every in-tree closer uses
/// (the conductor, `boss car retire`, `boss disprove`, the sweep judge)
/// still closes the packet through its terminal.
#[tokio::test]
async fn the_marker_then_the_completion_still_closes_the_packet() {
    let app = app();
    let job_id = open_job(&app).await;
    close_through_merged(&app, &job_id).await;
}

// ---------------------------------------------------------------------------
// (2) admission
// ---------------------------------------------------------------------------

/// A packet was admitted already closed, with any outcome it named.
/// It is admitted open (or draft) and reaches its end through its
/// protocol or a close — and no outcome rides in with it.
#[tokio::test]
async fn admission_refuses_a_packet_born_finished_or_with_an_outcome() {
    let app = app();
    for (status_word, metadata) in [
        ("closed", json!({ "outcome": "merged" })),
        ("closed", json!({})),
        ("cancelled", json!({})),
        ("open", json!({ "outcome": "merged" })),
    ] {
        let (status, answer) = send(
            &app,
            "POST",
            "/api/jobs",
            Some(packet_body(KIND, status_word, metadata.clone())),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "status {status_word} with {metadata} must be refused: {answer}"
        );
    }
    let (status, list) = send(&app, "GET", &format!("/api/jobs?kind={KIND}"), None).await;
    assert_eq!(status, StatusCode::OK, "{list}");
    assert_eq!(list["total"], 0, "nothing was admitted: {list:#}");
}

/// Control: open and draft are still admitted.
#[tokio::test]
async fn admission_still_takes_an_open_or_draft_packet() {
    let app = app();
    for status_word in ["open", "draft"] {
        let (status, answer) = send(
            &app,
            "POST",
            "/api/jobs",
            Some(packet_body(KIND, status_word, json!({}))),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{status_word}: {answer}");
    }
}

// ---------------------------------------------------------------------------
// (3) end states do not flip, and a cancel is an end
// ---------------------------------------------------------------------------

/// Closed to cancelled and back re-emitted JOB_CLOSED and re-fired every
/// close rule (spawn, clear_waiting, the subjob resolve). A finished
/// packet's end state does not move.
#[tokio::test]
async fn a_finished_packets_end_state_does_not_flip() {
    let app = app();
    let job_id = open_job(&app).await;
    let closed = close_through_merged(&app, &job_id).await;

    let mut body = job_body(&closed);
    body["status"] = json!("cancelled");
    let (status, answer) = put_job(&app, &job_id, body).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "closed to cancelled must be refused: {answer}"
    );
    assert_eq!(answer["stored_status"], "closed", "{answer}");
    let still = get_job(&app, &job_id).await;
    assert_eq!(still["status"], "closed", "{still:#}");
    assert_eq!(still["metadata"]["outcome"], "merged", "{still:#}");

    // Cancelled to closed: the move that re-emitted JOB_CLOSED.
    let other = open_job(&app).await;
    let mut body = job_body(&get_job(&app, &other).await);
    body["status"] = json!("cancelled");
    let (status, answer) = put_job(&app, &other, body).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "the cancel lands: {answer}");
    let cancelled = get_job(&app, &other).await;
    assert_eq!(cancelled["status"], "cancelled", "{cancelled:#}");
    let mut body = job_body(&cancelled);
    body["status"] = json!("closed");
    let (status, answer) = put_job(&app, &other, body).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "cancelled to closed must be refused: {answer}"
    );
    let (status, events) = send(&app, "GET", &format!("/api/jobs/{other}/events"), None).await;
    assert_eq!(status, StatusCode::OK, "{events}");
    let closes = events["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["kind"] == boss_jobs::events::JOB_CLOSED)
        .count();
    assert_eq!(closes, 0, "no close marker for a packet that never closed");
    assert_eq!(get_job(&app, &other).await["status"], "cancelled");
}

/// A cancel ends a packet exactly as a close does, so it takes the same
/// authority: a role that may edit a packet but not close it may not
/// cancel it either.
#[tokio::test]
async fn a_cancel_takes_the_close_authority() {
    let app = app();
    let job_id = open_job(&app).await;
    let job = get_job(&app, &job_id).await;

    // Control: the editor may edit.
    let mut body = job_body(&job);
    body["title"] = json!("Retitled by an editor");
    let (status, answer) = put_job_as(&app, &editor_header(), &job_id, body.clone()).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{answer}");

    body["status"] = json!("cancelled");
    let (status, answer) = put_job_as(&app, &editor_header(), &job_id, body).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a cancel without the close authority must be refused: {answer}"
    );
    assert_eq!(get_job(&app, &job_id).await["status"], "open");
}
