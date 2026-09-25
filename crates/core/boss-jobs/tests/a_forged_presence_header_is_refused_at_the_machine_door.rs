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

/// THE WEB'S SEQUENCE, on the step that has both doors (backlog
/// b568044a, round-4 review of car 7abc0154, 2026-09-25). A surface
/// stamps with the ceremony's ticket and then completes with a PUT. The
/// stamp is not the completion's assurance: the PUT is judged on its
/// OWN request, so a PUT carrying no ticket stays refused after a
/// genuine presence stamp — which is why both web surfaces stopped at
/// the stamp. The ticket the sign-off was granted on is the one the
/// completion carries: same step, same person, same shape (the stamp
/// writes no metadata, so the shape it bound still stands), and nothing
/// wider — the binding and the expiry are checked again here.
#[tokio::test]
async fn the_sign_offs_own_ticket_completes_the_step_and_the_stamp_alone_does_not() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;
    let t = ticket(SIGNED, GATEWAY_KEY, now_epoch() + 60);

    let (status, body) = sign_off(&app, Some(&t)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(
        stored(&jobs, SIGNED).await.sign_offs[0].assurance,
        Assurance::Presence
    );

    // The stamp alone: a presence stamp on the step does not lend its
    // assurance to a ticketless completion.
    let (status, body) = complete(&app, SIGNED, None).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "body: {body}");
    assert!(
        body.contains("\"required\":\"presence\""),
        "the refusal names what it wants, so a surface can answer it: {body}"
    );
    assert_eq!(stored(&jobs, SIGNED).await.status, StepStatus::Ready);

    // The same ticket, carried on the completion: it completes.
    let (status, body) = complete(&app, SIGNED, Some(&t)).await;
    assert!(status.is_success(), "{status}: {body}");
    assert_eq!(stored(&jobs, SIGNED).await.status, StepStatus::Completed);
}

// ---------------------------------------------------------------------
// THE CONTENT JUDGED IS THE CONTENT COMPLETED (backlog c0b56fd9,
// adversarial review of car 5b30ccf9, 2026-09-25).
//
// A genuine ticket binds step, person and the step's shape hash, and
// `update_step` judged it against the step BEFORE the PUT's overlay. So
// on a presence step with no sign-off roles a valid ticket completed the
// step while the same PUT replaced the plan it had been minted over:
// 204, the stored plan replaced, and nobody's passkey ever saw the bytes
// that were completed. (On a sign-off step the same body was caught only
// incidentally, by the stale-stamp 409.) Every test below is the real
// ticket the gateway would have issued — the forgery is not the point.
// ---------------------------------------------------------------------

async fn put(app: &Router, id: &str, presence: Option<&str>, body: &str) -> (StatusCode, String) {
    send(
        app,
        "PUT",
        format!("/api/jobs/{JOB}/steps/{id}"),
        presence,
        body,
    )
    .await
}

