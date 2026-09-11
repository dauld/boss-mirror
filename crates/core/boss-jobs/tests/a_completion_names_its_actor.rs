//! A completion names its actor.
//!
//! On 2026-09-08 David approved two design-review steps in the UI. The
//! completed steps carried `answer` and `assignee_id` and nothing
//! else — no who, no when — so the operator session could not tell a
//! human acceptance from a rule copying `proposed` into `answer`, and
//! had to ask. The audit log held the actor on the `step.done` event
//! the whole time, but nothing read it per packet (c17871fe).
//!
//! Two halves, both pinned here:
//!
//!  - every step completion stamps `completed_by` (the actor the API
//!    signed the write with) and `completed_at` (the clock's instant)
//!    on the step, server-side. A client-supplied value is overwritten,
//!    not honoured — provenance is the first correctness property and
//!    a stamp the writer chooses is not provenance;
//!  - `GET /api/jobs/{id}/events` reads the packet's own slice of the
//!    audit log, so the actor the log already holds is one click from
//!    the step.

use std::sync::Arc;

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

#[async_trait::async_trait]
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

/// `with_registry = false` leaves `kind_registry: None`, which skips
/// the append-a-step guard — the only route to POSTing an ad-hoc step
/// straight in as `completed`.
fn app(with_registry: bool) -> axum::Router {
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
    let kind_registry: Option<Arc<dyn WorkflowRegistry>> = if with_registry {
        let kinds = Arc::new(InMemoryWorkflows::new());
        for spec in seedable_platform_workflows() {
            kinds.seed(spec).expect("seed platform kind");
        }
        Some(kinds as Arc<dyn WorkflowRegistry>)
    } else {
        None
    };
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs,
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry,
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

/// The session the writes are signed with — a human, by id.
const ADMIN_ID: &str = "emp-bootstrap-admin";
const ADMIN: &str = r#"{"id":"emp-bootstrap-admin","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.expect("request");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let json = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            serde_json::Value::String(String::from_utf8_lossy(&bytes).to_string())
        })
    };
    (status, json)
}

fn req(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", ADMIN)
        .body(Body::from(body.to_string()))
        .expect("request")
}

