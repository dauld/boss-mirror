//! `GET /api/jobs/queue-age` — the queue-age lens (packet 2a0b034e):
//! how long has every outstanding obligation waited?
//!
//! A LENS, NOT A FIELD. The wait instant lives one layer below the
//! domain types (`became_ready_at` / `updated_at` in Postgres, the
//! write-instant maps in the in-memory adapter), so the endpoint
//! returns its own row shape — `Job` and `Step` stay untouched, the
//! same trade `terminal-report` made. Contracts pinned here, against
//! the in-memory adapter through the real router:
//!
//! 1. **Membership**: `ready` / `active` steps of `open` packets,
//!    nothing else — pending and completed steps and steps on closed
//!    packets are not obligations.
//! 2. **`since` is the ready flip, and later writes don't move it** —
//!    the property `updated_at` cannot have, and the reason the
//!    dedicated stamp exists (2a77e5fc: annotating a packet bumped
//!    every age).
//! 3. **The fallback is honest**: a step that never passed through
//!    `ready` (born active) reports its write instant with
//!    `exact: false` — a lower bound, labelled as one.
//! 4. **Longest-waiting first** — the lens exists to sort queues by
//!    staleness.
//! 5. **The read is policy-scoped like every queue surface**: an
//!    unreadable caller gets an empty lens, an `OwnerIs`-scoped one
//!    sees only their packets.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, NaiveDate, Utc};
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

/// The frozen "now" every waiting figure is computed against.
const NOW: &str = "2026-09-02T12:00:00Z";

fn t(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().into()
}

// SERIALISE THE REAL TYPE, never a copy of its wire shape (7c3649e2).
fn user_header(id: &str, role: &str) -> String {
    serde_json::to_string(&User {
        id: id.to_string(),
        role: role.to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User always serialises")
}

fn app() -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    // The lens wears the job read gate: "operator" reads everything,
    // "owner-only" reads their own packets, "outsider" reads nothing.
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .allow("owner-only", Action::Read, Resource::job(), Scope::Self_)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        jobs: jobs.clone(),
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: None,
        plugin_registry: None,
        job_edges: None,
        stations: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::FixedClockClient::new(
            boss_clock_client::ClockNow {
                now: t(NOW),
                simulated: false,
                epoch_start: None,
                epoch_end: None,
                paused: false,
                restart_in_progress: false,
                warp_factor: None,
            },
        )),
    };
    (router(state), jobs)
}

fn packet(id: &str, owner: &str, status: JobStatus, title: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "backlog-item".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "bosspipeline"),
        title: title.into(),
        owner_id: owner.into(),
        status,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 8, 29).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        simulated: false,
    }
}

fn step(id: &str, job_id: &str, status: StepStatus, title: &str) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::parse_str(id).unwrap()),
        job_id: JobId::from_uuid(Uuid::parse_str(job_id).unwrap()),
        kind: "generic".into(),
        title: title.into(),
        spec_slug: Some(title.to_lowercase().replace(' ', "-")),
        assignee_id: None,
        status,
        sort_order: 0,
        blocked_by: vec![],
        sign_offs_required: Vec::new(),
        assurance_required: None,
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: None,
        metadata: serde_json::json!({}),
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

async fn get_lens(app: &axum::Router, id: &str, role: &str) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/jobs/queue-age")
                .header("x-boss-user", user_header(id, role))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

const JOB_OPEN: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const JOB_CLOSED: &str = "bbbbbbbb-0000-4000-8000-000000000002";
const JOB_OTHER_OWNER: &str = "cccccccc-0000-4000-8000-000000000003";

const STEP_STALE: &str = "11111111-0000-4000-8000-000000000001";
const STEP_FRESH: &str = "22222222-0000-4000-8000-000000000002";
const STEP_PENDING: &str = "33333333-0000-4000-8000-000000000003";
const STEP_DONE: &str = "44444444-0000-4000-8000-000000000004";
const STEP_ON_CLOSED: &str = "55555555-0000-4000-8000-000000000005";
const STEP_BORN_ACTIVE: &str = "66666666-0000-4000-8000-000000000006";
const STEP_OTHER_OWNER: &str = "77777777-0000-4000-8000-000000000007";

