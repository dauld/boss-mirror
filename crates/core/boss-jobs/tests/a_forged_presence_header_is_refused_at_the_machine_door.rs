//! A presence stamp is granted on a SIGNED ticket, verified here — not
//! on a header that merely looks like the gateway wrote it.
//!
//! THE DEFECT (backlog 72fe3640, adversarial review of car 7abc0154,
//! 2026-09-24). `judge_assurance` parsed `x-boss-presence` as plain
//! JSON and granted `Assurance::Presence` whenever its `step_id`,
//! `shape_hash` and `employee_id` matched. The first two are public
//! inputs — anyone who can read the step can compute them — and the
//! third is compared against `x-boss-user`, which the machine door
//! (:7900) trusts verbatim. The HMAC over the ticket was checked in
//! exactly one place, the gateway's role_headers middleware, which
//! then re-injected the header as unsigned JSON. So every holder of the
//! machine token — the dev pod's `boss-api` door every agent uses, the
//! conductor, the dispatcher, the ops runner — could stamp passkey
//! presence on any step, as any person, emp-david included. That
//! forges a human's sign-off onto the record (provenance), and it is
//! the gate a destructive ops verb waits behind.
//!
//! THE FIX: the header now carries the gateway's SIGNED ticket, and the
//! jobs API verifies it itself — the same HMAC, the same constant-time
//! compare, the same expiry, from the one definition in
//! `boss_core::presence` that the gateway mints with. A header that does
//! not verify is REFUSED, never downgraded in silence, and a jobs API
//! with no key refuses every presence claim rather than trusting one.
//!
//! These tests drive the router the way the machine door does: no
//! gateway in front, the caller writes every header it likes. NEVER
//! point a forgery like this at the live system of record — this file
//! is the only place it runs.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{
    Assurance, Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject,
};
use boss_core::port::EventBus;
use boss_core::presence::PresenceTicket;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, PresenceKey, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use tower::ServiceExt;
use uuid::Uuid;

const JOB: &str = "00000000-0000-0000-0000-0000000fe001";
const GUARDED: &str = "00000000-0000-0000-0000-0000000fd001";
const SIGNED: &str = "00000000-0000-0000-0000-0000000fd002";
const ORDINARY: &str = "00000000-0000-0000-0000-0000000fd003";

/// The key the "gateway" in these tests signs with. A fixture value,
/// never the deployment's: the real key lives in the boss-session-key
/// Secret and appears in no repo, transcript or test.
const GATEWAY_KEY: &[u8] = b"presence-test-key-0123456789abcdef";

/// The person a forger would most like to be.
fn david() -> User {
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
            .allow(
                "platform-admin",
                Action::SignOff,
                Resource::new("step-signoff:platform-admin"),
                Scope::All,
            )
            .build(),
    )
}

fn step(id: &str, assurance: Option<Assurance>, sign_off: bool) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(id).unwrap()),
        job_id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
        kind: "generic".into(),
        title: "Approve the plan".into(),
        spec_slug: Some("approve".into()),
        assignee_id: Some("emp-david".into()),
        status: StepStatus::Ready,
        sort_order: 1,
        blocked_by: vec![],
        sign_offs_required: if sign_off {
            vec!["platform-admin".into()]
        } else {
            Vec::new()
        },
        assurance_required: assurance,
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: None,
        completed_by: None,
        completed_at: None,
        metadata: serde_json::json!({ "plan": "{\"verb\":\"wipe-a-disk\"}" }),
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

/// The jobs API as the machine door serves it. `key` is what the
/// binary loads from the gateway's key file; `None` is a jobs API that
/// never found one.
async fn seed(key: Option<&[u8]>) -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        presence_key: key.map(|k| Arc::new(PresenceKey::fixed(k.to_vec()))),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy(),
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    jobs.create_job(&Job {
        id: JobId::from_uuid(Uuid::parse_str(JOB).unwrap()),
        kind: "ops-request".into(),
        workflow_version: 2,
        subject: Subject::new("custom", "forge"),
        title: "wipe a disk on forge".into(),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 24).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    })
    .await
    .unwrap();
    jobs.add_step(&step(GUARDED, Some(Assurance::Presence), false))
        .await
        .unwrap();
    jobs.add_step(&step(SIGNED, Some(Assurance::Presence), true))
        .await
        .unwrap();
    jobs.add_step(&step(ORDINARY, None, false)).await.unwrap();
    (router(state), jobs)
}

