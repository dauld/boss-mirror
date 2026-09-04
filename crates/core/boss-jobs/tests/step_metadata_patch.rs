//! `PATCH /api/jobs/{id}/steps/{step_id}/metadata` — the step-side
//! twin of the Job's atomic top-level metadata merge.
//!
//! Steps had the same defect Jobs did before the 2026-08-21 UX audit:
//! with only the overlay PUT, every "set one metadata key" caller ran
//! GET → spread → PUT client-side, and a concurrent writer's keys were
//! erased by whichever write landed second. The merge now happens
//! server-side against the row as it stands. These tests pin the merge
//! semantics (add preserves, null removes), the envelope immunity
//! (status / assignee are untouchable through this route), the
//! terminal-step refusal (a completed step's metadata is frozen and
//! the caller is TOLD, matching the PUT's posture from job 903e6b90),
//! the policy gate, the STEP_UPDATED event carrying post-merge state,
//! and the readiness wake (a step-metadata-gated `ready_when` flips
//! without waiting for an unrelated status write).

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{AccessTier, Action, Resource, Scope, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

fn tech(id: &str) -> User {
    User {
        id: id.to_string(),
        role: "service-tech".to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("service".into()),
    }
}

fn open_job(id: &str, owner: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "user-feedback".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "/ux/jobs"),
        title: "A packet whose step gets annotated".into(),
        owner_id: owner.to_string(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        simulated: false,
    }
}

fn build_app(policy: Arc<dyn PolicyClient>) -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus,
        publisher,
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: None,
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
    };
    (router(state), jobs)
}

fn allow_step_update() -> Arc<dyn PolicyClient> {
    Arc::new(
        FakePolicyClient::builder()
            .allow("service-tech", Action::Update, Resource::step(), Scope::All)
            .build(),
    )
}