/// One fixture, exercised by every assertion below. Times are chosen
/// so each contract shows up as a distinct number.
async fn seed(jobs: &InMemoryJobs) {
    jobs.create_job_at(
        &packet(JOB_OPEN, "emp-david", JobStatus::Open, "the open packet"),
        t("2026-08-29T09:00:00Z"),
        &[],
    )
    .await
    .unwrap();
    let mut closed = packet(
        JOB_CLOSED,
        "emp-david",
        JobStatus::Closed,
        "a closed packet",
    );
    closed.closed_on = Some(NaiveDate::from_ymd_opt(2026, 8, 31).unwrap());
    jobs.create_job_at(&closed, t("2026-08-29T09:00:00Z"), &[])
        .await
        .unwrap();
    jobs.create_job_at(
        &packet(
            JOB_OTHER_OWNER,
            "emp-other",
            JobStatus::Open,
            "somebody else's packet",
        ),
        t("2026-08-29T09:00:00Z"),
        &[],
    )
    .await
    .unwrap();

    // STALE: born pending, promoted to ready on the 30th, ANNOTATED
    // on the 1st — `since` must stay the promotion instant.
    jobs.add_step_at(
        &step(STEP_STALE, JOB_OPEN, StepStatus::Pending, "triage"),
        t("2026-08-29T09:00:00Z"),
        &[],
    )
    .await
    .unwrap();
    let mut promoted = step(STEP_STALE, JOB_OPEN, StepStatus::Ready, "triage");
    promoted.assignee_id = Some("claude@algedonic.dev".into());
    jobs.update_step_at(&promoted, t("2026-08-30T10:00:00Z"), &[])
        .await
        .unwrap();
    let mut annotated = promoted.clone();
    annotated.metadata = serde_json::json!({"note": "an annotation must not reset the clock"});
    jobs.update_step_at(&annotated, t("2026-09-01T18:00:00Z"), &[])
        .await
        .unwrap();

    // FRESH: born ready this morning.
    jobs.add_step_at(
        &step(STEP_FRESH, JOB_OPEN, StepStatus::Ready, "build"),
        t("2026-09-02T09:00:00Z"),
        &[],
    )
    .await
    .unwrap();

    // Not obligations: a pending step, a completed step, a ready step
    // on a closed packet.
    jobs.add_step_at(
        &step(STEP_PENDING, JOB_OPEN, StepStatus::Pending, "later"),
        t("2026-08-29T09:00:00Z"),
        &[],
    )
    .await
    .unwrap();
    let mut done = step(STEP_DONE, JOB_OPEN, StepStatus::Ready, "already done");
    jobs.add_step_at(&done, t("2026-08-29T09:00:00Z"), &[])
        .await
        .unwrap();
    done.status = StepStatus::Completed;
    done.completed_on = Some(NaiveDate::from_ymd_opt(2026, 8, 30).unwrap());
    jobs.update_step_at(&done, t("2026-08-30T09:00:00Z"), &[])
        .await
        .unwrap();
    jobs.add_step_at(
        &step(STEP_ON_CLOSED, JOB_CLOSED, StepStatus::Ready, "stranded"),
        t("2026-08-29T09:00:00Z"),
        &[],
    )
    .await
    .unwrap();

    // BORN ACTIVE: never passed through ready — the lens can only
    // offer the write instant, and must say so.
    jobs.add_step_at(
        &step(STEP_BORN_ACTIVE, JOB_OPEN, StepStatus::Active, "rushed"),
        t("2026-09-01T12:00:00Z"),
        &[],
    )
    .await
    .unwrap();

    // A ready step on the other owner's packet — visible to the
    // operator, invisible to `owner-only` (emp-david).
    jobs.add_step_at(
        &step(
            STEP_OTHER_OWNER,
            JOB_OTHER_OWNER,
            StepStatus::Ready,
            "theirs",
        ),
        t("2026-08-31T09:00:00Z"),
        &[],
    )
    .await
    .unwrap();
}