/// THE BUG: the ceremony is over plan A, the completion carries plan B.
#[tokio::test]
async fn a_presence_ticket_cannot_complete_a_step_while_the_same_put_rewrites_its_plan() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;
    let t = ticket(GUARDED, GATEWAY_KEY, now_epoch() + 60);

    let (status, body) = put(
        &app,
        GUARDED,
        Some(&t),
        r#"{"status":"completed","metadata":{"plan":"{\"verb\":\"wipe-the-system-disk\"}"}}"#,
    )
    .await;

    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a ticket minted over one plan must not complete another; body: {body}"
    );
    assert!(
        body.contains(&format!("/api/jobs/{JOB}/steps/{GUARDED}/metadata")),
        "the refusal names the merge door the content goes through first: {body}"
    );
    let after = stored(&jobs, GUARDED).await;
    assert_eq!(after.status, StepStatus::Ready);
    assert_eq!(
        after.metadata,
        step(GUARDED, None, false).metadata,
        "a refused completion writes nothing — the plan the ceremony saw stays"
    );

    // The same body is refused on the sign-off step too — by this rule,
    // not by the stale-stamp check it used to fall to by accident.
    let s = ticket(SIGNED, GATEWAY_KEY, now_epoch() + 60);
    let (status, body) = sign_off(&app, Some(&s)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (status, body) = put(
        &app,
        SIGNED,
        Some(&s),
        r#"{"status":"completed","metadata":{"plan":"{\"verb\":\"wipe-the-system-disk\"}"}}"#,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert!(body.contains("/metadata"), "{body}");
    assert_eq!(stored(&jobs, SIGNED).await.status, StepStatus::Ready);

    // Scope control: a step that asks only for a session is not judged
    // by this rule, so a completion PUT may still carry its metadata.
    let (status, body) = put(
        &app,
        ORDINARY,
        None,
        r#"{"status":"completed","metadata":{"plan":"rewritten at completion"}}"#,
    )
    .await;
    assert!(status.is_success(), "{status}: {body}");
}

/// The title is half the shape hash, and a PUT may write it: the same
/// hole through the other input.
#[tokio::test]
async fn a_presence_ticket_cannot_complete_a_step_while_the_same_put_retitles_it() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;
    let t = ticket(GUARDED, GATEWAY_KEY, now_epoch() + 60);

    let (status, body) = put(
        &app,
        GUARDED,
        Some(&t),
        r#"{"status":"completed","title":"Approve something nobody read"}"#,
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    let after = stored(&jobs, GUARDED).await;
    assert_eq!(after.status, StepStatus::Ready);
    assert_eq!(after.title, "Approve the plan");
}

/// What must keep working: the status-only completion car 5b30ccf9's
/// surfaces send with the ceremony's ticket, and a read-merge-write
/// body that sends every stored key back UNCHANGED — the rule is about
/// a change, not about the key being present.
#[tokio::test]
async fn a_completion_that_changes_nothing_it_was_judged_on_still_completes() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;

    // The sign-off surface: stamp with the ticket, then complete with
    // `{status:'completed'}` alone and the same ticket.
    let s = ticket(SIGNED, GATEWAY_KEY, now_epoch() + 60);
    let (status, body) = sign_off(&app, Some(&s)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (status, body) = complete(&app, SIGNED, Some(&s)).await;
    assert!(status.is_success(), "{status}: {body}");
    assert_eq!(stored(&jobs, SIGNED).await.status, StepStatus::Completed);

    // A read-merge-write writer re-sends the plan exactly as stored.
    let t = ticket(GUARDED, GATEWAY_KEY, now_epoch() + 60);
    let resent = serde_json::json!({
        "status": "completed",
        "title": "Approve the plan",
        "metadata": step(GUARDED, None, false).metadata,
    })
    .to_string();
    let (status, body) = put(&app, GUARDED, Some(&t), &resent).await;
    assert!(status.is_success(), "{status}: {body}");
    assert_eq!(stored(&jobs, GUARDED).await.status, StepStatus::Completed);
}

/// DECIDED: a presence-gated step is not skipped through the PUT, with
/// or without a ticket. A skip satisfies `steps.<slug>.done` exactly as
/// a completion does (ops-request's `execute` waits on
/// `steps.approve.done`), but it walks round the completion contract —
/// the required-at-done `plan` and the sign-off stamps are judged only
/// on `completed`. The ticket carries no verb (step, person, shape,
/// nonce, expiry), so no fresh ticket could say "skip" rather than
/// "approve": the only safe skip is none. Declining is not a skip — it
/// is leaving the step open, or cancelling the packet.
#[tokio::test]
async fn a_presence_ticket_cannot_skip_the_step_it_was_minted_for() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;

    let t = ticket(GUARDED, GATEWAY_KEY, now_epoch() + 60);
    let (status, body) = put(&app, GUARDED, Some(&t), r#"{"status":"skipped"}"#).await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(stored(&jobs, GUARDED).await.status, StepStatus::Ready);

    // The ticket that stamped the sign-off step cannot skip it either —
    // a skip would bypass the stamp the completion checks.
    let s = ticket(SIGNED, GATEWAY_KEY, now_epoch() + 60);
    let (status, body) = sign_off(&app, Some(&s)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (status, body) = put(&app, SIGNED, Some(&s), r#"{"status":"skipped"}"#).await;
    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert_eq!(stored(&jobs, SIGNED).await.status, StepStatus::Ready);

    // Scope control: an ordinary step still skips.
    let (status, body) = put(&app, ORDINARY, None, r#"{"status":"skipped"}"#).await;
    assert!(status.is_success(), "{status}: {body}");
}

/// DECIDED, AND PINNED: re-sending a ticket to the sign-off door writes
/// no second stamp and does not move the first one's time. The door's
/// idempotence is keyed on (role, current shape hash), so a re-send of
/// the same ticket — or a second ceremony over the same content — is
/// answered with the stamp already on the record. A new stamp needs new
/// CONTENT (the old one goes stale), never merely a new ticket.
#[tokio::test]
async fn re_sending_a_ticket_to_the_sign_off_door_does_not_re_stamp() {
    let (app, jobs) = seed(Some(GATEWAY_KEY)).await;

    let t = ticket(SIGNED, GATEWAY_KEY, now_epoch() + 60);
    let (status, body) = sign_off(&app, Some(&t)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let first = stored(&jobs, SIGNED).await.sign_offs;
    assert_eq!(first.len(), 1);

    // The same ticket again, then a second ceremony's ticket.
    let (status, body) = sign_off(&app, Some(&t)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let (step_id, shape_hash) = binding(SIGNED);
    let second = PresenceTicket {
        i: "emp-david".into(),
        s: step_id,
        h: shape_hash,
        n: "ceremony-nonce-8".into(),
        e: now_epoch() + 60,
    }
    .encode(GATEWAY_KEY)
    .expect("a ticket signs");
    let (status, body) = sign_off(&app, Some(&second)).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    let after = stored(&jobs, SIGNED).await.sign_offs;
    assert_eq!(
        after, first,
        "the first stamp stands: same time, same nonce"
    );
}
