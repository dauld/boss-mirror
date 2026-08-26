//! `jobs.job.closed` is ONE contract with three emit sites.
//!
//! A Job can close three ways — a declared terminal step completing
//! (`close_job_on_terminal`), every step reaching a terminal state
//! (the `compute_job_status` catch-all), and a direct status PUT — and
//! each built its own payload. That is fine right up until a
//! dispatcher rule gates on one of the keys, because the expr binder
//! makes an ABSENT identifier a `PredicateFailed`, which the runner
//! NAKs and eventually dead-letters. An absent key is not a quiet
//! false; it is a retry storm.
//!
//! Measured shape of the drift when this test was written: the
//! direct-PUT site emitted only `id` + `closed_on`, so the shipped
//! `resolve-subjob-on-child-job-closed` rule (`when =
//! "parent_step_id != null"`) could not evaluate against it at all.
//!
//! So all three sites carry the same keys, always, with null standing
//! in for "no answer": `id`, `closed_on`, `kind`, `outcome`,
//! `parent_step_id`, `title`. `kind` and `outcome` are what let a rule
//! select its Workflow and its terminal as DATA rather than by fetching
//! the Job to find out whether the event was even about it; `title` is
//! what lets a rule that SPAWNS off a close name the packet it creates,
//! since the arg language offers only a literal or an identifier from
//! this payload and has no concatenation.
//!
//! The set grows by incident, not by design review, which is worth
//! saying out loud: `parent_step_id` was added because a subjob rule
//! could not evaluate, and `title` because a spawn rule dead-lettered.
//! Both times the missing key had been invisible until a rule reached
//! for it in production.

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

/// A Workflow with no DECLARED terminal: its one step completing
/// leaves every step terminal, so the Job closes through the
/// `compute_job_status` catch-all. That is the emit site under test —
/// none of the platform kinds can reach it, because they all declare
/// outcomes.
fn catch_all_spec() -> boss_jobs::registry::WorkflowSpec {
    let mut spec = boss_jobs::registry::WorkflowSpec::platform_seed(
        "closes-by-catch-all",
        "Closes by catch-all",
        "platform",
        vec!["custom".into()],
        vec![boss_jobs::registry::StepSpec {
            title: "work".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: "The only work".into(),
            authority_role: Some("platform-admin".into()),
            ..Default::default()
        }],
    );
    spec.metadata = serde_json::json!({ "owner_role": "platform-admin" });
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

/// The router plus the in-memory jobs adapter, kept so the test can
/// read back the events the outbox paths recorded.
fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::new());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    kinds
        .seed(catch_all_spec())
        .expect("seed the catch-all kind");
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
            .allow("platform-admin", Action::Close, Resource::job(), Scope::All)
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
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: Some(Arc::new(AdminRoster)),
        clock: Arc::new(boss_clock_client::WallClockClient),
    };
    (router(state), jobs)
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

