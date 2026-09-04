//! `PATCH /api/jobs/{id}/metadata` — the atomic top-level metadata
//! merge, and the race it retires.
//!
//! The 2026-08-21 UX audit's finding: with only the full-replacement
//! job PUT, every "set one metadata key" caller ran GET → spread → PUT
//! client-side (`dismissFromWatchlist`, the conductor's
//! `merge_job_metadata`). The race is over the ENVELOPE, not the field:
//! the board closes the packet (status closed + `metadata.outcome`
//! stamped) between the GET and the PUT, and the write resurrects it
//! open with the outcome erased — on the system of record. These tests
//! pin the merge semantics (add preserves, null removes), the envelope
//! immunity in exactly the audit's shape, the policy gate, and the
//! JOB_UPDATED event carrying the post-merge state.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::{AccessTier, Action, Resource, Scope, User};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use tower::ServiceExt;
use uuid::Uuid;

fn filer(id: &str) -> User {
    User {
        id: id.to_string(),
        role: "service-tech".to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: Some("service".into()),
    }
}

fn job_with_metadata(id: &str, owner: &str, metadata: serde_json::Value) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "user-feedback".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "/ux/jobs"),
        title: "The column picker forgets my choice".into(),
        owner_id: owner.to_string(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
        due_on: None,
        closed_on: None,
        metadata,
        tags: vec![],
        simulated: false,
    }
}

fn build_app(policy: Arc<dyn PolicyClient>) -> (Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus,
        publisher,
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: None,
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
    };
    (router(state), jobs)
}

fn allow_update() -> Arc<dyn PolicyClient> {
    Arc::new(
        FakePolicyClient::builder()
            .allow("service-tech", Action::Update, Resource::job(), Scope::All)
            .build(),
    )
}

