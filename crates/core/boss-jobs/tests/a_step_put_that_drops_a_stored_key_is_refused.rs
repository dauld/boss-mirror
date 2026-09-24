//! A step PUT whose `metadata` drops a stored key is refused, and the
//! refusal routes to the merge door (backlog e39a9d2a, design baf738b7
//! answered 2026-09-23: THE ONE-CAR RULE).
//!
//! `PUT /api/jobs/{id}/steps/{step_id}` overlays the body onto the
//! stored step, so a body `metadata` REPLACES the stored metadata
//! wholesale. Three keys were carved out of that replace, each after
//! someone lost it in production — `authority_role`, `human_only` and
//! `agent_run` (b91a2103: a completer that sent `metadata` without
//! merging erased the run edge, and the run died four hours later on
//! the silence clock as though the agent had gone quiet). The list grew
//! by incident, and two more step-level keys were in flight.
//!
//! The rule removes the class instead of enumerating it: a body whose
//! `metadata` omits ANY key the stored step has is refused 409, naming
//! the keys, and the caller is routed to `PATCH .../steps/{id}/metadata`
//! — where a key is cleared by sending it as `null`, on purpose. A
//! read-merge-write caller keeps working (it sends every stored key); a
//! status-only PUT with no `metadata` key keeps working; a caller that
//! read a stale copy while a concurrent writer added a key is now
//! caught instead of erasing that key.

use std::sync::Arc;

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

const ADMIN_ID: &str = "emp-bootstrap-admin";
const RUN_A: &str = "5b1d2c3e-0000-4000-8000-00000000000a";
const RUN_B: &str = "5b1d2c3e-0000-4000-8000-00000000000b";

struct AdminRoster;

#[async_trait::async_trait]
impl RosterLookup for AdminRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec![ADMIN_ID.to_string()],
            _ => Vec::new(),
        })
    }

    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == ADMIN_ID)
    }
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
            jobs.clone(),
            bus.clone(),
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    router(state)
}

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

