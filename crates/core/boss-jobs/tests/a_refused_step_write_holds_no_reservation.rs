//! A refused step write holds no calendar reservation, and its re-send
//! is not refused against one.
//!
//! Review of car 88123ae0 (backlog 558396ff, 2026-09-25). The step PUT
//! reserves the assignee's time (`calendar_hook::apply_step_transition`)
//! BEFORE it writes the step, so a hard conflict can refuse the write
//! with nothing stored. Since 88123ae0 the write itself can also be
//! refused — 409 "step changed while this write was computed", nothing
//! written, send it again. But the reservation had already been made:
//! the refused write left it behind, and the re-send's own reservation
//! then collided with it and was refused "calendar conflict" — against
//! the step's own residue, forever.
//!
//! The first repair released every hold keyed on the step — before each
//! start, and again after each refused write — and the review of car
//! 983696b5 held it: a racing start that LANDED holds the same key, so a
//! refused racer erased the landed start's hold and left an Active step
//! holding nothing. Every compensation now undoes only what its own
//! attempt did: a refused write cancels its reservation by id, a start
//! that finds the step's own hold on its exact time takes it rather than
//! replacing it, and a skip releases only once its write has landed.
//!
//! The round-2 review held it again, on the same race in the reverse
//! order: the refused racer's reservation WAS the hold a landed start
//! had taken as its own, so handing it back by id still left the Active
//! step holding nothing. The row as stored now decides: a refused start
//! keeps its reservation when the step is Active over that time, and a
//! start that landed on a hold it did not place re-asserts it.
//!
//! The calendar here is a small stateful adapter with the service's one
//! rule that matters (a subject holds one hard reservation per
//! overlapping window), not the call-recording fake: the defect is in
//! what the calendar HOLDS after the refusal, which a fake that forgets
//! every reservation cannot show.

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
use boss_policy_client::{AccessTier, Action, Resource, Scope, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

/// Live (uncancelled) reservations, as the calendar service keeps them.
#[derive(Default)]
struct HeldCalendar {
    held: Mutex<Vec<Reservation>>,
    /// A cancel another writer lands just after the next conflict this
    /// calendar answers — the racer whose hold a start took as its own
    /// handing that hold back before the start's write lands.
    cancel_after_next_conflict: Mutex<Option<ReservationId>>,
}

impl HeldCalendar {
    fn cancel_after_next_conflict(&self, id: ReservationId) {
        *self.cancel_after_next_conflict.lock().unwrap() = Some(id);
    }

    fn live_for(&self, ref_id: &str) -> usize {
        self.live_ids(ref_id).len()
    }

    fn live_ids(&self, ref_id: &str) -> Vec<ReservationId> {
        self.held
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.reason_ref_id == ref_id && r.cancelled_at.is_none())
            .map(|r| r.id)
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
            if let Some(id) = self.cancel_after_next_conflict.lock().unwrap().take() {
                for r in held.iter_mut().filter(|r| r.id == id) {
                    r.cancelled_at = Some(chrono::Utc::now());
                }
            }
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
        let mut held = self.held.lock().unwrap();
        for r in held
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
        let mut held = self.held.lock().unwrap();
        let mut n = 0;
        for r in held.iter_mut().filter(|r| {
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

fn technician() -> User {
    User {
        id: "emp-tech".to_string(),
        role: "technician".to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("service".into()),
    }
}

fn build_app() -> (Router, Arc<InMemoryJobs>, Arc<HeldCalendar>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let calendar = Arc::new(HeldCalendar::default());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("technician", Action::Update, Resource::step(), Scope::All)
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

/// A service visit whose step is scheduled and assigned, ready to start.
async fn scheduled(jobs: &InMemoryJobs) -> (Job, Step) {
    let job = Job {
        id: JobId::from_uuid(Uuid::parse_str("00000000-0000-0000-0000-000000558396").unwrap()),
        kind: "service-visit".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "visit"),
        title: "A visit whose start races a note".into(),
        owner_id: "emp-tech".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 25).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({}),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    };
    jobs.create_job(&job).await.unwrap();
    let mut step = Step::new(job.id, "task", "Visit", 1);
    step.status = StepStatus::Ready;
    step.assignee_id = Some("emp-tech".into());
    step.metadata = serde_json::json!({
        "scheduled_at": "2026-09-26T10:00:00Z",
        "duration_minutes": 90,
    });
    jobs.add_step(&step).await.unwrap();
    (job, step)
}

async fn start(app: &Router, job: &Job, step: &Step) -> (StatusCode, String) {
    put_status(app, job, step, "active").await
}

async fn put_status(app: &Router, job: &Job, step: &Step, status: &str) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}/steps/{}", job.id, step.id))
                .header("content-type", "application/json")
                .header("x-boss-user", serde_json::to_string(&technician()).unwrap())
                .body(Body::from(format!(r#"{{"status":"{status}"}}"#)))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn a_start_refused_as_stale_holds_no_reservation_and_its_resend_starts() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs).await;
    let ref_id = step.id.to_string();

    // A note lands between the handler's read and its write.
    let mut note = serde_json::Map::new();
    note.insert("note".into(), serde_json::json!("gate code 4411"));
    jobs.merge_after_next_read(&step.id, note);
    let (status, body) = start(&app, &job, &step).await;
    assert_eq!(status, StatusCode::CONFLICT, "precondition: {body}");
    assert!(
        body.contains("step changed while this write was computed"),
        "precondition — refused as stale, not as a calendar conflict: {body}"
    );
    assert_eq!(
        calendar.live_for(&ref_id),
        0,
        "the refusal says nothing was written; the calendar must agree"
    );

    let (status, body) = start(&app, &job, &step).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the re-send starts the step, not a conflict with its own reservation: {body}"
    );
    assert_eq!(calendar.live_for(&ref_id), 1, "one step, one reservation");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Active);
    assert_eq!(stored.metadata["note"], "gate code 4411");
}

/// The hold a start of `step` places: its assignee, its window, keyed
/// on the step — what a start that landed left on the calendar.
async fn hold(calendar: &HeldCalendar, step: &Step) -> ReservationId {
    let start_at = chrono::DateTime::parse_from_rfc3339("2026-09-26T10:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    calendar
        .reserve(ReservationRequest {
            subject: Subject::new("employee", "emp-tech"),
            window: TimeWindow::new(start_at, start_at + chrono::Duration::minutes(90)).unwrap(),
            reason_kind: boss_core::calendar::reason::JOB_STEP.to_string(),
            reason_ref_id: step.id.to_string(),
            strength: boss_core::calendar::ReservationStrength::Hard,
            notes: None,
            created_by: "emp-tech".into(),
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn a_landed_starts_hold_survives_a_racing_start_refused_as_stale() {
    // The review of car 983696b5 (backlog 558396ff, 2026-09-25): HOLD.
    // Two starts of one step read it Ready. The first lands — the step
    // is Active and the assignee's time is held. The second is refused
    // as stale, as it should be; but on its way it released every hold
    // keyed on the step ("residue") and then, refused, released again —
    // so the step the first start left Active held nothing at all. A
    // compensation may only undo what its OWN attempt did.
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs).await;
    let ref_id = step.id.to_string();

    // The first start: its reservation made (the hook reserves before
    // the write), its write landing between the second one's read and
    // write.
    let landed_hold = hold(&calendar, &step).await;
    jobs.change_before_next_judged_write(&step.id, |row| {
        row.status = StepStatus::Active;
        row.metadata["started_from"] = serde_json::json!("the tablet");
    });

    let (status, body) = start(&app, &job, &step).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "the racing start is refused: {body}"
    );
    assert_eq!(
        calendar.live_ids(&ref_id),
        vec![landed_hold],
        "the landed start's hold is the step's hold, and it survives the refusal"
    );

    // Its re-send reads the step Active: nothing to reserve, nothing
    // to release, and the hold is still the one the landed start made.
    let (status, body) = start(&app, &job, &step).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(calendar.live_ids(&ref_id), vec![landed_hold]);
}

#[tokio::test]
async fn a_start_refused_by_the_start_it_let_through_leaves_that_start_its_hold() {
    // The round-2 review of car 983696b5 (backlog 558396ff): HOLD, the
    // race above in the REVERSE order. Two starts R and W read the step
    // Ready. R reserves first. W's hook finds R's hold on the same step,
    // person and window and takes it as the step's (AlreadyHeld), so W
    // places nothing — and W's write lands: the step is Active. R's
    // write is then refused as stale and hands back ITS reservation by
    // id — the only hold the step had. The Active step held no time.
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs).await;
    let ref_id = step.id.to_string();

    // This request is R. W lands between R's reservation and R's write,
    // and W's own hook placed nothing: it found R's hold.
    jobs.change_before_next_judged_write(&step.id, |row| {
        row.status = StepStatus::Active;
    });

    let (status, body) = start(&app, &job, &step).await;
    assert_eq!(status, StatusCode::CONFLICT, "R is refused: {body}");
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().status,
        StepStatus::Active,
        "precondition: W's start is what stands"
    );
    assert_eq!(
        calendar.live_for(&ref_id),
        1,
        "the Active step W left holds its assignee's time"
    );
}

#[tokio::test]
async fn a_start_that_took_a_racers_hold_holds_time_when_the_racer_hands_it_back() {
    // The same race with R's refusal landing EARLIER: W's hook takes R's
    // hold as the step's (AlreadyHeld), R is refused — by some other
    // write, while the step still read Ready — and hands its hold back,
    // and only then does W's write land. W owns no reservation id, so
    // nothing of W's was cancelled; but the hold it counted on is gone.
    // A start that landed on a hold it did not place re-asserts it.
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs).await;
    let ref_id = step.id.to_string();
    let racers = hold(&calendar, &step).await;
    calendar.cancel_after_next_conflict(racers);

    let (status, body) = start(&app, &job, &step).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "W lands: {body}");
    assert!(
        !calendar.live_ids(&ref_id).contains(&racers),
        "precondition: the racer handed its hold back"
    );
    assert_eq!(
        calendar.live_for(&ref_id),
        1,
        "the Active step W left holds its assignee's time"
    );
}

