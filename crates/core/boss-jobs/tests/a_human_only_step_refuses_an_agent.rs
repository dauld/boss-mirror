//! A human-only step refuses an agent (c17871fe, car 2).
//!
//! David, 2026-09-08: "A human decision must be unambiguous on the
//! step: who, when, how." Measured that day: all 48 workable assigned
//! steps were on the agent actor and 0 on emp-david — including a
//! credential-rotation step its protocol declares `human_only` +
//! `destructive` ("Kill the old one", job ac356440), which automation
//! had assigned to the filer.
//!
//! WHO: a step whose metadata declares `human_only` refuses any
//! assignee or claimant the employee registry does not list as an
//! active person, with a 4xx naming the step, the assignee and the
//! rule; a step without the declaration is untouched; a packet filed by
//! automation leaves such a step unassigned, carrying its
//! `authority_role`, so it is claimable by role in My Day. And since
//! backlog adac8fa4, the act itself: completing or skipping such a step
//! is refused 403 unless a person signs the write, and no step write may
//! change the declaration on the way there.
//!
//! HOW: an `answer-question` completion records `accepted_as_proposed`
//! by comparing the answer to the packet's `proposed` text server-side.

use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, Priority, Step, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;

const DAVID: &str = "emp-david";
/// The pod session's identity: human-shaped, not on the roster.
const AGENT: &str = "claude@algedonic.dev";
const RULE: &str = "automation:rule:assign";

/// The people registry as the jobs API sees it: David is the one
/// active platform-admin; the agent session is not an employee.
struct FixedRoster;

#[async_trait]
impl RosterLookup for FixedRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec![DAVID.to_string()],
            _ => Vec::new(),
        })
    }
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == DAVID)
    }
}

fn user(id: &str, role: &str) -> String {
    serde_json::json!({
        "id": id,
        "role": role,
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": null,
    })
    .to_string()
}

/// The dispatcher's identity on its assignment PUT.
fn dispatcher() -> String {
    user("system:dispatcher", "system")
}

fn task(slug: &str, role: &str, defaults: serde_json::Value) -> StepSpec {
    StepSpec {
        title: slug.into(),
        kind: "task".into(),
        ready_when: "true".into(),
        title_template: slug.replace('-', " "),
        authority_role: Some(role.into()),
        metadata_defaults: defaults,
        ..Default::default()
    }
}

/// The rotate-a-credential shape, reduced: one step the protocol marks
/// human-only (as the live registry spells it — the string `"true"`),
/// one it does not.
fn rotation_spec() -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        "rotation",
        "Rotate a credential",
        "test",
        vec!["custom".into()],
        vec![
            task(
                "kill",
                "platform-admin",
                serde_json::json!({ "destructive": "true", "human_only": "true" }),
            ),
            task("install", "platform-admin", serde_json::json!(null)),
        ],
    )
}

/// A packet with a design decision on it.
fn review_spec() -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        "review",
        "Review",
        "test",
        vec!["custom".into()],
        vec![StepSpec {
            title: "decide".into(),
            kind: "answer-question".into(),
            ready_when: "true".into(),
            title_template: "Decide".into(),
            authority_role: Some("platform-admin".into()),
            ..Default::default()
        }],
    )
}

fn app() -> (Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds.seed(rotation_spec()).unwrap();
    kinds.seed(review_spec()).unwrap();
    let jobs = Arc::new(InMemoryJobs::new());
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
        roster: Some(Arc::new(FixedRoster)),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs)
}

async fn read(resp: axum::http::Response<Body>) -> (StatusCode, serde_json::Value, String) {
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::json!(null));
    (status, json, text)
}

/// File a packet the way automation does: the filer is a machine, the
/// owner resolves to a person server-side.
async fn file(app: &Router, jobs: &InMemoryJobs, kind: &str, metadata: serde_json::Value) -> Job {
    let mut job = Job::new(
        kind,
        Subject::new("custom", "boss-dev-forge-token"),
        "Maiden rotation",
        "automation:filer",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 9, 8).unwrap(),
    );
    job.metadata = metadata;
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
    let (status, body, _) = read(resp).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["id"].as_str().unwrap();
    let job_id = boss_core::job::JobId::from_uuid(uuid::Uuid::parse_str(id).unwrap());
    jobs.get_job(&job_id).await.unwrap().unwrap()
}

