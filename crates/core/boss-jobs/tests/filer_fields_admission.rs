//! Filer fields validate at admission, not on the reviewer.
//!
//! Required-at-done is correct for fields the EXECUTOR fills during
//! the work (`scheduled_at`, `decision`) and wrong for fields the
//! FILER must supply for the work to be doable at all. Motivating
//! incident (packet 27de796e, 2026-09-03): a hand-filed design-doc
//! packet's review step lacked `title`/`markdown`, so the 400
//! detonated on the REVIEWER mid-review — twice — the party least
//! able to fix it. The enforcement point must sit with the party who
//! can: admission.
//!
//! These tests run the REAL platform bundle's `design-doc` row
//! (infra/platform/workflows.toml) through the create handler, so the
//! registry data and the mechanism are proven together — the shape
//! the incident had, verbatim.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{
    StepSpec, Terminal, WorkflowRegistry, WorkflowSpec, platform_bundle_path,
};
use boss_jobs::seed_loader::load_workflows;
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use serde_json::json;
use tower::ServiceExt;

fn admin_header() -> String {
    json!({
        "id": "emp-david",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

/// A router over an in-memory stack seeded with the given Workflows.
fn app_with(specs: Vec<WorkflowSpec>) -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::new());
    for spec in specs {
        kinds.seed(spec).expect("spec seeds");
    }
    let jobs = Arc::new(InMemoryJobs::new());
    let kind_registry: Arc<dyn WorkflowRegistry> = kinds;
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
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: Some(kind_registry),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
    };
    (router(state), jobs)
}

/// The platform bundle's design-doc row — the registry data under test.
fn design_doc_spec() -> WorkflowSpec {
    load_workflows(platform_bundle_path())
        .expect("the platform bundle parses")
        .into_iter()
        .find(|w| w.kind == "design-doc")
        .expect("the bundle ships design-doc")
}

/// A design-doc job body in the shape `boss design` files, minus
/// whatever the test removes.
fn design_doc_body(metadata: serde_json::Value) -> serde_json::Value {
    json!({
        "kind": "design-doc",
        "status": "open",
        "title": "Filer fields validate at admission",
        "owner_id": "emp-david",
        "priority": "standard",
        "opened_on": "2026-09-03",
        "tags": ["design"],
        "subject": { "subject_kind": "custom", "id": "boss-platform" },
        "metadata": metadata,
    })
}

async fn post_job(app: &axum::Router, body: &serde_json::Value) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", admin_header())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).to_string())
}

/// THE INCIDENT, REFUSED AT THE RIGHT EDGE: a design-doc job POSTed
/// without `markdown` fails at CREATE, naming the step, the field,
/// and the party — instead of admitting a packet whose review step
/// can never complete and detonating on the reviewer.
#[tokio::test]
async fn a_design_doc_without_markdown_is_refused_at_create() {
    let (app, jobs) = app_with(vec![design_doc_spec()]);
    // The hand-filed shape from the incident: a title, no markdown.
    let body = design_doc_body(json!({
        "title": "Filer fields validate at admission",
        "no_open_questions": "false",
    }));
    let (status, text) = post_job(&app, &body).await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "admission must refuse a filer-field omission; got {status}: {text}"
    );
    assert!(
        text.contains("markdown") && text.contains("review"),
        "the refusal names the field and the step: {text}"
    );
    assert!(
        text.contains("supplied at filing"),
        "the refusal names the party who owes the value: {text}"
    );

    // Conservation: a refused admission writes nothing.
    let (stored, _) = jobs.list_jobs(&Default::default(), 10, 0).await.unwrap();
    assert!(
        stored.is_empty(),
        "a refused packet must not leave a half-created job behind"
    );
}

