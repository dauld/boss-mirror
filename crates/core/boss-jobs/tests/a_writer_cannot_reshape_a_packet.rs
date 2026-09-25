//! A writer cannot reshape a packet through the job and step PUTs.
//!
//! A packet is an envelope plus a protocol FIXED AT ADMISSION: the kind
//! and version it was admitted under, and each step's place in that
//! protocol — its kind, its slug, its index (which is how a completed
//! step is paired back to its spec, and so which terminal closes the
//! packet), the edges that order it, and the fields required at done.
//! Moving a packet between versions is an explicit, recorded act (`boss
//! job convert`, design 7cf202a9); nothing else may move any of these.
//!
//! Backlog b433bdf3 (the adversarial review of car 90bccc49, read on
//! origin/main c5ff2f5a) measured that both PUTs overlaid them from the
//! body: the job PUT took `kind` (and, in memory, `workflow_version`),
//! and the step PUT took `sort_order`, `blocked_by`, `kind`, `fields`
//! and `spec_slug`. Each test below is one road that review found, and
//! each is now refused out loud — never 204'd and dropped, which is the
//! silent-write class this crate keeps paying for (09576fab, 903e6b90).
//!
//! A read-modify-write caller that sends every field back UNCHANGED is
//! untouched; the controls below hold that too.

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

const KIND: &str = "reshape-guard";

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

