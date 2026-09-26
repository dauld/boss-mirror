//! A key its protocol reserves to one writer refuses every other
//! writer, at every step door that can change it (design f623e425,
//! David 2026-09-25; backlog 6c9183de).
//!
//! THE CLAIM, measured on origin/main 21561380 (2026-09-25) and again at
//! 604ed86f (2026-09-26): an ops-request's `approve` step carries the
//! keys the runner renders on the host — `plan`, `verb`, `host`, `args`,
//! `rendered_plan_sha256` — and any caller with Update on the step could
//! merge them, so the passkey could be asked to sign a plan the runner
//! never rendered (extend c reproduced exactly that: `PLAN wipe`).
//!
//! THE SHAPE TESTED is that step reduced to its keys: every runner key
//! declares `writer = "runner:ops"` on its field, one key (`comment`)
//! declares none, and the packet's `host` is `forge`. The forged caller
//! is the one the review named: the machine door's self-asserted
//! `x-boss-user` spelling the runner's own id with platform-admin, which
//! the policy admits — so a refusal here is the writer rule's, not the
//! policy's. The credentialed caller is a `CredentialedCaller` request
//! extension, set by a layer standing in for the credential door (the
//! next car); a client cannot set an extension, so no header in these
//! tests can reach it.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, Priority, Step, StepField, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::field_writer::CredentialedCaller;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const KIND: &str = "approval-carrier";
const RUNNER_KEYS: [&str; 5] = ["plan", "verb", "host", "args", "rendered_plan_sha256"];

fn field(name: &str, writer: Option<&str>) -> StepField {
    StepField {
        name: name.into(),
        field_type: "string".into(),
        required: false,
        filled_by: Default::default(),
        item_keys: Vec::new(),
        covers: None,
        binds: None,
        item_value_max_bytes: None,
        item_one_of: Vec::new(),
        writer: writer.map(str::to_string),
    }
}

fn spec() -> WorkflowSpec {
    let mut fields: Vec<StepField> = RUNNER_KEYS
        .iter()
        .map(|k| field(k, Some("runner:ops")))
        .collect();
    fields.push(field("comment", None));
    WorkflowSpec::platform_seed(
        KIND,
        "Carry an approval",
        "test",
        vec!["custom".into()],
        vec![StepSpec {
            title: "approve".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: "Approve the plan".into(),
            authority_role: Some("platform-admin".into()),
            fields,
            ..Default::default()
        }],
    )
}

fn user(id: &str, role: &str) -> String {
    json!({
        "id": id,
        "role": role,
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

/// The forged runner: the machine door's header, spelling the runner's
/// own id, exactly as infra/ops/ops-runner.sh builds it.
fn forged_runner() -> String {
    user("automation:ops-runner", "platform-admin")
}

fn runner_credential(host: &str) -> CredentialedCaller {
    CredentialedCaller {
        principal: "runner:ops".into(),
        actor_id: "automation:ops-runner".into(),
        host: Some(host.into()),
    }
}

/// A fresh store and the router over it.
fn app(caller: Option<CredentialedCaller>) -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    (app_with_jobs(jobs.clone(), caller), jobs)
}

async fn read(resp: axum::http::Response<Body>) -> (StatusCode, Value) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes).into_owned()));
    (status, json)
}

/// File the packet for `host`, and return its approve step.
async fn file(app: &Router, jobs: &InMemoryJobs, host: &str) -> Step {
    let mut job = Job::new(
        KIND,
        Subject::new("custom", "disk-1"),
        "Commission a disk",
        "automation:filer",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 9, 26).unwrap(),
    );
    job.metadata = json!({ "host": host, "verb": "commission-a-disk" });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", user("automation:filer", "system"))
                .body(Body::from(serde_json::to_vec(&job).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = read(resp).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap();
    let job_id = boss_core::job::JobId::from_uuid(uuid::Uuid::parse_str(id).unwrap());
    jobs.list_steps(&job_id)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.spec_slug.as_deref() == Some("approve"))
        .expect("the approve step")
}

async fn merge(app: &Router, step: &Step, patch: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!(
                    "/api/jobs/{}/steps/{}/metadata",
                    step.job_id, step.id
                ))
                .header("content-type", "application/json")
                .header("x-boss-user", forged_runner())
                .body(Body::from(patch.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    read(resp).await
}

async fn put(app: &Router, step: &Step, body: Value) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}/steps/{}", step.job_id, step.id))
                .header("content-type", "application/json")
                .header("x-boss-user", forged_runner())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    read(resp).await
}

async fn stored(jobs: &InMemoryJobs, step: &Step) -> Value {
    jobs.get_step(&step.id).await.unwrap().unwrap().metadata
}

fn assert_refused(status: StatusCode, body: &Value, key: &str) {
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a write to `{key}` by anyone but its declared writer must be refused: {body}"
    );
    let refused = body["refused_keys"].as_array().expect("refused_keys");
    assert!(
        refused
            .iter()
            .any(|r| r["key"] == key && r["writer"] == "runner:ops"),
        "the refusal names the key and its declared writer: {body}"
    );
    assert_eq!(
        body["asked_by"], "automation:ops-runner",
        "the refusal names who asked: {body}"
    );
}

