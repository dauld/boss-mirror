//! Layer 4: a `backlog-item` routed to DESIGN can close as covered by a
//! design that already exists.
//!
//! The defect (backlog 2d3cbeb2, found 2026-09-23 by the draft-design
//! analyst on f5c1e556, run 0bb07052). The design route opens
//! `draft-design`, whose one field is a required `design_id`, and
//! `design-review` is `ready_when = steps.draft-design.done`. So when
//! the analyst finds a design that ALREADY answers the item — f5c1e556
//! is covered by bffc0aba, decided long before — the step had two
//! exits and both were wrong: file a second design for a question
//! already answered, or complete the step with the old design's id,
//! which opens an empty `Decide the design` for David whose closing
//! rule (the design's own decision) fired long ago and will never fire
//! again. So the item sat at draft-design with `duplicate_of`
//! annotations and no terminal.
//!
//! THE FIX IS THE SAME ONE `build` AND `measure` GOT (6c114a23,
//! 2802ba8c): an OPTIONAL `disposition` on the step, a draft-design arm
//! on the `duplicate` terminal, and the review reading the NEGATIVE of
//! it, so a draft that filed a design (no disposition, which is Absent
//! and false against every literal) still opens the review exactly as
//! before. `design_id` stays required: on a duplicate it names the
//! design that covers the item, which is the record's whole point.
//!
//! These tests drive the REAL router against the REAL platform bundle,
//! for the reason `backlog_item_design_routes_onward` gives: a fixture
//! copy of the spec would keep passing while the shipped kind stayed
//! broken.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::seedable_platform_workflows;
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

async fn open_item(app: &axum::Router) -> String {
    let body = serde_json::json!({
        "kind": "backlog-item",
        "subject": { "subject_kind": "custom", "id": "bosspipeline" },
        "title": "A question a decided design already answers",
        "owner_id": "emp-bootstrap-admin",
        "priority": "standard",
        "status": "open",
        "metadata": { "area": "platform" },
        "tags": ["protocol"],
    })
    .to_string();
    let (status, job) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    job["id"].as_str().expect("job id").to_string()
}

async fn read(app: &axum::Router, job_id: &str) -> serde_json::Value {
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
    body
}

fn step_of<'a>(job: &'a serde_json::Value, slug: &str) -> &'a serde_json::Value {
    job["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["spec_slug"] == slug)
        .unwrap_or_else(|| panic!("no step `{slug}` on the packet: {job:#?}"))
}

fn status_of(job: &serde_json::Value, slug: &str) -> String {
    step_of(job, slug)["status"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn actionable(job: &serde_json::Value, slug: &str) -> bool {
    matches!(status_of(job, slug).as_str(), "ready" | "active")
}

/// Complete a step, merging `extra` over its current metadata —
/// never replacing, because `authority_role` and the step's
/// `procedure` share that object.
async fn try_complete(
    app: &axum::Router,
    job_id: &str,
    job: &serde_json::Value,
    slug: &str,
    extra: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let step = step_of(job, slug);
    let mut metadata = step["metadata"].clone();
    for (k, v) in extra.as_object().into_iter().flatten() {
        metadata[k] = v.clone();
    }
    send(
        app,
        Request::builder()
            .method("PUT")
            .uri(format!(
                "/api/jobs/{job_id}/steps/{}",
                step["id"].as_str().expect("step id")
            ))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({ "status": "completed", "metadata": metadata }).to_string(),
            ))
            .unwrap(),
    )
    .await
}

async fn complete(
    app: &axum::Router,
    job_id: &str,
    job: &serde_json::Value,
    slug: &str,
    extra: serde_json::Value,
) {
    assert!(
        actionable(job, slug),
        "step `{slug}` is `{}`, not actionable",
        status_of(job, slug)
    );
    let (status, body) = try_complete(app, job_id, job, slug, extra).await;
    assert!(
        status.is_success(),
        "completing `{slug}` failed {status}: {body}"
    );
}

/// The design that already answers the item — bffc0aba's shape, the
/// case the packet was filed from.
const ANSWERED_DESIGN: &str = "bffc0aba-4bd4-4b1a-9e0a-2d3cbeb2f5c1";

/// Route a fresh packet to `design`, returning it as read after triage.
async fn routed_to_design(app: &axum::Router) -> (String, serde_json::Value) {
    let job_id = open_item(app).await;
    let job = read(app, &job_id).await;
    complete(
        app,
        &job_id,
        &job,
        "triage",
        serde_json::json!({
            "disposition": "design",
            "evidence": "Measured: the question is a design decision, not a fix.",
        }),
    )
    .await;
    let after = read(app, &job_id).await;
    assert!(
        actionable(&after, "draft-design"),
        "the design route opens the draft"
    );
    (job_id, after)
}