/// A ship-a-change packet and its ready `scope` step — the shape a
/// dispatched run claims.
async fn open_job(app: &axum::Router) -> (String, String) {
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
    assert_eq!(status, StatusCode::CREATED, "job create: {status} {job}");
    let id = job["id"].as_str().expect("job id").to_string();
    let (status, full) = send(
        app,
        req("GET", &format!("/api/jobs/{id}"), serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    let step = full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["spec_slug"] == "scope")
        .expect("the scope step")
        .clone();
    assert_eq!(step["status"], "ready", "precondition: scope is ready");
    (id, step["id"].as_str().expect("step id").to_string())
}

/// The step's stored metadata, read back the way every consumer reads
/// it — through the API, not the adapter.
async fn stored_metadata(app: &axum::Router, job: &str, step: &str) -> serde_json::Value {
    let (status, full) = send(
        app,
        req("GET", &format!("/api/jobs/{job}"), serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["id"] == step)
        .expect("the step")["metadata"]
        .clone()
}

/// The edge as `boss dispatch` writes it: the merge door, one key.
async fn write_edge(app: &axum::Router, job: &str, step: &str, run: &str) {
    let (status, body) = send(
        app,
        req(
            "PATCH",
            &format!("/api/jobs/{job}/steps/{step}/metadata"),
            serde_json::json!({ boss_jobs::agent_runs::EDGE_KEY: run }),
        ),
    )
    .await;
    assert!(status.is_success(), "edge patch: {status} {body}");
}

async fn put_step(
    app: &axum::Router,
    job: &str,
    step: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    send(
        app,
        req("PUT", &format!("/api/jobs/{job}/steps/{step}"), body),
    )
    .await
}

/// THE DEFECT, refused. What a hand-built completion sends — the keys
/// the caller cares about, no read-modify-write — used to erase the run
/// edge. It is now refused, loudly, at the one call site that must
/// change, and nothing is written.
#[tokio::test]
async fn a_metadata_put_that_omits_a_stored_key_is_refused_and_writes_nothing() {
    let app = app();
    let (job, step) = open_job(&app).await;
    write_edge(&app, &job, &step, RUN_A).await;
    let before = stored_metadata(&app, &job, &step).await;

    let (status, body) = put_step(
        &app,
        &job,
        &step,
        serde_json::json!({
            "status": "completed",
            "metadata": {
                "summary": "what the change does",
                "excludes": "what it deliberately leaves alone",
            },
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "refused: {body}");
    let missing: Vec<&str> = body["missing_keys"]
        .as_array()
        .expect("the refusal names the missing keys")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(
        missing.contains(&boss_jobs::agent_runs::EDGE_KEY),
        "the run edge is named among the dropped keys: {body}",
    );
    assert_eq!(
        body["hint"],
        boss_jobs::step_metadata_write::OMITTED_KEYS_HINT,
        "the refusal routes to the merge door and says how to clear, in the one \
         sentence every double of this API also answers with: {body}",
    );
    assert_eq!(
        body["merge_door"],
        format!("/api/jobs/{job}/steps/{step}/metadata"),
        "and names THIS step's merge door, ready to call: {body}",
    );

    let after = stored_metadata(&app, &job, &step).await;
    assert_eq!(after, before, "a refused write writes nothing");
    let (_, full) = send(
        &app,
        req("GET", &format!("/api/jobs/{job}"), serde_json::json!({})),
    )
    .await;
    let row = full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["id"] == step.as_str())
        .expect("the step")
        .clone();
    assert_ne!(row["status"], "completed", "nor flips the status: {row}");
}

/// The required form: read the step, merge, write every key back. The
/// edge survives because the caller sent it, not because the server
/// hand-carried it.
#[tokio::test]
async fn a_read_merge_write_put_lands() {
    let app = app();
    let (job, step) = open_job(&app).await;
    write_edge(&app, &job, &step, RUN_A).await;

    let mut md = stored_metadata(&app, &job, &step).await;
    let obj = md.as_object_mut().expect("stored metadata is an object");
    obj.insert("summary".into(), serde_json::json!("what the change does"));
    let (status, body) = put_step(
        &app,
        &job,
        &step,
        serde_json::json!({ "metadata": md.clone() }),
    )
    .await;
    assert!(status.is_success(), "a merged PUT lands: {status} {body}");

    let stored = stored_metadata(&app, &job, &step).await;
    assert_eq!(stored, md, "the whole merged body is what is stored");
    assert_eq!(
        stored.get(boss_jobs::agent_runs::EDGE_KEY),
        Some(&serde_json::json!(RUN_A)),
        "{stored}"
    );
}

/// A PUT with no `metadata` key at all — a status-only flip — touches
/// no metadata and is not judged by this rule.
#[tokio::test]
async fn a_put_without_metadata_is_not_judged() {
    let app = app();
    let (job, step) = open_job(&app).await;
    write_edge(&app, &job, &step, RUN_A).await;
    let before = stored_metadata(&app, &job, &step).await;

    let (status, body) =
        put_step(&app, &job, &step, serde_json::json!({ "status": "active" })).await;
    assert!(status.is_success(), "status-only PUT: {status} {body}");
    assert_eq!(stored_metadata(&app, &job, &step).await, before);
}

/// `authority_role` is still not a caller's to change: a PUT that
/// sends every key but a DIFFERENT authority lands with the stored one.
/// (The omission half of the old carry is the refusal above; this is
/// the half that was never about omission — the merge door strips the
/// key for the same reason.)
#[tokio::test]
async fn a_put_cannot_change_the_required_authority() {
    let app = app();
    let (job, step) = open_job(&app).await;
    let mut md = stored_metadata(&app, &job, &step).await;
    let Some(stored_role) = md.get("authority_role").cloned() else {
        panic!("precondition: the scope step declares an authority_role: {md}");
    };
    md["authority_role"] = serde_json::json!("somebody-else");
    let (status, body) = put_step(&app, &job, &step, serde_json::json!({ "metadata": md })).await;
    assert!(status.is_success(), "{status} {body}");
    assert_eq!(
        stored_metadata(&app, &job, &step).await["authority_role"],
        stored_role
    );
}

/// The door the refusal names does what it says. A step re-claimed by a
/// different run must name the run that now holds it, and `boss
/// dispatch` writes that through this door right after the claim; an
/// explicit `null` is how a key is cleared on purpose.
#[tokio::test]
async fn the_merge_door_still_overwrites_and_clears_the_run_edge() {
    let app = app();
    let (job, step) = open_job(&app).await;
    write_edge(&app, &job, &step, RUN_A).await;

    // A re-claim: the next dispatch names its own run.
    write_edge(&app, &job, &step, RUN_B).await;
    let stored = stored_metadata(&app, &job, &step).await;
    assert_eq!(
        stored.get(boss_jobs::agent_runs::EDGE_KEY),
        Some(&serde_json::json!(RUN_B)),
        "no stale run id is pinned onto a step a new run holds: {stored}",
    );

    let (status, body) = send(
        &app,
        req(
            "PATCH",
            &format!("/api/jobs/{job}/steps/{step}/metadata"),
            serde_json::json!({ boss_jobs::agent_runs::EDGE_KEY: serde_json::Value::Null }),
        ),
    )
    .await;
    assert!(status.is_success(), "clearing patch: {status} {body}");
    let stored = stored_metadata(&app, &job, &step).await;
    assert!(
        stored.get(boss_jobs::agent_runs::EDGE_KEY).is_none(),
        "an explicit null still deletes the edge: {stored}",
    );
}

/// Complete the scope step the required way — read, merge, write — and
/// hand back what it stores.
async fn complete_scope(app: &axum::Router, job: &str, step: &str) -> serde_json::Value {
    let mut md = stored_metadata(app, job, step).await;
    md["summary"] = serde_json::json!("what the change does");
    md["excludes"] = serde_json::json!("what it leaves alone");
    let (status, body) = put_step(
        app,
        job,
        step,
        serde_json::json!({"status": "completed", "metadata": md}),
    )
    .await;
    assert!(status.is_success(), "completing scope: {status} {body}");
    stored_metadata(app, job, step).await
}

/// A TERMINAL step answers an omitting PUT with the terminal refusal,
/// not this one. The merge door this refusal names refuses a terminal
/// step too, so routing the caller there would send it from one 409 to
/// another; the terminal hint names the doors that work on a record
/// (the corrections door, the job's own metadata).
#[tokio::test]
async fn a_terminal_step_answers_an_omitting_put_with_the_terminal_refusal() {
    let app = app();
    let (job, step) = open_job(&app).await;
    let before = complete_scope(&app, &job, &step).await;

    let (status, body) = put_step(
        &app,
        &job,
        &step,
        serde_json::json!({"status": "completed", "metadata": {"summary": "rewritten"}}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["hint"],
        boss_jobs::corrections::TERMINAL_STEP_HINT,
        "the record's own refusal speaks: {body}"
    );
    assert_eq!(stored_metadata(&app, &job, &step).await, before);
}

/// THE IDEMPOTENT RE-SEND, AT THE MERGE DOOR. The writers moved onto
/// the merge door (e39a9d2a: the gate verdict, auto-park, boss park,
/// boss prove, boss design, the triage on park) re-send on a
/// redelivery or a retry. The PUT they used to send answered an
/// unchanged re-send to a terminal step with success — the freeze is
/// scoped to a real change — so the door they moved to must answer it
/// the same way, or a redelivery that changes nothing turns into a 409.
/// A re-send that DOES change a key is still refused, and writes
/// nothing.
#[tokio::test]
async fn the_merge_door_answers_an_unchanged_resend_to_a_terminal_step_with_success() {
    let app = app();
    let (job, step) = open_job(&app).await;
    let before = complete_scope(&app, &job, &step).await;
    let door = format!("/api/jobs/{job}/steps/{step}/metadata");

    let (status, body) = send(
        &app,
        req(
            "PATCH",
            &door,
            serde_json::json!({"summary": "what the change does", "never_set": null}),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "an unchanged re-send lands as the no-op it is: {body}"
    );

    let (status, body) = send(
        &app,
        req("PATCH", &door, serde_json::json!({"summary": "rewritten"})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a real change is refused: {body}"
    );
    assert_eq!(body["hint"], boss_jobs::corrections::TERMINAL_STEP_HINT);
    assert_eq!(stored_metadata(&app, &job, &step).await, before);
}
