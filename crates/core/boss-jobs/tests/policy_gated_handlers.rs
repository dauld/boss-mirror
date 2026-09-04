//! End-to-end proof that `boss-jobs` enforces `boss-policy` on write
//! endpoints and scope-filters reads. Uses `FakePolicyClient` so the
//! rules matrix is predictable.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::JobsRepository;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::step_registry::StepRegistry;
use boss_policy_client::{AccessTier, Action, Resource, Scope, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

fn service_tech(id: &str) -> User {
    User {
        id: id.to_string(),
        role: "service-tech".to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("service".into()),
    }
}

fn job_owned_by(id: &str, owner: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "field-service".into(),
        workflow_version: 1,
        subject: Subject::new("asset", "SYS-1"),
        title: "Repair".into(),
        owner_id: owner.to_string(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        due_on: None,
        closed_on: None,
        metadata: serde_json::Value::Null,
        tags: vec![],
        simulated: false,
    }
}

fn build_app(policy: Arc<dyn PolicyClient>) -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let step_registry = Arc::new(StepRegistry::v1());
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus,
        publisher,
        step_registry,
        policy,
        kind_registry: None,
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: std::sync::Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
    };
    (router(state), jobs)
}

fn user_header(u: &User) -> String {
    serde_json::to_string(u).unwrap()
}

#[tokio::test]
async fn service_tech_can_close_own_job() {
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("service-tech", Action::Close, Resource::job(), Scope::Self_)
            .allow("service-tech", Action::Read, Resource::job(), Scope::Self_)
            .build(),
    );
    let (app, jobs) = build_app(policy);

    let tech = service_tech("emp-42");
    let job = job_owned_by("00000000-0000-0000-0000-000000000001", &tech.id);
    jobs.create_job(&job).await.unwrap();

    let mut closed = job.clone();
    closed.status = JobStatus::Closed;
    closed.closed_on = Some(NaiveDate::from_ymd_opt(2026, 1, 15).unwrap());
    let body = serde_json::to_string(&closed).unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}", closed.id))
                .header("content-type", "application/json")
                .header("x-boss-user", user_header(&tech))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "tech should close own job"
    );
}

#[tokio::test]
async fn service_tech_cannot_close_someone_elses_job() {
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("service-tech", Action::Close, Resource::job(), Scope::Self_)
            .allow("service-tech", Action::Read, Resource::job(), Scope::Self_)
            .build(),
    );
    let (app, jobs) = build_app(policy);

    let tech = service_tech("emp-42");
    let other = "emp-99";
    let job = job_owned_by("00000000-0000-0000-0000-000000000002", other);
    jobs.create_job(&job).await.unwrap();

    let mut closed = job.clone();
    closed.status = JobStatus::Closed;
    closed.closed_on = Some(NaiveDate::from_ymd_opt(2026, 1, 15).unwrap());
    let body = serde_json::to_string(&closed).unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}", closed.id))
                .header("content-type", "application/json")
                .header("x-boss-user", user_header(&tech))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "tech should be blocked from closing someone else's job"
    );
}

#[tokio::test]
async fn service_tech_with_no_rule_gets_forbidden() {
    // FakePolicyClient with no seeded rules → every check denies.
    let policy: Arc<dyn PolicyClient> = Arc::new(FakePolicyClient::deny_all());
    let (app, jobs) = build_app(policy);

    let tech = service_tech("emp-42");
    let job = job_owned_by("00000000-0000-0000-0000-000000000003", &tech.id);
    jobs.create_job(&job).await.unwrap();

    let mut closed = job.clone();
    closed.status = JobStatus::Closed;
    let body = serde_json::to_string(&closed).unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}", closed.id))
                .header("content-type", "application/json")
                .header("x-boss-user", user_header(&tech))
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn list_jobs_applies_scope_filter() {
    // Service-tech has Read Scope::Self_ — should only see own jobs.
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("service-tech", Action::Read, Resource::job(), Scope::Self_)
            .build(),
    );
    let (app, jobs) = build_app(policy);

    let tech = service_tech("emp-42");
    let mine = job_owned_by("00000000-0000-0000-0000-000000000010", &tech.id);
    let theirs = job_owned_by("00000000-0000-0000-0000-000000000011", "emp-99");
    jobs.create_job(&mine).await.unwrap();
    jobs.create_job(&theirs).await.unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/jobs")
                .header("x-boss-user", user_header(&tech))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(
        body["total"], 1,
        "scope-filtered list should only show own job"
    );
    let ids: Vec<&str> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["00000000-0000-0000-0000-000000000010"]);
}

