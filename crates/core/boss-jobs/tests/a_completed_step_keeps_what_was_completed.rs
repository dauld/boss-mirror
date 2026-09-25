//! A completed step keeps what was completed: its title, its holder,
//! its notes, and the day the SERVER says it was completed.
//!
//! THE DEFECT (backlog 42e7c6b9), reproduced on a merged tree by the
//! review of car 52ad60e6, 2026-09-25. Two holes, both provenance:
//!
//! (A) After a ticketed completion of a presence step (204), a bare
//!     `PUT {"title": ...}` answered 204 and changed the stored title of
//!     the COMPLETED step. The terminal freeze in `update_step` checked
//!     only `completed_on` and `metadata`, and the Pg UPDATE wrote
//!     `title`, `assignee_id` and `notes` with no terminal CASE. The ops
//!     runner fails closed (its stamp no longer matches), but the record
//!     then shows a passkey approval of a title no passkey saw.
//!
//! (B) The completing PUT could carry its own `completed_on`, so a
//!     caller chose the day of its own completion — a backdate — while
//!     `completed_at` and `completed_by` beside it were already the
//!     server's. A body that set `completed_on` on a step it did NOT
//!     complete stored it too, and the flip later kept it.
//!
//! THE RULE, for both: a completed or skipped step's title, assignee
//! and notes are frozen like its metadata — a write that would change
//! one is refused 409 naming it, an unchanged re-send stays 204 — and
//! `completed_on` is the server clock's date at the flip, whatever the
//! body says, on every step.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_clock_client::{ClockNow, FixedClockClient};
use boss_core::job::{
    Assurance, Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject,
};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use tower::ServiceExt;
use uuid::Uuid;

const JOB: &str = "00000000-0000-0000-0000-00000000e001";
const GUARDED: &str = "00000000-0000-0000-0000-00000000f001";
const ORDINARY: &str = "00000000-0000-0000-0000-00000000f002";
const DONE: &str = "00000000-0000-0000-0000-00000000f003";

const PRESENCE_KEY: &[u8] = b"completed-step-keeps-presence-key-0123";

/// The day the server's clock reads. A body's `completed_on` is never
/// this, so a stored date equal to it can only have come from the clock.
fn server_day() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 9, 25).unwrap()
}

/// The backdate a caller tried to choose.
const BACKDATE: &str = "2020-01-01";

fn operator() -> User {
    User {
        id: "emp-david".to_string(),
        role: "platform-admin".to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn policy() -> Arc<dyn PolicyClient> {
    Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
            .allow(
                "platform-admin",
                Action::Update,
                Resource::job(),
                Scope::All,
            )
            .build(),
    )
}

fn step_id(id: &str) -> StepId {
    StepId::from_uuid(Uuid::parse_str(id).unwrap())
}

fn step(id: &str, status: StepStatus, assurance: Option<Assurance>) -> Step {
    Step {
        id: step_id(id),
        job_id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
        kind: "generic".into(),
        title: "Approve the plan".into(),
        spec_slug: Some(format!("step-{}", &id[id.len() - 3..])),
        assignee_id: Some("emp-david".into()),
        status,
        sort_order: 1,
        blocked_by: vec![],
        sign_offs_required: Vec::new(),
        assurance_required: assurance,
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: match status {
            StepStatus::Completed => NaiveDate::from_ymd_opt(2026, 9, 20),
            _ => None,
        },
        completed_by: None,
        completed_at: None,
        metadata: serde_json::json!({ "plan": "{\"verb\":\"commission-a-disk\"}" }),
        notes: Some("as planned".into()),
        step_plugin_version: 0,
        embedded_job: None,
    }
}

async fn seed() -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        presence_key: Some(Arc::new(boss_jobs::http::PresenceKey::fixed(
            PRESENCE_KEY.to_vec(),
        ))),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy(),
            Arc::new(FixedClockClient::new(ClockNow {
                now: server_day().and_hms_opt(12, 0, 0).unwrap().and_utc(),
                simulated: false,
                epoch_start: None,
                epoch_end: None,
                paused: false,
                restart_in_progress: false,
                warp_factor: None,
            })),
        )
    };
    jobs.create_job(&Job {
        id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
        kind: "ops-request".into(),
        workflow_version: 2,
        subject: Subject::new("custom", "forge"),
        title: "df on forge".into(),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 22).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    })
    .await
    .unwrap();
    for s in [
        step(GUARDED, StepStatus::Ready, Some(Assurance::Presence)),
        step(ORDINARY, StepStatus::Ready, None),
        step(DONE, StepStatus::Completed, None),
    ] {
        jobs.add_step(&s).await.unwrap();
    }
    (router(state), jobs)
}

