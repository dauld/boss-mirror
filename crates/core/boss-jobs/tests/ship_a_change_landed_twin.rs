//! 87f1c86a: a parked car whose commits landed inside ANOTHER car
//! closes through its own terminal — `landed-twin` — carrying the proof,
//! with its hold released; a car replaced by another closes through
//! `abandoned`, naming its successor.
//!
//! What happened (2026-09-24): four parked cars were twins of work that
//! had landed in train #634 inside other cars, or had been replaced by a
//! rebuild. No terminal said so and no verb could close them, so they
//! were held by hand and the dock read HELD 4 until someone closed them
//! by hand. `boss car retire` is the verb; this pins the half of it the
//! PROTOCOL owns, against the real router and the real platform bundle:
//! the exact writes `boss_jobs::car_retire::retire_writes` returns, in
//! the order the CLI performs them, close the car with the outcome, the
//! evidence on the step, and the hold gone.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::car_retire::{Retirement, retire_writes};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use serde_json::{Value, json};
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

/// A car at the dock, HELD: scope, build and gate completed the way the
/// surfaces complete them, review open with a hold on it.
async fn held_car(app: &axum::Router, branch: &str) -> String {
    let (status, job) = send(
        app,
        "POST",
        "/api/jobs",
        Some(json!({
            "kind": "ship-a-change",
            "subject": { "subject_kind": "custom", "id": branch },
            "title": "A car whose commits rode inside another",
            "owner_id": "emp-bootstrap-admin",
            "priority": "standard",
            "status": "open",
            "metadata": { "branch": branch },
            "tags": ["test"],
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    let id = job["id"].as_str().expect("job id").to_string();
    for _ in 0..6 {
        let current = get_job(app, &id).await;
        let actionable: Vec<Value> = current["steps"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s["spec_slug"] != "review")
            .filter(|s| s["status"] == "ready" || s["status"] == "active")
            .collect();
        if actionable.is_empty() {
            break;
        }
        for s in actionable {
            let mut metadata = s["metadata"].clone();
            for f in s["fields"].as_array().into_iter().flatten() {
                if f["required"].as_bool() == Some(true) {
                    let name = f["name"].as_str().unwrap_or_default();
                    metadata[name] = json!("x");
                }
            }
            let (status, body) = send(
                app,
                "PUT",
                &format!("/api/jobs/{id}/steps/{}", s["id"].as_str().unwrap()),
                Some(json!({ "status": "completed", "metadata": metadata })),
            )
            .await;
            assert!(
                status.is_success(),
                "completing {}: {status} {body}",
                s["title"]
            );
        }
    }
    let parked = get_job(app, &id).await;
    let review = slug(&parked, "review")["id"].as_str().unwrap().to_string();
    assert_eq!(slug(&parked, "review")["status"], "ready", "{parked:#}");
    let (status, body) = send(
        app,
        "PATCH",
        &format!("/api/jobs/{id}/steps/{review}/metadata"),
        Some(json!({ "hold": "Duplicate: its commits ride inside a newer green car" })),
    )
    .await;
    assert!(status.is_success(), "hold: {status} {body}");
    id
}

/// The writes `retire_writes` returns, performed in its order — the
/// order `boss car retire` performs them.
async fn retire(app: &axum::Router, id: &str, r: &Retirement) -> Value {
    let car = get_job(app, id).await;
    let w = retire_writes(&car, r).expect("the held car is retirable");
    let (status, body) = send(
        app,
        "PATCH",
        &w.outcome.merge_path(id),
        Some(w.outcome.metadata.clone()),
    )
    .await;
    assert!(status.is_success(), "evidence: {status} {body}");
    let review = w.held_review.clone().expect("the car is held");
    let (status, body) = send(
        app,
        "PATCH",
        &format!("/api/jobs/{id}/steps/{review}/metadata"),
        Some(json!({ "hold": null })),
    )
    .await;
    assert!(status.is_success(), "release: {status} {body}");
    let (status, body) = send(
        app,
        "PATCH",
        &format!("/api/jobs/{id}/metadata"),
        Some(w.marker.clone()),
    )
    .await;
    assert!(status.is_success(), "marker: {status} {body}");
    let marked = get_job(app, id).await;
    assert_eq!(
        slug(&marked, r.outcome_slug())["status"],
        "ready",
        "the marker readies the terminal: {marked:#}"
    );
    let (status, body) = send(
        app,
        "PUT",
        &w.outcome.status_path(id),
        Some(w.outcome.status_body.clone()),
    )
    .await;
    assert!(status.is_success(), "terminal: {status} {body}");
    get_job(app, id).await
}

#[tokio::test]
async fn a_landed_twin_closes_through_its_own_terminal_with_the_proof() {
    let app = app();
    let id = held_car(&app, "feat/a-region-owns-its-page-2-rerail").await;
    let closed = retire(
        &app,
        &id,
        &Retirement::CarriedBy {
            by: "feat/it-shop-floor-actors-and-a-plant-strip-2-rerail".into(),
            evidence: "697951cd carried as 2213c8f7".into(),
        },
    )
    .await;

    assert_eq!(closed["status"], "closed", "{closed:#}");
    assert_eq!(closed["metadata"]["outcome"], "landed-twin");
    let twin = slug(&closed, "landed-twin");
    assert_eq!(twin["status"], "completed");
    assert_eq!(
        twin["metadata"]["carried_by"],
        "feat/it-shop-floor-actors-and-a-plant-strip-2-rerail"
    );
    assert_eq!(twin["metadata"]["evidence"], "697951cd carried as 2213c8f7");
    assert_eq!(
        twin["metadata"]["outcome_kind"], "completed",
        "the work landed — a twin is not an abort"
    );
    for not_taken in ["merged", "abandoned", "settled", "review", "proven"] {
        assert_eq!(slug(&closed, not_taken)["status"], "skipped", "{not_taken}");
    }
    assert!(
        slug(&closed, "review")["metadata"].get("hold").is_none(),
        "the hold came off before the close froze the review step: {closed:#}"
    );
}

/// The terminal cannot be reached without the proof: its two fields are
/// REQUIRED at done, so completing it bare is refused.
#[tokio::test]
async fn a_landed_twin_without_its_proof_is_refused() {
    let app = app();
    let id = held_car(&app, "feat/delete-the-used-device-shop-engine-2").await;
    let (status, body) = send(
        &app,
        "PATCH",
        &format!("/api/jobs/{id}/metadata"),
        Some(json!({ "landed_twin": "true" })),
    )
    .await;
    assert!(status.is_success(), "marker: {status} {body}");
    let car = get_job(&app, &id).await;
    let twin = slug(&car, "landed-twin");
    assert_eq!(twin["status"], "ready");
    let (status, body) = send(
        &app,
        "PUT",
        &format!("/api/jobs/{id}/steps/{}", twin["id"].as_str().unwrap()),
        Some(json!({ "status": "completed" })),
    )
    .await;
    assert!(
        status.is_client_error(),
        "a twin terminal with no carrier and no proof must be refused: {status} {body}"
    );
    assert_eq!(get_job(&app, &id).await["status"], "open");
}

#[tokio::test]
async fn a_superseded_car_closes_through_abandoned_naming_its_successor() {
    let app = app();
    let id = held_car(&app, "feat/it-phone-strip-map").await;
    let closed = retire(
        &app,
        &id,
        &Retirement::SupersededBy {
            by: "d4439bcf-5e48-4010-856b-4a94ffa54ccc".into(),
            evidence: "feat/it-phone-strip-map-2 names the same item".into(),
        },
    )
    .await;
    assert_eq!(closed["status"], "closed", "{closed:#}");
    assert_eq!(closed["metadata"]["outcome"], "abandoned");
    assert_eq!(
        closed["metadata"]["superseded_by"],
        "d4439bcf-5e48-4010-856b-4a94ffa54ccc"
    );
    assert_eq!(
        slug(&closed, "abandoned")["metadata"]["superseded_by"],
        "d4439bcf-5e48-4010-856b-4a94ffa54ccc"
    );
    assert!(slug(&closed, "review")["metadata"].get("hold").is_none());
}
