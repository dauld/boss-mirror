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
    /// `emp-someone-else` is on the roster and holds no role: a human
    /// owner admission keeps as sent. `emp-departed` is not on it.
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == "emp-bootstrap-admin" || id == "emp-someone-else")
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

/// The body `jobs.spawn` sends for the child, as one delivery of one
/// `step.ready` event stamps it: the delivery-scoped provenance keys
/// beside the parent link (`jobs_spawn.rs`).
fn spawn_body_for_delivery(event_id: &str) -> String {
    let mut body: serde_json::Value = serde_json::from_str(&spawn_body("/system/flow")).unwrap();
    body["metadata"]["spawned_by_rule"] = "spawn-subjob-on-delegate-subjob-step-ready".into();
    body["metadata"]["triggered_by_event_id"] = event_id.into();
    body["metadata"]["triggered_by_topic"] = "step.ready.delegate-subjob".into();
    body.to_string()
}

#[tokio::test]
async fn a_second_step_ready_for_one_step_is_answered_its_child() {
    // Backlog 4bdb8150 (the round-3 review of car 983696b5). The child's
    // id is derived from the parent STEP, so a step that becomes Ready a
    // second time — a new `step.ready`, a new event id — re-sends the
    // same id. `triggered_by_event_id` names the delivery, not the
    // packet, and was compared like every other key sent: the re-send
    // was refused 409 `metadata`, `jobs.spawn` maps any non-2xx to a
    // downstream error, and the rule erred on every delivery of that
    // event. One step, one child, answered however often it is read.
    let (app, jobs) = harness();

    let (status, first) = post(&app, spawn_body_for_delivery("evt-ready-1")).await;
    assert_eq!(status, StatusCode::CREATED, "first admission: {first}");
    let events_once = jobs.recorded_events().len();

    let (status, second) = post(&app, spawn_body_for_delivery("evt-ready-2")).await;
    assert!(
        status.is_success(),
        "the second step.ready's re-send is answered, not refused: {status} {second}"
    );
    assert_eq!(second["id"], CHILD_ID, "{second}");
    assert_eq!(second["already_admitted"], true, "{second}");

    let child = jobs.get_job(&child_id()).await.unwrap().unwrap();
    assert_eq!(
        child.metadata["triggered_by_event_id"], "evt-ready-1",
        "the provenance is the FIRST admission's: the re-send writes nothing"
    );
    assert_eq!(jobs.list_steps(&child_id()).await.unwrap().len(), 2);
    assert_eq!(
        jobs.recorded_events().len(),
        events_once,
        "nothing recorded"
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

#[tokio::test]
async fn a_resend_naming_an_owner_admission_replaced_is_the_same_packet() {
    // The round-3 review of car 983696b5 (backlog dc7c91cc, SF-B):
    // admission replaces a person-shaped owner who is not on the active
    // roster with the kind's role holder (owner_resolution), so the
    // packet stores `emp-bootstrap-admin` for a body naming
    // `emp-departed`. The already-admitted check compared the SENT owner
    // with the stored one, so every byte-identical re-send was refused
    // 409 owner_id — forever. An owner admission would not keep as sent
    // is the server's to write, like an automation-shaped one.
    let (app, jobs) = harness();
    let mut body: serde_json::Value = serde_json::from_str(&spawn_body("/system/flow")).unwrap();
    body["owner_id"] = "emp-departed".into();

    let (status, first) = post(&app, body.to_string()).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    assert_eq!(
        jobs.get_job(&child_id()).await.unwrap().unwrap().owner_id,
        "emp-bootstrap-admin",
        "precondition: admission replaced the departed owner"
    );
    let events_once = jobs.recorded_events().len();

    let (status, second) = post(&app, body.to_string()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the re-send is the packet that exists: {second}"
    );
    assert_eq!(second["already_admitted"], true, "{second}");
    assert_eq!(
        jobs.recorded_events().len(),
        events_once,
        "nothing recorded"
    );
    assert_eq!(jobs.list_steps(&child_id()).await.unwrap().len(), 2);
}

// ---------------------------------------------------------------------
// The admission that raced past the existence check (backlog 9d2af748).
//
// The check above is a READ, and two first admissions under one id can
// both take it before either commits: both see no packet, both
// materialize, both reach the adapter. The job row's unique index lets
// one in; until 9d2af748 the other still inserted its steps (fresh ids,
// so their own guard never fired) onto the winner's packet, recorded a
// STEP_CREATED and a `step.ready` for each, and answered 201.
// `miss_next_job_read` is that moment: the check reads before the other
// admission is visible, and the adapter is where the two meet.
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_admission_that_raced_past_the_existence_check_writes_nothing() {
    let (app, jobs) = harness();
    let (status, first) = post(&app, spawn_body("/system/flow")).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let events_once = jobs.recorded_events().len();

    jobs.miss_next_job_read(&child_id());
    let (status, second) = post(&app, spawn_body("/system/flow")).await;

    assert_eq!(
        jobs.list_steps(&child_id()).await.unwrap().len(),
        2,
        "the loser hangs no second copy of the protocol's steps on the packet"
    );
    let after: Vec<String> = jobs.recorded_events()[events_once..]
        .iter()
        .map(|e| e.kind.clone())
        .collect();
    assert!(
        after.is_empty(),
        "the loser records nothing — no step.created, no step.ready: {after:?}"
    );
    assert_eq!(
        status,
        StatusCode::OK,
        "answered as the packet that exists, not as a creation: {second}"
    );
    assert_eq!(second["id"], CHILD_ID);
    assert_eq!(second["already_admitted"], true, "{second}");
}

#[tokio::test]
async fn a_racing_admission_of_a_different_packet_is_refused_naming_what_differs() {
    // The same comparison the existence check makes, made where the
    // race is decided: a loser whose body is not the packet that won is
    // refused, not told its packet exists.
    let (app, jobs) = harness();
    let (status, first) = post(&app, spawn_body("/system/flow")).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let events_once = jobs.recorded_events().len();

    jobs.miss_next_job_read(&child_id());
    let (status, body) = post(&app, spawn_body("/system/other")).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["differing_fields"]
            .as_array()
            .is_some_and(|f| f.iter().any(|v| v == "subject")),
        "the refusal names what differs: {body}"
    );
    assert_eq!(
        jobs.recorded_events().len(),
        events_once,
        "nothing recorded"
    );
    assert_eq!(jobs.list_steps(&child_id()).await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_racing_second_step_ready_for_one_step_is_answered_its_child() {
    // The rerail of this car onto 4bdb8150 and dc7c91cc: the comparison
    // is made in TWO places, and both must judge a re-send the same way.
    // A second `step.ready` for one parent step races the first to the
    // adapter carrying a new `triggered_by_event_id`; the key names the
    // delivery, not the packet, so the loser is answered its child, not
    // refused 409 `metadata` — which `jobs.spawn` would map to a
    // downstream error on every delivery.
    let (app, jobs) = harness();
    let (status, first) = post(&app, spawn_body_for_delivery("evt-ready-1")).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let events_once = jobs.recorded_events().len();

    jobs.miss_next_job_read(&child_id());
    let (status, second) = post(&app, spawn_body_for_delivery("evt-ready-2")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the racing re-send is the packet that exists: {second}"
    );
    assert_eq!(second["already_admitted"], true, "{second}");
    assert_eq!(
        jobs.recorded_events().len(),
        events_once,
        "nothing recorded"
    );
    assert_eq!(jobs.list_steps(&child_id()).await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_racing_resend_naming_an_owner_admission_replaced_is_the_same_packet() {
    // The same two-place rule for the owner (dc7c91cc): a body naming an
    // owner admission replaced is judged by the keep rule where the race
    // is decided too, or the loser of a race between two byte-identical
    // sends is refused 409 `owner_id` for a value the server wrote.
    let (app, jobs) = harness();
    let mut body: serde_json::Value = serde_json::from_str(&spawn_body("/system/flow")).unwrap();
    body["owner_id"] = "emp-departed".into();
    let (status, first) = post(&app, body.to_string()).await;
    assert_eq!(status, StatusCode::CREATED, "{first}");
    let events_once = jobs.recorded_events().len();

    jobs.miss_next_job_read(&child_id());
    let (status, second) = post(&app, body.to_string()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the racing re-send is the packet that exists: {second}"
    );
    assert_eq!(second["already_admitted"], true, "{second}");
    assert_eq!(
        jobs.recorded_events().len(),
        events_once,
        "nothing recorded"
    );
    assert_eq!(jobs.list_steps(&child_id()).await.unwrap().len(), 2);
}

/// The port half, without the handler: a second
/// `create_job_with_steps_at` under an id the store already holds —
/// its steps carrying fresh ids, as materialization mints them — answers
/// `AlreadyAdmitted` and writes no step and no event.
#[tokio::test]
async fn the_in_memory_port_admits_an_id_once() {
    use boss_core::actor::ActorId;
    use boss_core::job::{Job, Priority, Step, Subject};
    use boss_core::publisher::EventStamp;
    use boss_jobs::Admission;
    use boss_jobs::events::{JOB_CREATED, STEP_CREATED};

    let jobs = InMemoryJobs::new();
    let job = Job::new(
        KIND,
        Subject::new("custom", "/system/flow"),
        "Admitted once",
        "emp-bootstrap-admin",
        Priority::Standard,
        chrono::NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
    );
    let stamp = EventStamp::new("jobs", ActorId::Automation("test".into()));
    let admit = |n: i32| {
        let steps: Vec<Step> = (0..n)
            .map(|i| Step::new(job.id, "task", format!("Step {i}"), i))
            .collect();
        let events: Vec<_> = steps
            .iter()
            .map(|s| stamp.event(STEP_CREATED, boss_jobs::events::step_state_payload(s)))
            .collect();
        (steps, events)
    };
    let job_events = vec![stamp.event(JOB_CREATED, serde_json::to_value(&job).unwrap())];

    let (steps, events) = admit(2);
    let first = jobs
        .create_job_with_steps_at(&job, &steps, stamp.timestamp, &job_events, &events)
        .await
        .unwrap();
    assert_eq!(first, Admission::Admitted);
    let recorded_once = jobs.recorded_events().len();
    assert_eq!(recorded_once, 3, "precondition: the job and its two steps");

    let (steps, events) = admit(3);
    let second = jobs
        .create_job_with_steps_at(&job, &steps, stamp.timestamp, &job_events, &events)
        .await
        .unwrap();
    assert_eq!(second, Admission::AlreadyAdmitted);
    assert_eq!(jobs.list_steps(&job.id).await.unwrap().len(), 2);
    assert_eq!(jobs.recorded_events().len(), recorded_once);
}