/// THE DEFECT ITSELF. A draft that names an already-answered design as
/// a DUPLICATE closes the item on the `duplicate` terminal — and opens
/// no review, because the decision it would ask for was made on the
/// other design and nothing will ever complete a second copy of it.
#[tokio::test]
async fn a_draft_naming_an_answered_design_closes_as_a_duplicate_without_a_review() {
    let app = app();
    let (job_id, after) = routed_to_design(&app).await;

    complete(
        &app,
        &job_id,
        &after,
        "draft-design",
        serde_json::json!({ "design_id": ANSWERED_DESIGN, "disposition": "duplicate" }),
    )
    .await;
    let after = read(&app, &job_id).await;

    assert_eq!(
        status_of(&after, "design-review"),
        "skipped",
        "no empty decision for David — the design this names was decided already. Steps: {:#?}",
        after["steps"]
    );
    assert_eq!(
        status_of(&after, "build"),
        "skipped",
        "nothing was approved, so nothing is built"
    );
    assert_ne!(
        status_of(&after, "closed"),
        "ready",
        "`closed` must not race `duplicate` — the item was covered, not completed"
    );
    assert!(
        actionable(&after, "duplicate") || after["status"] == "closed",
        "a draft naming an answered design must reach `duplicate` — it is `{}`. Steps: {:#?}",
        status_of(&after, "duplicate"),
        after["steps"]
    );

    if actionable(&after, "duplicate") {
        complete(&app, &job_id, &after, "duplicate", serde_json::json!({})).await;
    }
    let done = read(&app, &job_id).await;
    assert_eq!(done["status"], "closed", "a covered item closes");
    assert_eq!(
        done["metadata"]["outcome"], "duplicate",
        "the closed item carries the duplicate outcome: {:#?}",
        done["metadata"]
    );
    assert_eq!(
        step_of(&done, "draft-design")["metadata"]["design_id"],
        ANSWERED_DESIGN,
        "and the draft step names the design that covers it"
    );
}

/// THE REGRESSION THIS MUST NOT CAUSE. A draft that filed a design
/// carries no disposition — `boss design --answers` writes none, and
/// Absent is false against every literal — or says `filed`. Either way
/// the review's `NOT (... = "duplicate")` is true and the review opens
/// exactly as it did before, with the duplicate terminal still alive
/// for a build that later refutes.
#[tokio::test]
async fn a_draft_that_filed_a_design_still_opens_the_review() {
    for extra in [
        serde_json::json!({ "design_id": "5fc71f03-db4f-4be2-9839-484ccf29781a" }),
        serde_json::json!({
            "design_id": "5fc71f03-db4f-4be2-9839-484ccf29781a",
            "disposition": "filed",
        }),
    ] {
        let app = app();
        let (job_id, after) = routed_to_design(&app).await;

        complete(&app, &job_id, &after, "draft-design", extra.clone()).await;
        let after = read(&app, &job_id).await;

        assert!(
            actionable(&after, "design-review"),
            "a filed design opens its review ({extra}) — it is `{}`",
            status_of(&after, "design-review")
        );
        assert_eq!(status_of(&after, "duplicate"), "pending", "{extra}");
    }
}

/// The disposition is ENUM-CHECKED on the step. A misspelled
/// `duplicate` accepted in silence would read as an ordinary draft and
/// open the very review this exists to refuse — which is why the set
/// is two values and not one (a one-value type is not an enum to the
/// validator). A draft does not re-route an item either, so `build` is
/// refused with it.
#[tokio::test]
async fn a_draft_disposition_outside_the_set_is_refused() {
    for wrong in ["duplciate", "build"] {
        let app = app();
        let (job_id, after) = routed_to_design(&app).await;

        let (status, body) = try_complete(
            &app,
            &job_id,
            &after,
            "draft-design",
            serde_json::json!({ "design_id": ANSWERED_DESIGN, "disposition": wrong }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "disposition `{wrong}` must be refused, not accepted: {body}"
        );
    }
}

/// THE EXIT IS NAMED WHERE THE ANALYST READS. A disposition nobody is
/// told about is the defect again: the analyst on f5c1e556 read this
/// step's procedure and found no way out. So the procedure names the
/// field, the value and the verb that writes them.
#[test]
fn the_draft_procedure_names_the_duplicate_exit() {
    let spec = seedable_platform_workflows()
        .into_iter()
        .find(|s| s.kind == "backlog-item")
        .expect("backlog-item is in the platform bundle");
    let procedure = spec
        .steps
        .iter()
        .find(|s| s.title == "draft-design")
        .expect("backlog-item has a draft-design step")
        .metadata_defaults["procedure"]
        .as_str()
        .expect("draft-design carries a procedure")
        .to_string();
    for needle in [
        "--step draft-design",
        "--field disposition=duplicate",
        "2d3cbeb2",
    ] {
        assert!(
            procedure.contains(needle),
            "the draft-design procedure must name `{needle}`: {procedure}"
        );
    }
}