async fn open_job(app: &axum::Router, kind: &str, metadata: serde_json::Value) -> String {
    let (status, job) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({
                    "kind": kind,
                    "subject": { "subject_kind": "custom", "id": "/system/flow" },
                    "title": format!("A {kind} packet"),
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "metadata": metadata,
                    "tags": ["test"],
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    job["id"].as_str().expect("job id").to_string()
}

/// Complete every ready/active step, filling required fields from each
/// step's own declared schema, until nothing is actionable. `choose`
/// picks the value for an enum-shaped (pipe-separated) field.
async fn drive_to_quiescence(app: &axum::Router, job_id: &str, choose: &dyn Fn(&str) -> String) {
    for _ in 0..8 {
        let current = get_job(app, job_id).await;
        if current["status"] == "closed" {
            return;
        }
        let steps = current["steps"].as_array().cloned().unwrap_or_default();
        let actionable: Vec<serde_json::Value> = steps
            .iter()
            .filter(|s| s["status"] == "ready" || s["status"] == "active")
            .cloned()
            .collect();
        if actionable.is_empty() {
            return;
        }
        for s in actionable {
            let step_id = s["id"].as_str().expect("step id");
            // Merge, never replace: `authority_role` shares this object.
            let mut metadata = s["metadata"].clone();
            for f in s["fields"].as_array().into_iter().flatten() {
                if f["required"].as_bool() != Some(true) {
                    continue;
                }
                let name = f["name"].as_str().unwrap_or_default();
                let declared = f["field_type"].as_str().unwrap_or_default();
                metadata[name] = serde_json::Value::String(choose(declared));
            }
            let (status, body) = send(
                app,
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
}

/// The keys every emit site owes a consumer, in the shape a rule's
/// `when` binds them.
fn assert_close_marker_shape(payload: &serde_json::Value, site: &str) {
    // `title` joined the contract on 2026-08-15, and it was a live rule
    // that forced it: `spawn-car-on-sweep-remediated` binds
    // `title = "title"` to name the car it spawns, the key was not on
    // the payload, and the event NAKed eight times and dead-lettered.
    // The rule had never once fired — the mechanism that makes the dock
    // self-filling from a remediated sweep has never worked in
    // production. An arg is not softer than a predicate: both bind
    // through the same expr binder, and both turn a missing key into a
    // retry storm.
    for key in [
        "id",
        "closed_on",
        "kind",
        "outcome",
        "parent_step_id",
        "title",
    ] {
        assert!(
            payload.get(key).is_some(),
            "the {site} close marker omits `{key}` — a rule binding it gets \
             PredicateFailed → Retry → dead-letter, not a quiet false. Payload: {payload:#}"
        );
    }
}

fn close_markers(jobs: &InMemoryJobs) -> Vec<serde_json::Value> {
    jobs.recorded_events()
        .into_iter()
        .filter(|e| e.kind == "jobs.job.closed")
        .map(|e| e.payload)
        .collect()
}

/// Site 1: a declared terminal step completing. This is the path a
/// merged car takes, and `outcome` is the whole point — it is how a
/// rule tells "merged" from "abandoned" without fetching the Job.
#[tokio::test]
async fn the_declared_terminal_close_names_its_kind_and_outcome() {
    let (app, jobs) = app();
    let job_id = open_job(
        &app,
        "ship-a-change",
        serde_json::json!({ "branch": "feat/a-change" }),
    )
    .await;

    drive_to_quiescence(&app, &job_id, &|declared| {
        declared.split('|').next().unwrap_or("x").to_string()
    })
    .await;

    // The conductor's marker write at real merge time, then completing
    // the outcome it wakes (what the dispatcher's marker handler does).
    let parked = get_job(&app, &job_id).await;
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
    drive_to_quiescence(&app, &job_id, &|declared| {
        declared.split('|').next().unwrap_or("x").to_string()
    })
    .await;

    let markers = close_markers(&jobs);
    let marker = markers.last().expect("a close marker was recorded");
    assert_close_marker_shape(marker, "declared-terminal");
    assert_eq!(marker["kind"], "ship-a-change");
    assert_eq!(
        marker["outcome"], "merged",
        "the merged terminal must name itself: {marker:#}"
    );
}

/// Site 2: the all-steps-terminal catch-all. No declared outcome
/// fired, so `outcome` is null — present and null, which evaluates to
/// a clean false rather than blowing up the binder.
#[tokio::test]
async fn the_catch_all_close_carries_the_keys_with_null_for_no_answer() {
    let (app, jobs) = app();
    // A kind that declares no terminal, so completing its one step
    // leaves every step terminal and the catch-all does the closing.
    let job_id = open_job(&app, "closes-by-catch-all", serde_json::json!({})).await;

    drive_to_quiescence(&app, &job_id, &|declared| {
        declared.split('|').next().unwrap_or("x").to_string()
    })
    .await;

    let closed = get_job(&app, &job_id).await;
    assert_eq!(closed["status"], "closed", "expected a catch-all close");

    let markers = close_markers(&jobs);
    let marker = markers.last().expect("a close marker was recorded");
    assert_close_marker_shape(marker, "catch-all");
    assert_eq!(marker["kind"], "closes-by-catch-all");
    assert!(
        marker["outcome"].is_null(),
        "a catch-all close declares no outcome: {marker:#}"
    );
}

/// Site 3: a direct status PUT. The site that carried only two keys —
/// so the shipped subjob rule's `parent_step_id != null` predicate
/// could not evaluate against it at all.
#[tokio::test]
async fn the_direct_status_put_close_carries_the_same_keys() {
    let (app, jobs) = app();
    let job_id = open_job(&app, "closes-by-catch-all", serde_json::json!({})).await;

    let mut job = get_job(&app, &job_id).await;
    job["status"] = serde_json::Value::String("closed".into());
    let (status, body) = send(
        &app,
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{job_id}"))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(job.to_string()))
            .unwrap(),
    )
    .await;
    assert!(status.is_success(), "close PUT failed: {status} {body}");

    let markers = close_markers(&jobs);
    let marker = markers.last().expect("a close marker was recorded");
    assert_close_marker_shape(marker, "direct-status-put");
    assert_eq!(marker["kind"], "closes-by-catch-all");
}
