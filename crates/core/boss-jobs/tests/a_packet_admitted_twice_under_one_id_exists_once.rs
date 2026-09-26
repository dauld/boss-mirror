//! A packet admitted twice under one id exists once.
//!
//! Review of car 88123ae0 (backlog 558396ff, 2026-09-25). Since that car
//! landed, a step PUT that races a metadata write answers 409 and writes
//! nothing, and the writer sends it again. The dispatcher's `jobs.spawn`
//! handler opens a delegate-subjob's child and THEN PUTs the parent
//! step's `embedded_job`; a 409 on the PUT NAKs the event, JetStream
//! redelivers it, and the handler POSTs the child a second time. So the
//! child's id is now derived from the parent step — and a second POST
//! under an id that already names a packet must write NOTHING.
//!
//! It did not. The adapters' `ON CONFLICT (id) DO NOTHING` guards the job
//! row only: the handler materializes the Workflow's steps with fresh ids
//! every time, so a second admission under one id hung a second copy of
//! every step on the packet that already existed, and recorded a
//! `step.ready` for each — which is how a nested delegate-subjob would
//! spawn again one level down.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::JobId;
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{StepSpec, Terminal, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

const KIND: &str = "admitted-once";
const CHILD_ID: &str = "5a5a5a5a-0000-5000-8000-000000558396";

fn spec() -> WorkflowSpec {
    let mut spec = WorkflowSpec::platform_seed(
        KIND,
        "Admitted once",
        "platform",
        vec!["custom".into()],
        vec![
            StepSpec {
                title: "work".into(),
                kind: "task".into(),
                ready_when: "true".into(),
                title_template: "The work".into(),
                authority_role: Some("platform-admin".into()),
                ..Default::default()
            },
            StepSpec {
                title: "done".into(),
                kind: "outcome".into(),
                ready_when: "steps.work.done".into(),
                title_template: "Done".into(),
                authority_role: Some("platform-admin".into()),
                metadata_defaults: serde_json::json!({ "outcome_kind": "completed" }),
                terminal: Some(Terminal {
                    outcome: "done".into(),
                }),
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

fn rule_header() -> String {
    serde_json::json!({
        "id": "rule:spawn-subjob-on-delegate-subjob-step-ready",
        "role": "system",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": null,
    })
    .to_string()
}

fn harness() -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds.seed(spec()).expect("seed the kind");
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("system", Action::Create, Resource::job(), Scope::All)
            .allow("system", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
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

/// The body `jobs.spawn` sends for a delegate-subjob child, with the id
/// it now derives from the parent step.
fn spawn_body(subject: &str) -> String {
    serde_json::json!({
        "id": CHILD_ID,
        "kind": KIND,
        "subject": { "subject_kind": "custom", "id": subject },
        "title": "Auto-spawn from rule spawn-subjob-on-delegate-subjob-step-ready",
        "owner_id": "rule:spawn-subjob-on-delegate-subjob-step-ready",
        "priority": "standard",
        "status": "open",
        "metadata": { "parent_step_id": "parent-step-1", "parent_job_id": "parent-job-1" },
        "tags": ["dispatcher-spawned"],
    })
    .to_string()
}

async fn post(app: &axum::Router, body: String) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", rule_header())
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .expect("router responds");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

fn child_id() -> JobId {
    JobId::from_uuid(uuid::Uuid::parse_str(CHILD_ID).unwrap())
}

#[tokio::test]
async fn a_second_admission_under_the_same_id_answers_the_packet_and_writes_nothing() {
    let (app, jobs) = harness();

    let (status, first) = post(&app, spawn_body("/system/flow")).await;
    assert_eq!(status, StatusCode::CREATED, "first admission: {first}");
    assert_eq!(first["id"], CHILD_ID);
    let steps_once = jobs.list_steps(&child_id()).await.unwrap().len();
    assert_eq!(steps_once, 2, "precondition: the protocol's two steps");
    let events_once = jobs.recorded_events().len();

    // The redelivery: the same body, the same derived id.
    let (status, second) = post(&app, spawn_body("/system/flow")).await;
    assert!(
        status.is_success(),
        "a re-sent admission of the same packet is answered, not refused: {status} {second}"
    );
    assert_eq!(
        second["id"], CHILD_ID,
        "and it names the packet that exists"
    );

    assert_eq!(
        jobs.list_steps(&child_id()).await.unwrap().len(),
        steps_once,
        "the packet still has its protocol's steps once, not a second copy of each"
    );
    assert_eq!(
        jobs.recorded_events().len(),
        events_once,
        "nothing recorded: no step.created, no step.ready for steps that were never written"
    );
}

#[tokio::test]
async fn an_id_that_names_a_different_packet_is_refused() {
    let (app, jobs) = harness();
    let (status, first) = post(&app, spawn_body("/system/flow")).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let events_once = jobs.recorded_events().len();

    // Same id, a different subject: this is not the packet that exists,
    // and answering its id as though it were would hand the caller
    // someone else's packet.
    let (status, body) = post(&app, spawn_body("/system/other")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(jobs.recorded_events().len(), events_once);
    assert_eq!(jobs.list_steps(&child_id()).await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_body_that_differs_from_the_admitted_packet_in_any_field_the_caller_sent_is_refused() {
    // The round-2 review of car 983696b5 (SF2): the idempotence answer
    // compared kind and subject only, so a body under the same id with
    // another title, other metadata, another partition was answered 200
    // "already admitted" — the caller told its packet exists when the
    // packet that exists is not the one it described. Each field the
    // caller sends is compared; a difference is refused, naming it.
    let (app, jobs) = harness();
    let (status, first) = post(&app, spawn_body("/system/flow")).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let events_once = jobs.recorded_events().len();

    let base: serde_json::Value = serde_json::from_str(&spawn_body("/system/flow")).unwrap();
    let variants: [(&str, fn(&mut serde_json::Value)); 6] = [
        ("title", |b| b["title"] = "Another packet".into()),
        ("metadata", |b| {
            b["metadata"]["parent_step_id"] = "parent-step-2".into()
        }),
        ("partition", |b| b["simulated"] = true.into()),
        ("priority", |b| b["priority"] = "urgent".into()),
        ("tags", |b| b["tags"] = serde_json::json!(["hand-filed"])),
        ("owner_id", |b| b["owner_id"] = "emp-someone-else".into()),
    ];
    for (field, change) in variants {
        let mut body = base.clone();
        change(&mut body);
        let (status, answer) = post(&app, body.to_string()).await;
        assert_eq!(status, StatusCode::CONFLICT, "{field}: {answer}");
        assert!(
            answer["differing_fields"]
                .as_array()
                .is_some_and(|f| f.iter().any(|v| v == field)),
            "{field}: the refusal names what differs: {answer}"
        );
    }
    assert_eq!(
        jobs.recorded_events().len(),
        events_once,
        "nothing recorded"
    );
    assert_eq!(jobs.list_steps(&child_id()).await.unwrap().len(), 2);
}