async fn step_by_slug(jobs: &InMemoryJobs, job: &Job, slug: &str) -> Step {
    jobs.list_steps(&job.id)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.spec_slug.as_deref() == Some(slug))
        .unwrap_or_else(|| panic!("no step {slug}"))
}

/// The step's stored metadata with `keys` laid over it — the
/// read-merge-write body the step PUT requires, since it refuses a
/// metadata body that omits a stored key (e39a9d2a).
fn over(step: &Step, keys: serde_json::Value) -> serde_json::Value {
    let mut md = step.metadata.clone();
    if let (Some(m), Some(k)) = (md.as_object_mut(), keys.as_object()) {
        m.extend(k.clone());
    }
    md
}

async fn put_step(
    app: &Router,
    step: &Step,
    as_user: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}/steps/{}", step.job_id, step.id))
                .header("content-type", "application/json")
                .header("x-boss-user", as_user)
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    read(resp).await
}

async fn claim(
    app: &Router,
    step: &Step,
    as_user: &str,
) -> (StatusCode, serde_json::Value, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/jobs/{}/steps/{}/claim", step.job_id, step.id))
                .header("x-boss-user", as_user)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    read(resp).await
}

fn assert_refused(status: StatusCode, body: &serde_json::Value, step: &Step, assignee: &str) {
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a human-only step must refuse {assignee}: {body}"
    );
    assert_eq!(body["step_id"], step.id.to_string(), "{body}");
    assert_eq!(body["assignee_id"], assignee, "{body}");
    assert!(
        body["rule"]
            .as_str()
            .is_some_and(|r| r.contains("human_only")),
        "the refusal names the rule: {body}"
    );
}

/// THE CLAIM. A step declared `human_only` cannot be handed to an
/// agent session or an automation, and the refusal says which step,
/// which assignee, and why.
#[tokio::test]
async fn a_human_only_step_refuses_an_agent_assignee() {
    let (app, jobs) = app();
    let job = file(&app, &jobs, "rotation", serde_json::json!({})).await;
    let kill = step_by_slug(&jobs, &job, "kill").await;

    for assignee in [AGENT, RULE] {
        let (status, body, _) = put_step(
            &app,
            &kill,
            &dispatcher(),
            serde_json::json!({ "assignee_id": assignee }),
        )
        .await;
        assert_refused(status, &body, &kill, assignee);
    }
    let stored = jobs.get_step(&kill.id).await.unwrap().unwrap();
    assert_eq!(
        stored.assignee_id, None,
        "a refused assignment writes nothing"
    );
}

#[tokio::test]
async fn a_human_only_step_accepts_an_employee_assignee() {
    let (app, jobs) = app();
    let job = file(&app, &jobs, "rotation", serde_json::json!({})).await;
    let kill = step_by_slug(&jobs, &job, "kill").await;

    let (status, _, text) = put_step(
        &app,
        &kill,
        &dispatcher(),
        serde_json::json!({ "assignee_id": DAVID }),
    )
    .await;
    assert!(status.is_success(), "{status} {text}");
    let stored = jobs.get_step(&kill.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(DAVID));
}

#[tokio::test]
async fn a_step_that_is_not_human_only_is_untouched() {
    let (app, jobs) = app();
    let job = file(&app, &jobs, "rotation", serde_json::json!({})).await;
    let install = step_by_slug(&jobs, &job, "install").await;

    let (status, _, text) = put_step(
        &app,
        &install,
        &dispatcher(),
        serde_json::json!({ "assignee_id": AGENT }),
    )
    .await;
    assert!(status.is_success(), "{status} {text}");
    let stored = jobs.get_step(&install.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(AGENT));
}

