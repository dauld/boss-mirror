//! A PUT does not replace the holder of a step someone is working.
//!
//! THE DEFECT (backlog 650ebd0c, from the review of car e341f7cd). The
//! dispatcher nominates a ready step with a bare `PUT {assignee_id}`.
//! When a claim lands between the handler's read and its write, the
//! row-version compare refuses the PUT 409 (STEP_CHANGED, 6ec22d71),
//! the dispatcher NAKs, and JetStream redelivers. The redelivered
//! handler judges the EVENT payload — still unassigned — so it PUTs
//! again; this time the handler reads the row fresh (active, held by
//! the claimant), the version compare passes because it read the row
//! itself, and the dispatcher's pick is written over the person or
//! agent who claimed the step about a second earlier. The record then
//! credits the wrong holder (provenance), and the claimant's own
//! completion later arrives on a step the log says someone else holds.
//!
//! THE BOUNDARY IS THE SERVER'S, for every caller: an ACTIVE step whose
//! stored holder is set refuses a PUT naming a DIFFERENT holder, 409,
//! naming the holder. The shape mirrors the terminal freeze's refusal
//! (`step_status` + `refused_fields: ["assignee_id"]`) so a caller that
//! already reads that one reads this one.
//!
//! WHAT STILL MOVES, and is pinned below so the refusal does not
//! overreach:
//! - a re-send of the stored holder (read-merge-write, redelivery) is
//!   not a change and passes;
//! - a READY step's nomination can still be moved — nobody has claimed
//!   it, so moving it takes nothing from anyone;
//! - a RELEASE — `{"status":"ready","assignee_id":null}`, the body the
//!   abandoned-step reclaim sends — still frees an active step. That is
//!   the explicit way to take an active step off its holder: free it,
//!   and the next holder claims it through the CAS, two writes, each on
//!   the record.
//!
//! AND THE WAY ROUND IT (backlog 0f42efa0, the review of this car): a
//! bare clear — `{"assignee_id":null}` or `""` with the step left Active
//! — answered 204 and left nobody holding it, so the next PUT installed
//! anyone. A clear passes only beside a status that leaves Active.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
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

const JOB: &str = "00000000-0000-0000-0000-00000000c0a1";
const STEP: &str = "00000000-0000-0000-0000-00000000c0b1";

const CLAIMANT: &str = "emp-claimant";
const DISPATCHER_PICK: &str = "agent-dispatcher-pick";

