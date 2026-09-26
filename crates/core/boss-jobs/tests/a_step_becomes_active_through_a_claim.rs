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
//! THE REWORK (the adversarial review of this car, 2026-09-26):
//!
//! - H1: the declared executor was read off the step's LIVE metadata,
//!   and `audience` was a key any step writer could PATCH — so a caller
//!   with no `step-assign` wrote `{"audience":{"individual":<self>}}`
//!   (204) and then claimed for anyone (200). The executor is now read
//!   from the PROTOCOL the packet is pinned to (the Workflow row at
//!   `job.workflow_version`, the step by `spec_slug`), and `audience` is
//!   a protocol key neither write door lets a writer change.
//! - S1: the claim CAS judges status and holder, not the row, so a
//!   schedule PATCHed between the claim's read and its CAS landed with
//!   the hold still on the OLD window. A landed claim whose stored
//!   schedule moved hands its reservation back and holds the time as
//!   stored.
//! - S2: `claimed_for` is resolved to the one id the registries know
//!   (an agent login to its registered id) BEFORE anything is reserved
//!   or recorded, and a nominee no registry knows is refused.
//! - S3: every `individual` audience in the platform bundle names an
//!   automation, because that audience is claim-for authority.
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
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::agent_budget::BudgetDoor;
use boss_jobs::agent_runs::InMemoryAgentRuns;
use boss_jobs::agents::InMemoryAgents;
use boss_jobs::audience::Audience;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

/// A step write to land between a claim's read and its CAS: the
/// calendar's `reserve` is the one call the claim makes in that gap, so
/// the calendar lands it there — the S1 race, made deterministic.
type MoveOnReserve = (Arc<InMemoryJobs>, StepId, &'static str);

#[derive(Default)]
struct HeldCalendar {
    held: Mutex<Vec<Reservation>>,
    move_on_reserve: Mutex<Option<MoveOnReserve>>,
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
        let racing = self.move_on_reserve.lock().unwrap().take();
        if let Some((jobs, step_id, scheduled_at)) = racing {
            let mut step = jobs.get_step(&step_id).await.unwrap().unwrap();
            step.metadata["scheduled_at"] = serde_json::json!(scheduled_at);
            jobs.update_step(&step).await.unwrap();
        }
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
const AGENT: &str = "agent-claude";
const AGENT_LOGIN: &str = "claude@algedonic.dev";
const KIND: &str = "service-visit";
/// The step of [`KIND`] that declares no executor.
const VISIT: &str = "visit";
/// The step of [`KIND`] whose Workflow row names [`EXECUTOR`].
const RUN: &str = "run";

/// The protocol the packets here are pinned to: `visit` declares no
/// audience, `run` declares [`EXECUTOR`] as its `individual`.
fn spec() -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        KIND,
        "Service visit",
        "platform",
        vec!["custom".into()],
        vec![
            StepSpec {
                title: VISIT.into(),
                kind: "scheduling".into(),
                ready_when: "true".into(),
                title_template: "Visit".into(),
                ..Default::default()
            },
            StepSpec {
                title: RUN.into(),
                kind: "scheduling".into(),
                ready_when: "true".into(),
                title_template: "Run".into(),
                audience: Some(Audience::Individual(EXECUTOR.into())),
                ..Default::default()
            },
        ],
    )
}

/// The people roster: three active employees.
struct Roster;

#[async_trait]
impl RosterLookup for Roster {
    async fn active_holders(&self, _role: &str) -> Result<Vec<String>, String> {
        Ok(vec![])
    }
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok([TECH, OTHER, LEAD].contains(&id))
    }
}

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
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds.seed(spec()).expect("seed the kind");
    let state = JobsApiState {
        calendar: Some(calendar.clone() as Arc<dyn CalendarClient>),
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(Roster)),
        agent_budget: Some(Arc::new(BudgetDoor {
            agents: Arc::new(InMemoryAgents::new().with_agent(AGENT, [AGENT_LOGIN])),
            runs: Arc::new(InMemoryAgentRuns::new(vec![])),
        })),
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

/// A scheduled Ready `visit` step, held by `assignee` (or nobody), with
/// `extra` merged into its metadata.
async fn scheduled(
    jobs: &InMemoryJobs,
    assignee: Option<&str>,
    extra: serde_json::Value,
) -> (Job, Step) {
    scheduled_as(jobs, VISIT, assignee, extra).await
}