/// Opens a `ship-a-change` packet and returns `(job_id, scope_step_id)`
/// — `scope` is a `task` step that is ready at admission.
async fn open_packet(app: &axum::Router) -> (String, String) {
    let (status, job) = send(
        app,
        req(
            "POST",
            "/api/jobs",
            serde_json::json!({
                "kind": "ship-a-change",
                "subject": {"subject_kind": "custom", "id": "feat/x"},
                "title": "t", "owner_id": ADMIN_ID,
                "status": "open", "priority": "standard",
                "metadata": {}, "tags": [],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "job create failed: {job}");
    let job_id = job["id"].as_str().expect("job id").to_string();
    let scope = scope_step(app, &job_id).await;
    assert_eq!(scope["status"], "ready", "precondition: scope is ready");
    assert!(
        scope["completed_by"].is_null() && scope["completed_at"].is_null(),
        "precondition: an open step carries no completion stamp: {scope}"
    );
    (job_id, scope["id"].as_str().expect("step id").to_string())
}

async fn scope_step(app: &axum::Router, job_id: &str) -> serde_json::Value {
    let (status, full) = send(
        app,
        req("GET", &format!("/api/jobs/{job_id}"), serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["spec_slug"] == "scope")
        .expect("scope step present")
        .clone()
}

fn completion_body() -> serde_json::Value {
    serde_json::json!({
        "status": "completed",
        "metadata": {"summary": "s", "excludes": "e",
                     "authority_role": "platform-admin"},
    })
}

/// A plain completion — no stamp in the body — reads back stamped with
/// the actor the write was signed with and the instant it landed.
#[tokio::test]
async fn a_completion_is_stamped_with_the_signing_actor_and_the_instant() {
    let app = app(true);
    let (job_id, scope_id) = open_packet(&app).await;

    let (status, body) = send(
        &app,
        req(
            "PUT",
            &format!("/api/jobs/{job_id}/steps/{scope_id}"),
            completion_body(),
        ),
    )
    .await;
    assert!(status.is_success(), "completing scope: {status}: {body}");

    let scope = scope_step(&app, &job_id).await;
    assert_eq!(scope["status"], "completed", "{scope}");
    assert_eq!(
        scope["completed_by"], ADMIN_ID,
        "completed_by is the actor the API signed the write with: {scope}"
    );
    let at = scope["completed_at"]
        .as_str()
        .unwrap_or_else(|| panic!("completed_at is set on completion: {scope}"));
    assert!(
        chrono::DateTime::parse_from_rfc3339(at).is_ok(),
        "completed_at is an RFC 3339 instant, got {at:?}"
    );
}

/// A body that supplies its own `completed_by` / `completed_at` does
/// not get to choose them: the server's stamp wins, silently. Provenance
/// the writer picks is not provenance.
#[tokio::test]
async fn a_client_supplied_stamp_is_overwritten_with_the_real_actor() {
    let app = app(true);
    let (job_id, scope_id) = open_packet(&app).await;

    let mut body = completion_body();
    body["completed_by"] = serde_json::json!("forged");
    body["completed_at"] = serde_json::json!("1999-01-01T00:00:00Z");
    let (status, resp) = send(
        &app,
        req("PUT", &format!("/api/jobs/{job_id}/steps/{scope_id}"), body),
    )
    .await;
    assert!(status.is_success(), "completing scope: {status}: {resp}");

    let scope = scope_step(&app, &job_id).await;
    assert_eq!(
        scope["completed_by"], ADMIN_ID,
        "a forged completed_by is overwritten with the signing actor: {scope}"
    );
    assert_ne!(
        scope["completed_at"], "1999-01-01T00:00:00Z",
        "a forged completed_at is overwritten with the server's instant: {scope}"
    );
    assert!(scope["completed_at"].is_string(), "{scope}");
}

/// A stamp is written once, at the flip, and a later write cannot move
/// it: a re-PUT of the completed step carrying a different
/// `completed_by` is neither honoured nor refused — the stamp is
/// server-owned, so the body's copy is simply not read.
#[tokio::test]
async fn a_stamp_survives_a_later_write_unchanged() {
    let app = app(true);
    let (job_id, scope_id) = open_packet(&app).await;

    let (status, body) = send(
        &app,
        req(
            "PUT",
            &format!("/api/jobs/{job_id}/steps/{scope_id}"),
            completion_body(),
        ),
    )
    .await;
    assert!(status.is_success(), "completing scope: {status}: {body}");
    let first = scope_step(&app, &job_id).await;

    // An idempotent redelivery of the same completion, now also
    // claiming a different actor.
    let mut again = completion_body();
    again["completed_by"] = serde_json::json!("someone-else");
    let (status, body) = send(
        &app,
        req(
            "PUT",
            &format!("/api/jobs/{job_id}/steps/{scope_id}"),
            again,
        ),
    )
    .await;
    assert!(
        status.is_success(),
        "a redelivery is a no-op: {status}: {body}"
    );

    let second = scope_step(&app, &job_id).await;
    assert_eq!(second["completed_by"], first["completed_by"], "{second}");
    assert_eq!(second["completed_at"], first["completed_at"], "{second}");
}

/// The packet's own slice of the audit log is one read away: the
/// `step.done.<kind>` event for the completed step lists the actor the
/// step's stamp names.
#[tokio::test]
async fn the_events_read_lists_the_step_done_event_with_its_actor() {
    let app = app(true);
    let (job_id, scope_id) = open_packet(&app).await;

    let (status, body) = send(
        &app,
        req(
            "PUT",
            &format!("/api/jobs/{job_id}/steps/{scope_id}"),
            completion_body(),
        ),
    )
    .await;
    assert!(status.is_success(), "completing scope: {status}: {body}");

    let (status, events) = send(
        &app,
        req(
            "GET",
            &format!("/api/jobs/{job_id}/events"),
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "events read: {events}");
    let rows = events["data"].as_array().expect("data is an array");
    assert!(
        rows.iter().all(|r| r["job_id"] == job_id.as_str()),
        "every row belongs to the packet asked about: {events}"
    );
    let done = rows
        .iter()
        .find(|r| r["kind"] == "step.done.task" && r["step_id"] == scope_id.as_str())
        .unwrap_or_else(|| panic!("step.done.task for scope in the packet's events: {events}"));
    assert_eq!(done["actor"], ADMIN_ID, "the event names the actor: {done}");
    assert!(
        done["at"].is_string(),
        "the event names the instant: {done}"
    );
    // Chronological: admission precedes the completion.
    let created = rows
        .iter()
        .position(|r| r["kind"] == "jobs.job.created")
        .expect("the packet's admission is in its own history");
    let done_at = rows
        .iter()
        .position(|r| r["kind"] == "step.done.task")
        .expect("done");
    assert!(created < done_at, "events read oldest first: {events}");
}

/// Another packet's events are not this packet's: the read is scoped
/// by the job id in the WHERE, not by filtering a page.
#[tokio::test]
async fn the_events_read_is_scoped_to_one_packet() {
    let app = app(true);
    let (job_a, scope_a) = open_packet(&app).await;
    let (job_b, _scope_b) = open_packet(&app).await;

    let (status, body) = send(
        &app,
        req(
            "PUT",
            &format!("/api/jobs/{job_a}/steps/{scope_a}"),
            completion_body(),
        ),
    )
    .await;
    assert!(status.is_success(), "completing scope: {status}: {body}");

    let (status, events) = send(
        &app,
        req(
            "GET",
            &format!("/api/jobs/{job_b}/events"),
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "events read: {events}");
    let rows = events["data"].as_array().expect("data is an array");
    assert!(!rows.is_empty(), "packet B has its own admission events");
    assert!(
        rows.iter().all(|r| r["kind"] != "step.done.task"),
        "packet A's completion is not in packet B's history: {events}"
    );
}

/// A step POSTed straight in as `completed` is a completion too: it is
/// born with the signing actor's stamps, and the body's own are
/// overwritten the same way the PUT's are.
#[tokio::test]
async fn a_step_born_completed_carries_the_same_stamps() {
    // No kind registry: opens a job under a spec-less kind and skips
    // the append-a-step guard.
    let app = app(false);
    let (status, job) = send(
        &app,
        req(
            "POST",
            "/api/jobs",
            serde_json::json!({
                "kind": "generic",
                "subject": {"subject_kind": "custom", "id": "no-spec"},
                "title": "t", "owner_id": ADMIN_ID,
                "status": "open", "priority": "standard",
                "metadata": {}, "tags": [],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "job create failed: {job}");
    let job_id = job["id"].as_str().expect("job id").to_string();

    let step_id = uuid::Uuid::new_v4().to_string();
    let (status, body) = send(
        &app,
        req(
            "POST",
            &format!("/api/jobs/{job_id}/steps"),
            serde_json::json!({
                "id": step_id,
                "job_id": job_id,
                "kind": "generic",
                "title": "recorded after the fact",
                "status": "completed",
                "blocked_by": [],
                "sign_offs_required": [],
                "sign_offs": [],
                "metadata": {},
                "completed_by": "forged",
                "completed_at": "1999-01-01T00:00:00Z",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "step create failed: {body}");

    let (status, full) = send(
        &app,
        req("GET", &format!("/api/jobs/{job_id}"), serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    let step = full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["id"] == step_id.as_str())
        .expect("the posted step")
        .clone();
    assert_eq!(step["completed_by"], ADMIN_ID, "{step}");
    assert_ne!(step["completed_at"], "1999-01-01T00:00:00Z", "{step}");
    assert!(step["completed_at"].is_string(), "{step}");
}