/// The declaration is protocol data on the step and a body cannot
/// strip it by omission: a metadata PUT that omits `human_only` is
/// refused (the drop refusal, e39a9d2a — it used to be carried forward
/// by hand), the step stays human-only, and the next assignment is
/// still refused.
#[tokio::test]
async fn human_only_cannot_be_stripped_by_a_metadata_put() {
    let (app, jobs) = app();
    let job = file(&app, &jobs, "rotation", serde_json::json!({})).await;
    let kill = step_by_slug(&jobs, &job, "kill").await;

    let (status, body, text) = put_step(
        &app,
        &kill,
        &user(AGENT, "platform-admin"),
        serde_json::json!({ "metadata": { "note": "stripped" } }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert!(
        body["missing_keys"]
            .as_array()
            .is_some_and(|k| k.iter().any(|k| k == boss_jobs::human_only::KEY)),
        "the refusal names the dropped declaration: {text}"
    );
    let stored = jobs.get_step(&kill.id).await.unwrap().unwrap();
    assert!(
        boss_jobs::human_only::declared(&stored.metadata),
        "human_only survives an omitting PUT: {}",
        stored.metadata
    );

    let (status, body, _) = put_step(
        &app,
        &kill,
        &dispatcher(),
        serde_json::json!({ "assignee_id": AGENT }),
    )
    .await;
    assert_refused(status, &body, &kill, AGENT);
}

/// A packet filed by automation leaves the human-only step unassigned,
/// carrying its `authority_role` — the shape My Day's "open to you"
/// queue lists by role.
#[tokio::test]
async fn a_job_filed_by_automation_leaves_the_human_only_step_unassigned_with_its_role() {
    let (app, jobs) = app();
    let job = file(&app, &jobs, "rotation", serde_json::json!({})).await;
    assert_eq!(job.owner_id, DAVID, "the owner resolves to a person (Q7)");

    let kill = step_by_slug(&jobs, &job, "kill").await;
    assert_eq!(kill.assignee_id, None, "the filer is not the assignee");
    assert_eq!(kill.metadata["authority_role"], "platform-admin");
    assert!(boss_jobs::human_only::declared(&kill.metadata));
    assert_eq!(kill.status, boss_core::job::StepStatus::Ready);
}

#[tokio::test]
async fn an_agent_cannot_claim_a_human_only_step_but_an_employee_can() {
    let (app, jobs) = app();
    let job = file(&app, &jobs, "rotation", serde_json::json!({})).await;
    let kill = step_by_slug(&jobs, &job, "kill").await;

    let (status, body, _) = claim(&app, &kill, &user(AGENT, "platform-admin")).await;
    assert_refused(status, &body, &kill, AGENT);
    let stored = jobs.get_step(&kill.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id, None);

    let (status, _, text) = claim(&app, &kill, &user(DAVID, "platform-admin")).await;
    assert!(status.is_success(), "{status} {text}");
    let stored = jobs.get_step(&kill.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(DAVID));
}

/// HOW: the completion records whether the answer is the proposal
/// verbatim — computed server-side, so a UI accept and a rule copy read
/// the same, and a body cannot claim an acceptance it did not make.
#[tokio::test]
async fn an_answer_question_completion_records_whether_the_proposal_was_accepted() {
    let (app, jobs) = app();
    let proposed = "Stamp completed_by on every step; record accepted_as_proposed.";

    // Accepted as proposed: the answer IS the proposal.
    let job = file(
        &app,
        &jobs,
        "review",
        serde_json::json!({ "proposed": proposed }),
    )
    .await;
    let decide = step_by_slug(&jobs, &job, "decide").await;
    let (status, _, text) = put_step(
        &app,
        &decide,
        &user(DAVID, "platform-admin"),
        serde_json::json!({
            "status": "completed",
            "metadata": over(&decide, serde_json::json!({ "verdict": "approved", "answer": proposed })),
        }),
    )
    .await;
    assert!(status.is_success(), "{status} {text}");
    let stored = jobs.get_step(&decide.id).await.unwrap().unwrap();
    assert_eq!(
        stored.metadata["accepted_as_proposed"], true,
        "{}",
        stored.metadata
    );

    // Edited: the answer differs, and the body's own claim of
    // acceptance is overwritten.
    let job = file(
        &app,
        &jobs,
        "review",
        serde_json::json!({ "proposed": proposed }),
    )
    .await;
    let decide = step_by_slug(&jobs, &job, "decide").await;
    let (status, _, text) = put_step(
        &app,
        &decide,
        &user(DAVID, "platform-admin"),
        serde_json::json!({
            "status": "completed",
            "metadata": over(&decide, serde_json::json!({
                "verdict": "approved",
                "answer": "Stamp completed_by only; drop the rest.",
                "accepted_as_proposed": true,
            })),
        }),
    )
    .await;
    assert!(status.is_success(), "{status} {text}");
    let stored = jobs.get_step(&decide.id).await.unwrap().unwrap();
    assert_eq!(
        stored.metadata["accepted_as_proposed"], false,
        "{}",
        stored.metadata
    );
}

// ── COMPLETION (backlog adac8fa4) ──────────────────────────────────
//
// The checks above run at assignment and claim. A human-only step that
// nobody holds could still be FLIPPED by an agent through the step PUT,
// which is the act the declaration reserves — found by the flights car-1
// builder from the code on 2026-09-24 and measured here first: before
// the fix each test below completed the step and answered 204.

async fn patch_metadata(
    app: &Router,
    step: &Step,
    as_user: &str,
    patch: serde_json::Value,
) -> (StatusCode, serde_json::Value, String) {
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
                .header("x-boss-user", as_user)
                .body(Body::from(patch.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    read(resp).await
}

fn assert_completion_refused(
    status: StatusCode,
    body: &serde_json::Value,
    text: &str,
    step: &Step,
    actor: &str,
) {
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a human-only step must refuse completion by {actor}: {text}"
    );
    assert_eq!(body["step_id"], step.id.to_string(), "{text}");
    assert_eq!(body["actor_id"], actor, "{text}");
    assert!(
        body["rule"]
            .as_str()
            .is_some_and(|r| r.contains("human_only") && r.contains("complete")),
        "the refusal names the completion rule: {text}"
    );
}

async fn assert_still_open(jobs: &InMemoryJobs, step: &Step) {
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(
        stored.status,
        boss_core::job::StepStatus::Ready,
        "a refused completion writes nothing"
    );
    assert_eq!(stored.completed_by, None);
    assert!(
        boss_jobs::human_only::declared(&stored.metadata),
        "the declaration survives: {}",
        stored.metadata
    );
}

/// THE HOLE. An unassigned human-only step, and an agent flips it —
/// in each spelling an agent reaches the API with: the session login
/// (human-shaped, not on the roster), its resolved registered-agent id,
/// and an `<mode>:<model>` session id.
#[tokio::test]
async fn an_agent_cannot_complete_an_unassigned_human_only_step() {
    let (app, jobs) = app();
    let job = file(&app, &jobs, "rotation", serde_json::json!({})).await;
    let kill = step_by_slug(&jobs, &job, "kill").await;
    assert_eq!(
        kill.assignee_id, None,
        "the measured shape: nobody holds it"
    );

    for agent in [AGENT, "agent-claude", "claude:opus-5"] {
        let (status, body, text) = put_step(
            &app,
            &kill,
            &user(agent, "platform-admin"),
            serde_json::json!({ "status": "completed" }),
        )
        .await;
        assert_completion_refused(status, &body, &text, &kill, agent);
        assert_still_open(&jobs, &kill).await;
    }
}

/// Skipping is completing by another name — a dependent's `ready_when`
/// reads `steps.kill.done`, which a skip satisfies — so it is refused
/// the same way.
#[tokio::test]
async fn an_agent_cannot_skip_a_human_only_step() {
    let (app, jobs) = app();
    let job = file(&app, &jobs, "rotation", serde_json::json!({})).await;
    let kill = step_by_slug(&jobs, &job, "kill").await;

    let (status, body, text) = put_step(
        &app,
        &kill,
        &user(AGENT, "platform-admin"),
        serde_json::json!({ "status": "skipped" }),
    )
    .await;
    assert_completion_refused(status, &body, &text, &kill, AGENT);
    assert_still_open(&jobs, &kill).await;
}

/// An automation is not a person either, and naming one in the body's
/// `completed_by` (the sim's proxy attribution) does not make it one:
/// the check reads who SIGNED the write, because a human-only step's
/// whole claim is that a person did the act.
#[tokio::test]
async fn an_automation_cannot_complete_a_human_only_step_by_naming_a_person() {
    let (app, jobs) = app();
    let job = file(&app, &jobs, "rotation", serde_json::json!({})).await;
    let kill = step_by_slug(&jobs, &job, "kill").await;

    let (status, body, text) = put_step(
        &app,
        &kill,
        &dispatcher(),
        serde_json::json!({ "status": "completed", "completed_by": DAVID }),
    )
    .await;
    assert_completion_refused(status, &body, &text, &kill, "system:dispatcher");
    assert_still_open(&jobs, &kill).await;
}

/// The declaration cannot be lifted on the way to completion: not in
/// the completing PUT's own body, and not through the merge door first
/// (a key sent as `null` is deleted there). The rule is the protocol's
/// — a new workflow version changes it, never a step write.
#[tokio::test]
async fn the_declaration_cannot_be_lifted_on_the_way_to_completion() {
    let (app, jobs) = app();
    let job = file(&app, &jobs, "rotation", serde_json::json!({})).await;
    let kill = step_by_slug(&jobs, &job, "kill").await;
    let agent = user(AGENT, "platform-admin");

    // In the completing PUT itself.
    let (status, body, text) = put_step(
        &app,
        &kill,
        &agent,
        serde_json::json!({
            "status": "completed",
            "metadata": over(&kill, serde_json::json!({ "human_only": false })),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert_eq!(body["step_id"], kill.id.to_string(), "{text}");
    assert_still_open(&jobs, &kill).await;

    // In a PUT that does not complete, to set up a later flip.
    let (status, _, text) = put_step(
        &app,
        &kill,
        &agent,
        serde_json::json!({
            "metadata": over(&kill, serde_json::json!({ "human_only": "false" })),
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    assert_still_open(&jobs, &kill).await;

    // Through the merge door, deleted or flipped.
    for patch in [
        serde_json::json!({ "human_only": null }),
        serde_json::json!({ "human_only": false }),
    ] {
        let (status, body, text) = patch_metadata(&app, &kill, &agent, patch).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
        assert_eq!(body["step_id"], kill.id.to_string(), "{text}");
        assert_still_open(&jobs, &kill).await;
    }

    // ...so the status PUT that would follow is still refused.
    let (status, body, text) = put_step(
        &app,
        &kill,
        &agent,
        serde_json::json!({ "status": "completed" }),
    )
    .await;
    assert_completion_refused(status, &body, &text, &kill, AGENT);

    // An unchanged re-send is not a change: the merge door still takes
    // other keys beside the declaration as it stands.
    let (status, _, text) = patch_metadata(
        &app,
        &kill,
        &agent,
        serde_json::json!({ "human_only": "true", "note": "looked at it" }),
    )
    .await;
    assert!(status.is_success(), "{status} {text}");
}

/// The person the step is reserved for completes it, and the record
/// names them.
#[tokio::test]
async fn an_employee_completes_a_human_only_step() {
    let (app, jobs) = app();
    let job = file(&app, &jobs, "rotation", serde_json::json!({})).await;
    let kill = step_by_slug(&jobs, &job, "kill").await;

    let (status, _, text) = put_step(
        &app,
        &kill,
        &user(DAVID, "platform-admin"),
        serde_json::json!({ "status": "completed" }),
    )
    .await;
    assert!(status.is_success(), "{status} {text}");
    let stored = jobs.get_step(&kill.id).await.unwrap().unwrap();
    assert_eq!(stored.status, boss_core::job::StepStatus::Completed);
    assert_eq!(
        stored.completed_by,
        Some(boss_core::actor::ActorId::Human(DAVID.into()))
    );
}

/// Automation that completes steps the protocol did NOT reserve is
/// untouched — the dispatcher's own flips, and an agent's.
#[tokio::test]
async fn automation_and_agents_still_complete_a_step_that_is_not_human_only() {
    for completer in [dispatcher(), user(AGENT, "platform-admin")] {
        let (app, jobs) = app();
        let job = file(&app, &jobs, "rotation", serde_json::json!({})).await;
        let install = step_by_slug(&jobs, &job, "install").await;
        let (status, _, text) = put_step(
            &app,
            &install,
            &completer,
            serde_json::json!({ "status": "completed" }),
        )
        .await;
        assert!(status.is_success(), "{completer}: {status} {text}");
        let stored = jobs.get_step(&install.id).await.unwrap().unwrap();
        assert_eq!(stored.status, boss_core::job::StepStatus::Completed);
    }
}
