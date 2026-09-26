//! A writer cannot steer a packet by its status, its assurance, or its
//! recorded outcome.
//!
//! Backlog 36352452 — the review of car 9392b8b5 (which made the job and
//! step PUTs refuse a reshaped protocol, b433bdf3) probed three roads it
//! left open, on the in-memory router:
//!
//! - (S1) The blocker gate skips a step stored Ready or Active, because
//!   the engine opened it. Nothing refused a HAND PUT of a Pending step
//!   to Ready or Active, so `PUT done {status: ready}` then `{status:
//!   completed}` closed the packet `done` while its work was never
//!   completed. A hand promotion out of Pending is now judged by the
//!   same blocker gate a completion is, so a step stored Ready or
//!   Active was opened by the engine or past its blockers.
//! - (S2) `assurance_required` was taken from the body: the in-memory
//!   adapter stored it, the Pg adapter silently dropped it, so `null`
//!   lowered a raised requirement in one and answered 204 over nothing
//!   in the other. `step_plugin_version` drifted the same way. Both are
//!   now part of a step's place in its protocol, refused like `kind`.
//! - (N1) After a terminal close, a job PUT `{status: open, metadata:
//!   {outcome: aborted}}` reopened the packet and rewrote its outcome,
//!   and `PATCH /metadata {outcome: forged}` on a closed packet answered
//!   204. A closed packet does not reopen, and `outcome` is written by
//!   the close — the metadata merge takes it only as the repair of a
//!   close that lost it, and only as the value its completed terminal
//!   declares.
//!
//! Each road is one test, refused out loud; the controls hold the
//! callers that are legitimate.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const KIND: &str = "status-steer-guard";
/// A kind the registry has no protocol for: nothing re-evaluates its
/// steps, so nothing but a writer can move them.
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
        writer: None,
    }
}