#[tokio::test]
async fn list_jobs_unrestricted_for_ceo() {
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let (app, jobs) = build_app(policy);

    let ceo = User {
        id: "emp-ceo".into(),
        role: "ceo".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    };

    jobs.create_job(&job_owned_by(
        "00000000-0000-0000-0000-000000000020",
        "emp-1",
    ))
    .await
    .unwrap();
    jobs.create_job(&job_owned_by(
        "00000000-0000-0000-0000-000000000021",
        "emp-2",
    ))
    .await
    .unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/jobs")
                .header("x-boss-user", user_header(&ceo))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(body["total"], 2);
}

#[tokio::test]
async fn list_jobs_filters_by_asset_id_query_param() {
    // Regression: the HTTP handler must honor the `asset_id` and
    // `subject_id` query params. A prior bug had `boss-jobs-client`
    // passing `subject_id` while the handler only read `asset_id`,
    // so device-insights counted every field-service job in the DB
    // (up to the fetch limit of 50) as "prior service" for every
    // device. Both aliases now route to the same filter.
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let (app, jobs) = build_app(policy);

    let mine = job_owned_by("00000000-0000-0000-0000-000000000001", "emp-1");
    let mut other = job_owned_by("00000000-0000-0000-0000-000000000002", "emp-1");
    other.subject = Subject::new("asset", "SYS-2");
    jobs.create_job(&mine).await.unwrap();
    jobs.create_job(&other).await.unwrap();

    let ceo = User {
        id: "emp-ceo".into(),
        role: "ceo".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    };

    // Using `asset_id` — returns the one matching job.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/jobs?subject_id=SYS-1")
                .header("x-boss-user", user_header(&ceo))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["total"], 1, "asset_id should narrow to the one match");

    // Using `subject_id` — same result (alias).
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/jobs?subject_id=SYS-1")
                .header("x-boss-user", user_header(&ceo))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["total"], 1, "subject_id should behave like asset_id");
}

// --- Step sign-off gating -------------------------------------------------

fn sign_off_step(job_id: JobId, authority_role: &str) -> Step {
    let mut s = Step::new(job_id, "sign-off", "Final approval", 0)
        .with_sign_offs_required(vec![authority_role.to_string()]);
    s.metadata = serde_json::json!({ "authority_role": authority_role });
    s
}

async fn post_sign_off(
    app: Router,
    user: &User,
    step: &Step,
    role: &str,
) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/jobs/{}/steps/{}/sign-offs",
                step.job_id, step.id,
            ))
            .header("content-type", "application/json")
            .header("x-boss-user", user_header(user))
            .body(Body::from(format!("{{\"role\":\"{role}\"}}")))
            .unwrap(),
    )
    .await
    .unwrap()
}

async fn put_step(app: Router, user: &User, step: &Step) -> axum::http::Response<Body> {
    let body = serde_json::to_string(step).unwrap();
    app.oneshot(
        Request::builder()
            .method("PUT")
            .uri(format!("/api/jobs/{}/steps/{}", step.job_id, step.id,))
            .header("content-type", "application/json")
            .header("x-boss-user", user_header(user))
            .body(Body::from(body))
            .unwrap(),
    )
    .await
    .unwrap()
}

// Sign-off authority is enforced THROUGH policy: stamping authorizes
// against the role-scoped resource `step-signoff:<role>`
// (architecture-decisions.md §Step types are property bundles).
// There is no bespoke role compare and no sim exemption — the
// policy rule set decides who may stamp, uniformly.
#[tokio::test]
async fn stamp_blocked_when_policy_denies_the_role_scoped_resource() {
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("service-tech", Action::Update, Resource::step(), Scope::All)
            .allow(
                "qa-lead",
                Action::SignOff,
                Resource::new("step-signoff:qa-lead"),
                Scope::All,
            )
            .build(),
    );
    let (app, jobs) = build_app(policy);

    let tech = service_tech("emp-42");
    let job = job_owned_by("00000000-0000-0000-0000-000000000030", &tech.id);
    jobs.create_job(&job).await.unwrap();
    let step = sign_off_step(job.id, "qa-lead");
    jobs.add_step(&step).await.unwrap();

    let resp = post_sign_off(app, &tech, &step, "qa-lead").await;
    assert_eq!(
        resp.status(),
        StatusCode::FORBIDDEN,
        "policy must deny a stamp from a role with no step-signoff:qa-lead rule"
    );
    let body_bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = std::str::from_utf8(&body_bytes).unwrap();
    assert!(
        body.contains("qa-lead") && body.contains("service-tech"),
        "deny reason should name both required and caller roles, got: {body}"
    );
}