/// The binding a forger computes from what they can read: the step id,
/// and the shape hash of the step's public title + metadata.
fn binding(id: &str) -> (String, String) {
    let s = step(id, None, false);
    (
        s.id.to_string(),
        boss_core::job::step_shape_hash(&s.title, &s.metadata),
    )
}

/// THE FORGERY, in the exact shape the gateway used to inject: plain
/// JSON, every field matching, no signature anywhere.
fn forged_json(id: &str) -> String {
    let (step_id, shape_hash) = binding(id);
    serde_json::json!({
        "employee_id": "emp-david",
        "step_id": step_id,
        "shape_hash": shape_hash,
        "nonce": "forged-nonce",
    })
    .to_string()
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A ticket as the gateway's `assert_finish` mints it, signed with `key`.
fn ticket(id: &str, key: &[u8], expires: u64) -> String {
    let (step_id, shape_hash) = binding(id);
    PresenceTicket {
        i: "emp-david".into(),
        s: step_id,
        h: shape_hash,
        n: "ceremony-nonce-7".into(),
        e: expires,
    }
    .encode(key)
    .expect("a ticket signs")
}

async fn send(
    app: &Router,
    method: &str,
    uri: String,
    presence: Option<&str>,
    body: &str,
) -> (StatusCode, String) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-boss-user", serde_json::to_string(&david()).unwrap());
    if let Some(p) = presence {
        req = req.header("x-boss-presence", p);
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
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn complete(app: &Router, id: &str, presence: Option<&str>) -> (StatusCode, String) {
    send(
        app,
        "PUT",
        format!("/api/jobs/{JOB}/steps/{id}"),
        presence,
        r#"{"status":"completed"}"#,
    )
    .await
}

async fn sign_off(app: &Router, presence: Option<&str>) -> (StatusCode, String) {
    send(
        app,
        "POST",
        format!("/api/jobs/{JOB}/steps/{SIGNED}/sign-offs"),
        presence,
        r#"{"role":"platform-admin"}"#,
    )
    .await
}

async fn stored(jobs: &InMemoryJobs, id: &str) -> Step {
    jobs.get_step(&StepId::from_uuid(Uuid::parse_str(id).unwrap()))
        .await
        .unwrap()
        .expect("the step is there")
}

/// THE BUG on the completion door: a machine-token holder completes a
/// presence-required step as emp-david with a hand-written header.
#[tokio::test]
async fn a_forged_presence_header_cannot_complete_a_presence_required_step() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;

    let (status, body) = complete(&app, GUARDED, Some(&forged_json(GUARDED))).await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an unsigned x-boss-presence header must not buy a passkey's assurance; body: {body}"
    );
    assert_eq!(stored(&jobs, GUARDED).await.status, StepStatus::Ready);
}

/// THE BUG on the sign-off door: the forged header wrote a Presence
/// stamp naming emp-david onto the record.
#[tokio::test]
async fn a_forged_presence_header_writes_no_presence_stamp() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;

    let (status, body) = sign_off(&app, Some(&forged_json(SIGNED))).await;

    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a forged header must be refused at the sign-off door; body: {body}"
    );
    assert!(
        stored(&jobs, SIGNED).await.sign_offs.is_empty(),
        "a refused forgery must leave no stamp on the record"
    );
}