fn row<'a>(body: &'a serde_json::Value, step_id: &str) -> &'a serde_json::Value {
    body["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["step_id"] == step_id)
        .unwrap_or_else(|| panic!("no row for step {step_id}"))
}

#[tokio::test]
async fn only_outstanding_obligations_are_rows() {
    let (app, jobs) = app();
    seed(&jobs).await;
    let body = get_lens(&app, "emp-ops", "operator").await;

    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["step_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids.len(),
        4,
        "ready/active steps of open packets only, got {ids:?}"
    );
    for excluded in [STEP_PENDING, STEP_DONE, STEP_ON_CLOSED] {
        assert!(!ids.contains(&excluded), "{excluded} is not an obligation");
    }
    assert_eq!(body["total"], 4);
}

#[tokio::test]
async fn since_is_the_ready_flip_and_annotation_does_not_move_it() {
    let (app, jobs) = app();
    seed(&jobs).await;
    let body = get_lens(&app, "emp-ops", "operator").await;

    let stale = row(&body, STEP_STALE);
    assert_eq!(stale["since"], "2026-08-30T10:00:00Z");
    assert_eq!(stale["exact"], true);
    // 2026-08-30T10:00 → 2026-09-02T12:00 = 3d2h = 266_400s. The
    // annotation on 09-01 must not have shrunk this to 18h.
    assert_eq!(stale["waiting_seconds"], 266_400);
    assert_eq!(stale["status"], "ready");
    assert_eq!(stale["assignee_id"], "claude@algedonic.dev");
    // A row names its packet — protocol + title, not just a UUID.
    assert_eq!(stale["job_kind"], "backlog-item");
    assert_eq!(stale["job_title"], "the open packet");

    let fresh = row(&body, STEP_FRESH);
    assert_eq!(fresh["since"], "2026-09-02T09:00:00Z");
    assert_eq!(fresh["waiting_seconds"], 3 * 3600);
}

#[tokio::test]
async fn a_step_that_never_went_ready_reports_a_labelled_lower_bound() {
    let (app, jobs) = app();
    seed(&jobs).await;
    let body = get_lens(&app, "emp-ops", "operator").await;

    let rushed = row(&body, STEP_BORN_ACTIVE);
    assert_eq!(rushed["exact"], false, "no ready flip was ever recorded");
    assert_eq!(rushed["since"], "2026-09-01T12:00:00Z");
    assert_eq!(rushed["status"], "active");
}

#[tokio::test]
async fn longest_waiting_first() {
    let (app, jobs) = app();
    seed(&jobs).await;
    let body = get_lens(&app, "emp-ops", "operator").await;

    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["step_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![STEP_STALE, STEP_OTHER_OWNER, STEP_BORN_ACTIVE, STEP_FRESH],
        "oldest obligation leads"
    );
}

#[tokio::test]
async fn the_lens_is_policy_scoped() {
    let (app, jobs) = app();
    seed(&jobs).await;

    // No job read at all → an empty lens, same shape as the station
    // surfaces, not a 403.
    let body = get_lens(&app, "emp-x", "outsider").await;
    assert_eq!(body["total"], 0);
    assert_eq!(body["data"], serde_json::json!([]));

    // OwnerIs scope → only emp-david's packets; the other owner's
    // ready step is not in the caller's world.
    let body = get_lens(&app, "emp-david", "owner-only").await;
    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["step_id"].as_str().unwrap())
        .collect();
    assert!(!ids.contains(&STEP_OTHER_OWNER));
    assert_eq!(ids.len(), 3);
}
