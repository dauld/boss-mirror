//! Layer 4: a `backlog-item` routed to DESIGN can still be built.
//!
//! The defect (af28e250, measured 2026-09-09 against the live
//! registry, backlog-item v4). `build` read
//! `steps.triage.done AND steps.triage.metadata.disposition = "build"`
//! and referenced `design-review` nowhere, so the moment triage
//! resolved to `design` every non-matching branch became provably
//! unsatisfiable — every step it references terminal, predicate still
//! false — and `build` was SKIPPED. Hours later the reviewer answered,
//! `closed` fired on a bare `steps.design-review.done`, and the packet
//! terminated on the decision with nothing built. Measured on three
//! items: 2d4a5a8b and 9e82ee62 both closed with build skipped and the
//! decision made; e67cdd56 open, its decide step ready and its build
//! step ALREADY skipped.
//!
//! The fix is a new authored version whose `build` also references
//! `design-review`. A step stays Pending while ANY step it references
//! is non-terminal, so naming `design-review` is what keeps `build`
//! alive across the review instead of skipped at triage time.
//!
//! These tests drive the REAL router against the REAL platform bundle,
//! for the reason `user_feedback_lifecycle` gives: a fixture copy of
//! the spec would keep passing while the shipped kind stayed broken.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{JobId, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{materialize_steps, reevaluate, seedable_platform_workflows};
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
        cadence: None,
        delivery: None,
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

fn file_item_body() -> String {
    serde_json::json!({
        "kind": "backlog-item",
        "subject": { "subject_kind": "custom", "id": "bosspipeline" },
        "title": "A design review that is answered CLOSES the item",
        "owner_id": "emp-bootstrap-admin",
        "priority": "standard",
        "status": "open",
        "metadata": { "area": "protocol/backlog-item" },
        "tags": ["protocol"],
    })
    .to_string()
}

async fn open_item(app: &axum::Router) -> String {
    let (status, job) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(file_item_body()))
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

