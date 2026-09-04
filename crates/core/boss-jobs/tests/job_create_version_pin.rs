//! The create handler must pin a new Job to its kind's *active*
//! version. Per docs/architecture-decisions.md §Jobs, Workflows, Steps:
//! creation is blocked against draft/retired kinds, and in-flight Jobs
//! pin to the version they opened under — so a freshly created Job's
//! `workflow_version` is the active version at open time, never the
//! schema DEFAULT 1 and never a client-supplied value.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{WorkflowRegistry, WorkflowSpec, WorkflowStatus};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use tower::ServiceExt;

/// The reconcile write-path actor — every registry write records an
/// event now, and reconcile attributes to the named automation.
fn reconciler() -> boss_core::actor::ActorId {
    boss_core::actor::ActorId::Automation("bootstrap-reconciler".into())
}

fn ceo_header() -> String {
    serde_json::json!({
        "id": "emp-ceo",
        "role": "ceo",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "executive",
    })
    .to_string()
}

fn versioned_spec(version: i32, status: WorkflowStatus) -> WorkflowSpec {
    let mut s = WorkflowSpec::platform_seed(
        "versioned",
        "Versioned",
        "test",
        vec!["system".into()],
        Vec::new(),
    );
    s.version = version;
    s.status = status;
    s
}

#[tokio::test]
async fn new_job_pins_to_active_version_not_default_one() {
    // A kind whose ACTIVE version is 3 (v1, v2 retired by prior publishes).
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds
        .seed(versioned_spec(1, WorkflowStatus::Retired))
        .unwrap();
    kinds
        .seed(versioned_spec(2, WorkflowStatus::Retired))
        .unwrap();
    kinds
        .seed(versioned_spec(3, WorkflowStatus::Active))
        .unwrap();

    let jobs = Arc::new(InMemoryJobs::new());
    let kind_registry: Arc<dyn WorkflowRegistry> = kinds;
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Create, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: Some(kind_registry),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
    };
    let app = router(state);

    // Client sends a deliberately-wrong version (99); the server must
    // override it with the kind's active version (3).
    let mut job = Job::new(
        "versioned",
        Subject::new("system", "sys-1"),
        "Pin test",
        "emp-ceo",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 4, 28).unwrap(),
    );
    job.workflow_version = 99;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", ceo_header())
                .body(Body::from(serde_json::to_vec(&job).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "create should succeed against the active kind"
    );

    let stored = jobs
        .get_job(&job.id)
        .await
        .unwrap()
        .expect("job was stored");
    assert_eq!(
        stored.workflow_version, 3,
        "new Job must pin to the active version (3), not the client value (99) or DEFAULT 1"
    );
}