#[tokio::test]
async fn a_skip_refused_as_stale_leaves_the_active_steps_hold() {
    // The same shape on the other compensation: a skip cancelled the
    // step's hold BEFORE its write, so a skip refused as stale left an
    // Active step — its start landed — holding nothing (558396ff).
    let (app, jobs, calendar) = build_app();
    let (job, mut step) = scheduled(&jobs).await;
    step.status = StepStatus::Active;
    jobs.update_step(&step).await.unwrap();
    let held = hold(&calendar, &step).await;

    let mut note = serde_json::Map::new();
    note.insert("note".into(), serde_json::json!("parts arrived"));
    jobs.merge_after_next_read(&step.id, note);
    let (status, body) = put_status(&app, &job, &step, "skipped").await;
    assert_eq!(status, StatusCode::CONFLICT, "precondition: {body}");
    assert_eq!(
        calendar.live_ids(&step.id.to_string()),
        vec![held],
        "a skip that did not land releases nothing"
    );
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().status,
        StepStatus::Active
    );
}

#[tokio::test]
async fn a_skip_that_lands_releases_the_hold() {
    let (app, jobs, calendar) = build_app();
    let (job, mut step) = scheduled(&jobs).await;
    step.status = StepStatus::Active;
    jobs.update_step(&step).await.unwrap();
    hold(&calendar, &step).await;

    let (status, body) = put_status(&app, &job, &step, "skipped").await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(calendar.live_for(&step.id.to_string()), 0);
}