/// THE CLAIM, at the merge door: the forged runner cannot plant any of
/// the five runner keys, and the stored step does not move.
#[tokio::test]
async fn the_merge_door_refuses_a_forged_runner_every_reserved_key() {
    let (app, jobs) = app(None);
    let step = file(&app, &jobs, "forge").await;
    let before = stored(&jobs, &step).await;
    for key in RUNNER_KEYS {
        let (status, body) = merge(&app, &step, json!({ key: "PLAN wipe" })).await;
        assert_refused(status, &body, key);
    }
    assert_eq!(stored(&jobs, &step).await, before, "nothing was written");
}

/// The same claim at the step PUT: its metadata body is the other door
/// that overlays step keys.
#[tokio::test]
async fn the_step_put_refuses_a_forged_runner_a_reserved_key() {
    let (app, jobs) = app(None);
    let step = file(&app, &jobs, "forge").await;
    let mut md = stored(&jobs, &step).await;
    md["plan"] = json!("PLAN wipe");
    let (status, body) = put(&app, &step, json!({ "metadata": md })).await;
    assert_refused(status, &body, "plan");
    assert!(stored(&jobs, &step).await.get("plan").is_none());
}

/// Deleting a reserved key is a change too: a planted plan must not be
/// replaceable by a removal the runner then has to notice.
#[tokio::test]
async fn deleting_a_reserved_key_is_refused_like_writing_it() {
    let (app, jobs) = app(Some(runner_credential("forge")));
    let step = file(&app, &jobs, "forge").await;
    let (status, body) = merge(&app, &step, json!({ "plan": "PLAN a" })).await;
    assert!(status.is_success(), "the runner writes its plan: {body}");

    let app_forged = app_with_jobs(jobs.clone(), None);
    let (status, body) = merge(&app_forged, &step, json!({ "plan": null })).await;
    assert_refused(status, &body, "plan");
    assert_eq!(stored(&jobs, &step).await["plan"], "PLAN a");
}

/// CONTROLS. A key with no declared writer is untouched by the rule,
/// and so is an unchanged re-send of a reserved one — a
/// read-modify-write caller sending the stored value back is not
/// writing it.
#[tokio::test]
async fn undeclared_keys_and_unchanged_re_sends_are_admitted() {
    let (app, jobs) = app(None);
    let step = file(&app, &jobs, "forge").await;
    let (status, body) = merge(&app, &step, json!({ "comment": "looks right" })).await;
    assert!(
        status.is_success(),
        "an undeclared key is not reserved: {body}"
    );

    let md = stored(&jobs, &step).await;
    let (status, body) = put(&app, &step, json!({ "metadata": md })).await;
    assert!(
        status.is_success(),
        "sending the stored metadata back changes no reserved key: {body}"
    );
}

/// THE WRITER ITSELF: a caller the server resolved from a credential for
/// `runner:ops`, bound to this packet's host, writes every reserved key.
#[tokio::test]
async fn the_declared_writer_writes_its_keys() {
    let (app, jobs) = app(Some(runner_credential("forge")));
    let step = file(&app, &jobs, "forge").await;
    let patch: serde_json::Map<String, Value> = RUNNER_KEYS
        .iter()
        .map(|k| (k.to_string(), json!(format!("{k} as rendered"))))
        .collect();
    let (status, body) = merge(&app, &step, Value::Object(patch)).await;
    assert!(status.is_success(), "the runner writes its keys: {body}");
    assert_eq!(stored(&jobs, &step).await["plan"], "plan as rendered");
}

/// A runner for host h writes only requests whose host is h: the
/// credential of another host's runner is refused, naming both hosts.
#[tokio::test]
async fn another_hosts_runner_is_refused() {
    let (app, jobs) = app(Some(runner_credential("boss-gcp")));
    let step = file(&app, &jobs, "forge").await;
    let (status, body) = merge(&app, &step, json!({ "plan": "PLAN wipe" })).await;
    assert_refused(status, &body, "plan");
    let why = body["refused_keys"][0]["why"].as_str().unwrap_or_default();
    assert!(
        why.contains("boss-gcp") && why.contains("forge"),
        "the refusal names both hosts: {body}"
    );
}

/// A credential for a different principal is not the declared writer.
#[tokio::test]
async fn a_credential_for_another_principal_is_refused() {
    let (app, jobs) = app(Some(CredentialedCaller {
        principal: "runner:other".into(),
        ..runner_credential("forge")
    }));
    let step = file(&app, &jobs, "forge").await;
    let (status, body) = merge(&app, &step, json!({ "plan": "PLAN wipe" })).await;
    assert_refused(status, &body, "plan");
}

/// The router over `jobs`, and — when `caller` is given — a layer that
/// inserts it as the server-resolved credential, standing in for the
/// credential door design f623e425 mounts next. Over an existing store,
/// so one test can write as the credentialed runner and then try the
/// forged caller on the same row.
fn app_with_jobs(jobs: Arc<InMemoryJobs>, caller: Option<CredentialedCaller>) -> Router {
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds.seed(spec()).unwrap();
    let mut policy = FakePolicyClient::builder();
    for role in ["system", "platform-admin"] {
        policy = policy
            .allow(role, Action::Create, Resource::job(), Scope::All)
            .allow(role, Action::Update, Resource::step(), Scope::All)
            .allow(role, Action::Update, Resource::job(), Scope::All);
    }
    let policy: Arc<dyn PolicyClient> = Arc::new(policy.build());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        ..JobsApiState::minimal(
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    let router = router(state);
    match caller {
        Some(c) => router.layer(axum::Extension(c)),
        None => router,
    }
}