/// One piece of work with a required field, a presence-assured approval
/// behind it, then two terminals: the one the work leads to, and an
/// abort behind a flag.
fn spec() -> WorkflowSpec {
    let mut spec = WorkflowSpec::platform_seed(
        KIND,
        "Status steer guard",
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
                title: "approve".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                title_template: "Approve in person".into(),
                authority_role: Some("platform-admin".into()),
                assurance_required: Some(boss_core::job::Assurance::Presence),
                ..Default::default()
            },
            StepSpec {
                title: "done".into(),
                kind: "outcome".into(),
                ready_when: "steps.work.done".into(),
                title_template: "Done".into(),
                authority_role: Some("platform-admin".into()),
                metadata_defaults: json!({ "outcome_kind": "completed" }),
                terminal: Some(Terminal {
                    outcome: "done".into(),
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

/// The router, and the store under it — so a test can put a row in the
/// shape a past defect left it in, which no route can produce any more.
fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    app_with(true)
}

/// `protocols: false` plumbs no Workflow registry: every kind is
/// admitted, steps are added by hand, and nothing re-evaluates them.
fn app_with(protocols: bool) -> (axum::Router, Arc<InMemoryJobs>) {
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
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Close, Resource::job(), Scope::All)
            .allow(
                "platform-admin",
                Action::Create,
                Resource::step(),
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
        kind_registry: protocols.then_some(kinds as Arc<dyn WorkflowRegistry>),
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
) -> (StatusCode, Value) {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", admin_header());
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

async fn get_job(app: &axum::Router, job_id: &str) -> Value {
    let (status, job) = send(app, "GET", &format!("/api/jobs/{job_id}"), None).await;
    assert_eq!(status, StatusCode::OK, "read failed: {job}");
    job
}

async fn open_job_of(app: &axum::Router, kind: &str) -> String {
    let (status, job) = send(
        app,
        "POST",
        "/api/jobs",
        Some(json!({
            "kind": kind,
            "subject": { "subject_kind": "custom", "id": "/system/flow" },
            "title": "A packet someone tries to steer",
            "owner_id": "emp-bootstrap-admin",
            "priority": "standard",
            "status": "open",
            "metadata": {},
            "tags": ["test"],
        })),
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

async fn put_job(app: &axum::Router, job_id: &str, body: Value) -> (StatusCode, Value) {
    send(app, "PUT", &format!("/api/jobs/{job_id}"), Some(body)).await
}

async fn patch_job_metadata(app: &axum::Router, job_id: &str, patch: Value) -> (StatusCode, Value) {
    send(
        app,
        "PATCH",
        &format!("/api/jobs/{job_id}/metadata"),
        Some(patch),
    )
    .await
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

/// Complete `work` with its required evidence, so `done` is ready.
async fn finish_the_work(app: &axum::Router, job_id: &str) -> Value {
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
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the honest completion lands: {body}"
    );
    let job = get_job(app, job_id).await;
    assert_eq!(step_by_slug(&job, "done")["status"], "ready");
    job
}

/// Run the packet to its `done` terminal the honest way: it closes
/// with outcome `done`.
async fn close_through_done(app: &axum::Router, job_id: &str) -> Value {
    let job = finish_the_work(app, job_id).await;
    let done = step_by_slug(&job, "done");
    let (status, answer) = put_step(app, job_id, &done, json!({ "status": "completed" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{answer}");
    let closed = get_job(app, job_id).await;
    assert_eq!(closed["status"], "closed", "{closed:#}");
    assert_eq!(closed["metadata"]["outcome"], "done", "{closed:#}");
    closed
}

// ---------------------------------------------------------------------------
// S1 — a Pending step is not opened past its blockers
// ---------------------------------------------------------------------------

/// The review's road: the terminal is Pending behind `work`, which was
/// never done. A hand PUT to Ready (or Active) made the gate treat it
/// as opened by the engine, and the next PUT completed it and closed
/// the packet `done`.
#[tokio::test]
async fn a_hand_put_cannot_open_a_pending_step_to_skip_its_blockers() {
    let (app, _) = app();
    let job_id = open_job(&app).await;
    let job = get_job(&app, &job_id).await;
    let done = step_by_slug(&job, "done");
    let work_id = step_by_slug(&job, "work")["id"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(done["status"], "pending", "precondition: behind `work`");

    for opened in ["ready", "active"] {
        let (status, answer) = put_step(&app, &job_id, &done, json!({ "status": opened })).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "a hand PUT to {opened} must be refused: {answer}"
        );
        assert_eq!(answer["error"], "step has unresolved blockers");
        assert_eq!(
            answer["unresolved_blockers"],
            json!([format!("{work_id}=ready")]),
            "the refusal names what it waits on: {answer}"
        );
    }

    let (status, answer) = put_step(&app, &job_id, &done, json!({ "status": "completed" })).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the gate still holds: {answer}"
    );
    assert_eq!(answer["error"], "step has unresolved blockers");
    let still = get_job(&app, &job_id).await;
    assert_eq!(still["status"], "open", "nothing closed: {still:#}");
    assert!(still["metadata"].get("outcome").is_none(), "{still:#}");
    assert_eq!(step_by_slug(&still, "done")["status"], "pending");
}

/// Control: a step the ENGINE opened is still the executor's to start
/// and finish — Ready to Active by PUT, then completed — and a Pending
/// step whose protocol is satisfied is opened by the re-evaluator as
/// before.
#[tokio::test]
async fn a_step_the_engine_opened_is_still_started_and_finished_by_put() {
    let (app, _) = app();
    let job_id = open_job(&app).await;
    let job = finish_the_work(&app, &job_id).await;
    let done = step_by_slug(&job, "done");

    let (status, answer) = put_step(&app, &job_id, &done, json!({ "status": "active" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "ready to active: {answer}");
    let (status, answer) = put_step(&app, &job_id, &done, json!({ "status": "completed" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{answer}");
    let closed = get_job(&app, &job_id).await;
    assert_eq!(closed["status"], "closed");
    assert_eq!(closed["metadata"]["outcome"], "done");
}

/// Control: a Pending step with nothing blocking it still starts by
/// hand — the web's Start and the step plugins' saves move a Pending
/// step to `active` by PUT, and on a packet no engine re-evaluates that
/// is the only way it moves.
#[tokio::test]
async fn a_pending_step_with_nothing_blocking_it_still_starts_by_hand() {
    let (app, _) = app_with(false);
    let job_id = open_job_of(&app, LOOSE_KIND).await;
    let (status, step) = send(
        &app,
        "POST",
        &format!("/api/jobs/{job_id}/steps"),
        Some(json!({ "kind": "task", "title": "Loose work", "status": "pending" })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "add rejected: {step}");
    let (status, answer) = put_step(&app, &job_id, &step, json!({ "status": "active" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{answer}");
}

// ---------------------------------------------------------------------------
// S2 — assurance_required and step_plugin_version are the protocol's
// ---------------------------------------------------------------------------

/// `null` lowered a Presence requirement in memory, so the next bare
/// completion went through on a session. The Pg adapter dropped the
/// same write and answered 204. Refused on both, out loud.
#[tokio::test]
async fn a_step_put_cannot_lower_its_assurance() {
    let (app, _) = app();
    let job_id = open_job(&app).await;
    let job = get_job(&app, &job_id).await;
    let approve = step_by_slug(&job, "approve");
    assert_eq!(
        approve["assurance_required"], "presence",
        "precondition: {approve:#}"
    );

    let (status, answer) = put_step(
        &app,
        &job_id,
        &approve,
        json!({ "assurance_required": null }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a lowered requirement must be refused: {answer}"
    );
    assert_eq!(answer["refused_fields"], json!(["assurance_required"]));

    let (status, answer) = put_step(
        &app,
        &job_id,
        &approve,
        json!({ "status": "completed", "assurance_required": "session" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["refused_fields"], json!(["assurance_required"]));

    let (status, answer) =
        put_step(&app, &job_id, &approve, json!({ "status": "completed" })).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "the requirement stands: {answer}"
    );
    let still = get_job(&app, &job_id).await;
    let approve = step_by_slug(&still, "approve");
    assert_eq!(approve["assurance_required"], "presence");
    assert_eq!(approve["status"], "ready");
}

/// Nor can a body raise it: the requirement is a Workflow edit.
#[tokio::test]
async fn a_step_put_cannot_raise_its_assurance() {
    let (app, _) = app();
    let job_id = open_job(&app).await;
    let job = get_job(&app, &job_id).await;
    let work = step_by_slug(&job, "work");
    let (status, answer) = put_step(
        &app,
        &job_id,
        &work,
        json!({ "assurance_required": "presence" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["refused_fields"], json!(["assurance_required"]));
}

/// The plugin version a step renders under was pinned when the step
/// was written; the Pg adapter never wrote a body's, the in-memory one
/// did.
#[tokio::test]
async fn a_step_put_cannot_move_its_plugin_version() {
    let (app, _) = app();
    let job_id = open_job(&app).await;
    let job = get_job(&app, &job_id).await;
    let work = step_by_slug(&job, "work");
    let pinned = work["step_plugin_version"].clone();
    let moved = pinned.as_i64().unwrap_or_default() + 4;

    let (status, answer) = put_step(
        &app,
        &job_id,
        &work,
        json!({ "step_plugin_version": moved }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["refused_fields"], json!(["step_plugin_version"]));
    let still = get_job(&app, &job_id).await;
    assert_eq!(step_by_slug(&still, "work")["step_plugin_version"], pinned);
}

/// Control: the whole step sent back as read — assurance and plugin
/// version included — with one field changed still lands.
#[tokio::test]
async fn a_step_sent_back_as_read_still_lands() {
    let (app, _) = app();
    let job_id = open_job(&app).await;
    let job = get_job(&app, &job_id).await;
    let mut approve = step_by_slug(&job, "approve");
    approve["title"] = json!("Approve, in person");
    let (status, answer) = put_step(&app, &job_id, &approve.clone(), approve).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{answer}");
    let still = get_job(&app, &job_id).await;
    let approve = step_by_slug(&still, "approve");
    assert_eq!(approve["title"], "Approve, in person");
    assert_eq!(approve["assurance_required"], "presence");
}

// ---------------------------------------------------------------------------
// N1 — a closed packet stays closed, and its outcome is the close's
// ---------------------------------------------------------------------------

/// The review's road: after the terminal close, a job PUT reopened the
/// packet and rewrote the outcome in one write.
#[tokio::test]
async fn a_job_put_cannot_reopen_a_closed_packet() {
    let (app, _) = app();
    let job_id = open_job(&app).await;
    let closed = close_through_done(&app, &job_id).await;

    let mut body = job_body(&closed);
    body["status"] = json!("open");
    body["metadata"]["outcome"] = json!("aborted");
    let (status, answer) = put_job(&app, &job_id, body.clone()).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a reopen must be refused: {answer}"
    );
    assert_eq!(answer["stored_status"], "closed", "{answer}");

    // The reopen alone, outcome untouched, is the same refusal.
    let mut body = job_body(&closed);
    body["status"] = json!("open");
    let (status, answer) = put_job(&app, &job_id, body).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");

    let still = get_job(&app, &job_id).await;
    assert_eq!(still["status"], "closed", "{still:#}");
    assert_eq!(still["metadata"]["outcome"], "done", "{still:#}");
}

/// Nor may a PUT that leaves it closed rewrite what it closed with.
#[tokio::test]
async fn a_job_put_cannot_rewrite_a_closed_packets_outcome() {
    let (app, _) = app();
    let job_id = open_job(&app).await;
    let closed = close_through_done(&app, &job_id).await;

    for forged in [json!("aborted"), Value::Null] {
        let mut body = job_body(&closed);
        body["metadata"]["outcome"] = forged.clone();
        let (status, answer) = put_job(&app, &job_id, body).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "outcome {forged} must be refused: {answer}"
        );
        assert_eq!(answer["refused_keys"], json!(["outcome"]), "{answer}");
    }
    let still = get_job(&app, &job_id).await;
    assert_eq!(still["metadata"]["outcome"], "done", "{still:#}");
}

/// Control: a read-modify-write of a closed packet that leaves the
/// outcome as it stands — or was built without it — still lands, and
/// keeps it.
#[tokio::test]
async fn a_job_put_that_leaves_the_outcome_alone_still_lands() {
    let (app, _) = app();
    let job_id = open_job(&app).await;
    let closed = close_through_done(&app, &job_id).await;

    let mut body = job_body(&closed);
    body["title"] = json!("Retitled after the close");
    let (status, answer) = put_job(&app, &job_id, body.clone()).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{answer}");

    body["metadata"]
        .as_object_mut()
        .expect("metadata")
        .remove("outcome");
    body["title"] = json!("Retitled again");
    let (status, answer) = put_job(&app, &job_id, body).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{answer}");
    let still = get_job(&app, &job_id).await;
    assert_eq!(still["title"], "Retitled again");
    assert_eq!(
        still["metadata"]["outcome"], "done",
        "carried forward: {still:#}"
    );
}

/// The merge door answered 204 to a forged outcome on a closed packet.
/// Set, changed or deleted, it is refused; and on an open packet, where
/// no close has written one, a merge cannot write one ahead of it.
#[tokio::test]
async fn the_metadata_merge_cannot_write_an_outcome() {
    let (app, _) = app();
    let open_id = open_job(&app).await;
    let (status, answer) = patch_job_metadata(&app, &open_id, json!({ "outcome": "done" })).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an open packet has no outcome to write: {answer}"
    );

    let job_id = open_job(&app).await;
    close_through_done(&app, &job_id).await;
    for forged in [json!("forged"), Value::Null] {
        let (status, answer) =
            patch_job_metadata(&app, &job_id, json!({ "outcome": forged.clone() })).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "outcome {forged} must be refused: {answer}"
        );
        assert!(
            answer["hint"]
                .as_str()
                .is_some_and(|h| h.contains("boss job outcome")),
            "the refusal names the repair door: {answer}"
        );
    }
    let still = get_job(&app, &job_id).await;
    assert_eq!(still["metadata"]["outcome"], "done", "{still:#}");

    // Control: an unchanged re-send beside another key is not a change.
    let (status, answer) = patch_job_metadata(
        &app,
        &job_id,
        json!({ "outcome": "done", "note": "annotated after the close" }),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{answer}");
    let still = get_job(&app, &job_id).await;
    assert_eq!(still["metadata"]["note"], "annotated after the close");
}

/// The legitimate writer: `boss job outcome` (228c9a7d) repairs a close
/// that lost its outcome by merging the value its completed terminal
/// declares. That still lands — and only that value does.
#[tokio::test]
async fn the_repair_of_a_lost_outcome_still_lands_and_only_as_derived() {
    let (app, jobs) = app();
    let job_id = open_job(&app).await;
    close_through_done(&app, &job_id).await;

    // The shape car 6b23d135 was left in: closed through `done`, the
    // outcome erased by a racing whole-row close. No route can produce
    // it now, so the row is put that way underneath the router.
    let id = boss_core::job::JobId::from_uuid(job_id.parse().expect("uuid"));
    let mut lost = jobs.get_job(&id).await.unwrap().expect("job");
    lost.metadata
        .as_object_mut()
        .expect("metadata")
        .remove("outcome");
    jobs.update_job_at(&lost, lost.status, chrono::Utc::now(), &[])
        .await
        .expect("strip the outcome");
    assert!(
        get_job(&app, &job_id).await["metadata"]
            .get("outcome")
            .is_none()
    );

    let (status, answer) = patch_job_metadata(&app, &job_id, json!({ "outcome": "aborted" })).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "only the completed terminal's outcome may be written: {answer}"
    );
    let (status, answer) = patch_job_metadata(&app, &job_id, json!({ "outcome": "done" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "the repair lands: {answer}");
    assert_eq!(get_job(&app, &job_id).await["metadata"]["outcome"], "done");
}