/// Complete a step, merging `extra` over its current metadata —
/// never replacing, because `authority_role` shares that object.
async fn complete(
    app: &axum::Router,
    job_id: &str,
    job: &serde_json::Value,
    slug: &str,
    extra: serde_json::Value,
) {
    let step = step_of(job, slug);
    assert!(
        step["status"] == "ready" || step["status"] == "active",
        "step `{slug}` is `{}`, not actionable",
        step["status"]
    );
    let mut metadata = step["metadata"].clone();
    for (k, v) in extra.as_object().into_iter().flatten() {
        metadata[k] = v.clone();
    }
    let (status, body) = send(
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
    .await;
    assert!(
        status.is_success(),
        "completing `{slug}` failed {status}: {body}"
    );
}

fn triage_design() -> serde_json::Value {
    serde_json::json!({
        "disposition": "design",
        "evidence": "Measured in the live registry: build never references design-review.",
        "context_md": "The predicates, as filed.",
        "proposed": "Give build an edge from the review.",
    })
}

fn review(verdict: &str) -> serde_json::Value {
    serde_json::json!({ "verdict": verdict, "answer": "The route the answer carries." })
}

/// HALF ONE of the fix. Routing to design must leave `build` PENDING.
///
/// This is where the work was lost, and it was lost at TRIAGE time —
/// not when the review was answered. Under v4 `build` referenced only
/// `triage`; with `triage` terminal and the predicate false, the
/// skip rule (registry.rs, `refs_all_terminal`) had every right to
/// mark it Skipped, and did. A skipped step never comes back.
#[tokio::test]
async fn routing_to_design_leaves_build_pending_not_skipped() {
    let app = app();
    let job_id = open_item(&app).await;

    let job = read(&app, &job_id).await;
    complete(&app, &job_id, &job, "triage", triage_design()).await;

    let after = read(&app, &job_id).await;
    assert_eq!(
        step_of(&after, "build")["status"],
        "pending",
        "an item routed to design must keep `build` alive across the review — it is `{}`. \
         Steps: {:#?}",
        step_of(&after, "build")["status"],
        after["steps"]
    );
    let review_status = step_of(&after, "design-review")["status"].clone();
    assert!(
        review_status == "ready" || review_status == "active",
        "the review must be actionable — it is `{review_status}`"
    );
    assert_ne!(
        after["status"], "closed",
        "routing to design must not close"
    );
}

/// HALF TWO. An APPROVED review is a decision to build, so `build`
/// becomes Ready — and `closed` must NOT fire alongside it. Under v4
/// `closed` read `steps.measure.done OR steps.design-review.done OR
/// steps.build.done`, so a bare answered review terminated the packet.
#[tokio::test]
async fn an_approved_review_makes_build_ready_and_does_not_close() {
    let app = app();
    let job_id = open_item(&app).await;

    let job = read(&app, &job_id).await;
    complete(&app, &job_id, &job, "triage", triage_design()).await;
    let job = read(&app, &job_id).await;
    complete(&app, &job_id, &job, "design-review", review("approved")).await;

    let after = read(&app, &job_id).await;
    let build_status = step_of(&after, "build")["status"].clone();
    assert!(
        build_status == "ready" || build_status == "active",
        "an approved design review is a decision to build — `build` is `{build_status}`. \
         Steps: {:#?}",
        after["steps"]
    );
    assert_ne!(
        after["status"], "closed",
        "the item must stay open until the build it authorized is done"
    );
    assert_eq!(
        step_of(&after, "closed")["status"],
        "pending",
        "`closed` must not race the build it just authorized"
    );

    // …and the build, once done, still closes the item.
    complete(&app, &job_id, &after, "build", serde_json::json!({})).await;
    let done = read(&app, &job_id).await;
    let closed_status = step_of(&done, "closed")["status"].clone();
    if closed_status == "ready" || closed_status == "active" {
        complete(&app, &job_id, &done, "closed", serde_json::json!({})).await;
    }
    assert_eq!(
        read(&app, &job_id).await["status"],
        "closed",
        "a built item still reaches closed"
    );
}

/// A review that ANSWERS the question routes nowhere further: `build`
/// is skipped and the item closes. This is v4's only correct path and
/// it must survive the fix.
#[tokio::test]
async fn an_answered_review_skips_build_and_closes_the_item() {
    let app = app();
    let job_id = open_item(&app).await;

    let job = read(&app, &job_id).await;
    complete(&app, &job_id, &job, "triage", triage_design()).await;
    let job = read(&app, &job_id).await;
    complete(&app, &job_id, &job, "design-review", review("answered")).await;

    let after = read(&app, &job_id).await;
    assert_eq!(
        step_of(&after, "build")["status"],
        "skipped",
        "an answered review authorizes no build"
    );
    let closed_status = step_of(&after, "closed")["status"].clone();
    assert!(
        closed_status == "ready" || closed_status == "active" || after["status"] == "closed",
        "an answered review closes the item — `closed` is `{closed_status}`. Steps: {:#?}",
        after["steps"]
    );
}

/// A DECLINED review closes without action, through the `declined`
/// outcome rather than the completed one — and `closed` must not fire
/// beside it. Two terminals ready at once is the same race the
/// approved path must not have, in the other direction.
#[tokio::test]
async fn a_declined_review_closes_without_action_and_does_not_race_closed() {
    let app = app();
    let job_id = open_item(&app).await;

    let job = read(&app, &job_id).await;
    complete(&app, &job_id, &job, "triage", triage_design()).await;
    let job = read(&app, &job_id).await;
    complete(&app, &job_id, &job, "design-review", review("declined")).await;

    let after = read(&app, &job_id).await;
    let declined_status = step_of(&after, "declined")["status"].clone();
    assert!(
        declined_status == "ready" || declined_status == "active" || after["status"] == "closed",
        "a declined review must reach the `declined` outcome — it is `{declined_status}`. \
         Steps: {:#?}",
        after["steps"]
    );
    assert_ne!(
        step_of(&after, "closed")["status"],
        "ready",
        "`closed` must not race the `declined` terminal — one answer, one terminal"
    );
    assert_eq!(step_of(&after, "build")["status"], "skipped");
}

/// Every triage disposition still drives the item to a terminal. The
/// viability lint proves each enum value HAS a successor; this proves
/// the successor is reachable and terminates. The new edge widened
/// three predicates, so the whole fork is re-exercised.
#[tokio::test]
async fn every_disposition_still_reaches_a_terminal() {
    for disposition in ["verify", "design", "build", "duplicate", "stale", "decline"] {
        let app = app();
        let job_id = open_item(&app).await;

        let mut closed = false;
        for round in 0..8 {
            let current = read(&app, &job_id).await;
            if current["status"] == "closed" {
                closed = true;
                break;
            }
            let steps = current["steps"].as_array().cloned().unwrap_or_default();
            let actionable: Vec<&serde_json::Value> = steps
                .iter()
                .filter(|s| s["status"] == "ready" || s["status"] == "active")
                .collect();
            assert!(
                !actionable.is_empty(),
                "`{disposition}` round {round}: neither closed nor actionable — an item \
                 routed here would sit on the board forever. Steps: {steps:#?}"
            );
            for step in actionable {
                let mut metadata = step["metadata"].clone();
                let fill = |name: &str, declared: &str, metadata: &mut serde_json::Value| {
                    let value = if declared.split('|').any(|v| v == disposition) {
                        disposition.to_string()
                    } else {
                        declared.split('|').next().unwrap_or("x").to_string()
                    };
                    metadata[name] = serde_json::Value::String(value);
                };
                for f in step["fields"].as_array().into_iter().flatten() {
                    if f["required"].as_bool() != Some(true) {
                        continue;
                    }
                    fill(
                        f["name"].as_str().unwrap_or_default(),
                        f["field_type"].as_str().unwrap_or_default(),
                        &mut metadata,
                    );
                }
                if let Some(st) = StepRegistry::v1().get(step["kind"].as_str().unwrap_or_default())
                {
                    for f in st.fields.iter().filter(|f| f.required) {
                        if metadata.get(f.name).is_none() {
                            fill(f.name, f.field_type, &mut metadata);
                        }
                    }
                }
                let (status, body) = send(
                    &app,
                    Request::builder()
                        .method("PUT")
                        .uri(format!(
                            "/api/jobs/{job_id}/steps/{}",
                            step["id"].as_str().expect("step id")
                        ))
                        .header("content-type", "application/json")
                        .header("x-boss-user", admin_header())
                        .body(Body::from(
                            serde_json::json!({ "status": "completed", "metadata": metadata })
                                .to_string(),
                        ))
                        .unwrap(),
                )
                .await;
                assert!(
                    status.is_success(),
                    "`{disposition}`: completing `{}` failed {status}: {body}",
                    step["spec_slug"].as_str().unwrap_or("?")
                );
            }
        }
        assert!(closed, "`{disposition}` never reached a terminal");
    }
}

/// A review whose verdict is ABSENT must not error and must not wedge.
///
/// The API cannot produce this — `verdict` is required at completion
/// and enum-checked on the step — so it is asserted at the spec, where
/// `reevaluate` can be handed the state directly. boss-expr resolves a
/// missing identifier to Absent (7b756357), which is false against
/// every literal and false in boolean position, so `build` and
/// `declined` read false and are skipped. `closed`'s design-review
/// disjunct is deliberately the NEGATIVE of the two routed verdicts,
/// so Absent lands there: the item CLOSES rather than sitting with
/// every successor skipped and no terminal reachable.
#[test]
fn an_absent_verdict_closes_the_item_rather_than_wedging_it() {
    let spec = seedable_platform_workflows()
        .into_iter()
        .find(|s| s.kind == "backlog-item")
        .expect("backlog-item is in the platform bundle");
    let subject = Subject::new("custom", "bosspipeline");
    let job_metadata = serde_json::json!({});
    let mut steps = materialize_steps(&spec, &subject, JobId::new(), &job_metadata, StepId::new);

    let idx = |slug: &str| {
        spec.steps
            .iter()
            .position(|s| s.title == slug)
            .unwrap_or_else(|| panic!("no spec step `{slug}`"))
    };

    steps[idx("filed")].status = StepStatus::Completed;
    steps[idx("triage")].status = StepStatus::Completed;
    steps[idx("triage")].metadata = serde_json::json!({ "disposition": "design" });
    reevaluate(&spec, &mut steps, &subject, &job_metadata);
    assert_eq!(
        steps[idx("build")].status,
        StepStatus::Pending,
        "the design route keeps build alive"
    );

    // The review completes carrying no verdict at all.
    steps[idx("design-review")].status = StepStatus::Completed;
    steps[idx("design-review")].metadata = serde_json::json!({});
    reevaluate(&spec, &mut steps, &subject, &job_metadata);

    assert_eq!(
        steps[idx("build")].status,
        StepStatus::Skipped,
        "an unreadable answer authorizes nothing"
    );
    assert_eq!(
        steps[idx("closed")].status,
        StepStatus::Ready,
        "an unreadable answer must still reach a terminal, not wedge the packet open"
    );
}