#[tokio::test]
async fn stamp_then_complete_succeeds_and_unstamped_complete_409s() {
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("qa-lead", Action::Update, Resource::step(), Scope::All)
            .allow(
                "qa-lead",
                Action::SignOff,
                Resource::new("step-signoff:qa-lead"),
                Scope::All,
            )
            .build(),
    );
    let (app, jobs) = build_app(policy);

    let qa = User {
        id: "emp-qa".into(),
        role: "qa-lead".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("qa".into()),
    };

    let job = job_owned_by("00000000-0000-0000-0000-000000000031", "emp-1");
    jobs.create_job(&job).await.unwrap();
    let step = sign_off_step(job.id, "qa-lead");
    jobs.add_step(&step).await.unwrap();

    // Completing without the stamp must 409 with the missing role.
    let mut done = step.clone();
    done.status = StepStatus::Completed;
    let resp = put_step(app.clone(), &qa, &done).await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "completion before stamping must be refused"
    );

    // Stamp, then complete.
    let resp = post_sign_off(app.clone(), &qa, &step, "qa-lead").await;
    assert_eq!(resp.status(), StatusCode::OK, "qa-lead may stamp");
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["sign_offs"][0]["role"], "qa-lead");
    assert_eq!(body["sign_offs"][0]["authority_id"], "emp-qa");

    let mut signed: Step = serde_json::from_value(body).unwrap();
    signed.status = StepStatus::Completed;
    let resp = put_step(app, &qa, &signed).await;
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "stamped step completes"
    );
}

#[tokio::test]
async fn editing_a_stamped_step_invalidates_the_stamp() {
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("qa-lead", Action::Update, Resource::step(), Scope::All)
            .allow(
                "qa-lead",
                Action::SignOff,
                Resource::new("step-signoff:qa-lead"),
                Scope::All,
            )
            .build(),
    );
    let (app, jobs) = build_app(policy);
    let qa = User {
        id: "emp-qa".into(),
        role: "qa-lead".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("qa".into()),
    };
    let job = job_owned_by("00000000-0000-0000-0000-000000000032", "emp-1");
    jobs.create_job(&job).await.unwrap();
    let step = sign_off_step(job.id, "qa-lead");
    jobs.add_step(&step).await.unwrap();

    let resp = post_sign_off(app.clone(), &qa, &step, "qa-lead").await;
    assert_eq!(resp.status(), StatusCode::OK);
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let mut stamped: Step = serde_json::from_value(body).unwrap();

    // Change the completion-relevant shape, then try to complete:
    // the stamp attested the old shape, so completion must 409.
    stamped.metadata = serde_json::json!({
        "authority_role": "qa-lead",
        "edited_after_stamp": true,
    });
    stamped.status = StepStatus::Completed;
    let resp = put_step(app, &qa, &stamped).await;
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "a stale stamp (shape changed) must not satisfy completion"
    );
}