async fn patch_step_metadata(
    app: &Router,
    user: &User,
    job_id: &str,
    step_id: &str,
    body: &str,
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/jobs/{job_id}/steps/{step_id}/metadata"))
                .header("content-type", "application/json")
                .header("x-boss-user", serde_json::to_string(user).unwrap())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// A job + one step seeded straight through the repo — the light
/// harness. `status` is what the test needs the step to be in.
async fn seed_step(
    jobs: &Arc<InMemoryJobs>,
    job_id: &str,
    status: StepStatus,
    metadata: serde_json::Value,
) -> (Job, Step) {
    let job = open_job(job_id, "emp-1");
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Do the work", 0).with_assignee("emp-1");
    step.status = status;
    step.metadata = metadata;
    jobs.add_step(&step).await.unwrap();
    (job, step)
}

fn step_updated_events(jobs: &Arc<InMemoryJobs>) -> Vec<boss_core::event::Event> {
    jobs.recorded_events()
        .into_iter()
        .filter(|e| e.kind == "jobs.step.updated")
        .collect()
}

#[tokio::test]
async fn merge_adds_a_key_and_preserves_the_existing_ones() {
    let (app, jobs) = build_app(allow_step_update());
    let user = tech("emp-1");
    let (job, step) = seed_step(
        &jobs,
        "00000000-0000-0000-0000-000000000001",
        StepStatus::Ready,
        serde_json::json!({ "route": "/ux/jobs", "flag": "a" }),
    )
    .await;

    let resp = patch_step_metadata(
        &app,
        &user,
        &job.id.to_string(),
        &step.id.to_string(),
        r#"{"annotation":"checked"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let after = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(after.metadata["annotation"], "checked");
    assert_eq!(
        after.metadata["route"], "/ux/jobs",
        "existing keys must survive the merge"
    );
    assert_eq!(after.metadata["flag"], "a");
    assert_eq!(after.status, StepStatus::Ready, "status untouched");
    assert_eq!(after.assignee_id.as_deref(), Some("emp-1"));
}

#[tokio::test]
async fn a_null_value_removes_the_key_and_only_that_key() {
    // The same overlay convention as the job merge: null is a removal,
    // how a caller sheds a stale key instead of carrying "" forever.
    let (app, jobs) = build_app(allow_step_update());
    let user = tech("emp-1");
    let (job, step) = seed_step(
        &jobs,
        "00000000-0000-0000-0000-000000000002",
        StepStatus::Ready,
        serde_json::json!({ "route": "/ux/jobs", "stale": "x" }),
    )
    .await;

    let resp = patch_step_metadata(
        &app,
        &user,
        &job.id.to_string(),
        &step.id.to_string(),
        r#"{"stale":null}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let after = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert!(
        after.metadata.get("stale").is_none(),
        "null must remove the key: {}",
        after.metadata
    );
    assert_eq!(after.metadata["route"], "/ux/jobs");
}

#[tokio::test]
async fn a_step_whose_metadata_was_never_an_object_still_merges() {
    let (app, jobs) = build_app(allow_step_update());
    let user = tech("emp-1");
    let (job, step) = seed_step(
        &jobs,
        "00000000-0000-0000-0000-000000000003",
        StepStatus::Ready,
        serde_json::Value::Null,
    )
    .await;

    let resp = patch_step_metadata(
        &app,
        &user,
        &job.id.to_string(),
        &step.id.to_string(),
        r#"{"annotation":"checked"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let after = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(after.metadata["annotation"], "checked");
}

#[tokio::test]
async fn a_status_key_lands_in_metadata_and_transitions_nothing() {
    // The envelope is untouchable through this route. A `status` key in
    // the body is just a metadata key named "status" — same contract as
    // the job merge — and in particular it must NOT complete the step
    // or fire completion side-effects.
    let (app, jobs) = build_app(allow_step_update());
    let user = tech("emp-1");
    let (job, step) = seed_step(
        &jobs,
        "00000000-0000-0000-0000-000000000004",
        StepStatus::Ready,
        serde_json::json!({}),
    )
    .await;

    let resp = patch_step_metadata(
        &app,
        &user,
        &job.id.to_string(),
        &step.id.to_string(),
        r#"{"status":"completed"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let after = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(
        after.status,
        StepStatus::Ready,
        "a metadata patch must not transition status"
    );
    assert_eq!(after.completed_on, None);
    assert!(
        !jobs
            .recorded_events()
            .iter()
            .any(|e| e.kind == "jobs.step.completed" || e.kind.starts_with("step.done.")),
        "no completion markers may fire off a metadata patch"
    );
}

#[tokio::test]
async fn the_merge_records_step_updated_with_the_post_merge_state() {
    let (app, jobs) = build_app(allow_step_update());
    let user = tech("emp-1");
    let (job, step) = seed_step(
        &jobs,
        "00000000-0000-0000-0000-000000000005",
        StepStatus::Ready,
        serde_json::json!({ "route": "/ux/jobs" }),
    )
    .await;

    let resp = patch_step_metadata(
        &app,
        &user,
        &job.id.to_string(),
        &step.id.to_string(),
        r#"{"annotation":"checked"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let updated = step_updated_events(&jobs);
    assert_eq!(updated.len(), 1, "exactly one state event per merge");
    let payload = &updated[0].payload;
    assert_eq!(
        payload["step_id"],
        step.id.to_string(),
        "state events carry the marker identity key"
    );
    assert_eq!(payload["metadata"]["annotation"], "checked");
    assert_eq!(
        payload["metadata"]["route"], "/ux/jobs",
        "the event carries the MERGED metadata, not the patch"
    );
    assert_eq!(payload["status"], "ready");
    assert!(
        payload.get("_actor").is_some(),
        "outbox events carry the actor stamp"
    );
}

#[tokio::test]
async fn a_completed_step_is_refused_loudly_and_stays_untouched() {
    let (app, jobs) = build_app(allow_step_update());
    let user = tech("emp-1");
    let (job, step) = seed_step(
        &jobs,
        "00000000-0000-0000-0000-000000000006",
        StepStatus::Completed,
        serde_json::json!({ "evidence": "the record" }),
    )
    .await;

    let resp = patch_step_metadata(
        &app,
        &user,
        &job.id.to_string(),
        &step.id.to_string(),
        r#"{"annotation":"late edit"}"#,
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "a terminal step's metadata is frozen and the caller is TOLD"
    );
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["step_status"], "completed");
    assert!(
        body["refused_fields"]
            .as_array()
            .is_some_and(|f| f.iter().any(|v| v == "metadata")),
        "the refusal names the frozen field: {body}"
    );
    assert!(
        body["hint"]
            .as_str()
            .is_some_and(|h| h.contains("PATCH /api/jobs/{id}/metadata")),
        "the refusal names the door that works — the JOB metadata merge: {body}"
    );

    let after = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(
        after.metadata,
        serde_json::json!({ "evidence": "the record" })
    );
    assert!(
        step_updated_events(&jobs).is_empty(),
        "a refused patch must not record an event"
    );
}

#[tokio::test]
async fn a_skipped_step_is_refused_the_same_way() {
    let (app, jobs) = build_app(allow_step_update());
    let user = tech("emp-1");
    let (job, step) = seed_step(
        &jobs,
        "00000000-0000-0000-0000-000000000007",
        StepStatus::Skipped,
        serde_json::json!({}),
    )
    .await;

    let resp = patch_step_metadata(
        &app,
        &user,
        &job.id.to_string(),
        &step.id.to_string(),
        r#"{"annotation":"late edit"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn authority_role_is_immutable_through_the_patch() {
    // Same rule as the step PUT: a body can neither raise nor lower the
    // required sign-off authority — and null cannot shed it either.
    let (app, jobs) = build_app(allow_step_update());
    let user = tech("emp-1");
    let (job, step) = seed_step(
        &jobs,
        "00000000-0000-0000-0000-000000000008",
        StepStatus::Ready,
        serde_json::json!({ "authority_role": "qa-lead" }),
    )
    .await;

    let resp = patch_step_metadata(
        &app,
        &user,
        &job.id.to_string(),
        &step.id.to_string(),
        r#"{"authority_role":"ceo","annotation":"checked"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let after = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(
        after.metadata["authority_role"], "qa-lead",
        "the persisted authority wins over the body"
    );
    assert_eq!(after.metadata["annotation"], "checked", "other keys land");

    let resp = patch_step_metadata(
        &app,
        &user,
        &job.id.to_string(),
        &step.id.to_string(),
        r#"{"authority_role":null}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let after = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(
        after.metadata["authority_role"], "qa-lead",
        "null cannot shed the required authority"
    );
}

#[tokio::test]
async fn changing_the_steps_shape_invalidates_stamps_loudly() {
    // Same loud-invalidation contract as the step PUT: an edit that
    // changes the completion-relevant shape makes existing stamps stale
    // — they attested different content — and the surface is told who
    // must re-sign rather than left rendering stamps that won't count.
    let (app, jobs) = build_app(allow_step_update());
    let user = tech("emp-1");
    let job = open_job("00000000-0000-0000-0000-000000000009", "emp-1");
    jobs.create_job(&job).await.unwrap();
    let mut step =
        Step::new(job.id, "task", "Signed work", 0).with_sign_offs_required(vec!["qa-lead".into()]);
    step.status = StepStatus::Ready;
    step.metadata = serde_json::json!({ "spec": "v1" });
    step.sign_offs.push(boss_core::job::SignOffStamp {
        authority_id: "emp-qa".into(),
        role: "qa-lead".into(),
        stamped_at: chrono::Utc::now(),
        shape_hash: boss_core::job::step_shape_hash(&step.title, &step.metadata),
        assurance: boss_core::job::Assurance::Session,
        presence_nonce: None,
    });
    jobs.add_step(&step).await.unwrap();

    let resp = patch_step_metadata(
        &app,
        &user,
        &job.id.to_string(),
        &step.id.to_string(),
        r#"{"spec":"v2"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let invalidated: Vec<_> = jobs
        .recorded_events()
        .into_iter()
        .filter(|e| e.kind == "jobs.step.stamps_invalidated")
        .collect();
    assert_eq!(invalidated.len(), 1, "the stale stamps are announced");
    assert_eq!(
        invalidated[0].payload["stale_roles"],
        serde_json::json!(["qa-lead"])
    );

    // Re-sending the SAME content changes no shape and announces
    // nothing — the marker means "re-sign", not "somebody wrote".
    let resp = patch_step_metadata(
        &app,
        &user,
        &job.id.to_string(),
        &step.id.to_string(),
        r#"{"spec":"v2"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    let invalidated_count = jobs
        .recorded_events()
        .iter()
        .filter(|e| e.kind == "jobs.step.stamps_invalidated")
        .count();
    assert_eq!(invalidated_count, 1, "a shape-preserving patch is quiet");
}

#[tokio::test]
async fn policy_denial_is_403_and_writes_nothing() {
    let (app, jobs) = build_app(Arc::new(FakePolicyClient::deny_all()));
    let user = tech("emp-1");
    let (job, step) = seed_step(
        &jobs,
        "00000000-0000-0000-0000-00000000000a",
        StepStatus::Ready,
        serde_json::json!({}),
    )
    .await;

    let resp = patch_step_metadata(
        &app,
        &user,
        &job.id.to_string(),
        &step.id.to_string(),
        r#"{"annotation":"checked"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let after = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert!(after.metadata.get("annotation").is_none());
    assert!(step_updated_events(&jobs).is_empty());
}

#[tokio::test]
async fn a_non_object_body_is_refused() {
    let (app, jobs) = build_app(allow_step_update());
    let user = tech("emp-1");
    let (job, step) = seed_step(
        &jobs,
        "00000000-0000-0000-0000-00000000000b",
        StepStatus::Ready,
        serde_json::json!({}),
    )
    .await;

    let resp = patch_step_metadata(
        &app,
        &user,
        &job.id.to_string(),
        &step.id.to_string(),
        r#"["not","an","object"]"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn an_unknown_step_is_404_and_a_foreign_step_is_404() {
    let (app, jobs) = build_app(allow_step_update());
    let user = tech("emp-1");
    let (job, step) = seed_step(
        &jobs,
        "00000000-0000-0000-0000-00000000000c",
        StepStatus::Ready,
        serde_json::json!({}),
    )
    .await;

    let resp = patch_step_metadata(
        &app,
        &user,
        &job.id.to_string(),
        "00000000-0000-0000-0000-0000000000ff",
        r#"{"a":"1"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    // A real step addressed through a job it does not belong to is
    // not found THERE — same containment rule as the claim route.
    let other = open_job("00000000-0000-0000-0000-00000000000d", "emp-1");
    jobs.create_job(&other).await.unwrap();
    let resp = patch_step_metadata(
        &app,
        &user,
        &other.id.to_string(),
        &step.id.to_string(),
        r#"{"a":"1"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// The readiness wake — a step-metadata-gated predicate flips off this
// route. Needs the full harness: a seeded Workflow, HTTP admission (so
// steps materialize with spec slugs), and the re-evaluator.
// ---------------------------------------------------------------------------

/// Two steps: `work` is born ready; `gated` waits on a METADATA fact
/// of the live `work` step, not on its completion — the shape the
/// predicate language supports (`steps.<slug>.metadata.<field>`) and
/// the reason this endpoint must wake the re-evaluator.
fn metadata_gated_spec() -> boss_jobs::registry::WorkflowSpec {
    let mut spec = boss_jobs::registry::WorkflowSpec::platform_seed(
        "metadata-gated",
        "Metadata gated",
        "platform",
        vec!["custom".into()],
        vec![
            boss_jobs::registry::StepSpec {
                title: "work".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                title_template: "The work".into(),
                authority_role: Some("platform-admin".into()),
                ..Default::default()
            },
            boss_jobs::registry::StepSpec {
                title: "gated".into(),
                kind: "task".into(),
                ready_when: "steps.work.metadata.flag = \"on\"".into(),
                title_template: "The gated follow-up".into(),
                authority_role: Some("platform-admin".into()),
                ..Default::default()
            },
        ],
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

fn admin() -> User {
    User {
        id: "emp-bootstrap-admin".into(),
        role: "platform-admin".into(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("platform".into()),
    }
}

fn registry_app() -> Router {
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds
        .seed(metadata_gated_spec())
        .expect("seed the metadata-gated kind");
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

async fn send(app: &Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

#[tokio::test]
async fn a_metadata_patch_wakes_a_metadata_gated_ready_when() {
    let app = registry_app();
    let user = admin();
    let header = serde_json::to_string(&user).unwrap();

    let (status, job) = send(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", header.clone())
            .body(Body::from(
                serde_json::json!({
                    "kind": "metadata-gated",
                    "subject": { "subject_kind": "custom", "id": "wake-test" },
                    "title": "a packet whose gate is a metadata fact",
                    "owner_id": "emp-bootstrap-admin",
                    "priority": "standard",
                    "status": "open",
                    "metadata": {},
                    "tags": [],
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "open failed: {job}");
    let job_id = job["id"].as_str().expect("job id").to_string();

    let (status, full) = send(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/jobs/{job_id}"))
            .header("x-boss-user", header.clone())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let steps = full["steps"].as_array().expect("steps");
    let work_id = steps
        .iter()
        .find(|s| s["spec_slug"] == "work")
        .and_then(|s| s["id"].as_str())
        .expect("the work step")
        .to_string();
    assert_eq!(
        steps.iter().find(|s| s["spec_slug"] == "gated").unwrap()["status"],
        "pending",
        "the gate starts unsatisfied"
    );

    let (status, body) = send(
        &app,
        Request::builder()
            .method("PATCH")
            .uri(format!("/api/jobs/{job_id}/steps/{work_id}/metadata"))
            .header("content-type", "application/json")
            .header("x-boss-user", header.clone())
            .body(Body::from(r#"{"flag":"on"}"#.to_string()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "patch failed: {body}");

    let (status, full) = send(
        &app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/jobs/{job_id}"))
            .header("x-boss-user", header)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let gated = full["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .find(|s| s["spec_slug"] == "gated")
        .expect("the gated step")
        .clone();
    assert_eq!(
        gated["status"], "ready",
        "the metadata write must wake the re-evaluator, same as the job \
         metadata patch does: {gated}"
    );
}
