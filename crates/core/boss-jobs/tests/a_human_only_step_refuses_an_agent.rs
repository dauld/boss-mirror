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
//! `authority_role`, so it is claimable by role in My Day.
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
use boss_jobs::step_registry::StepRegistry;
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
        roster: Some(Arc::new(FixedRoster)),
        clock: Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
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
/// strip it: a metadata PUT that omits `human_only` leaves the step
/// human-only, so the next assignment is still refused.
#[tokio::test]
async fn human_only_cannot_be_stripped_by_a_metadata_put() {
    let (app, jobs) = app();
    let job = file(&app, &jobs, "rotation", serde_json::json!({})).await;
    let kill = step_by_slug(&jobs, &job, "kill").await;

    let (status, _, text) = put_step(
        &app,
        &kill,
        &user(AGENT, "platform-admin"),
        serde_json::json!({ "metadata": { "note": "stripped" } }),
    )
    .await;
    assert!(status.is_success(), "{status} {text}");
    let stored = jobs.get_step(&kill.id).await.unwrap().unwrap();
    assert!(
        boss_jobs::human_only::declared(&stored.metadata),
        "human_only is carried forward like authority_role: {}",
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
            "metadata": { "verdict": "approved", "answer": proposed },
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
            "metadata": {
                "verdict": "approved",
                "answer": "Stamp completed_by only; drop the rest.",
                "accepted_as_proposed": true,
            },
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
