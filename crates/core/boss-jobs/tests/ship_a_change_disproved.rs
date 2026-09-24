//! 08664157: a car that LANDED and whose claim was then MEASURED FALSE
//! closes through its own terminal — `disproved` — carrying the failing
//! probe's run, what it means, and the remedy that supersedes it.
//!
//! What happened (2026-09-24): car 6b23d135 claimed boss-dev is not the
//! first eviction under disk pressure; after it converged boss-dev was
//! evicted for ephemeral-storage anyway. `boss prove` refuses to record a
//! failing probe, so the car could only stand at `proven`, counted
//! "ours" in the shed, for ever. `boss prove --disproved` is the verb;
//! this pins the half of it the PROTOCOL owns, against the real router
//! and the real platform bundle: the exact writes
//! `boss_jobs::car_disprove::disprove_writes` returns, in the order the
//! CLI performs them, close the car with the outcome and the evidence on
//! the step — and nothing reaches the terminal without that evidence, or
//! before the car has landed.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::car_disprove::{Disproof, disprove_writes};
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

/// A car that went through the whole pipeline up to `proven`: scope,
/// build, gate and review completed the way the surfaces complete them,
/// and — when `landed` — the conductor's `merged` marker set, so
/// `proven` stands ready. Car 6b23d135's shape on 2026-09-24.
async fn car_at_the_shed(app: &axum::Router, branch: &str, landed: bool) -> String {
    let (status, job) = send(
        app,
        "POST",
        "/api/jobs",
        Some(json!({
            "kind": "ship-a-change",
            "subject": { "subject_kind": "custom", "id": branch },
            "title": "A car whose claim was measured false",
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
    for _ in 0..8 {
        let current = get_job(app, &id).await;
        let actionable: Vec<Value> = current["steps"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s["kind"] != "outcome" && s["spec_slug"] != "proven")
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
    let at_review = get_job(app, &id).await;
    assert_eq!(slug(&at_review, "review")["status"], "completed");
    if landed {
        let (status, body) = send(
            app,
            "PATCH",
            &format!("/api/jobs/{id}/metadata"),
            Some(json!({ "merged": "true", "merge_ref": "87c2e3667cdf" })),
        )
        .await;
        assert!(status.is_success(), "merged marker: {status} {body}");
        let landed = get_job(app, &id).await;
        assert_eq!(slug(&landed, "proven")["status"], "ready", "{landed:#}");
    }
    id
}

fn measured_false() -> Disproof {
    Disproof {
        verified: "boss-dev (priority 1000) was evicted for ephemeral-storage on w-1 at \
                   2026-09-24T01:27:23Z, after this car converged"
            .into(),
        disproof: json!({"probe": "kubectl get events …", "exit": 1,
                         "stdout": "EVICTED boss-dev-7889cd5f98-hnl2n 2026-09-24T01:27:23Z",
                         "stderr": ""})
        .to_string(),
        superseded_by: "3f2a08ab-0000-4000-8000-000000000000".into(),
        remedy: "landed car 3f2a08ab (fix/the-scratch-floor-follows-the-build)".into(),
        completed_at: "2026-09-24T20:30:00Z".into(),
    }
}

/// The writes `disprove_writes` returns, performed in its order — the
/// order `boss prove --disproved` performs them.
async fn disprove(app: &axum::Router, id: &str, d: &Disproof) -> Value {
    let car = get_job(app, id).await;
    let w = disprove_writes(&car, d).expect("the landed car is disprovable");
    assert_eq!(w.held_review, None, "a landed car has no hold to release");
    let (status, body) = send(
        app,
        "PATCH",
        &w.outcome.merge_path(id),
        Some(w.outcome.metadata.clone()),
    )
    .await;
    assert!(status.is_success(), "evidence: {status} {body}");
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
        slug(&marked, "disproved")["status"],
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
async fn a_disproved_car_closes_through_its_own_terminal_with_the_failing_probe() {
    let app = app();
    let id = car_at_the_shed(&app, "fix/dev-pod-not-first-evicted", true).await;
    let closed = disprove(&app, &id, &measured_false()).await;

    assert_eq!(closed["status"], "closed", "{closed:#}");
    assert_eq!(closed["metadata"]["outcome"], "disproved");
    let d = slug(&closed, "disproved");
    assert_eq!(d["status"], "completed");
    assert!(
        d["metadata"]["verified"]
            .as_str()
            .unwrap()
            .contains("evicted"),
        "{d:#}"
    );
    assert!(
        d["metadata"]["disproof"]
            .as_str()
            .unwrap()
            .contains("\"exit\":1"),
        "the failing run, verbatim: {d:#}"
    );
    assert_eq!(
        d["metadata"]["superseded_by"],
        "3f2a08ab-0000-4000-8000-000000000000"
    );
    assert_eq!(
        closed["metadata"]["superseded_by"], "3f2a08ab-0000-4000-8000-000000000000",
        "the remedy on the packet too, where a reader finds it without the step"
    );
    // STILL IN THE RECORD, STILL LANDED: the car is closed, not deleted,
    // and the conductor's marker that it shipped is untouched.
    assert_eq!(closed["metadata"]["merged"], "true");
    // The shed reads open cars at `proven`; this one is neither now.
    for not_taken in ["proven", "merged", "abandoned", "settled", "landed-twin"] {
        assert_eq!(slug(&closed, not_taken)["status"], "skipped", "{not_taken}");
    }
}

/// The terminal cannot be reached without the evidence: its three
/// fields are REQUIRED at done, so completing it bare is refused.
#[tokio::test]
async fn a_disproved_terminal_without_its_evidence_is_refused() {
    let app = app();
    let id = car_at_the_shed(&app, "fix/a-claim-nobody-measured", true).await;
    let (status, body) = send(
        &app,
        "PATCH",
        &format!("/api/jobs/{id}/metadata"),
        Some(json!({ "disproved": "true" })),
    )
    .await;
    assert!(status.is_success(), "marker: {status} {body}");
    let car = get_job(&app, &id).await;
    let d = slug(&car, "disproved");
    assert_eq!(d["status"], "ready");
    let (status, body) = send(
        &app,
        "PUT",
        &format!("/api/jobs/{id}/steps/{}", d["id"].as_str().unwrap()),
        Some(json!({ "status": "completed" })),
    )
    .await;
    assert!(
        status.is_client_error(),
        "a disproved terminal with no probe run and no remedy must be refused: {status} {body}"
    );
    assert_eq!(get_job(&app, &id).await["status"], "open");
}

/// A change that never shipped cannot be disproved in production: the
/// marker alone does not ready the terminal, and the writes are refused.
#[tokio::test]
async fn a_car_that_has_not_landed_cannot_be_disproved() {
    let app = app();
    let id = car_at_the_shed(&app, "fix/still-on-the-dock", false).await;
    let (status, body) = send(
        &app,
        "PATCH",
        &format!("/api/jobs/{id}/metadata"),
        Some(json!({ "disproved": "true" })),
    )
    .await;
    assert!(status.is_success(), "marker: {status} {body}");
    let car = get_job(&app, &id).await;
    assert_eq!(
        slug(&car, "disproved")["status"],
        "pending",
        "the terminal waits on `merged` as `proven` does: {car:#}"
    );
    let e = disprove_writes(&car, &measured_false()).unwrap_err();
    assert!(e.contains("has not landed"), "{e}");
}