#[tokio::test]
async fn put_step_accepts_partial_body_with_just_status() {
    // PATCH semantics: the sim (and the frontend) want to flip a
    // single field without round-tripping the whole Step. Sending
    // `{"status":"completed"}` must succeed and leave title + other
    // fields intact.
    let policy: Arc<dyn PolicyClient> = Arc::new(
        // The step PUT first clears a coarse (Update, step) gate before
        // the mechanics under test run; grant it to the caller's role.
        FakePolicyClient::builder()
            .allow("service-tech", Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    let (app, jobs) = build_app(policy);

    let tech = service_tech("emp-1");
    let job = job_owned_by("00000000-0000-0000-0000-000000000040", &tech.id);
    jobs.create_job(&job).await.unwrap();

    let step = Step::new(job.id, "generic", "Replace filter", 0);
    let step_id = step.id;
    jobs.add_step(&step).await.unwrap();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}/steps/{}", job.id, step_id))
                .header("content-type", "application/json")
                .header("x-boss-user", user_header(&tech))
                .body(Body::from(r#"{"status":"completed"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "partial PUT with just {{status}} must succeed"
    );

    let after = jobs.get_step(&step_id).await.unwrap().unwrap();
    assert_eq!(
        after.status,
        StepStatus::Completed,
        "status flipped to done"
    );
    assert_eq!(
        after.title, "Replace filter",
        "title preserved from original step — partial body must not erase it"
    );
}

#[tokio::test]
async fn put_step_done_rejects_unresolved_blockers() {
    // Invariant I-4: a step with open blockers cannot flip to `done`.
    // The HTTP handler calls `resolve_blockers` and returns 409 when
    // any blocker is not in a terminal state. Without this gate a
    // caller with step-write permission could fire a terminal
    // transition regardless of upstream dependencies, violating the
    // human-powered-state-machine framing's "preconditions enforced"
    // rule.
    let policy: Arc<dyn PolicyClient> = Arc::new(
        // The step PUT first clears a coarse (Update, step) gate before
        // the mechanics under test run; grant it to the caller's role.
        FakePolicyClient::builder()
            .allow("service-tech", Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    let (app, jobs) = build_app(policy);

    let tech = service_tech("emp-blocker");
    let job = job_owned_by("00000000-0000-0000-0000-000000000050", &tech.id);
    jobs.create_job(&job).await.unwrap();

    let blocker = Step::new(job.id, "generic", "Order part", 0);
    let blocker_id = blocker.id;
    jobs.add_step(&blocker).await.unwrap();

    let gated = Step::new(job.id, "generic", "Install part", 1).with_blocked_by(vec![blocker_id]);
    let gated_id = gated.id;
    jobs.add_step(&gated).await.unwrap();

    // Attempt to mark the gated step done while the blocker is still
    // pending. Must fail with 409.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}/steps/{}", job.id, gated_id))
                .header("content-type", "application/json")
                .header("x-boss-user", user_header(&tech))
                .body(Body::from(r#"{"status":"completed"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CONFLICT,
        "unresolved blocker must block status=done"
    );
    let body: serde_json::Value =
        serde_json::from_slice(&resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["error"], "step has unresolved blockers");

    // State unchanged.
    let after = jobs.get_step(&gated_id).await.unwrap().unwrap();
    assert_eq!(
        after.status,
        StepStatus::Pending,
        "gated step must still be Pending after a rejected PUT",
    );

    // Now close the blocker and re-try — should succeed.
    let mut done_blocker = jobs.get_step(&blocker_id).await.unwrap().unwrap();
    done_blocker.status = StepStatus::Completed;
    jobs.update_step(&done_blocker).await.unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}/steps/{}", job.id, gated_id))
                .header("content-type", "application/json")
                .header("x-boss-user", user_header(&tech))
                .body(Body::from(r#"{"status":"completed"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "with blocker resolved, flipping done must succeed"
    );
}

#[tokio::test]
async fn auto_close_stamps_step_completed_on_when_supplied() {
    // When the last step on a Job flips to done, the auto-transition
    // closes the Job. closed_on should anchor on the step's
    // completed_on (which carries the sim-day in sim runs) rather
    // than wall-clock NOW(). Mirrors the contract the step-completion
    // → invoice flow already enforces.
    let policy: Arc<dyn PolicyClient> = Arc::new(
        // The step PUT first clears a coarse (Update, step) gate before
        // the mechanics under test run; grant it to the caller's role.
        FakePolicyClient::builder()
            .allow("service-tech", Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    let (app, jobs) = build_app(policy);

    let tech = service_tech("emp-auto-close");
    let job = job_owned_by("00000000-0000-0000-0000-000000000060", &tech.id);
    jobs.create_job(&job).await.unwrap();

    // One step. When it flips to done, the Job must auto-close.
    let only = Step::new(job.id, "generic", "Do the thing", 0);
    let only_id = only.id;
    jobs.add_step(&only).await.unwrap();

    let sim_day = NaiveDate::from_ymd_opt(2027, 3, 14).unwrap();
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}/steps/{}", job.id, only_id))
                .header("content-type", "application/json")
                .header("x-boss-user", user_header(&tech))
                .body(Body::from(
                    serde_json::json!({
                        "status":"completed",
                        "completed_on": sim_day.to_string(),
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let after = jobs.get_job(&job.id).await.unwrap().expect("job exists");
    assert_eq!(after.status, JobStatus::Closed);
    assert_eq!(
        after.closed_on,
        Some(sim_day),
        "closed_on must anchor on the closing step's completed_on"
    );
}

#[tokio::test]
async fn put_step_active_allowed_with_open_blockers() {
    // The gate fires only at `done`. Moving to `active` (or any
    // non-terminal status) is always fine; a tech may stage work
    // while waiting on a sign-off upstream.
    let policy: Arc<dyn PolicyClient> = Arc::new(
        // The step PUT first clears a coarse (Update, step) gate before
        // the mechanics under test run; grant it to the caller's role.
        FakePolicyClient::builder()
            .allow("service-tech", Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    let (app, jobs) = build_app(policy);

    let tech = service_tech("emp-prep");
    let job = job_owned_by("00000000-0000-0000-0000-000000000052", &tech.id);
    jobs.create_job(&job).await.unwrap();

    let blocker = Step::new(job.id, "generic", "Approve", 0);
    let blocker_id = blocker.id;
    jobs.add_step(&blocker).await.unwrap();

    let gated = Step::new(job.id, "generic", "Prep parts", 1).with_blocked_by(vec![blocker_id]);
    let gated_id = gated.id;
    jobs.add_step(&gated).await.unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}/steps/{}", job.id, gated_id))
                .header("content-type", "application/json")
                .header("x-boss-user", user_header(&tech))
                .body(Body::from(r#"{"status":"active"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "active transition must be allowed even with open blockers"
    );
}

// --- Assurance: how strong a stamp had to be ------------------------------
//
// A stamp already recorded WHO attested and WHAT they attested
// (`shape_hash`). It never recorded how hard it was to produce, so
// "David clicked approve" and "David logged in this morning and
// something clicked approve" were the same fact.
//
// David, 2026-08-16, on the bypass question: "an assurance level with
// a bypass is a comment, not a control." So a stamp's assurance is
// what the server VERIFIED, never what the caller asked for — and
// until the WebAuthn ceremony exists, a step demanding presence simply
// cannot be stamped. These tests pin both halves: the default path is
// unchanged, and the raised path refuses rather than pretending.

fn qa_policy() -> Arc<dyn PolicyClient> {
    Arc::new(
        FakePolicyClient::builder()
            .allow("qa-lead", Action::Update, Resource::step(), Scope::All)
            .allow(
                "qa-lead",
                Action::SignOff,
                Resource::new("step-signoff:qa-lead"),
                Scope::All,
            )
            .build(),
    )
}

fn qa_lead(id: &str) -> User {
    let mut u = service_tech(id);
    u.role = "qa-lead".to_string();
    u
}

/// The default is unchanged, which is what makes this safe to ship.
///
/// Every existing step leaves `assurance_required` unset and every
/// StepType floors at `Session`, so the gate is inert until a protocol
/// raises it. A stamp records `session` — the truth about how it was
/// produced — rather than leaving the field meaningless.
#[tokio::test]
async fn an_ordinary_step_still_stamps_and_records_session_assurance() {
    let (app, jobs) = build_app(qa_policy());
    let user = qa_lead("emp-90");
    let job = job_owned_by("00000000-0000-0000-0000-000000000090", &user.id);
    jobs.create_job(&job).await.unwrap();
    let step = sign_off_step(job.id, "qa-lead");
    jobs.add_step(&step).await.unwrap();

    let resp = post_sign_off(app, &user, &step, "qa-lead").await;
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "the default path must not change"
    );

    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    let stamp = stored.sign_offs.first().expect("a stamp was appended");
    assert_eq!(
        stamp.assurance,
        boss_core::job::Assurance::Session,
        "a session-authenticated stamp must say so, not claim more"
    );
}

/// A step that demands presence REFUSES a plain session request.
///
/// This is the test that would catch a bypass being added later: if
/// someone makes the endpoint honour a caller-supplied assurance (the
/// body, a query param — anything other than the gateway-vouched
/// `x-boss-presence` header), this starts returning 200 and the
/// control becomes decoration.
#[tokio::test]
async fn a_step_requiring_presence_refuses_without_a_ceremony_ticket() {
    let (app, jobs) = build_app(qa_policy());
    let user = qa_lead("emp-91");
    let job = job_owned_by("00000000-0000-0000-0000-000000000091", &user.id);
    jobs.create_job(&job).await.unwrap();
    let mut step = sign_off_step(job.id, "qa-lead");
    step.assurance_required = Some(boss_core::job::Assurance::Presence);
    jobs.add_step(&step).await.unwrap();

    let resp = post_sign_off(app, &user, &step, "qa-lead").await;
    assert_eq!(
        resp.status(),
        StatusCode::UNPROCESSABLE_ENTITY,
        "a session request cannot satisfy presence, so the stamp must be refused"
    );
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["required"], "presence");
    assert_eq!(body["produced"], "session");

    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert!(
        stored.sign_offs.is_empty(),
        "a refused stamp must leave no trace on the step"
    );
}

/// The sign-off request with the gateway's presence header attached.
/// In production the edge strips every inbound `x-boss-*` header and
/// re-injects this one only after verifying a passkey ticket, so its
/// presence at this service means the gateway vouched. See
/// role_headers.rs; the machine token guards the service port itself.
async fn post_sign_off_with_presence(
    app: Router,
    user: &User,
    step: &Step,
    role: &str,
    presence: &serde_json::Value,
) -> axum::http::Response<Body> {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri(format!(
                "/api/jobs/{}/steps/{}/sign-offs",
                step.job_id, step.id,
            ))
            .header("content-type", "application/json")
            .header("x-boss-user", user_header(user))
            .header("x-boss-presence", presence.to_string())
            .body(Body::from(format!("{{\"role\":\"{role}\"}}")))
            .unwrap(),
    )
    .await
    .unwrap()
}

/// The ceremony's happy path: a gateway-vouched presence header whose
/// binding matches the step's CURRENT shape produces a Presence stamp
/// carrying the challenge nonce — the audit trail from stamp back to
/// the exact single-use ceremony that produced it.
#[tokio::test]
async fn a_matching_presence_header_produces_a_presence_stamp_with_its_nonce() {
    let (app, jobs) = build_app(qa_policy());
    let user = qa_lead("emp-92");
    let job = job_owned_by("00000000-0000-0000-0000-000000000092", &user.id);
    jobs.create_job(&job).await.unwrap();
    let mut step = sign_off_step(job.id, "qa-lead");
    step.assurance_required = Some(boss_core::job::Assurance::Presence);
    jobs.add_step(&step).await.unwrap();

    let shape = boss_core::job::step_shape_hash(&step.title, &step.metadata);
    let presence = serde_json::json!({
        "employee_id": user.id,
        "step_id": step.id.to_string(),
        "shape_hash": shape,
        "nonce": "ceremony-nonce-1",
    });
    let resp = post_sign_off_with_presence(app, &user, &step, "qa-lead", &presence).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    let stamp = stored.sign_offs.first().expect("a stamp was appended");
    assert_eq!(stamp.assurance, boss_core::job::Assurance::Presence);
    assert_eq!(
        stamp.presence_nonce.as_deref(),
        Some("ceremony-nonce-1"),
        "the stamp names the exact challenge that produced it"
    );
}

/// The binding is to CONTENT, not to the step id: a ticket minted
/// before an edit carries the old shape hash, and the stamp must not
/// survive an edit it never saw. The refusal names the staleness so
/// the actor knows to re-run the ceremony, not to hunt a bug.
#[tokio::test]
async fn a_stale_presence_header_downgrades_to_session_and_refuses() {
    let (app, jobs) = build_app(qa_policy());
    let user = qa_lead("emp-93");
    let job = job_owned_by("00000000-0000-0000-0000-000000000093", &user.id);
    jobs.create_job(&job).await.unwrap();
    let mut step = sign_off_step(job.id, "qa-lead");
    step.assurance_required = Some(boss_core::job::Assurance::Presence);
    jobs.add_step(&step).await.unwrap();

    let presence = serde_json::json!({
        "employee_id": user.id,
        "step_id": step.id.to_string(),
        "shape_hash": "a-hash-from-before-the-step-was-edited",
        "nonce": "ceremony-nonce-2",
    });
    let resp = post_sign_off_with_presence(app, &user, &step, "qa-lead", &presence).await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["produced"], "session");
    assert!(
        body["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("stale"),
        "the refusal must name staleness: {}",
        body["detail"]
    );

    let stored = jobs.get_step(&step.id).await.unwrap().unwrap();
    assert!(stored.sign_offs.is_empty());
}

/// Ordering is the contract: a step may raise, and `Presence` is
/// strictly stronger than `Session`.
#[test]
fn assurance_orders_presence_above_session() {
    use boss_core::job::Assurance;
    assert!(Assurance::Presence > Assurance::Session);
    assert_eq!(Assurance::default(), Assurance::Session);
    // max() is how a step's requirement combines with its kind's
    // floor; a Workflow may raise but never lower.
    assert_eq!(
        Assurance::Session.max(Assurance::Presence),
        Assurance::Presence
    );
}
