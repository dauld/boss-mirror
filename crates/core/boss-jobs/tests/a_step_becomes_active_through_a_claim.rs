//! The claim door starts a step the way the PUT did, and says who may
//! start one for someone else.
//!
//! Design 611fbffd ("A step becomes Active only through a claim",
//! answered by David 2026-09-26), clause (2) of backlog 6ef4a36b. Every
//! surface's Start and the sim's workforce take a Ready step to Active
//! with a step PUT naming the holder — so the record cannot tell "X took
//! this work" from "someone assigned X and started the clock". The claim
//! door is the one that records a claim, and two things kept the
//! surfaces off it:
//!
//! - THE CLAIM DOOR DID NOT RESERVE. `calendar_hook::apply_step_transition`
//!   ran only in `update_step`, so moving the Scheduling surface's Start to
//!   the claim door would have silently stopped reserving the holder's
//!   time. Both doors now start a step through one function
//!   (`start_hold`), and a claim the calendar refuses, or the CAS refuses
//!   after the calendar said yes, holds nothing.
//! - THE CLAIM DOOR COULD ONLY CLAIM FOR ITS CALLER. Q1 decided who may
//!   claim FOR someone else: the executor the step's own audience names
//!   (the automation a Workflow row declares), or a holder of the
//!   `step-assign` authority (platform-admin today). The claim records
//!   both: the event is signed by the caller, the step is held by the
//!   nominee, and the assignment marker names both (`claimed_by`,
//!   `claimed_for`). Everyone else claims only for themselves.
//!
//! The calendar here keeps what it holds (one hard hold per subject per
//! overlapping window), because the defect is in what the calendar HOLDS
//! after a claim, which a call-recording fake cannot show.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_calendar_client::{CalendarClient, CalendarClientError};
use boss_core::calendar::{
    BusinessCalendar, Reservation, ReservationId, ReservationRequest, TimeWindow,
};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

#[derive(Default)]
struct HeldCalendar {
    held: Mutex<Vec<Reservation>>,
}

impl HeldCalendar {
    fn live(&self) -> Vec<Reservation> {
        self.held
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.cancelled_at.is_none())
            .cloned()
            .collect()
    }

    fn live_for(&self, ref_id: &str) -> Vec<Reservation> {
        self.live()
            .into_iter()
            .filter(|r| r.reason_ref_id == ref_id)
            .collect()
    }
}

#[async_trait]
impl CalendarClient for HeldCalendar {
    async fn reserve(&self, req: ReservationRequest) -> Result<ReservationId, CalendarClientError> {
        let mut held = self.held.lock().unwrap();
        let clashing: Vec<Reservation> = held
            .iter()
            .filter(|r| {
                r.cancelled_at.is_none()
                    && r.subject == req.subject
                    && r.window.overlaps(&req.window)
            })
            .cloned()
            .collect();
        if !clashing.is_empty() {
            return Err(CalendarClientError::Conflict { existing: clashing });
        }
        let id = ReservationId::new();
        held.push(Reservation {
            id,
            subject: req.subject,
            window: req.window,
            reason_kind: req.reason_kind,
            reason_ref_id: req.reason_ref_id,
            strength: req.strength,
            notes: req.notes,
            created_by: req.created_by,
            created_at: chrono::Utc::now(),
            cancelled_at: None,
        });
        Ok(id)
    }

    async fn list(
        &self,
        _subject: &Subject,
        _window: TimeWindow,
    ) -> Result<Vec<Reservation>, CalendarClientError> {
        Ok(Vec::new())
    }

    async fn cancel(&self, id: ReservationId, _actor: &str) -> Result<(), CalendarClientError> {
        for r in self
            .held
            .lock()
            .unwrap()
            .iter_mut()
            .filter(|r| r.id == id && r.cancelled_at.is_none())
        {
            r.cancelled_at = Some(chrono::Utc::now());
        }
        Ok(())
    }

    async fn cancel_by_reason(
        &self,
        reason_kind: &str,
        reason_ref_id: &str,
        _actor: &str,
    ) -> Result<usize, CalendarClientError> {
        let mut n = 0;
        for r in self.held.lock().unwrap().iter_mut().filter(|r| {
            r.cancelled_at.is_none()
                && r.reason_kind == reason_kind
                && r.reason_ref_id == reason_ref_id
        }) {
            r.cancelled_at = Some(chrono::Utc::now());
            n += 1;
        }
        Ok(n)
    }