/// One piece of work with a required field, then two terminals: the
/// one the work leads to, and an abort behind a flag. The two terminals
/// differ only in their index and their `outcome_kind` — exactly the
/// two things a body could forge.
fn spec() -> WorkflowSpec {
    let mut spec = WorkflowSpec::platform_seed(
        KIND,
        "Reshape guard",
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

fn app() -> axum::Router {
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
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    router(state)
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

async fn open_job(app: &axum::Router) -> String {
    let (status, job) = send(
        app,
        "POST",
        "/api/jobs",
        Some(json!({
            "kind": KIND,
            "subject": { "subject_kind": "custom", "id": "/system/flow" },
            "title": "A packet someone tries to reshape",
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

// ---------------------------------------------------------------------------
// The job PUT
// ---------------------------------------------------------------------------

/// The review's first road: PUT the job to a kind with no protocol, add
/// a forged step while no spec can judge it, PUT the kind back. The kind
/// a packet was admitted under does not move on this route.
#[tokio::test]
async fn a_job_put_cannot_change_the_kind() {
    let app = app();
    let job_id = open_job(&app).await;
    let mut body = job_body(&get_job(&app, &job_id).await);
    body["kind"] = json!("no-such-kind");

    let (status, answer) = send(&app, "PUT", &format!("/api/jobs/{job_id}"), Some(body)).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a kind change must be refused: {answer}"
    );
    assert_eq!(
        answer["refused_fields"],
        json!(["kind"]),
        "the refusal names the field: {answer}"
    );
    assert!(
        answer["hint"]
            .as_str()
            .is_some_and(|h| h.contains("boss job convert")),
        "the refusal names the door that moves a packet: {answer}"
    );
    assert_eq!(get_job(&app, &job_id).await["kind"], KIND);
}

/// And the version it was admitted under: moving one is `boss job
/// convert`, which records the move. (The in-memory adapter used to
/// store the body's version, the Pg adapter not — the two disagreed.)
#[tokio::test]
async fn a_job_put_cannot_change_the_workflow_version() {
    let app = app();
    let job_id = open_job(&app).await;
    let mut body = job_body(&get_job(&app, &job_id).await);
    body["workflow_version"] = json!(7);

    let (status, answer) = send(&app, "PUT", &format!("/api/jobs/{job_id}"), Some(body)).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a version change must be refused: {answer}"
    );
    assert_eq!(answer["refused_fields"], json!(["workflow_version"]));
    assert_eq!(get_job(&app, &job_id).await["workflow_version"], 1);
}

/// Control: the read-modify-write caller — every field sent back as
/// read, one changed — is untouched. So is a body that omits the
/// version, which deserialises to a default and must not read as a move.
#[tokio::test]
async fn a_job_put_that_resends_the_kind_and_version_still_lands() {
    let app = app();
    let job_id = open_job(&app).await;
    let mut body = job_body(&get_job(&app, &job_id).await);
    body["title"] = json!("Retitled");
    let (status, answer) = send(
        &app,
        "PUT",
        &format!("/api/jobs/{job_id}"),
        Some(body.clone()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "an unchanged kind is not a move: {answer}"
    );
    assert_eq!(get_job(&app, &job_id).await["title"], "Retitled");

    body.as_object_mut().unwrap().remove("workflow_version");
    body["title"] = json!("Retitled again");
    let (status, answer) = send(&app, "PUT", &format!("/api/jobs/{job_id}"), Some(body)).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "an omitted version is not a move: {answer}"
    );
    let job = get_job(&app, &job_id).await;
    assert_eq!(job["title"], "Retitled again");
    assert_eq!(job["workflow_version"], 1);
}

// ---------------------------------------------------------------------------
// The step PUT
// ---------------------------------------------------------------------------

/// A completed step is paired back to its spec BY INDEX, and the spec
/// at that index says which terminal closes the packet. A body that
/// moved `sort_order` onto the abort's index closed a finished packet
/// as `aborted`.
#[tokio::test]
async fn a_step_put_cannot_choose_which_terminal_closes_the_packet() {
    let app = app();
    let job_id = open_job(&app).await;
    let job = finish_the_work(&app, &job_id).await;
    let done = step_by_slug(&job, "done");
    let aborted_index = step_by_slug(&job, "aborted")["sort_order"].clone();

    let (status, answer) = put_step(
        &app,
        &job_id,
        &done,
        json!({ "status": "completed", "sort_order": aborted_index }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a moved index must be refused: {answer}"
    );
    assert_eq!(answer["refused_fields"], json!(["sort_order"]));
    let still = get_job(&app, &job_id).await;
    assert_eq!(still["status"], "open", "nothing closed: {still:#}");
    assert_eq!(step_by_slug(&still, "done")["status"], "ready");

    // Control: the whole step sent back as read, status flipped — the
    // packet closes on the terminal it actually reached.
    let mut whole = step_by_slug(&still, "done");
    whole["status"] = json!("completed");
    let (status, answer) = put_step(&app, &job_id, &done, whole).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "a full round-trip still lands: {answer}"
    );
    let closed = get_job(&app, &job_id).await;
    assert_eq!(closed["status"], "closed");
    assert_eq!(closed["metadata"]["outcome"], "done", "{closed:#}");
}

/// The blocker gate reads `blocked_by`; a body that emptied it completed
/// a step the engine had never opened.
#[tokio::test]
async fn a_step_put_cannot_clear_its_blockers_to_complete_out_of_order() {
    let app = app();
    let job_id = open_job(&app).await;
    let job = get_job(&app, &job_id).await;
    let done = step_by_slug(&job, "done");
    assert_eq!(
        done["status"], "pending",
        "precondition: behind its blocker"
    );
    assert_ne!(
        done["blocked_by"],
        json!([]),
        "precondition: it has a blocker"
    );

    let (status, answer) = put_step(
        &app,
        &job_id,
        &done,
        json!({ "status": "completed", "blocked_by": [] }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "an emptied edge list must be refused: {answer}"
    );
    assert_eq!(answer["refused_fields"], json!(["blocked_by"]));
    let still = get_job(&app, &job_id).await;
    assert_eq!(still["status"], "open");
    assert_eq!(step_by_slug(&still, "done")["status"], "pending");
}

/// Required-at-done is judged against the step's `fields`; a body that
/// sent `fields: []` completed without the evidence the protocol asks
/// for.
#[tokio::test]
async fn a_step_put_cannot_drop_its_required_fields() {
    let app = app();
    let job_id = open_job(&app).await;
    let job = get_job(&app, &job_id).await;
    let work = step_by_slug(&job, "work");
    assert_eq!(
        work["fields"][0]["name"], "evidence",
        "precondition: {work:#}"
    );

    let (status, answer) = put_step(
        &app,
        &job_id,
        &work,
        json!({ "status": "completed", "fields": [] }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a dropped contract must be refused: {answer}"
    );
    assert_eq!(answer["refused_fields"], json!(["fields"]));
    let still = get_job(&app, &job_id).await;
    let work = step_by_slug(&still, "work");
    assert_eq!(work["status"], "ready");
    assert_eq!(
        work["fields"][0]["name"], "evidence",
        "the contract stands: {work:#}"
    );
}

/// The kind sets the kind bundle's required fields and the assurance
/// floor; neither is the body's to choose. Nor is the slug, which names
/// the step to every rule and predicate.
#[tokio::test]
async fn a_step_put_cannot_change_its_kind_or_slug() {
    let app = app();
    let job_id = open_job(&app).await;
    let job = get_job(&app, &job_id).await;
    let work = step_by_slug(&job, "work");

    let (status, answer) = put_step(
        &app,
        &job_id,
        &work,
        json!({ "kind": "outcome", "spec_slug": "done" }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["refused_fields"], json!(["kind", "spec_slug"]));
    let still = get_job(&app, &job_id).await;
    let work = step_by_slug(&still, "work");
    assert_eq!(work["kind"], "task");
}

// ---------------------------------------------------------------------------
// outcome_kind — the abort exemption's key
// ---------------------------------------------------------------------------

/// The abort-from-any-state exemption reads the STORED `outcome_kind`.
/// The merge door wrote any key, so a PATCH of `outcome_kind: aborted`
/// onto an ordinary terminal, then a bare completing PUT, walked past
/// the blocker gate.
#[tokio::test]
async fn the_merge_door_cannot_write_outcome_kind() {
    let app = app();
    let job_id = open_job(&app).await;
    let job = get_job(&app, &job_id).await;
    let done = step_by_slug(&job, "done");
    let step_id = done["id"].as_str().unwrap();

    let (status, answer) = send(
        &app,
        "PATCH",
        &format!("/api/jobs/{job_id}/steps/{step_id}/metadata"),
        Some(json!({ "outcome_kind": "aborted" })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the merge door must refuse it: {answer}"
    );
    assert_eq!(answer["refused_keys"], json!(["outcome_kind"]));

    // Nor can it be deleted, which would turn a real abort into a gated step.
    let (status, answer) = send(
        &app,
        "PATCH",
        &format!("/api/jobs/{job_id}/steps/{step_id}/metadata"),
        Some(json!({ "outcome_kind": null })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");

    let (status, answer) = put_step(&app, &job_id, &done, json!({ "status": "completed" })).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the gate still holds: {answer}"
    );
    assert_eq!(answer["error"], "step has unresolved blockers");
    let still = get_job(&app, &job_id).await;
    assert_eq!(
        step_by_slug(&still, "done")["metadata"]["outcome_kind"],
        "completed"
    );
}

/// The same key through the PUT, in a write that does NOT complete (the
/// completing one is already held by `a_body_cannot_claim_aborted_to_
/// pass_the_gate`): stored, it would open the gate for the next PUT.
#[tokio::test]
async fn a_step_put_cannot_write_outcome_kind_ahead_of_the_completion() {
    let app = app();
    let job_id = open_job(&app).await;
    let job = get_job(&app, &job_id).await;
    let done = step_by_slug(&job, "done");
    let mut metadata = done["metadata"].clone();
    metadata["outcome_kind"] = json!("aborted");

    let (status, answer) = put_step(&app, &job_id, &done, json!({ "metadata": metadata })).await;
    assert_eq!(status, StatusCode::CONFLICT, "{answer}");
    assert_eq!(answer["refused_keys"], json!(["outcome_kind"]));

    let (status, answer) = put_step(&app, &job_id, &done, json!({ "status": "completed" })).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the gate still holds: {answer}"
    );
    assert_eq!(answer["error"], "step has unresolved blockers");
}

/// Control: a merge that re-sends the stored value, or leaves the key
/// alone, is not a change and still lands.
#[tokio::test]
async fn a_merge_that_leaves_outcome_kind_as_it_stands_still_lands() {
    let app = app();
    let job_id = open_job(&app).await;
    let job = get_job(&app, &job_id).await;
    let done = step_by_slug(&job, "done");
    let step_id = done["id"].as_str().unwrap();

    let (status, answer) = send(
        &app,
        "PATCH",
        &format!("/api/jobs/{job_id}/steps/{step_id}/metadata"),
        Some(json!({ "outcome_kind": "completed", "note": "annotated" })),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{answer}");
    let still = get_job(&app, &job_id).await;
    assert_eq!(
        step_by_slug(&still, "done")["metadata"]["note"],
        "annotated"
    );
}