/// A PUT, optionally carrying the gateway's signed presence ticket.
async fn put(
    app: &Router,
    id: &str,
    body: serde_json::Value,
    presence: Option<String>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder()
        .method("PUT")
        .uri(format!("/api/jobs/{JOB}/steps/{id}"))
        .header("content-type", "application/json")
        .header("x-boss-user", serde_json::to_string(&operator()).unwrap());
    if let Some(ticket) = presence {
        req = req.header("x-boss-presence", ticket);
    }
    let resp = app
        .clone()
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

/// The ticket the gateway's ceremony mints for the step as it stands.
fn ticket_for(stored: &Step, nonce: &str) -> String {
    boss_core::presence::PresenceTicket {
        i: operator().id,
        s: stored.id.to_string(),
        h: boss_core::job::step_shape_hash(&stored.title, &stored.metadata),
        n: nonce.to_string(),
        e: boss_core::presence::now_epoch() + 60,
    }
    .encode(PRESENCE_KEY)
    .expect("a ticket signs")
}

async fn stored(jobs: &InMemoryJobs, id: &str) -> Step {
    jobs.get_step(&step_id(id))
        .await
        .unwrap()
        .expect("the step is there")
}

fn refused(body: &serde_json::Value) -> Vec<String> {
    body["refused_fields"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// THE BUG, in the shape the review measured: a ticketed completion of
/// a presence step, then a bare PUT of a new title.
#[tokio::test]
async fn a_bare_put_cannot_retitle_a_completed_presence_step() {
    let (app, jobs) = seed().await;
    let before = stored(&jobs, GUARDED).await;
    let (status, body) = put(
        &app,
        GUARDED,
        serde_json::json!({ "status": "completed" }),
        Some(ticket_for(&before, "ceremony-1")),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the ticketed completion lands: {body}"
    );

    let (status, body) = put(
        &app,
        GUARDED,
        serde_json::json!({ "title": "Run rm -rf on the forge" }),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a completed step's title is what its ceremony saw; rewriting it after the fact \
         makes the record show a passkey approval of a title no passkey saw: {body}"
    );
    assert_eq!(
        refused(&body),
        vec!["title"],
        "the refusal names the field: {body}"
    );
    assert_eq!(
        stored(&jobs, GUARDED).await.title,
        "Approve the plan",
        "and the stored title is untouched"
    );
}

/// Title, holder and notes, one at a time, on an ordinary completed
/// step: each is refused by name and none moves.
#[tokio::test]
async fn each_frozen_field_is_refused_by_name_on_a_completed_step() {
    let (app, jobs) = seed().await;
    for (field, value) in [
        ("title", serde_json::json!("A different title")),
        ("assignee_id", serde_json::json!("emp-someone-else")),
        ("notes", serde_json::json!("written after the fact")),
    ] {
        let (status, body) = put(&app, DONE, serde_json::json!({ field: value }), None).await;
        assert_eq!(status, StatusCode::CONFLICT, "{field}: {body}");
        assert_eq!(refused(&body), vec![field.to_string()], "{field}: {body}");
        // The dispatcher's assign reads these two keys to learn that a
        // step finished before its assignment arrived
        // (`finished_before_assignment` in boss-dispatcher).
        assert_eq!(body["step_status"], "completed", "{field}: {body}");
    }
    let after = stored(&jobs, DONE).await;
    assert_eq!(after.title, "Approve the plan");
    assert_eq!(after.assignee_id.as_deref(), Some("emp-david"));
    assert_eq!(after.notes.as_deref(), Some("as planned"));
}

/// Clearing a frozen field is a change too.
#[tokio::test]
async fn clearing_the_holder_or_notes_of_a_completed_step_is_refused() {
    let (app, jobs) = seed().await;
    let (status, body) = put(
        &app,
        DONE,
        serde_json::json!({ "assignee_id": null, "notes": null }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(refused(&body), vec!["assignee_id", "notes"], "{body}");
    let after = stored(&jobs, DONE).await;
    assert_eq!(after.assignee_id.as_deref(), Some("emp-david"));
    assert_eq!(after.notes.as_deref(), Some("as planned"));
}

/// The freeze exists so racing writers stay harmless: a re-send of what
/// is already stored is not a change and keeps its 204.
#[tokio::test]
async fn an_unchanged_resend_of_a_completed_step_still_succeeds() {
    let (app, _jobs) = seed().await;
    let (status, body) = put(
        &app,
        DONE,
        serde_json::json!({
            "status": "completed",
            "title": "Approve the plan",
            "assignee_id": "emp-david",
            "notes": "as planned",
        }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
}

/// (B) on the presence step: the ticketed completion cannot choose its
/// own day.
#[tokio::test]
async fn a_ticketed_completion_cannot_backdate_itself() {
    let (app, jobs) = seed().await;
    let before = stored(&jobs, GUARDED).await;
    let (status, body) = put(
        &app,
        GUARDED,
        serde_json::json!({ "status": "completed", "completed_on": BACKDATE }),
        Some(ticket_for(&before, "ceremony-2")),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let after = stored(&jobs, GUARDED).await;
    assert_eq!(
        after.completed_on,
        Some(server_day()),
        "completed_on is the server clock's day, as completed_at is its instant"
    );
    assert_eq!(
        after.completed_at.map(|t| t.date_naive()),
        after.completed_on,
        "the day and the instant of one completion agree"
    );
}

/// (B) on every step, not only presence ones: no caller dates its own
/// completion.
#[tokio::test]
async fn an_ordinary_completion_cannot_backdate_itself() {
    let (app, jobs) = seed().await;
    let (status, body) = put(
        &app,
        ORDINARY,
        serde_json::json!({ "status": "completed", "completed_on": BACKDATE }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        stored(&jobs, ORDINARY).await.completed_on,
        Some(server_day())
    );
}

/// The two-step walk round: plant a date on an OPEN step, then send a
/// bare completion. Neither write gets to choose the day.
#[tokio::test]
async fn a_date_planted_on_an_open_step_does_not_survive_its_completion() {
    let (app, jobs) = seed().await;
    let (status, body) = put(
        &app,
        ORDINARY,
        serde_json::json!({ "completed_on": BACKDATE }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        stored(&jobs, ORDINARY).await.completed_on,
        None,
        "an open step has no completion day to store"
    );

    let (status, body) = put(
        &app,
        ORDINARY,
        serde_json::json!({ "status": "completed" }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        stored(&jobs, ORDINARY).await.completed_on,
        Some(server_day())
    );
}

/// The adapter half: a write that reaches the in-memory row without the
/// handler (a stale whole-row copy) cannot move a terminal row's title,
/// holder or notes either — the same freeze the Pg UPDATE's CASE holds
/// (pinned against the real SQL in a_completion_names_its_actor_pg.rs).
#[tokio::test]
async fn the_in_memory_row_freezes_title_holder_and_notes() {
    let (_app, jobs) = seed().await;
    let mut stale = stored(&jobs, DONE).await;
    stale.title = "A different title".into();
    stale.assignee_id = Some("emp-someone-else".into());
    stale.notes = Some("written after the fact".into());
    jobs.update_step(&stale).await.unwrap();

    let after = stored(&jobs, DONE).await;
    assert_eq!(after.title, "Approve the plan");
    assert_eq!(after.assignee_id.as_deref(), Some("emp-david"));
    assert_eq!(after.notes.as_deref(), Some("as planned"));
}