#[tokio::test]
async fn a_skip_computed_before_a_completion_keeps_the_completed_steps_hold() {
    // A completed step's hold is its record of past work (the hook's
    // `done_does_not_cancel`). A skip read Active, and the step then
    // completed under it: the skip is refused, and the completed step's
    // hold must outlive it — the release runs only for a skip that
    // landed.
    let (app, jobs, calendar) = build_app();
    let (job, mut step) = scheduled(&jobs).await;
    step.status = StepStatus::Active;
    jobs.update_step(&step).await.unwrap();
    let held = hold(&calendar, &step).await;

    jobs.change_before_next_judged_write(&step.id, |row| {
        row.status = StepStatus::Completed;
    });
    let (status, body) = put_status(&app, &job, &step, "skipped").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().status,
        StepStatus::Completed,
        "precondition: the completion is what stands"
    );
    assert_eq!(calendar.live_ids(&step.id.to_string()), vec![held]);
}

#[tokio::test]
async fn a_reservation_left_by_an_earlier_attempt_does_not_refuse_the_start() {
    // The residue can also outlive its request — a process that died
    // between the reservation and the write leaves one no compensation
    // ran for. A start that finds the step's own hold on exactly the
    // time it asks for takes that hold as its own rather than refusing
    // against it — and releases nothing, because nothing here can tell
    // residue from the hold of a start that landed (the review of car
    // 983696b5 found the release erasing the latter).
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs).await;
    let residue = hold(&calendar, &step).await;

    let (status, body) = start(&app, &job, &step).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        calendar.live_ids(&step.id.to_string()),
        vec![residue],
        "taken as the step's hold, not doubled"
    );
}

#[tokio::test]
async fn someone_elses_reservation_still_refuses_the_start() {
    // The residue rule is keyed on THIS step. Another step's hold on
    // the same person at the same time is a real conflict and stays one.
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs).await;
    let start_at = chrono::DateTime::parse_from_rfc3339("2026-09-26T10:30:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    calendar
        .reserve(ReservationRequest {
            subject: Subject::new("employee", "emp-tech"),
            window: TimeWindow::new(start_at, start_at + chrono::Duration::minutes(30)).unwrap(),
            reason_kind: boss_core::calendar::reason::JOB_STEP.to_string(),
            reason_ref_id: "another-step".into(),
            strength: boss_core::calendar::ReservationStrength::Hard,
            notes: None,
            created_by: "emp-tech".into(),
        })
        .await
        .unwrap();

    let (status, body) = start(&app, &job, &step).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("calendar conflict"), "{body}");
    assert_eq!(calendar.live_for("another-step"), 1, "untouched");
    assert_eq!(
        jobs.get_step(&step.id).await.unwrap().unwrap().status,
        StepStatus::Ready
    );
}