async fn patch_metadata(
    app: &Router,
    user: &User,
    job_id: &str,
    body: &str,
) -> axum::http::Response<Body> {
    app.clone()
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(format!("/api/jobs/{job_id}/metadata"))
                .header("content-type", "application/json")
                .header("x-boss-user", serde_json::to_string(user).unwrap())
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn merge_adds_a_key_and_preserves_the_existing_ones() {
    let (app, jobs) = build_app(allow_update());
    let user = filer("emp-1");
    let job = job_with_metadata(
        "00000000-0000-0000-0000-000000000001",
        &user.id,
        serde_json::json!({ "route": "/ux/jobs", "submitted_by": "emp-1" }),
    );
    jobs.create_job(&job).await.unwrap();

    let resp = patch_metadata(
        &app,
        &user,
        "00000000-0000-0000-0000-000000000001",
        r#"{"watchlist_dismissed":"true"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let after = jobs.get_job(&job.id).await.unwrap().unwrap();
    assert_eq!(after.metadata["watchlist_dismissed"], "true");
    assert_eq!(
        after.metadata["route"], "/ux/jobs",
        "existing keys must survive the merge"
    );
    assert_eq!(after.metadata["submitted_by"], "emp-1");
}

#[tokio::test]
async fn a_null_value_removes_the_key_and_only_that_key() {
    // The conductor's overlay convention (`overlay_metadata`): null is
    // a removal, how a caller sheds a stale key instead of carrying ""
    // forever.
    let (app, jobs) = build_app(allow_update());
    let user = filer("emp-1");
    let job = job_with_metadata(
        "00000000-0000-0000-0000-000000000002",
        &user.id,
        serde_json::json!({ "route": "/ux/jobs", "watchlist_dismissed": "true" }),
    );
    jobs.create_job(&job).await.unwrap();

    let resp = patch_metadata(
        &app,
        &user,
        "00000000-0000-0000-0000-000000000002",
        r#"{"watchlist_dismissed":null}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let after = jobs.get_job(&job.id).await.unwrap().unwrap();
    assert!(
        after.metadata.get("watchlist_dismissed").is_none(),
        "null must remove the key: {}",
        after.metadata
    );
    assert_eq!(after.metadata["route"], "/ux/jobs");
}

#[tokio::test]
async fn a_job_whose_metadata_was_never_an_object_still_merges() {
    // Jobs born with `metadata: null` exist (the policy test fixtures
    // are the in-repo example). The merge folds that to `{}`.
    let (app, jobs) = build_app(allow_update());
    let user = filer("emp-1");
    let job = job_with_metadata(
        "00000000-0000-0000-0000-000000000003",
        &user.id,
        serde_json::Value::Null,
    );
    jobs.create_job(&job).await.unwrap();

    let resp = patch_metadata(
        &app,
        &user,
        "00000000-0000-0000-0000-000000000003",
        r#"{"watchlist_dismissed":"true"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let after = jobs.get_job(&job.id).await.unwrap().unwrap();
    assert_eq!(after.metadata["watchlist_dismissed"], "true");
}

/// THE AUDIT'S EXACT RACE SHAPE. The board closed the packet — status
/// closed, `metadata.outcome` stamped — and only then does the dismiss
/// land. Through the old GET → PUT it resurrected the packet open with
/// the outcome erased. Through the PATCH the envelope is untouchable:
/// status stays closed, the outcome stays present, and the one patched
/// key lands beside it.
#[tokio::test]
async fn patching_one_key_on_a_closed_packet_leaves_it_closed_with_its_outcome() {
    let (app, jobs) = build_app(allow_update());
    let user = filer("emp-1");
    let closed_on = NaiveDate::from_ymd_opt(2026, 8, 20).unwrap();
    let mut job = job_with_metadata(
        "00000000-0000-0000-0000-000000000004",
        &user.id,
        serde_json::json!({ "route": "/ux/jobs", "outcome": "completed" }),
    );
    job.status = JobStatus::Closed;
    job.closed_on = Some(closed_on);
    jobs.create_job(&job).await.unwrap();

    let resp = patch_metadata(
        &app,
        &user,
        "00000000-0000-0000-0000-000000000004",
        r#"{"watchlist_dismissed":"true"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let after = jobs.get_job(&job.id).await.unwrap().unwrap();
    assert_eq!(
        after.status,
        JobStatus::Closed,
        "a metadata patch must not resurrect a closed packet"
    );
    assert_eq!(
        after.closed_on,
        Some(closed_on),
        "closed_on is envelope, not metadata"
    );
    assert_eq!(
        after.metadata["outcome"], "completed",
        "the terminal's outcome must survive the dismiss"
    );
    assert_eq!(after.metadata["watchlist_dismissed"], "true");

    // And the JOB_UPDATED the merge recorded says the same: the event
    // is what the rebuild replays, so a payload carrying `open` here
    // would re-create the resurrection in the rebuild path.
    let updated: Vec<_> = jobs
        .recorded_events()
        .into_iter()
        .filter(|e| e.kind == "jobs.job.updated")
        .collect();
    let payload = &updated.last().expect("a JOB_UPDATED was recorded").payload;
    assert_eq!(payload["status"], "closed");
    assert_eq!(payload["metadata"]["outcome"], "completed");
    assert_eq!(payload["metadata"]["watchlist_dismissed"], "true");
}

#[tokio::test]
async fn the_merge_records_job_updated_with_the_post_merge_state() {
    let (app, jobs) = build_app(allow_update());
    let user = filer("emp-1");
    let job = job_with_metadata(
        "00000000-0000-0000-0000-000000000005",
        &user.id,
        serde_json::json!({ "route": "/ux/jobs" }),
    );
    jobs.create_job(&job).await.unwrap();

    let resp = patch_metadata(
        &app,
        &user,
        "00000000-0000-0000-0000-000000000005",
        r#"{"watchlist_dismissed":"true"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let updated: Vec<_> = jobs
        .recorded_events()
        .into_iter()
        .filter(|e| e.kind == "jobs.job.updated")
        .collect();
    assert_eq!(updated.len(), 1, "exactly one state event per merge");
    let payload = &updated[0].payload;
    assert_eq!(payload["id"], job.id.to_string());
    assert_eq!(payload["metadata"]["watchlist_dismissed"], "true");
    assert_eq!(
        payload["metadata"]["route"], "/ux/jobs",
        "the event carries the MERGED metadata, not the patch"
    );
    assert!(
        payload.get("_actor").is_some(),
        "outbox events carry the actor stamp"
    );
}

#[tokio::test]
async fn policy_denial_is_403_and_writes_nothing() {
    let (app, jobs) = build_app(Arc::new(FakePolicyClient::deny_all()));
    let user = filer("emp-1");
    let job = job_with_metadata(
        "00000000-0000-0000-0000-000000000006",
        &user.id,
        serde_json::json!({ "route": "/ux/jobs" }),
    );
    jobs.create_job(&job).await.unwrap();

    let resp = patch_metadata(
        &app,
        &user,
        "00000000-0000-0000-0000-000000000006",
        r#"{"watchlist_dismissed":"true"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    let after = jobs.get_job(&job.id).await.unwrap().unwrap();
    assert!(
        after.metadata.get("watchlist_dismissed").is_none(),
        "a denied patch must not write"
    );
    assert!(
        !jobs
            .recorded_events()
            .iter()
            .any(|e| e.kind == "jobs.job.updated"),
        "a denied patch must not record an event"
    );
}

#[tokio::test]
async fn a_job_outside_the_callers_scope_is_403() {
    // Same scope gate as the job PUT: Update within Scope::Self_ does
    // not reach somebody else's packet.
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "service-tech",
                Action::Update,
                Resource::job(),
                Scope::Self_,
            )
            .build(),
    );
    let (app, jobs) = build_app(policy);
    let user = filer("emp-1");
    let job = job_with_metadata(
        "00000000-0000-0000-0000-000000000007",
        "emp-somebody-else",
        serde_json::json!({}),
    );
    jobs.create_job(&job).await.unwrap();

    let resp = patch_metadata(
        &app,
        &user,
        "00000000-0000-0000-0000-000000000007",
        r#"{"watchlist_dismissed":"true"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_non_object_body_is_refused() {
    let (app, jobs) = build_app(allow_update());
    let user = filer("emp-1");
    let job = job_with_metadata(
        "00000000-0000-0000-0000-000000000008",
        &user.id,
        serde_json::json!({}),
    );
    jobs.create_job(&job).await.unwrap();

    let resp = patch_metadata(
        &app,
        &user,
        "00000000-0000-0000-0000-000000000008",
        r#"["not","an","object"]"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn an_unknown_job_is_404() {
    let (app, _jobs) = build_app(allow_update());
    let user = filer("emp-1");
    let resp = patch_metadata(
        &app,
        &user,
        "00000000-0000-0000-0000-0000000000ff",
        r#"{"watchlist_dismissed":"true"}"#,
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
