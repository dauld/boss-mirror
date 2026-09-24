//! `POST /api/jobs/{id}/convert` moves a packet to another version of
//! its protocol, and the move is true of the packet and on the record.
//!
//! MEASURED (backlog 1e973965, the draft-design analyst on 4347a1af,
//! 2026-09-23). The door answered `converted: true` after changing only
//! `jobs.workflow_version`. Each step row keeps what materialisation
//! copied onto it from the admission version — on page-audit c0d2caf0
//! (pinned v1, active v3) `measure` and `file` still held v1 procedure
//! text after v3 changed them — and a step the target inserts was never
//! created. The interim car refused both moves.
//!
//! Design 7cf202a9 decided the door (all five questions accepted as
//! proposed, 2026-09-23), and these pin it: a re-pin re-projects every
//! step not yet completed and materialises every inserted step (Q2);
//! it records a `jobs.job.repinned` event and appends to the packet's
//! reserved `repins` list (Q3); only the authority that publishes a
//! protocol version may move a packet to one (Q4); and a dry run
//! answers the same verdict and plan without writing (Q1).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{WorkflowSpec, WorkflowStatus, seedable_platform_workflows};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

const KIND: &str = "ship-a-change";

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

/// The platform's active ship-a-change, as seeded.
fn active_spec() -> WorkflowSpec {
    seedable_platform_workflows()
        .into_iter()
        .find(|s| s.kind == KIND)
        .expect("ship-a-change is a platform workflow")
}

/// A later version of it, edited by `edit` — seeded as a draft so the
/// active row the packet opens under stays the one `get_active` finds,
/// and the conversion names its target with `to_version`.
fn next_version(kinds: &InMemoryWorkflows, bump: i32, edit: impl FnOnce(&mut WorkflowSpec)) -> i32 {
    let mut spec = active_spec();
    spec.version += bump;
    spec.status = WorkflowStatus::Draft;
    edit(&mut spec);
    let version = spec.version;
    kinds.seed(spec).expect("seed target version");
    version
}

fn set_procedure(spec: &mut WorkflowSpec, slug: &str, text: &str) {
    let step = spec
        .steps
        .iter_mut()
        .find(|s| s.title == slug)
        .unwrap_or_else(|| panic!("step {slug} in {KIND}"));
    let mut defaults = match &step.metadata_defaults {
        serde_json::Value::Object(m) => m.clone(),
        _ => serde_json::Map::new(),
    };
    defaults.insert("procedure".into(), serde_json::Value::String(text.into()));
    step.metadata_defaults = serde_json::Value::Object(defaults);
}

struct App {
    router: axum::Router,
    kinds: Arc<InMemoryWorkflows>,
    jobs: Arc<InMemoryJobs>,
}