    async fn get_business_calendar(
        &self,
        _code: &str,
    ) -> Result<Option<BusinessCalendar>, CalendarClientError> {
        Ok(None)
    }
}

const TECH: &str = "emp-tech";
const OTHER: &str = "emp-other";
const LEAD: &str = "emp-lead";
const EXECUTOR: &str = "automation:boss-step";

fn user(id: &str, role: &str) -> User {
    User {
        id: id.to_string(),
        role: role.to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

/// Every role here may write steps; only `lead` holds `step-assign`.
fn build_app() -> (Router, Arc<InMemoryJobs>, Arc<HeldCalendar>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let calendar = Arc::new(HeldCalendar::default());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("technician", Action::Update, Resource::step(), Scope::All)
            .allow("lead", Action::Update, Resource::step(), Scope::All)
            .allow("lead", Action::Update, Resource::step_assign(), Scope::All)
            .allow("system", Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    let state = JobsApiState {
        calendar: Some(calendar.clone() as Arc<dyn CalendarClient>),
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    (router(state), jobs, calendar)
}

/// A scheduled Ready step, held by `assignee` (or nobody), with `extra`
/// merged into its metadata.
async fn scheduled(
    jobs: &InMemoryJobs,
    assignee: Option<&str>,
    extra: serde_json::Value,
) -> (Job, Step) {
    let job = Job {
        id: JobId::from_uuid(Uuid::new_v4()),
        kind: "service-visit".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "visit"),
        title: "A visit started through the claim door".into(),
        owner_id: TECH.into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 26).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    };
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "scheduling", "Visit", 1);
    step.status = StepStatus::Ready;
    step.assignee_id = assignee.map(str::to_string);
    let mut metadata = serde_json::json!({
        "scheduled_at": "2026-09-27T10:00:00Z",
        "duration_minutes": 90,
    });
    if let (Some(m), Some(e)) = (metadata.as_object_mut(), extra.as_object()) {
        m.extend(e.clone());
    }
    step.metadata = metadata;
    jobs.add_step(&step).await.unwrap();
    (job, step)
}

async fn claim(
    app: &Router,
    job: &Job,
    step: &Step,
    as_user: &User,
    claimed_for: Option<&str>,
) -> (StatusCode, String) {
    let query = claimed_for
        .map(|who| format!("?claimed_for={who}"))
        .unwrap_or_default();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!(
                    "/api/jobs/{}/steps/{}/claim{query}",
                    job.id, step.id
                ))
                .header("x-boss-user", serde_json::to_string(as_user).unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn the_claim_door_reserves_the_holders_time_as_the_put_did() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let holds = calendar.live_for(&step.id.to_string());
    assert_eq!(
        holds.len(),
        1,
        "a claimed scheduled step holds its holder's time, as a PUT start did"
    );
    assert_eq!(holds[0].subject, Subject::new("employee", TECH));
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Active);
    assert_eq!(stored.assignee_id.as_deref(), Some(TECH));
}

#[tokio::test]
async fn a_claim_the_calendar_refuses_is_not_made() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;
    // Someone else's booking already holds the technician's morning.
    let start_at = chrono::DateTime::parse_from_rfc3339("2026-09-27T09:30:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    calendar
        .reserve(ReservationRequest {
            subject: Subject::new("employee", TECH),
            window: TimeWindow::new(start_at, start_at + chrono::Duration::minutes(60)).unwrap(),
            reason_kind: boss_core::calendar::reason::JOB_STEP.to_string(),
            reason_ref_id: "another-step".into(),
            strength: boss_core::calendar::ReservationStrength::Hard,
            notes: None,
            created_by: "emp-dispatch".into(),
        })
        .await
        .unwrap();

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("calendar conflict"), "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Ready, "nothing was claimed");
    assert_eq!(stored.assignee_id, None);
}

#[tokio::test]
async fn a_claim_the_cas_refuses_hands_its_reservation_back() {
    let (app, jobs, calendar) = build_app();
    // Nominated to someone else: the CAS refuses the technician's claim
    // AFTER the calendar said yes to it.
    let (job, step) = scheduled(&jobs, Some(OTHER), serde_json::json!({})).await;

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), None).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("not claimable"), "{body}");
    assert!(
        calendar.live_for(&step.id.to_string()).is_empty(),
        "a refused claim says nothing was written; the calendar must agree: {:?}",
        calendar.live()
    );
}