/// [`scheduled`], as the step of the pinned protocol named `slug`.
async fn scheduled_as(
    jobs: &InMemoryJobs,
    slug: &str,
    assignee: Option<&str>,
    extra: serde_json::Value,
) -> (Job, Step) {
    let job = Job {
        id: JobId::from_uuid(Uuid::new_v4()),
        kind: KIND.into(),
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
    step.spec_slug = Some(slug.to_string());
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
    // The PINNED protocol names the executor; the step's own metadata
    // says nothing about it.
    let (job, step) = scheduled_as(&jobs, RUN, None, serde_json::json!({})).await;

    let (status, body) = claim(&app, &job, &step, &user(EXECUTOR, "system"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(OTHER));

    // The same automation on a step its protocol does NOT declare it
    // for: refused.
    let (job2, step2) = scheduled(&jobs, None, serde_json::json!({})).await;
    let (status, body) = claim(&app, &job2, &step2, &user(EXECUTOR, "system"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
}

/// H1: an audience in the step's METADATA is not a declaration — only
/// the protocol the packet is pinned to declares an executor. Stored
/// straight into the row, as a writer that got round the doors would
/// leave it.
#[tokio::test]
async fn an_audience_in_step_metadata_makes_no_one_an_executor() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(
        &jobs,
        None,
        serde_json::json!({ "audience": { "individual": TECH } }),
    )
    .await;

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Ready);
    assert_eq!(stored.assignee_id, None);
    assert!(calendar.live().is_empty());
}

/// H1, in the shape of the review's probe: a caller with no
/// `step-assign` PATCHes itself in as the step's audience, then claims
/// the step for someone else. The PATCH is refused (`audience` is the
/// protocol's), and the claim is refused whatever the PATCH did.
#[tokio::test]
async fn a_writer_cannot_patch_itself_into_the_executor() {
    let (app, jobs, _calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/jobs/{}/steps/{}/metadata", job.id, step.id))
                .header("content-type", "application/json")
                .header(
                    "x-boss-user",
                    serde_json::to_string(&user(TECH, "technician")).unwrap(),
                )
                .body(Body::from(
                    serde_json::json!({ "audience": { "individual": TECH } }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = String::from_utf8_lossy(&bytes).into_owned();
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body.contains("audience"),
        "the refusal names the key: {body}"
    );
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert!(stored.metadata.get("audience").is_none());

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), Some(OTHER)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id, None);
}

/// S1: the claim CAS judges status and holder, not the row, so a
/// schedule PATCHed between the claim's read and its CAS lands under
/// it. The hold follows the time as STORED, and the reservation made
/// for the old time is handed back.
#[tokio::test]
async fn a_schedule_moved_under_a_claim_is_held_as_stored() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;
    *calendar.move_on_reserve.lock().unwrap() =
        Some((jobs.clone(), step.id, "2026-09-27T14:00:00Z"));

    let (status, body) = claim(&app, &job, &step, &user(TECH, "technician"), None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.metadata["scheduled_at"], "2026-09-27T14:00:00Z");
    let holds = calendar.live_for(&step.id.to_string());
    assert_eq!(
        holds.len(),
        1,
        "one hold, not the old one beside the new: {holds:?}"
    );
    let at = chrono::DateTime::parse_from_rfc3339("2026-09-27T14:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    assert_eq!(holds[0].window.start, at, "the hold is on the stored time");
}

/// S2: a nominee is resolved to the id the agents registry knows before
/// anything is reserved or recorded — an agent's login becomes its
/// registered id on the step, the event and the marker alike.
#[tokio::test]
async fn a_claim_for_an_agents_login_holds_its_registered_id() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;

    let (status, body) = claim(&app, &job, &step, &user(LEAD, "lead"), Some(AGENT_LOGIN)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.assignee_id.as_deref(), Some(AGENT));

    let (updated, assigned) = claim_events(&jobs, &step);
    assert_eq!(updated["assignee_id"], AGENT);
    assert_eq!(assigned["claimed_for"], AGENT);
    let holds = calendar.live_for(&step.id.to_string());
    assert_eq!(holds.len(), 1, "{holds:?}");
    assert_eq!(holds[0].subject, Subject::new("employee", AGENT));
}

/// S2: a nominee neither the roster nor the agents registry knows is
/// refused, with nothing reserved or stored.
#[tokio::test]
async fn a_claim_for_someone_no_registry_knows_is_refused() {
    let (app, jobs, calendar) = build_app();
    let (job, step) = scheduled(&jobs, None, serde_json::json!({})).await;

    let (status, body) = claim(&app, &job, &step, &user(LEAD, "lead"), Some("emp-nobody")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert!(
        body.contains("emp-nobody"),
        "the refusal names the nominee: {body}"
    );
    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert_eq!(stored.status, StepStatus::Ready);
    assert_eq!(stored.assignee_id, None);
    assert!(calendar.live().is_empty());
}

/// S3: an `individual` audience is claim-for authority — its executor
/// may start the step for anyone with no `step-assign` grant. So every
/// one the platform bundle declares names an automation, never a person
/// or an agent login, who would hold that authority by virtue of a
/// protocol row. Kept here, beside the rule it guards, rather than on
/// the tail of `platform_bundle.rs` (§9a's contended tail line).
#[test]
fn every_individual_executor_in_the_platform_bundle_is_an_automation() {
    let rows = boss_jobs::seed_loader::load_workflows(boss_jobs::registry::platform_bundle_path())
        .expect("the platform bundle parses");
    let declared: Vec<String> = rows
        .iter()
        .flat_map(|w| {
            w.steps.iter().filter_map(move |s| match &s.audience {
                Some(Audience::Individual(id)) => {
                    Some(format!("{} v{} step {}: {id}", w.kind, w.version, s.title))
                }
                _ => None,
            })
        })
        .collect();
    assert!(
        !declared.is_empty(),
        "a bundle with no individual audience proves nothing"
    );
    let offenders: Vec<&String> = declared
        .iter()
        .filter(|d| !d.contains(": automation:"))
        .collect();
    assert!(
        offenders.is_empty(),
        "an individual audience names someone who is not an automation: {offenders:?}"
    );
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
