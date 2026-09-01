//! The precise-instant stamps behind the terminal report's cycle
//! time.
//!
//! `opened_on` / `closed_on` are business DATES — one-day resolution
//! by construction, so a packet closed 30 minutes after opening
//! measured 0 days and a protocol iteration that halved an
//! afternoon's cycle was invisible (1a60cf7a). Adding timestamp
//! columns was rejected (66 struct-literal sites); the instants ride
//! in the schemaless Job metadata instead: `opened_at` at admission,
//! `closed_at` at the two step-driven close hooks. The terminal
//! report prefers the stamps when both parse
//! (tests/terminal_report_http.rs, tests/terminal_report_pg.rs).
//!
//! The stamps are the clock's instants, so they are only written when
//! the clock owns the date: an admission that carries an explicit
//! (backdated, sim-historical) `opened_on` names a different instant
//! than `now`, and a stamp that disagreed with the date beside it
//! would override that date in the cycle-time preference.

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
/// `compute_job_status` catch-all — the second of the two close hooks
/// under test.
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

fn app() -> axum::Router {
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

/// POST the packet and read it back — the create response carries
/// only `{ id }`.
async fn open_job(app: &axum::Router, body: serde_json::Value) -> serde_json::Value {
    let (status, job) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    get_job(app, job["id"].as_str().expect("job id")).await
}

fn job_body(kind: &str, metadata: serde_json::Value) -> serde_json::Value {
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
}

/// Complete every ready/active step, filling required fields from each
/// step's own declared schema, until nothing is actionable.
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

/// The stamp as the report's preference will read it — present, a
/// string, and RFC3339-parseable — or the test says which of those it
/// is not.
fn parsed_stamp(job: &serde_json::Value, key: &str) -> chrono::DateTime<chrono::FixedOffset> {
    let raw = job["metadata"][key]
        .as_str()
        .unwrap_or_else(|| panic!("metadata.{key} missing or not a string: {job:#}"));
    chrono::DateTime::parse_from_rfc3339(raw)
        .unwrap_or_else(|e| panic!("metadata.{key} is not RFC3339 ({e}): {raw}"))
}

#[tokio::test]
async fn admission_stamps_the_precise_open_instant() {
    let app = app();
    let job = open_job(
        &app,
        job_body("closes-by-catch-all", serde_json::json!({ "note": "kept" })),
    )
    .await;

    parsed_stamp(&job, "opened_at");
    assert_eq!(
        job["metadata"]["note"], "kept",
        "the stamp merges into the caller's metadata, never replaces it: {job:#}"
    );
}

#[tokio::test]
async fn a_backdated_admission_gets_no_open_stamp() {
    let app = app();
    let mut body = job_body("closes-by-catch-all", serde_json::json!({}));
    // The simulator's shape: an explicit historical opened_on. The
    // clock's instant is not that date's instant, so no stamp — the
    // packet keeps date arithmetic in the terminal report.
    body["opened_on"] = serde_json::json!("2026-01-05");
    let job = open_job(&app, body).await;

    assert_eq!(job["opened_on"], "2026-01-05");
    assert!(
        job["metadata"].get("opened_at").is_none(),
        "a backdated open must not carry a clock stamp that contradicts \
         its date: {job:#}"
    );
}

#[tokio::test]
async fn the_catch_all_close_stamps_the_precise_close_instant() {
    let app = app();
    let job = open_job(&app, job_body("closes-by-catch-all", serde_json::json!({}))).await;
    let job_id = job["id"].as_str().expect("job id").to_string();

    drive_to_quiescence(&app, &job_id, &|declared| {
        declared.split('|').next().unwrap_or("x").to_string()
    })
    .await;

    let closed = get_job(&app, &job_id).await;
    assert_eq!(closed["status"], "closed", "expected a catch-all close");
    let opened_at = parsed_stamp(&closed, "opened_at");
    let closed_at = parsed_stamp(&closed, "closed_at");
    assert!(
        closed_at >= opened_at,
        "the close instant precedes the open instant: {closed:#}"
    );
}

#[tokio::test]
async fn the_declared_terminal_close_stamps_the_precise_close_instant() {
    let app = app();
    let job = open_job(
        &app,
        job_body(
            "ship-a-change",
            serde_json::json!({ "branch": "feat/a-change" }),
        ),
    )
    .await;
    let job_id = job["id"].as_str().expect("job id").to_string();

    drive_to_quiescence(&app, &job_id, &|declared| {
        declared.split('|').next().unwrap_or("x").to_string()
    })
    .await;

    // The conductor's marker write at real merge time (a full-body
    // PUT), then completing the outcome step it wakes.
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

    let closed = get_job(&app, &job_id).await;
    assert_eq!(closed["status"], "closed", "expected a terminal close");
    assert_eq!(closed["metadata"]["outcome"], "merged");
    let opened_at = parsed_stamp(&closed, "opened_at");
    let closed_at = parsed_stamp(&closed, "closed_at");
    assert!(
        closed_at >= opened_at,
        "the close instant precedes the open instant: {closed:#}"
    );
}
