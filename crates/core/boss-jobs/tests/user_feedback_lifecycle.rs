//! Layer 4: the feedback flow can actually be driven from open to
//! closed through the real HTTP surface.
//!
//! `boss-jobs` had fifteen integration tests and not one of them
//! opened a Job and closed it. That hole shipped a `user-feedback`
//! Workflow whose triage step could never complete: it used the
//! `acknowledgment` kind, whose schema requires `document_title`, and
//! metadata validators run at `completed` rather than at create. So
//! the Job materialized cleanly, sat in the triage board's waiting
//! column looking healthy, and returned
//! `400 … required field 'document_title' is missing` the first time
//! a human tried to act on it. The bug reached an operator because
//! every layer below this one was satisfied.
//!
//! The lib test `user_feedback_steps_close_without_operator_supplied_fields`
//! covers the same defect at the spec, which is cheaper and names the
//! offending kind directly. This one is deliberately not redundant
//! with it: it drives the real router, so it also covers a wrong
//! `ready_when`, a blocker gate that never opens, and a terminal that
//! never fires — none of which the spec test can see.
//!
//! Scoped to `user-feedback` on purpose. The invariant worth having
//! is "every platform Workflow can be driven from open to closed using
//! only what its own surfaces supply", but the other two kinds are
//! driven by authoring UIs that DO supply fields (`workflow-design`'s
//! publish step takes a `workflow_spec`), so generalizing needs a
//! per-kind fixture describing what each surface posts. Feedback is
//! the one flow where the answer is "nothing", which is exactly why
//! it is the one that broke.

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

/// The triage step's `authority_role` is `platform-admin`, and the
/// Workflow's `owner_role` is too — so the roster must hold one or the
/// create handler rejects the Job for having no human owner.
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
    // Seeded from the real platform registry, not a hand-built spec —
    // a fixture copy would have kept passing while the shipped kind
    // was broken.
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

/// Exactly the body `FeedbackControl.svelte` posts from the chrome bar.
fn submit_feedback_body() -> String {
    serde_json::json!({
        "kind": "user-feedback",
        "subject": { "subject_kind": "custom", "id": "/ux/jobs" },
        "title": "Feedback on /ux/jobs",
        "owner_id": "emp-bootstrap-admin",
        "priority": "standard",
        "status": "open",
        "metadata": {
            "message": "The column picker forgets my choice.",
            "route": "/ux/jobs",
            "submitted_by": "emp-bootstrap-admin",
        },
        "tags": ["feedback"],
    })
    .to_string()
}

/// Every disposition a triager can choose must drive the Job to
/// closed. That is the property a fork actually needs: the viability
/// lint proves each enum value HAS a successor, and this proves the
/// successor is reachable, completable, and terminates.
///
/// Fields are read from the step's own `fields` array rather than
/// hardcoded, which is what a real UI does — so a new required field
/// on any step is exercised here instead of surfacing as a 400 the
/// first time someone triages.
#[tokio::test]
async fn every_disposition_drives_the_job_to_closed() {
    for disposition in [
        "reproduce",
        "design",
        "build",
        "needs-info",
        "duplicate",
        "decline",
    ] {
        let app = app();

        let (status, job) = send(
            &app,
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", admin_header())
                .body(Body::from(submit_feedback_body()))
                .unwrap(),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
        let job_id = job["id"].as_str().expect("job id").to_string();

        let mut closed = false;
        for round in 0..8 {
            let (status, current) = send(
                &app,
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/jobs/{job_id}"))
                    .header("x-boss-user", admin_header())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "read failed: {current}");

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
                "`{disposition}` round {round}: Job is neither closed nor actionable — \
                 nothing is ready and nothing can become ready, so an item routed here \
                 would sit on the board forever. Steps: {steps:#?}"
            );

            for step in actionable {
                let step_id = step["id"].as_str().expect("step id");

                // Merge, never replace: `authority_role` shares this
                // object and is what keeps the step gated.
                let mut metadata = step["metadata"].clone();
                for f in step["fields"].as_array().into_iter().flatten() {
                    if f["required"].as_bool() != Some(true) {
                        continue;
                    }
                    let name = f["name"].as_str().unwrap_or_default();
                    let declared = f["field_type"].as_str().unwrap_or_default();
                    // The disposition under test where the step asks
                    // for it; otherwise the first legal enum value.
                    let value = if declared.split('|').any(|v| v == disposition) {
                        disposition
                    } else {
                        declared.split('|').next().unwrap_or("x")
                    };
                    metadata[name] = serde_json::Value::String(value.to_string());
                }
                // …and the KIND's own required fields, exactly as the
                // kind's surface collects them (v11's design-review is
                // `answer-question`: its plugin's form is what asks
                // for verdict + answer, so the operator's completion
                // carries them). Filled from the StepType registry —
                // the same union the API validates against.
                if let Some(st) = boss_jobs::step_registry::StepRegistry::v1()
                    .get(step["kind"].as_str().unwrap_or_default())
                {
                    for f in st.fields.iter().filter(|f| f.required) {
                        if metadata.get(f.name).is_none() {
                            let sample = f.field_type.split('|').next().unwrap_or("x");
                            metadata[f.name] = serde_json::Value::String(sample.to_string());
                        }
                    }
                }

                let (status, body) = send(
                    &app,
                    Request::builder()
                        .method("PUT")
                        .uri(format!("/api/jobs/{job_id}/steps/{step_id}"))
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
                    "`{disposition}`: completing step `{}` (kind `{}`) failed with \
                     {status}: {body}",
                    step["title"].as_str().unwrap_or("?"),
                    step["kind"].as_str().unwrap_or("?"),
                );
            }
        }

        assert!(
            closed,
            "`{disposition}` did not reach `closed` after 8 rounds of completing every \
             ready step"
        );
    }
}

