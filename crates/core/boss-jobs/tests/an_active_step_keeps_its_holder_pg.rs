//! Postgres half of `an_active_step_keeps_its_holder` (backlog 0f42efa0).
//!
//! The rule is the handler's, judged before any write, so the adapter
//! should not matter — which is the claim this file exists to check
//! rather than assume. Over `PgJobs`, through the same HTTP door: the
//! claim is the Pg compare-and-set, the bare clear and the install that
//! followed it (the review's probed pair, both 204 on in-memory before
//! the fix) are each refused and the row still names the claimant, and
//! the release in one body still frees the step for the next claim.

use std::sync::Arc;

use axum::http::StatusCode;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{JobsRepository, PgJobs};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::NaiveDate;
use uuid::Uuid;

const JOB: &str = "00000000-0000-4000-8000-00000000c0a2";
const STEP: &str = "00000000-0000-4000-8000-00000000c0b2";
const CLAIMANT: &str = "emp-claimant";

async fn app(db: &TestDb) -> (axum::Router, Arc<PgJobs>) {
    let jobs = Arc::new(PgJobs::new(db.pool.clone()));
    let job_id = JobId::from_uuid(Uuid::parse_str(JOB).unwrap());
    jobs.create_job(&Job {
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
    })
    .await
    .expect("packet lands");
    let mut step = Step::new(job_id, "task", "Build the change", 1);
    step.id = StepId::from_uuid(Uuid::parse_str(STEP).unwrap());
    step.spec_slug = Some("build".into());
    step.status = StepStatus::Ready;
    step.metadata = serde_json::json!({ "authority_role": "platform-admin" });
    jobs.add_step(&step).await.expect("step lands");

    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
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
    );
    let app = router(JobsApiState::minimal(
        jobs.clone(),
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    ));
    (app, jobs)
}

async fn claim(app: &axum::Router, who: &str) {
    TestRequest::post(format!("/api/jobs/{JOB}/steps/{STEP}/claim"))
        .as_user(who, "platform-admin")
        .json(&serde_json::json!({}))
        .send(app)
        .await
        .assert_status(StatusCode::OK);
}

async fn put(app: &axum::Router, body: serde_json::Value) -> boss_testing::TestResponse {
    TestRequest::put(format!("/api/jobs/{JOB}/steps/{STEP}"))
        .as_user("emp-op", "platform-admin")
        .json(&body)
        .send(app)
        .await
}

async fn stored(jobs: &PgJobs) -> Step {
    jobs.get_step(&StepId::from_uuid(Uuid::parse_str(STEP).unwrap()))
        .await
        .unwrap()
        .expect("step exists")
}

#[tokio::test(flavor = "multi_thread")]
async fn the_clear_then_install_pair_is_refused_over_postgres() {
    let db = TestDb::new().await;
    let (app, jobs) = app(&db).await;
    claim(&app, CLAIMANT).await;

    for body in [
        serde_json::json!({ "assignee_id": null }),
        serde_json::json!({ "assignee_id": "" }),
        serde_json::json!({ "assignee_id": "emp-other" }),
    ] {
        let resp = put(&app, body.clone()).await;
        resp.assert_status(StatusCode::CONFLICT);
        let v: serde_json::Value = resp.assert_json();
        assert_eq!(
            v,
            boss_jobs::active_holder::refusal_body(STEP, CLAIMANT),
            "{body}"
        );
        let after = stored(&jobs).await;
        assert_eq!(after.assignee_id.as_deref(), Some(CLAIMANT), "{body}");
        assert_eq!(after.status, StepStatus::Active, "{body}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_release_in_one_body_still_frees_the_step_over_postgres() {
    let db = TestDb::new().await;
    let (app, jobs) = app(&db).await;
    claim(&app, CLAIMANT).await;

    put(
        &app,
        serde_json::json!({ "status": "active", "assignee_id": null }),
    )
    .await
    .assert_status(StatusCode::CONFLICT);

    put(
        &app,
        serde_json::json!({ "status": "ready", "assignee_id": null }),
    )
    .await
    .assert_status(StatusCode::NO_CONTENT);
    let after = stored(&jobs).await;
    assert_eq!(after.status, StepStatus::Ready);
    assert_eq!(after.assignee_id, None);

    claim(&app, "emp-next").await;
    assert_eq!(stored(&jobs).await.assignee_id.as_deref(), Some("emp-next"));
}

/// A release spelled `""` stored `Some("")`, and the Pg claim CAS —
/// `assignee_id IS NULL OR` the claimant — then refused every claimant
/// 409 `holder: ""` (backlog 6ef4a36b, probed on in-memory; the Pg
/// predicate at postgres.rs has the same shape). Stored as NULL, the
/// next claim wins.
#[tokio::test(flavor = "multi_thread")]
async fn a_blank_release_is_stored_as_null_and_claimable_over_postgres() {
    let db = TestDb::new().await;
    let (app, jobs) = app(&db).await;
    claim(&app, CLAIMANT).await;

    put(
        &app,
        serde_json::json!({ "status": "ready", "assignee_id": "  " }),
    )
    .await
    .assert_status(StatusCode::NO_CONTENT);
    let after = stored(&jobs).await;
    assert_eq!(after.status, StepStatus::Ready);
    assert_eq!(after.assignee_id, None, "a blank release stores NULL");

    claim(&app, "emp-next").await;
    assert_eq!(stored(&jobs).await.assignee_id.as_deref(), Some("emp-next"));
}