/// The door still opens: the shape `boss design` files (job metadata
/// carrying title + markdown) admits cleanly, and the binding carries
/// the filer's values onto the review step at materialization.
#[tokio::test]
async fn a_design_doc_with_filer_fields_admits_and_binds_them_to_the_step() {
    let (app, jobs) = app_with(vec![design_doc_spec()]);
    let body = design_doc_body(json!({
        "title": "Filer fields validate at admission",
        "markdown": "# The doc under review",
        "questions": [{ "anchor": "Q1", "title": "Which edge?", "proposal": "Admission." }],
        "no_open_questions": "false",
    }));
    let (status, text) = post_job(&app, &body).await;
    assert_eq!(status, StatusCode::CREATED, "create should succeed: {text}");

    let (stored, _) = jobs.list_jobs(&Default::default(), 10, 0).await.unwrap();
    let job = stored.first().expect("job stored");
    let steps = jobs.list_steps(&job.id).await.unwrap();
    let review = steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some("review"))
        .expect("the review step materialized");
    assert_eq!(
        review.metadata.get("markdown").and_then(|v| v.as_str()),
        Some("# The doc under review"),
        "the filer's markdown rides the step from admission, not a follow-up PUT"
    );
    assert_eq!(
        review.metadata.get("title").and_then(|v| v.as_str()),
        Some("Filer fields validate at admission"),
    );
}

/// Executor fields keep the create-side contract they always had: a
/// required-at-done field absent at create admits fine, and still
/// refuses completion until filled — admission catches filer fields
/// FIRST, it does not move the completion check.
#[tokio::test]
async fn executor_fields_stay_create_legal_and_required_at_done() {
    use boss_core::job::{FilledBy, StepField};
    let spec = WorkflowSpec::platform_seed(
        "exec-fields",
        "Executor fields",
        "test",
        vec!["custom".into()],
        vec![
            StepSpec {
                title: "open".into(),
                kind: "trigger".into(),
                ready_when: "true".into(),
                title_template: "Opened".into(),
                metadata_defaults: json!({ "trigger_kind": "operator", "trigger_name": "t" }),
                ..Default::default()
            },
            StepSpec {
                title: "work".into(),
                kind: "task".into(),
                ready_when: "steps.open.done".into(),
                title_template: "Do the work".into(),
                authority_role: Some("platform-admin".into()),
                fields: vec![StepField {
                    name: "result".into(),
                    field_type: "string".into(),
                    required: true,
                    filled_by: FilledBy::Executor,
                }],
                ..Default::default()
            },
            StepSpec {
                title: "done".into(),
                kind: "outcome".into(),
                ready_when: "steps.work.done".into(),
                title_template: "Done".into(),
                metadata_defaults: json!({ "outcome_kind": "completed" }),
                terminal: Some(Terminal {
                    outcome: "completed".into(),
                }),
                ..Default::default()
            },
        ],
    );
    let (app, jobs) = app_with(vec![spec]);
    let body = json!({
        "kind": "exec-fields",
        "status": "open",
        "title": "Executor contract unchanged",
        "owner_id": "emp-david",
        "priority": "standard",
        "opened_on": "2026-09-03",
        "tags": [],
        "subject": { "subject_kind": "custom", "id": "boss-platform" },
        "metadata": {},
    });
    let (status, text) = post_job(&app, &body).await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "an absent EXECUTOR field must not block admission: {text}"
    );

    // Completion without the field: still refused (required-at-done).
    let (stored, _) = jobs.list_jobs(&Default::default(), 10, 0).await.unwrap();
    let job = stored.first().expect("job stored");
    let steps = jobs.list_steps(&job.id).await.unwrap();
    let work = steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some("work"))
        .expect("work step materialized");
    let complete = async |body: serde_json::Value| {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/jobs/{}/steps/{}", job.id, work.id))
                    .header("content-type", "application/json")
                    .header("x-boss-user", admin_header())
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        resp.status()
    };
    // Bare completion — metadata untouched by the overlay, `result`
    // still absent — refuses, exactly as before this change.
    let refused = complete(json!({ "status": "completed" })).await;
    assert_eq!(
        refused,
        StatusCode::BAD_REQUEST,
        "completion without the executor field must still refuse"
    );
    // Clients merge metadata (PATCH-on-PUT contract): existing keys +
    // the filled field.
    let mut merged = work.metadata.clone();
    merged["result"] = json!("done it");
    let accepted = complete(json!({ "status": "completed", "metadata": merged })).await;
    assert!(
        accepted.is_success(),
        "completion with the executor field filled must pass, got {accepted}"
    );
}