/// Refused, not quietly downgraded: a claim of presence the API cannot
/// verify is refused even on a step that asks only for a session, and
/// the refusal says why — silence is the one failure this door may not
/// have.
#[tokio::test]
async fn an_unverifiable_presence_header_is_refused_even_where_a_session_would_do() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;

    let (status, body) = complete(&app, ORDINARY, Some(&forged_json(ORDINARY))).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(
        body.contains("did not verify"),
        "the refusal must name the unverified header: {body}"
    );
    assert_eq!(stored(&jobs, ORDINARY).await.status, StepStatus::Ready);

    // The control: with no header at all the same write goes through,
    // so the refusal above is the header's and nothing else's.
    let (status, body) = complete(&app, ORDINARY, None).await;
    assert!(status.is_success(), "{status}: {body}");
}

/// A ticket signed with any key but the gateway's is a forgery too.
#[tokio::test]
async fn a_ticket_signed_with_another_key_is_refused() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;
    let t = ticket(
        SIGNED,
        b"a-key-the-forger-made-up-0123456789",
        now_epoch() + 60,
    );

    let (status, body) = sign_off(&app, Some(&t)).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(stored(&jobs, SIGNED).await.sign_offs.is_empty());
}

/// The gateway's two-minute expiry holds here too: a genuine ticket
/// presented late is refused.
#[tokio::test]
async fn an_expired_ticket_is_refused() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;
    let t = ticket(SIGNED, GATEWAY_KEY, now_epoch() - 1);

    let (status, body) = sign_off(&app, Some(&t)).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(stored(&jobs, SIGNED).await.sign_offs.is_empty());
}

/// Fail closed: a jobs API that never found the key cannot verify
/// anything, so it grants presence to nothing — not even a genuine
/// ticket — rather than falling back to trusting the header.
#[tokio::test]
async fn a_jobs_api_without_the_key_grants_presence_to_nothing() {
    let (app, jobs) = seed(None).await;
    let t = ticket(SIGNED, GATEWAY_KEY, now_epoch() + 60);

    let (status, body) = sign_off(&app, Some(&t)).await;

    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(stored(&jobs, SIGNED).await.sign_offs.is_empty());
}

/// THE GATEWAY PATH KEEPS WORKING: a ticket the gateway signed, for
/// this step's current shape, stamps Presence with its nonce on the
/// sign-off door and completes the step on the completion door.
#[tokio::test]
async fn a_genuine_ticket_still_stamps_presence_and_completes() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;

    let t = ticket(SIGNED, GATEWAY_KEY, now_epoch() + 60);
    let (status, body) = sign_off(&app, Some(&t)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let stamp = stored(&jobs, SIGNED)
        .await
        .sign_offs
        .first()
        .cloned()
        .expect("a stamp was appended");
    assert_eq!(stamp.assurance, Assurance::Presence);
    assert_eq!(stamp.presence_nonce.as_deref(), Some("ceremony-nonce-7"));

    let t = ticket(GUARDED, GATEWAY_KEY, now_epoch() + 60);
    let (status, body) = complete(&app, GUARDED, Some(&t)).await;
    assert!(status.is_success(), "{status}: {body}");
    assert_eq!(stored(&jobs, GUARDED).await.status, StepStatus::Completed);
}

/// A genuine ticket still binds to the step it was minted for and to
/// the person who performed the ceremony: signed is necessary, not
/// sufficient.
#[tokio::test]
async fn a_genuine_ticket_for_another_step_or_person_is_not_presence() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;

    // Minted for GUARDED, presented at SIGNED.
    let t = ticket(GUARDED, GATEWAY_KEY, now_epoch() + 60);
    let (status, body) = sign_off(&app, Some(&t)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");

    // Minted for someone else, presented by emp-david.
    let (step_id, shape_hash) = binding(SIGNED);
    let t = PresenceTicket {
        i: "emp-someone-else".into(),
        s: step_id,
        h: shape_hash,
        n: "n".into(),
        e: now_epoch() + 60,
    }
    .encode(GATEWAY_KEY)
    .expect("a ticket signs");
    let (status, body) = sign_off(&app, Some(&t)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(stored(&jobs, SIGNED).await.sign_offs.is_empty());
}
