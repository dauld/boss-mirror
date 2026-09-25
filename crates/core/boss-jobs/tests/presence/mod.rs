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
//! point a forgery like this at the live system of record — these
//! files are the only place it runs.
//!
//! LAYOUT (backlog 7f885247, 2026-09-25). This module is the fixture
//! every presence-door test shares; each test is a file of its own
//! beside it that says `mod presence;`. The tests used to live in one
//! file, and three cars in one day (5b30ccf9, 52ad60e6, 66de0e4b) each
//! appended to its tail — one needed a rerail, because a plain union
//! of two appends drops the closing brace of the test before them. A
//! new presence test is a new file, touching no shared line (CLAUDE.md
//! 9a, the contended-tail-line class). Add a helper here only when a
//! test genuinely needs one.

#![allow(dead_code, unused_imports)]

pub use std::sync::Arc;

pub use axum::Router;
pub use axum::body::Body;
pub use axum::http::{Request, StatusCode};
pub use boss_core::job::{
    Assurance, Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject,
};
pub use boss_core::port::EventBus;
pub use boss_core::presence::PresenceTicket;
pub use boss_core::publisher::DomainPublisher;
pub use boss_jobs::http::{JobsApiState, PresenceKey, router};
pub use boss_jobs::{InMemoryJobs, JobsRepository};
pub use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
pub use boss_testing::RecordingEventBus;
pub use chrono::NaiveDate;
pub use tower::ServiceExt;
pub use uuid::Uuid;

pub const JOB: &str = "00000000-0000-0000-0000-0000000fe001";
pub const GUARDED: &str = "00000000-0000-0000-0000-0000000fd001";
pub const SIGNED: &str = "00000000-0000-0000-0000-0000000fd002";
pub const ORDINARY: &str = "00000000-0000-0000-0000-0000000fd003";

/// The key the "gateway" in these tests signs with. A fixture value,
/// never the deployment's: the real key lives in the boss-session-key
/// Secret and appears in no repo, transcript or test.
pub const GATEWAY_KEY: &[u8] = b"presence-test-key-0123456789abcdef";

/// The person a forger would most like to be.
pub fn david() -> User {
    User {
        id: "emp-david".to_string(),
        role: "platform-admin".to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

pub fn policy() -> Arc<dyn PolicyClient> {
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

pub fn step(id: &str, assurance: Option<Assurance>, sign_off: bool) -> Step {
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
pub async fn seed(key: Option<&[u8]>) -> (Router, Arc<InMemoryJobs>) {
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
pub fn binding(id: &str) -> (String, String) {
    let s = step(id, None, false);
    (
        s.id.to_string(),
        boss_core::job::step_shape_hash(&s.title, &s.metadata),
    )
}

/// THE FORGERY, in the exact shape the gateway used to inject: plain
/// JSON, every field matching, no signature anywhere.
pub fn forged_json(id: &str) -> String {
    let (step_id, shape_hash) = binding(id);
    serde_json::json!({
        "employee_id": "emp-david",
        "step_id": step_id,
        "shape_hash": shape_hash,
        "nonce": "forged-nonce",
    })
    .to_string()
}

pub fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

/// A ticket as the gateway's `assert_finish` mints it, signed with `key`.
pub fn ticket(id: &str, key: &[u8], expires: u64) -> String {
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

pub async fn send(
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

pub async fn complete(app: &Router, id: &str, presence: Option<&str>) -> (StatusCode, String) {
    send(
        app,
        "PUT",
        format!("/api/jobs/{JOB}/steps/{id}"),
        presence,
        r#"{"status":"completed"}"#,
    )
    .await
}

pub async fn sign_off(app: &Router, presence: Option<&str>) -> (StatusCode, String) {
    send(
        app,
        "POST",
        format!("/api/jobs/{JOB}/steps/{SIGNED}/sign-offs"),
        presence,
        r#"{"role":"platform-admin"}"#,
    )
    .await
}

pub async fn stored(jobs: &InMemoryJobs, id: &str) -> Step {
    jobs.get_step(&StepId::from_uuid(Uuid::parse_str(id).unwrap()))
        .await
        .unwrap()
        .expect("the step is there")
}

pub async fn put(
    app: &Router,
    id: &str,
    presence: Option<&str>,
    body: &str,
) -> (StatusCode, String) {
    send(
        app,
        "PUT",
        format!("/api/jobs/{JOB}/steps/{id}"),
        presence,
        body,
    )
    .await
}