#[tokio::test]
async fn a_claim_for_someone_else_is_refused_to_anyone_without_the_authority() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert!(
        body.contains("claimed_for"),
        "the refusal names the rule: {body}"
    );
    assert!(body.contains("step-assign"), "and the authority: {body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Ready);
    assert_eq!(stored.assignee_id, None);
    assert!(calendar.live().is_empty());
}

#[tokio::test]
async fn a_claim_for_yourself_by_name_is_an_ordinary_claim() {
    let (app, jobs, _calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), Some(TECH)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(TECH));
}

/// The STEP_UPDATED and assignment marker a claim recorded for `step`.
fn claim_events(jobs: &InMemoryJobs, step: &Step) -> (serde_json::Value, serde_json::Value) {
    let events = jobs.recorded_events();
    let sid = step.id.to_string();
    let updated = events
        .iter()
        .rev()
        .find(|e| e.kind == "jobs.step.updated" && e.payload["id"] == sid.as_str())
        .map(|e| e.payload.clone())
        .expect("the claim recorded its STEP_UPDATED");
    let assigned = events
        .iter()
        .rev()
        .find(|e| e.kind.starts_with("step.assigned.") && e.payload["step_id"] == sid.as_str())
        .map(|e| e.payload.clone())
        .expect("a claim for someone else records its assignment marker");
    (updated, assigned)
}

#[tokio::test]
async fn a_holder_of_step_assign_claims_for_someone_else_and_the_record_names_both() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;

    let (status, body) = claim(&app, &job, &step, &user(LEAD, "lead"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Active);
    assert_eq!(
        stored.assignee_id.as_deref(),
        Some(OTHER),
        "the nominee holds it"
    );

    let (updated, assigned) = claim_events(&jobs, &step);
    assert_eq!(updated["_actor"], LEAD, "the event is signed by the caller");
    assert_eq!(updated["assignee_id"], OTHER);
    assert_eq!(assigned["claimed_by"], LEAD);
    assert_eq!(assigned["claimed_for"], OTHER);

    let holds = calendar.live_for(&step.id.to_string());
    assert_eq!(holds.len(), 1, "the nominee's time is held");
    assert_eq!(holds[0].subject, Subject::new("employee", OTHER));
    assert_eq!(holds[0].created_by, LEAD);
}

#[tokio::test]
async fn the_executor_a_step_declares_claims_for_someone_else() {
    let (app, jobs, _calendar) = build_app();
    let (job, step) = scheduled(
        &jobs,
        None,
        serde_json::json!({ "audience": { "individual": EXECUTOR } }),
    )
    .await;

    let (status, body) = claim(&app, &job, &step, &user(EXECUTOR, "system"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(OTHER));

    // The same automation on a step that does NOT declare it: refused.
    let (job2, step2) = scheduled(&jobs, None, serde_json::json!({})).await;
    let (status, body) = claim(&app, &job2, &step2, &user(EXECUTOR, "system"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// BOTH DOORS START A STEP THROUGH ONE FUNCTION (design 611fbffd). The
/// reservation moved out of `update_step` so the claim could make it;
/// a later edit that inlined it back into one door would leave the other
/// starting steps with no hold, which is the defect this pins. Read from
/// the handler source, because "calls the same function" is a property
/// of the code, not of any one request.
#[test]
fn both_doors_start_a_step_through_one_function() {
    let src = include_str!("../src/http/steps.rs");
    let body_of = |name: &str| -> &str {
        let head = format!("pub(super) async fn {name}<");
        let start = src.find(&head).unwrap_or_else(|| panic!("no fn {name}"));
        let rest = &src[start + head.len()..];
        let end = rest.find("\npub(super) async fn ").unwrap_or(rest.len());
        &rest[..end]
    };
    for door in ["update_step", "claim_step"] {
        let body = body_of(door);
        for call in ["start_hold(", "settle_start_hold("] {
            assert!(body.contains(call), "{door} must call {call}");
        }
    }
    assert_eq!(
        src.matches("calendar_hook::apply_step_transition(").count(),
        1,
        "the reservation is made in one place, the shared start"
    );
}