/// Pinning at create is only half a contract. The other half is that
/// the pin HOLDS: a Job must keep being evaluated against the spec it
/// opened under, however the registry moves on.
///
/// It did not. `bootstrap_reconcile` rewrote a bootstrap-owned row's
/// body in place and kept its version, and step re-evaluation resolved
/// `get_active` — so a running Job silently began answering predicates
/// from a spec it never agreed to. Where the new spec had more steps,
/// re-evaluation's length-guard bailed and the Job stopped advancing:
/// triaged, work done, and unable to close. Two real feedback Jobs
/// were stranded that way and had to be closed by skipping their
/// outcome.
#[tokio::test]
async fn a_republish_does_not_strand_a_job_opened_under_the_old_version() {
    use boss_jobs::registry::{StepSpec, Terminal};

    /// v1: open → gate → closed, where closure depends on the gate.
    fn v1() -> WorkflowSpec {
        WorkflowSpec::platform_seed(
            "versioned",
            "Versioned",
            "test",
            vec!["system".into()],
            vec![
                StepSpec {
                    title: "open".into(),
                    kind: "trigger".into(),
                    ready_when: "true".into(),
                    title_template: "Opened".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "gate".into(),
                    kind: "task".into(),
                    ready_when: "steps.open.done".into(),
                    title_template: "Do the thing".into(),
                    authority_role: Some("ceo".into()),
                    ..Default::default()
                },
                StepSpec {
                    title: "closed".into(),
                    kind: "outcome".into(),
                    ready_when: "steps.gate.done".into(),
                    title_template: "Closed".into(),
                    terminal: Some(Terminal {
                        outcome: "completed".into(),
                    }),
                    ..Default::default()
                },
            ],
        )
    }

    /// v2 adds a branch and moves closure behind it — the exact shape
    /// that stranded the real Jobs, because a v1 Job has no `branch`
    /// step for `steps.branch.done` to ever be true about.
    fn v2() -> WorkflowSpec {
        let mut s = v1();
        s.label = "Versioned v2".into();
        s.steps.insert(
            2,
            StepSpec {
                title: "branch".into(),
                kind: "task".into(),
                ready_when: "steps.gate.done".into(),
                title_template: "Extra work".into(),
                authority_role: Some("ceo".into()),
                ..Default::default()
            },
        );
        s.steps[3].ready_when = "steps.branch.done".into();
        s
    }

    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds
        .bootstrap_reconcile(&[v1()], &reconciler(), chrono::Utc::now())
        .await
        .expect("seed v1");

    let jobs = Arc::new(InMemoryJobs::new());
    let kind_registry: Arc<dyn WorkflowRegistry> = kinds.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Create, Resource::job(), Scope::All)
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .allow("ceo", Action::Update, Resource::step(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let app = router(JobsApiState {
        job_edges: None,
        stations: None,
        jobs: jobs.clone(),
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy,
        kind_registry: Some(kind_registry),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
    });

    let mut job = Job::new(
        "versioned",
        Subject::new("system", "sys-1"),
        "Opened under v1",
        "emp-ceo",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 4, 28).unwrap(),
    );
    // `Job::new` yields a Draft; a Draft never reaches closure, so the
    // assertion below would pass vacuously.
    job.status = boss_core::job::JobStatus::Open;
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", ceo_header())
                .body(Body::from(serde_json::to_vec(&job).unwrap()))
                .unwrap(),
        )
        .await
        .expect("create");
    assert_eq!(resp.status(), StatusCode::CREATED);

    let (stored, _) = jobs.list_jobs(&Default::default(), 10, 0).await.unwrap();
    let opened = stored.first().expect("job stored").clone();
    assert_eq!(opened.workflow_version, 1);

    // The registry moves on WHILE the Job is in flight.
    let stats = kinds
        .bootstrap_reconcile(&[v2()], &reconciler(), chrono::Utc::now())
        .await
        .expect("republish");
    assert_eq!(stats.republished, 1);
    assert_eq!(
        kinds.get_active("versioned").await.unwrap().version,
        2,
        "republish must mint a version, or the pin has nothing to point at"
    );

    // Now drive the Job's gate. Under v1 that is the last human step,
    // so the Job must close.
    let steps = jobs.list_steps(&opened.id).await.unwrap();
    let gate = steps
        .iter()
        .find(|s| s.metadata.get("authority_role").is_some())
        .expect("gate step");
    let resp = app
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/api/jobs/{}/steps/{}", opened.id, gate.id))
                .header("content-type", "application/json")
                .header("x-boss-user", ceo_header())
                .body(Body::from(
                    serde_json::json!({"status": "completed"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .expect("complete gate");
    assert!(
        resp.status().is_success(),
        "gate completion: {:?}",
        resp.status()
    );

    // What re-evaluation controls is which steps become READY; a
    // separate rule completes terminals. So the assertion is that the
    // terminal opened at all — under v1 its predicate is
    // `steps.gate.done`, which just became true. Under v2 it is
    // `steps.branch.done`, and this Job has no `branch` step, so a
    // stranded Job leaves it Pending forever.
    let after = jobs.list_steps(&opened.id).await.unwrap();
    let terminal = after
        .iter()
        .find(|s| s.kind == "outcome")
        .expect("terminal step");
    assert_ne!(
        terminal.status,
        boss_core::job::StepStatus::Pending,
        "the terminal never opened: the Job is pinned to v{} but was evaluated against a \
         spec whose closure depends on steps it does not have. Steps: {:?}",
        opened.workflow_version,
        after
            .iter()
            .map(|s| (s.title.as_str(), s.status))
            .collect::<Vec<_>>()
    );
}