fn app() -> App {
    let kinds = Arc::new(InMemoryWorkflows::new());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let jobs = Arc::new(InMemoryJobs::new());
    let mut policy = FakePolicyClient::builder();
    // The engineer writes jobs exactly as the admin does — every
    // permission the door asked for before Q4 — and does not publish
    // protocols.
    for role in ["platform-admin", "engineer"] {
        for (action, resource) in [
            (Action::Create, Resource::job()),
            (Action::Read, Resource::job()),
            (Action::Update, Resource::job()),
            (Action::Update, Resource::step()),
        ] {
            policy = policy.allow(role, action, resource, Scope::All);
        }
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(
        policy
            .allow(
                "platform-admin",
                Action::Publish,
                Resource::workflow(),
                Scope::All,
            )
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(kinds.clone() as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus.clone(),
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    App {
        router: router(state),
        kinds,
        jobs,
    }
}

const ADMIN: &str = r#"{"id":"emp-bootstrap-admin","role":"platform-admin","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;
const ENGINEER: &str = r#"{"id":"emp-engineer","role":"engineer","access_tier":"operator","territory_account_ids":[],"direct_report_ids":[],"department":"platform"}"#;

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

fn req_as(user: &str, method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", user)
        .body(Body::from(body.to_string()))
        .expect("request")
}

fn req(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
    req_as(ADMIN, method, uri, body)
}

async fn get_job(app: &axum::Router, id: &str) -> serde_json::Value {
    let (status, full) = send(
        app,
        req("GET", &format!("/api/jobs/{id}"), serde_json::json!({})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "job read: {full}");
    full
}

fn step_named<'a>(job: &'a serde_json::Value, slug: &str) -> &'a serde_json::Value {
    job["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["spec_slug"] == slug)
        .unwrap_or_else(|| panic!("a {slug} step on the packet: {job}"))
}

/// A packet opened under the active version, standing at `build`.
async fn open_at_build(app: &axum::Router) -> String {
    let (status, job) = send(
        app,
        req(
            "POST",
            "/api/jobs",
            serde_json::json!({
                "kind": KIND,
                "subject": {"subject_kind": "custom", "id": "feat/x"},
                "title": "t", "owner_id": "emp-bootstrap-admin",
                "status": "open", "priority": "standard",
                "metadata": {}, "tags": [],
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "job create: {job}");
    let id = job["id"].as_str().expect("job id").to_string();
    let full = get_job(app, &id).await;
    let scope = step_named(&full, "scope").clone();
    let scope_id = scope["id"].as_str().expect("step id");
    let (status, body) = send(
        app,
        req(
            "PUT",
            &format!("/api/jobs/{id}/steps/{scope_id}"),
            scope_completion(&scope["metadata"]),
        ),
    )
    .await;
    assert!(status.is_success(), "complete scope: {status}: {body}");
    id
}

async fn convert_as(
    app: &axum::Router,
    user: &str,
    id: &str,
    to: i32,
) -> (StatusCode, serde_json::Value) {
    send(
        app,
        req_as(
            user,
            "POST",
            &format!("/api/jobs/{id}/convert"),
            serde_json::json!({ "to_version": to }),
        ),
    )
    .await
}

async fn convert(app: &axum::Router, id: &str, to: i32) -> (StatusCode, serde_json::Value) {
    convert_as(app, ADMIN, id, to).await
}

fn repinned_events(jobs: &InMemoryJobs) -> Vec<serde_json::Value> {
    jobs.recorded_events()
        .into_iter()
        .filter(|e| e.kind == boss_jobs::events::JOB_REPINNED)
        .map(|e| e.payload)
        .collect()
}

/// THE MEASURED CASE: the target changes the procedure of a step the
/// packet has not reached. Before the interim car: 200, `converted:
/// true`, and the `build` row still carrying the admission text. Now
/// the row reads the target's text, and the step the packet already
/// completed keeps the text it ran under.
#[tokio::test]
async fn a_pending_procedure_is_reprojected_and_a_completed_one_is_kept() {
    let App {
        router: app,
        kinds,
        jobs,
    } = app();
    let id = open_at_build(&app).await;
    let before = get_job(&app, &id).await;
    let from = before["workflow_version"].as_i64().expect("pinned");
    let to = next_version(&kinds, 1, |s| {
        set_procedure(s, "build", "Build it, and say what you measured.");
        set_procedure(s, "scope", "Scope it, in writing.");
    });

    let (status, body) = convert(&app, &id, to).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["converted"], true, "{body}");

    let after = get_job(&app, &id).await;
    assert_eq!(after["workflow_version"], to);
    assert_eq!(
        step_named(&after, "build")["metadata"]["procedure"],
        "Build it, and say what you measured.",
        "the pending step reads the version the packet was moved to"
    );
    assert_eq!(
        step_named(&after, "scope")["metadata"]["procedure"],
        step_named(&before, "scope")["metadata"]["procedure"],
        "the completed step keeps the text it ran under"
    );

    // On the record, twice: the packet's own list, and the event.
    let repins = after["metadata"]["repins"].as_array().expect("repins list");
    assert_eq!(repins.len(), 1, "{repins:?}");
    assert_eq!(repins[0]["from"], from);
    assert_eq!(repins[0]["to"], to);
    assert_eq!(repins[0]["by"], "emp-bootstrap-admin");
    let build = repins[0]["reprojected"]
        .as_array()
        .expect("reprojected")
        .iter()
        .find(|r| r["step"] == "build")
        .expect("build named as re-projected");
    assert!(
        build["changed"]
            .as_array()
            .expect("changed")
            .iter()
            .any(|c| c == "`procedure`"),
        "{build}"
    );
    let events = repinned_events(&jobs);
    assert_eq!(events.len(), 1, "one move, one event: {events:?}");
    assert_eq!(events[0]["job_id"], id.as_str());
    assert_eq!(events[0]["to"], to);
}

/// A step the target inserts ahead of the packet gets a row, pending,
/// and the record names it.
#[tokio::test]
async fn an_inserted_step_is_materialised_and_named() {
    let App {
        router: app,
        kinds,
        jobs,
    } = app();
    let id = open_at_build(&app).await;
    let to = next_version(&kinds, 2, |s| {
        let mut extra = s
            .steps
            .iter()
            .find(|st| st.title == "settled")
            .expect("settled step")
            .clone();
        extra.title = "archived".to_string();
        s.steps.push(extra);
    });

    let (status, body) = convert(&app, &id, to).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let after = get_job(&app, &id).await;
    let archived = step_named(&after, "archived");
    assert_ne!(archived["status"], "completed", "{archived}");
    assert_eq!(
        after["steps"].as_array().map(Vec::len),
        Some(active_spec().steps.len() + 1)
    );
    assert_eq!(repinned_events(&jobs)[0]["inserted"][0]["step"], "archived");
}

/// THE SAFETY VERDICT STILL REFUSES. A required field added to a step
/// the packet has already completed would claim evidence it never
/// collected; re-projection cannot make that true, so the move is
/// refused, the obstacle names the step, and nothing is written.
#[tokio::test]
async fn a_move_that_demands_evidence_retroactively_is_refused_and_writes_nothing() {
    let App {
        router: app,
        kinds,
        jobs,
    } = app();
    let id = open_at_build(&app).await;
    let pinned = get_job(&app, &id).await["workflow_version"].clone();
    let to = next_version(&kinds, 3, |s| {
        let scope = s
            .steps
            .iter_mut()
            .find(|st| st.title == "scope")
            .expect("scope");
        scope.fields.push(boss_core::job::StepField {
            name: "blast_radius".into(),
            field_type: "string".into(),
            required: true,
            filled_by: boss_core::job::FilledBy::Executor,
            item_keys: Vec::new(),
            covers: None,
            binds: None,
            item_value_max_bytes: None,
            item_one_of: Vec::new(),
        });
    });

    let (status, body) = convert(&app, &id, to).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["converted"], false);
    assert!(
        body["obstacles"]
            .as_array()
            .expect("obstacles")
            .iter()
            .any(|o| o["step"] == "scope"),
        "{body}"
    );
    let after = get_job(&app, &id).await;
    assert_eq!(after["workflow_version"], pinned, "the pin stays");
    assert!(after["metadata"].get("repins").is_none(), "{after}");
    assert!(repinned_events(&jobs).is_empty());
}

/// Q4: moving a packet is the registry owner's act. A caller who may
/// write the job but may not publish a protocol version is refused,
/// and the pin stays where it was.
#[tokio::test]
async fn a_job_writer_who_cannot_publish_a_protocol_may_not_move_a_packet() {
    let App {
        router: app, kinds, ..
    } = app();
    let id = open_at_build(&app).await;
    let pinned = get_job(&app, &id).await["workflow_version"].clone();
    let to = next_version(&kinds, 1, |s| set_procedure(s, "build", "New text."));

    let (status, body) = convert_as(&app, ENGINEER, &id, to).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(get_job(&app, &id).await["workflow_version"], pinned);
}

/// Q1: the dry run is a READ — the verdict and the plan the write would
/// carry, with nothing written. A cohort is previewed by running it in
/// a loop, which is why a job reader may ask it.
#[tokio::test]
async fn a_dry_run_answers_the_plan_and_writes_nothing() {
    let App {
        router: app,
        kinds,
        jobs,
    } = app();
    let id = open_at_build(&app).await;
    let pinned = get_job(&app, &id).await["workflow_version"].clone();
    let to = next_version(&kinds, 1, |s| set_procedure(s, "build", "New text."));
    let events_before = jobs.recorded_events().len();

    let (status, body) = send(
        &app,
        req_as(
            ENGINEER,
            "GET",
            &format!("/api/jobs/{id}/convert?to_version={to}"),
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["convertible"], true, "{body}");
    assert_eq!(body["from"], pinned);
    assert_eq!(body["to"], to);
    assert!(
        body["reprojected"]
            .as_array()
            .expect("reprojected")
            .iter()
            .any(|r| r["step"] == "build"),
        "{body}"
    );
    assert_eq!(get_job(&app, &id).await["workflow_version"], pinned);
    assert_eq!(
        jobs.recorded_events().len(),
        events_before,
        "a dry run records nothing"
    );
}

/// Q3: the record is append-only. The generic metadata PATCH could
/// rewrite or erase it, so it refuses the key and names the door.
#[tokio::test]
async fn the_metadata_patch_refuses_the_repins_list() {
    let App { router: app, .. } = app();
    let id = open_at_build(&app).await;
    let (status, body) = send(
        &app,
        req(
            "PATCH",
            &format!("/api/jobs/{id}/metadata"),
            serde_json::json!({ "repins": [] }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.to_string().contains("/convert"), "{body}");
}

/// The scope completion as a read-merge-write: the step's stored
/// metadata with the evidence laid over it. The step PUT refuses a
/// metadata body that omits a stored key (e39a9d2a), so a completer
/// sends back everything it read.
fn scope_completion(stored: &serde_json::Value) -> serde_json::Value {
    let mut metadata = stored.clone();
    metadata["summary"] = serde_json::json!("s");
    metadata["excludes"] = serde_json::json!("e");
    serde_json::json!({"status": "completed", "metadata": metadata})
}