/// An investigation that finds the item needs a DESIGN DECISION routes
/// it there, and the packet stays open until that decision is made.
///
/// This is the defect a001c78a reported, driven through the real
/// router. In v9 every branch predicate read
/// `steps.triage.metadata.disposition` and nothing else, so the packet
/// was routed once — before the investigation that produces the
/// evidence — and `closed` fired on `steps.investigate.done`. An
/// investigator who discovered a design question had no move:
/// recording it on the investigate step was decorative, and completing
/// the step ended the item. Observed live on `3f5f7f63`, whose
/// recommendation is stranded on a closed packet.
///
/// The two assertions are the two halves of the fix, and BOTH failed
/// before it: `design-review` must become actionable, and the Job must
/// NOT be closed.
#[tokio::test]
async fn an_investigation_can_route_the_packet_onward_instead_of_ending_it() {
    let app = app();

    let (status, job) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(submit_feedback_body()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    let job_id = job["id"].as_str().expect("job id").to_string();

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

    async fn complete(
        app: &axum::Router,
        job_id: &str,
        current: &serde_json::Value,
        slug: &str,
        disposition: &str,
    ) {
        let step = current["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|s| s["spec_slug"] == slug)
            .unwrap_or_else(|| panic!("no step `{slug}` on the packet: {current:#?}"));
        assert!(
            step["status"] == "ready" || step["status"] == "active",
            "step `{slug}` is `{}`, not actionable",
            step["status"]
        );
        // Merge, never replace: `authority_role` shares this object.
        let mut metadata = step["metadata"].clone();
        metadata["disposition"] = serde_json::Value::String(disposition.to_string());
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

    // Triage can only see a bug report, so it asks for a reproduction.
    let current = read(&app, &job_id).await;
    complete(&app, &job_id, &current, "triage", "reproduce").await;

    // The investigation finds the real answer is a design decision.
    let current = read(&app, &job_id).await;
    complete(&app, &job_id, &current, "investigate", "design").await;

    let after = read(&app, &job_id).await;
    let by_slug: std::collections::HashMap<&str, &serde_json::Value> = after["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["spec_slug"].as_str().map(|slug| (slug, s)))
        .collect();

    let review = by_slug["design-review"];
    assert!(
        review["status"] == "ready" || review["status"] == "active",
        "the investigation routed to `design`, so design-review must be actionable — \
         it is `{}`. Steps: {:#?}",
        review["status"],
        after["steps"]
    );
    assert_ne!(
        after["status"], "closed",
        "finishing an investigation that routed onward must NOT close the packet — \
         that is how 3f5f7f63's recommendation was lost"
    );
}
