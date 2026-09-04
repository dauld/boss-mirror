//! aa9980c8: a ship-a-change Job must survive its review completing
//! and close through its Merged OUTCOME, not around it.
//!
//! What actually happened (2026-08-09, all four passengers of train
//! #219): the conductor completed `review` at boarding, the
//! re-evaluator saw both outcomes' predicates false with every
//! referenced STEP terminal, inferred "provably unsatisfiable",
//! skipped them — and the all-steps-terminal catch-all closed the Job
//! 134ms after boarding. The audit trail then said the change was
//! neither merged nor abandoned, and the conductor's
//! `metadata.merged = "true"` write at real merge time landed on a
//! closed Job.
//!
//! Two halves, both pinned here against the real router:
//!
//! 1. A predicate referencing `job.metadata` is never "provably
//!    unsatisfiable" from step terminality — the marker arrives
//!    later. The outcome stays Pending; the Job stays open.
//! 2. The Job-update endpoint re-evaluates readiness, so the marker
//!    write itself promotes the outcome to Ready (before this,
//!    metadata writes woke nothing and the v3 flow could not run at
//!    all).
//!
//! The registry lib test `reevaluate_never_skips_a_metadata_gated_outcome`
//! covers half 1 at the pure function; this drives POST → tasks →
//! marker → outcome → closed through HTTP, which also covers the
//! catch-all close path and the terminal skip of the branch not taken.

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
    // The real platform registry — a fixture copy would keep passing
    // while the shipped ship-a-change kind stayed broken.
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

fn step<'a>(job: &'a serde_json::Value, title: &str) -> &'a serde_json::Value {
    job["steps"]
        .as_array()
        .and_then(|s| s.iter().find(|x| x["title"] == title))
        .unwrap_or_else(|| panic!("no step titled {title:?} in {job:#}"))
}

#[tokio::test]
async fn merged_outcome_survives_review_and_closes_on_the_marker() {
    let app = app();

    let (status, job) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({
                    "kind": "ship-a-change",
                    "subject": { "subject_kind": "custom", "id": "crates/example" },
                    "title": "A change that ships",
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "metadata": { "spec_slug": "a-change", "branch": "feat/a-change" },
                    "tags": ["test"],
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    let job_id = job["id"].as_str().expect("job id").to_string();

    // Drive every task to completed the way the surfaces do: complete
    // whatever is ready/active, filling required fields from the
    // step's own schema, until only the outcomes remain.
    for _ in 0..8 {
        let current = get_job(&app, &job_id).await;
        let steps = current["steps"].as_array().cloned().unwrap_or_default();
        let actionable: Vec<serde_json::Value> = steps
            .iter()
            .filter(|s| s["status"] == "ready" || s["status"] == "active")
            .cloned()
            .collect();
        if actionable.is_empty() {
            break;
        }
        for s in actionable {
            let step_id = s["id"].as_str().expect("step id");
            // Merge, never replace: `authority_role` shares the object.
            let mut metadata = s["metadata"].clone();
            for f in s["fields"].as_array().into_iter().flatten() {
                if f["required"].as_bool() != Some(true) {
                    continue;
                }
                let name = f["name"].as_str().unwrap_or_default();
                let declared = f["field_type"].as_str().unwrap_or_default();
                metadata[name] = serde_json::Value::String(
                    declared.split('|').next().unwrap_or("x").to_string(),
                );
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
                "completing `{}` failed with {status}: {body}",
                s["title"].as_str().unwrap_or("?"),
            );
        }
    }

    // THE defect: with review done and no marker yet, the Job must
    // still be open with both outcomes awaiting their markers — not
    // closed with them skipped.
    let parked = get_job(&app, &job_id).await;
    assert_eq!(
        parked["status"], "open",
        "Job closed around its outcomes: {parked:#}"
    );
    assert_eq!(
        step(&parked, "Merged")["status"],
        "pending",
        "Merged must await its marker"
    );
    assert_eq!(
        step(&parked, "Abandoned")["status"],
        "pending",
        "Abandoned must await its marker"
    );

    // The conductor's write at real merge time: whole-Job PUT with
    // the marker merged into metadata (exactly merge_job_metadata).
    let mut updated = parked.clone();
    updated["metadata"]["merged"] = serde_json::Value::String("true".into());
    let (status, body) = send(
        &app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}"))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(updated.to_string()))
            .unwrap(),
    )
    .await;
    assert!(status.is_success(), "marker write failed: {status} {body}");

    // The marker write itself must wake the chain — which now starts
    // at `proven`, not at the terminal: merged means visible in prod,
    // not landed on main (David, 2026-08-19), so the marker readies
    // the proof step and the terminal keeps waiting.
    let marked = get_job(&app, &job_id).await;
    assert_eq!(
        step(&marked, "Proven in prod")["status"],
        "ready",
        "the metadata write must promote the proof step: {marked:#}"
    );
    assert_eq!(
        step(&marked, "Merged")["status"],
        "pending",
        "the terminal must wait for the prod proof, not the marker"
    );

    // The proof: verified evidence recorded at the consuming layer
    // (what the post-converge browser check writes).
    let proven_id = step(&marked, "Proven in prod")["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mut proven_md = step(&marked, "Proven in prod")["metadata"].clone();
    proven_md["verified"] =
        serde_json::Value::String("uxprobe: surface renders on prod, controls present".into());
    proven_md["method"] = serde_json::Value::String("browser".into());
    let (status, body) = send(
        &app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}/steps/{proven_id}"))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({ "status": "completed", "metadata": proven_md }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert!(status.is_success(), "proving failed: {status} {body}");

    let proven = get_job(&app, &job_id).await;
    assert_eq!(
        step(&proven, "Merged")["status"],
        "ready",
        "the prod proof must promote the Merged outcome: {proven:#}"
    );

    // Completing the outcome (what the dispatcher's marker handler
    // does) closes the Job THROUGH it: Merged completed, the branch
    // not taken skipped.
    let merged_id = step(&proven, "Merged")["id"].as_str().unwrap().to_string();
    let (status, body) = send(
        &app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}/steps/{merged_id}"))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({ "status": "completed" }).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert!(
        status.is_success(),
        "completing Merged failed: {status} {body}"
    );

    let closed = get_job(&app, &job_id).await;
    assert_eq!(closed["status"], "closed", "outcome closes the Job");
    assert_eq!(step(&closed, "Merged")["status"], "completed");
    assert_eq!(
        step(&closed, "Abandoned")["status"],
        "skipped",
        "the branch not taken skips at terminal close"
    );
}