fn user(id: &str) -> User {
    User {
        id: id.to_string(),
        role: "platform-admin".to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn allow_update() -> Arc<dyn PolicyClient> {
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

async fn seed(status: StepStatus, assignee: Option<&str>) -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let state = JobsApiState::minimal(
        jobs.clone(),
        bus,
        publisher,
        allow_update(),
        Arc::new(boss_clock_client::WallClockClient),
    );
    let job_id = JobId::from_uuid(Uuid::parse_str(JOB).unwrap());
    let job = Job {
        id: job_id,
        kind: "backlog-item".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "bosspipeline"),
        title: "A step someone is working".into(),
        owner_id: "emp-owner".into(),
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
    let mut step = Step::new(job_id, "task", "Build the change", 1);
    step.id = StepId::from_uuid(Uuid::parse_str(STEP).unwrap());
    step.spec_slug = Some("build".into());
    step.status = status;
    step.assignee_id = assignee.map(str::to_string);
    step.metadata = serde_json::json!({ "authority_role": "platform-admin" });
    jobs.add_step(&step).await.unwrap();
    (router(state), jobs)
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    as_user: &str,
    body: &str,
) -> (StatusCode, String) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .header(
                    "x-boss-user",
                    serde_json::to_string(&user(as_user)).unwrap(),
                )
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn put_step(app: &Router, as_user: &str, body: &str) -> (StatusCode, String) {
    send(
        app,
        "PUT",
        &format!("/api/jobs/{JOB}/steps/{STEP}"),
        as_user,
        body,
    )
    .await
}

async fn claim(app: &Router, as_user: &str) {
    let (status, body) = send(
        app,
        "POST",
        &format!("/api/jobs/{JOB}/steps/{STEP}/claim"),
        as_user,
        "{}",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the claim itself must win: {body}");
}

async fn stored(jobs: &InMemoryJobs) -> Step {
    jobs.get_step(&StepId::from_uuid(Uuid::parse_str(STEP).unwrap()))
        .await
        .unwrap()
        .expect("step exists")
}

/// THE BUG, in the shape the redelivery has: a claim wins, then the
/// dispatcher's bare nomination PUT arrives and reads the fresh row.
#[tokio::test]
async fn a_nomination_after_a_claim_does_not_replace_the_claimant() {
    let (app, jobs) = seed(StepStatus::Ready, None).await;
    claim(&app, CLAIMANT).await;

    let (status, body) = put_step(
        &app,
        "svc-dispatcher",
        &format!(r#"{{"assignee_id":"{DISPATCHER_PICK}"}}"#),
    )
    .await;

    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a PUT naming a different holder on an ACTIVE, held step must be refused — \
         it replaced the claimant about a second after the claim (650ebd0c).\nbody: {body}"
    );
    let v: serde_json::Value = serde_json::from_str(&body).expect("the refusal is JSON");
    assert_eq!(
        v["step_status"], "active",
        "names the status that holds it: {body}"
    );
    assert_eq!(v["holder"], CLAIMANT, "names who holds it: {body}");
    assert!(
        v["refused_fields"]
            .as_array()
            .is_some_and(|f| f.iter().any(|x| x == "assignee_id")),
        "names the refused field, in the terminal freeze's shape: {body}"
    );
    // The wire body IS the builder's output, which the dispatcher's test
    // feeds its reader — one shape, read by both ends (CLAUDE.md §9a).
    assert_eq!(
        v,
        boss_jobs::active_holder::refusal_body(STEP, CLAIMANT),
        "the refusal on the wire is the one the dispatcher's test reads"
    );

    let after = stored(&jobs).await;
    assert_eq!(
        after.assignee_id.as_deref(),
        Some(CLAIMANT),
        "the claimant still holds it"
    );
    assert_eq!(after.status, StepStatus::Active);
}

/// A re-send of the stored holder is not a reassignment — a redelivery
/// or a read-merge-write that sends the whole step back must pass.
#[tokio::test]
async fn re_sending_the_holder_is_not_a_reassignment() {
    let (app, jobs) = seed(StepStatus::Ready, None).await;
    claim(&app, CLAIMANT).await;

    let (status, body) = put_step(
        &app,
        CLAIMANT,
        &format!(r#"{{"assignee_id":"{CLAIMANT}","notes":"working it"}}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(stored(&jobs).await.notes.as_deref(), Some("working it"));
}

/// Nobody has claimed a ready step, so moving its nomination takes
/// nothing from anyone — the refusal is scoped to an ACTIVE holder.
#[tokio::test]
async fn a_ready_steps_nomination_can_still_move() {
    let (app, jobs) = seed(StepStatus::Ready, Some("emp-first-pick")).await;

    let (status, body) = put_step(&app, "emp-op", r#"{"assignee_id":"emp-second-pick"}"#).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        stored(&jobs).await.assignee_id.as_deref(),
        Some("emp-second-pick")
    );
}

/// The release — the body the abandoned-step reclaim sends — still
/// frees an active step. Freeing, then claiming, is how an active step
/// changes hands.
#[tokio::test]
async fn a_release_still_frees_an_active_step() {
    let (app, jobs) = seed(StepStatus::Ready, None).await;
    claim(&app, CLAIMANT).await;

    let (status, body) = put_step(
        &app,
        "svc-reclaimer",
        r#"{"status":"ready","assignee_id":null}"#,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let after = stored(&jobs).await;
    assert_eq!(after.assignee_id, None);
    assert_eq!(after.status, StepStatus::Ready);

    // ...and the next holder takes it through the claim door.
    claim(&app, "emp-next").await;
    assert_eq!(stored(&jobs).await.assignee_id.as_deref(), Some("emp-next"));
}

/// THE WAY AROUND THE RULE ABOVE, in one write (backlog 0f42efa0, the
/// review of car fb3e9424): a bare clear left the step Active with no
/// holder, answered 204. An active step nobody holds is a step nobody
/// can be asked about, and the dispatcher's defensive net routes it.
/// `""` and a blank are the same clear — the guard reads them as no
/// holder, so they must be refused as one.
#[tokio::test]
async fn a_bare_clear_does_not_leave_an_active_step_without_its_holder() {
    for cleared in ["null", r#""""#, r#""   ""#] {
        let (app, jobs) = seed(StepStatus::Ready, None).await;
        claim(&app, CLAIMANT).await;

        let (status, body) =
            put_step(&app, "emp-op", &format!(r#"{{"assignee_id":{cleared}}}"#)).await;
        assert_eq!(
            status,
            StatusCode::CONFLICT,
            "assignee_id {cleared} on an ACTIVE, held step that stays Active must be \
             refused — it answered 204 and left nobody holding it (0f42efa0).\nbody: {body}"
        );
        let v: serde_json::Value = serde_json::from_str(&body).expect("the refusal is JSON");
        assert_eq!(
            v,
            boss_jobs::active_holder::refusal_body(STEP, CLAIMANT),
            "the same refusal as a replacement, so the same readers read it: {body}"
        );

        let after = stored(&jobs).await;
        assert_eq!(after.assignee_id.as_deref(), Some(CLAIMANT), "{cleared}");
        assert_eq!(after.status, StepStatus::Active, "{cleared}");
    }
}

/// The probed sequence whole: clear, then install. With the first write
/// refused the second meets a held step and is refused too, so nobody
/// ends up holding an active step they never claimed.
#[tokio::test]
async fn the_two_write_install_does_not_hand_over_an_active_step() {
    let (app, jobs) = seed(StepStatus::Ready, None).await;
    claim(&app, CLAIMANT).await;

    let (first, body) = put_step(&app, "emp-op", r#"{"assignee_id":null}"#).await;
    assert_eq!(first, StatusCode::CONFLICT, "the clear: {body}");
    let (second, body) = put_step(&app, "emp-op", r#"{"assignee_id":"emp-other"}"#).await;
    assert_eq!(second, StatusCode::CONFLICT, "the install: {body}");

    let after = stored(&jobs).await;
    assert_eq!(after.assignee_id.as_deref(), Some(CLAIMANT));
    assert_eq!(after.status, StepStatus::Active);
}

/// The status in the SAME body decides. Naming `active` is not moving
/// out of it, so a clear beside it is still refused; `""` beside
/// `ready` is a release exactly as `null` is; and a release names
/// nobody — handing the step to someone in the releasing write is the
/// one-write reassignment the claim door exists to refuse.
#[tokio::test]
async fn the_status_in_the_same_body_decides_whether_a_clear_is_a_release() {
    let (app, jobs) = seed(StepStatus::Ready, None).await;
    claim(&app, CLAIMANT).await;

    let (status, body) =
        put_step(&app, "emp-op", r#"{"status":"active","assignee_id":null}"#).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "active is not a release: {body}"
    );
    assert_eq!(stored(&jobs).await.assignee_id.as_deref(), Some(CLAIMANT));

    let (status, body) = put_step(
        &app,
        "emp-op",
        r#"{"status":"ready","assignee_id":"emp-other"}"#,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a release that names a holder: {body}"
    );
    assert_eq!(stored(&jobs).await.assignee_id.as_deref(), Some(CLAIMANT));

    let (status, body) = put_step(&app, "emp-op", r#"{"status":"ready","assignee_id":""}"#).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "\"\" releases like null: {body}"
    );
    let after = stored(&jobs).await;
    assert_eq!(after.status, StepStatus::Ready);
    // NULL, not `Some("")` (backlog 6ef4a36b, the review of car
    // 781b9209): this assertion accepted a blank, and a stored blank
    // is a holder to both claim CASes, which admit only NULL or the
    // claimant — the release answered 204 and stranded the step.
    assert_eq!(after.assignee_id, None, "a blank release stores nobody");
}

/// THE STRANDED STEP (backlog 6ef4a36b, the review of car 781b9209). A
/// release spelled with `""` — or a blank — answered 204 and stored
/// `Some("")`, and every claim after it answered 409 `holder: ""`:
/// both claim CASes admit a step whose holder is NULL or the claimant,
/// and a blank is neither. So nobody could ever take the step. A blank
/// holder is stored as NULL on every write, and the step is claimable.
#[tokio::test]
async fn a_blank_release_leaves_a_step_the_next_holder_can_claim() {
    for blank in [r#""""#, r#""   ""#] {
        let (app, jobs) = seed(StepStatus::Ready, None).await;
        claim(&app, CLAIMANT).await;

        let (status, body) = put_step(
            &app,
            "emp-op",
            &format!(r#"{{"status":"ready","assignee_id":{blank}}}"#),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{blank}: {body}");
        assert_eq!(
            stored(&jobs).await.assignee_id,
            None,
            "{blank} is stored as nobody"
        );

        claim(&app, "emp-next").await;
        let after = stored(&jobs).await;
        assert_eq!(after.assignee_id.as_deref(), Some("emp-next"), "{blank}");
        assert_eq!(after.status, StepStatus::Active, "{blank}");
    }
}

/// And a blank NOMINATION of a ready step is nobody too — the same
/// normalisation, off the release path: `{"assignee_id":""}` on a ready
/// step used to store `Some("")`, which left it unclaimable exactly as
/// the blank release did.
#[tokio::test]
async fn a_blank_nomination_is_stored_as_nobody() {
    let (app, jobs) = seed(StepStatus::Ready, Some("emp-first-pick")).await;

    let (status, body) = put_step(&app, "emp-op", r#"{"assignee_id":" "}"#).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(stored(&jobs).await.assignee_id, None);

    claim(&app, "emp-next").await;
    assert_eq!(stored(&jobs).await.assignee_id.as_deref(), Some("emp-next"));
}

/// An active step that already has no holder (a row from before the
/// rule, or an unassigned `PUT {status: active}`) has nothing to keep:
/// a clear there changes nothing and is not refused.
#[tokio::test]
async fn an_unheld_active_step_is_not_judged() {
    let (app, jobs) = seed(StepStatus::Active, None).await;

    let (status, body) = put_step(&app, "emp-op", r#"{"assignee_id":null}"#).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(stored(&jobs).await.status, StepStatus::Active);
}

/// The holder completing its own step is untouched.
#[tokio::test]
async fn the_holder_still_completes_its_step() {
    let (app, jobs) = seed(StepStatus::Ready, None).await;
    claim(&app, CLAIMANT).await;

    let (status, body) = put_step(&app, CLAIMANT, r#"{"status":"completed"}"#).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let after = stored(&jobs).await;
    assert_eq!(after.status, StepStatus::Completed);
    assert_eq!(after.assignee_id.as_deref(), Some(CLAIMANT));
}
