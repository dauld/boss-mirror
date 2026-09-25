//! f9256445 (design d812f1b7 D2, Q1): a car whose LANDING MAIN LOST
//! closes through its own terminal — `unlanded` — and its work rides
//! again as a linked SUCCESSOR car for the same branch. Never a reopen.
//!
//! What happened (2026-09-25): train 20:04 merged car dec3136a as
//! c85941b4 at 20:10:41Z; by 20:11:14Z forge main was back at 777a5888.
//! The car read landed ("landed on main as c85941b423bc"), its `proven`
//! step stood ready, and none of the five terminals said what happened.
//! `boss car unland` is the one writer; this pins the half of it the
//! PROTOCOL owns, against the real router and the real platform bundle:
//! the correction beside the landing note, the successor opened at
//! `gate` with `supersedes`, and the writes
//! `boss_jobs::car_unland::unland_writes` returns, in the order the CLI
//! performs them, closing the car with the evidence on the step — and
//! nothing reaches the terminal without that evidence.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::car::{building_car_for, is_landed, landed_car_for};
use boss_jobs::car_unland::{
    Unlanding, review_correction, successor_body, successor_writes, unland_writes,
};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const BRANCH: &str = "fix/a-stamp-cannot-land-on-a-moved-shape";
const MERGE: &str = "c85941b423bc";

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
    // The real platform bundle: a fixture copy would pass while the
    // shipped ship-a-change had no such terminal.
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
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", admin_header())
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
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

async fn get_job(app: &axum::Router, id: &str) -> Value {
    let (status, job) = send(app, "GET", &format!("/api/jobs/{id}"), None).await;
    assert_eq!(status, StatusCode::OK, "read failed: {job}");
    job
}

fn slug<'a>(job: &'a Value, slug: &str) -> &'a Value {
    job["steps"]
        .as_array()
        .and_then(|s| s.iter().find(|x| x["spec_slug"] == slug))
        .unwrap_or_else(|| panic!("no step {slug:?} in {job:#}"))
}

/// Merge `metadata` onto a step, then complete it — the two writes every
/// car writer performs (backlog e39a9d2a).
async fn complete(app: &axum::Router, id: &str, step: &Value, metadata: Value) {
    let sid = step["id"].as_str().unwrap();
    let (status, body) = send(
        app,
        "PATCH",
        &format!("/api/jobs/{id}/steps/{sid}/metadata"),
        Some(metadata),
    )
    .await;
    assert!(
        status.is_success(),
        "merge onto {}: {status} {body}",
        step["title"]
    );
    let (status, body) = send(
        app,
        "PUT",
        &format!("/api/jobs/{id}/steps/{sid}"),
        Some(json!({"status": "completed"})),
    )
    .await;
    assert!(
        status.is_success(),
        "completing {}: {status} {body}",
        step["title"]
    );
}

/// Car dec3136a at 21:15Z: scope, build and gate completed the way the
/// surfaces complete them, review completed by the conductor's landing
/// with its note, and the `merged` marker on the job — `proven` ready.
async fn landed_car(app: &axum::Router) -> String {
    let (status, job) = send(
        app,
        "POST",
        "/api/jobs",
        Some(json!({
            "kind": "ship-a-change",
            "subject": { "subject_kind": "custom", "id": BRANCH },
            "title": "A stamp cannot land on a moved shape",
            "owner_id": "emp-bootstrap-admin",
            "priority": "standard",
            "status": "open",
            "metadata": { "branch": BRANCH, "summary": "A stamp cannot land on a moved shape." },
            "tags": ["test"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    let id = job["id"].as_str().expect("job id").to_string();
    for (s, md) in [
        (
            "scope",
            json!({"summary": "A stamp cannot land on a moved shape.",
                   "excludes": "the claim door's UI"}),
        ),
        ("build", json!({"test": "boss-jobs: 12 new tests"})),
        ("gate", json!({"gates": "auto", "verified": "green"})),
        (
            "review",
            json!({"pr_url": "http://forge/boss/pulls/687",
                   "note": format!("landed on main as {MERGE}")}),
        ),
    ] {
        let car = get_job(app, &id).await;
        complete(app, &id, slug(&car, s), md).await;
    }
    let (status, body) = send(
        app,
        "PATCH",
        &format!("/api/jobs/{id}/metadata"),
        Some(json!({ "merged": "true", "merge_ref": MERGE })),
    )
    .await;
    assert!(status.is_success(), "merged marker: {status} {body}");
    let landed = get_job(app, &id).await;
    assert_eq!(slug(&landed, "proven")["status"], "ready", "{landed:#}");
    id
}

fn lost(successor: &str) -> Unlanding {
    Unlanding {
        merge_ref: MERGE.into(),
        evidence: "forge main 777a5888504f at 2026-09-25T21:19:00Z (ls-remote); git \
                   merge-base --is-ancestor c85941b423bc 777a5888504f exited 1, on two \
                   readings"
            .into(),
        superseded_by: successor.into(),
        completed_at: "2026-09-25T21:19:00Z".into(),
    }
}

/// The whole unlanding, in the CLI's order: the correction beside the
/// landing note, the successor opened and brought to `gate`, then the
/// three writes that close the car through `unlanded`.
#[tokio::test]
async fn a_car_whose_landing_main_lost_closes_unlanded_and_its_successor_rides_again() {
    let app = app();
    let id = landed_car(&app).await;

    // 1. THE CORRECTION, beside the note — the step stays as it was.
    let car = get_job(&app, &id).await;
    let said = "merged as c85941b423bc at 20:10:41Z; main lost it by 20:11:14Z; not on main";
    let (reads, should_read) = review_correction(&car, said).expect("the note is owed one");
    let review = slug(&car, "review")["id"].as_str().unwrap().to_string();
    let (status, body) = send(
        &app,
        "POST",
        &format!("/api/jobs/{id}/steps/{review}/corrections"),
        Some(
            json!({"field": "note", "reads": reads, "should_read": should_read,
                    "why": "main lost the merge"}),
        ),
    )
    .await;
    assert!(status.is_success(), "the corrections door: {status} {body}");
    let corrected = get_job(&app, &id).await;
    assert_eq!(
        slug(&corrected, "review")["metadata"]["note"],
        "landed on main as c85941b423bc",
        "the landing note itself is untouched"
    );
    assert_eq!(
        review_correction(&corrected, said),
        None,
        "a correction handed to the step is not owed twice"
    );

    // 2. THE SUCCESSOR, opened at `gate`.
    let (status, successor) = send(
        &app,
        "POST",
        "/api/jobs",
        Some(successor_body(&corrected).expect("a successor body")),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "successor rejected: {successor}"
    );
    let sid = successor["id"].as_str().unwrap().to_string();
    let fresh = get_job(&app, &sid).await;
    for w in successor_writes(&fresh, &corrected, MERGE, "2026-09-25T21:20:00Z").unwrap() {
        let step = fresh["steps"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["id"] == w.step_id.as_str())
            .unwrap()
            .clone();
        complete(&app, &sid, &step, w.metadata.clone()).await;
    }
    let successor = get_job(&app, &sid).await;
    assert_eq!(successor["metadata"]["supersedes"], id.as_str());
    assert_eq!(successor["metadata"]["branch"], BRANCH);
    assert_eq!(slug(&successor, "gate")["status"], "ready", "{successor:#}");

    // 3. THE TERMINAL: evidence onto the step, the marker, the status.
    let w = unland_writes(&corrected, &lost(&sid)).expect("the landed car is unlandable");
    let (status, body) = send(
        &app,
        "PATCH",
        &w.outcome.merge_path(&id),
        Some(w.outcome.metadata.clone()),
    )
    .await;
    assert!(status.is_success(), "evidence: {status} {body}");
    let (status, body) = send(
        &app,
        "PATCH",
        &format!("/api/jobs/{id}/metadata"),
        Some(w.marker.clone()),
    )
    .await;
    assert!(status.is_success(), "marker: {status} {body}");
    let marked = get_job(&app, &id).await;
    assert_eq!(slug(&marked, "unlanded")["status"], "ready", "{marked:#}");
    let (status, body) = send(
        &app,
        "PUT",
        &w.outcome.status_path(&id),
        Some(w.outcome.status_body.clone()),
    )
    .await;
    assert!(status.is_success(), "terminal: {status} {body}");

    let closed = get_job(&app, &id).await;
    assert_eq!(closed["status"], "closed", "{closed:#}");
    assert_eq!(closed["metadata"]["outcome"], "unlanded");
    let u = slug(&closed, "unlanded");
    assert_eq!(u["metadata"]["merge_ref"], MERGE);
    assert_eq!(u["metadata"]["superseded_by"], sid.as_str());
    assert!(
        u["metadata"]["evidence"]
            .as_str()
            .unwrap()
            .contains("exited 1")
    );
    assert_eq!(closed["metadata"]["superseded_by"], sid.as_str());
    // BOTH FACTS: the forge did merge it, and main lost it.
    assert_eq!(closed["metadata"]["merged"], "true");
    for not_taken in ["proven", "merged", "abandoned", "disproved"] {
        assert_eq!(slug(&closed, not_taken)["status"], "skipped", "{not_taken}");
    }
    // And the branch has NOT landed, so its successor's green is adopted
    // onto the successor rather than skipped as landed content.
    let cars = vec![closed.clone(), successor.clone()];
    assert!(!is_landed(&closed));
    assert!(landed_car_for(&cars, BRANCH).is_none());
    assert_eq!(
        building_car_for(&cars, BRANCH).map(|c| &c["id"]),
        Some(&json!(sid))
    );
}

/// The terminal cannot be reached without the evidence: its fields are
/// REQUIRED at done, so completing it bare is refused — an aborted
/// terminal completes from any open state, and this is its only gate.
#[tokio::test]
async fn an_unlanded_terminal_without_its_evidence_is_refused() {
    let app = app();
    let id = landed_car(&app).await;
    let (status, body) = send(
        &app,
        "PATCH",
        &format!("/api/jobs/{id}/metadata"),
        Some(json!({ "unlanded": "true" })),
    )
    .await;
    assert!(status.is_success(), "marker: {status} {body}");
    let car = get_job(&app, &id).await;
    let u = slug(&car, "unlanded");
    assert_eq!(u["status"], "ready");
    assert_eq!(
        u["metadata"]["outcome_kind"], "aborted",
        "Q1: the delivery did not complete"
    );
    let (status, body) = send(
        &app,
        "PUT",
        &format!("/api/jobs/{id}/steps/{}", u["id"].as_str().unwrap()),
        Some(json!({ "status": "completed" })),
    )
    .await;
    assert!(
        status.is_client_error(),
        "an unlanded terminal with no reading and no successor must be refused: {status} {body}"
    );
    assert_eq!(get_job(&app, &id).await["status"], "open");
}
