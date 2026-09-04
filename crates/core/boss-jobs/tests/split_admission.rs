//! Split admission — Tier 2 of the experiments program
//! (docs/design/network-experiments.md; packet 6ea5a12a).
//!
//! Q3 (574c2adf): an experiment IS a packet — kind
//! `protocol-experiment`, terminals `promoted` / `retired`. Q1: its
//! arms are two versions of ONE kind. So the experiment record is the
//! packet's own job metadata (`kind_under_test`, `control_version`,
//! `candidate_version`, `split`), its window is the packet's open
//! interval, and admission is where cohort membership is decided:
//! while such a packet is OPEN, every new packet of the kind under
//! test hash-splits by its own job id into an arm, pins to that arm's
//! version, and carries the `experiment_arm` / `experiment_id` stamp
//! in the JOB_CREATED event — so the rebuilder replays the recorded
//! choice, never the coin.
//!
//! Fail-safe is the load-bearing property: a malformed declaration or
//! an arm version the registry cannot produce leaves admission exactly
//! as it stood — active version, no stamp. An experiment must never
//! break the kind it measures.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobStatus, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::experiments::{ARM_CANDIDATE, ARM_CONTROL, EXPERIMENT_KIND};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{StepSpec, Terminal, WorkflowRegistry, WorkflowSpec, WorkflowStatus};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use tower::ServiceExt;

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

/// v2 (control) closes in one gate; v3 (candidate) adds a `branch`
/// step so a candidate-arm packet is visibly running a DIFFERENT
/// protocol — the step graph, not just the pin, must come from the
/// arm's spec.
fn spec(version: i32, status: WorkflowStatus) -> WorkflowSpec {
    let mut steps = vec![
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
    ];
    if version >= 3 {
        steps.insert(
            2,
            StepSpec {
                title: "branch".into(),
                kind: "task".into(),
                ready_when: "steps.gate.done".into(),
                title_template: "Candidate-only work".into(),
                authority_role: Some("ceo".into()),
                ..Default::default()
            },
        );
        steps[3].ready_when = "steps.branch.done".into();
    }
    let mut s = WorkflowSpec::platform_seed(
        "versioned",
        "Versioned",
        "test",
        vec!["system".into()],
        steps,
    );
    s.version = version;
    s.status = status;
    s
}

/// An OPEN experiment packet in the jobs projection, declaring a
/// split over `versioned`. The candidate share is `split` percent.
fn experiment(split: serde_json::Value) -> Job {
    let mut e = Job::new(
        EXPERIMENT_KIND,
        Subject::new("custom", "proto-versioned"),
        "Experiment: versioned v3 vs v2",
        "emp-ceo",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
    );
    e.status = JobStatus::Open;
    e.metadata = serde_json::json!({
        "kind_under_test": "versioned",
        "control_version": 2,
        "candidate_version": 3,
        "split": split,
    });
    e
}

async fn app_with(
    experiments: Vec<Job>,
    registry_rows: Vec<WorkflowSpec>,
) -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::new());
    for row in registry_rows {
        kinds.seed(row).unwrap();
    }
    let jobs = Arc::new(InMemoryJobs::new());
    for e in experiments {
        jobs.create_job(&e).await.unwrap();
    }
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
    (router(state), jobs)
}

fn packet() -> Job {
    let mut j = Job::new(
        "versioned",
        Subject::new("system", "sys-1"),
        "Split me",
        "emp-ceo",
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 9, 2).unwrap(),
    );
    j.status = JobStatus::Open;
    j
}

async fn create(app: &axum::Router, job: &Job) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", ceo_header())
                .body(Body::from(serde_json::to_vec(job).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED, "admission must succeed");
}

#[tokio::test]
async fn a_full_candidate_split_pins_to_the_candidate_and_stamps_the_arm() {
    let exp = experiment(serde_json::json!(100));
    let exp_id = exp.id;
    let (app, jobs) = app_with(
        vec![exp],
        vec![
            spec(2, WorkflowStatus::Active),
            spec(3, WorkflowStatus::Draft),
        ],
    )
    .await;

    // The client tries to forge the arm; the server's stamp must win.
    let mut job = packet();
    job.metadata = serde_json::json!({ "experiment_arm": "control" });
    create(&app, &job).await;

    let stored = jobs.get_job(&job.id).await.unwrap().expect("stored");
    assert_eq!(
        stored.workflow_version, 3,
        "split=100 sends every packet to the candidate arm (v3, a draft)"
    );
    assert_eq!(
        stored
            .metadata
            .get("experiment_arm")
            .and_then(|v| v.as_str()),
        Some(ARM_CANDIDATE),
        "the arm stamp is server-assigned, never the client's claim"
    );
    assert_eq!(
        stored
            .metadata
            .get("experiment_id")
            .and_then(|v| v.as_str()),
        Some(exp_id.to_string().as_str()),
        "the stamp names WHICH experiment governed the admission"
    );

    // The protocol set is the candidate's: v3 has the `branch` step.
    let steps = jobs.list_steps(&job.id).await.unwrap();
    assert!(
        steps.iter().any(|s| s.title == "Candidate-only work"),
        "candidate-arm steps must materialize from the candidate spec; got {:?}",
        steps.iter().map(|s| s.title.as_str()).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn a_zero_split_pins_to_the_control_version() {
    let (app, jobs) = app_with(
        vec![experiment(serde_json::json!(0))],
        vec![
            spec(2, WorkflowStatus::Active),
            spec(3, WorkflowStatus::Draft),
        ],
    )
    .await;

    let job = packet();
    create(&app, &job).await;

    let stored = jobs.get_job(&job.id).await.unwrap().expect("stored");
    assert_eq!(stored.workflow_version, 2, "split=0 is all-control");
    assert_eq!(
        stored
            .metadata
            .get("experiment_arm")
            .and_then(|v| v.as_str()),
        Some(ARM_CONTROL),
        "control packets are stamped too — the report's arm dimension \
         distinguishes experiment-window control traffic from bystanders"
    );
}

#[tokio::test]
async fn no_open_experiment_means_admission_as_it_always_was() {
    // The experiment exists but is CLOSED — its window is over.
    let mut done = experiment(serde_json::json!(100));
    done.status = JobStatus::Closed;
    let (app, jobs) = app_with(
        vec![done],
        vec![
            spec(2, WorkflowStatus::Active),
            spec(3, WorkflowStatus::Draft),
        ],
    )
    .await;

    let job = packet();
    create(&app, &job).await;

    let stored = jobs.get_job(&job.id).await.unwrap().expect("stored");
    assert_eq!(stored.workflow_version, 2, "active version, as before");
    assert!(
        stored.metadata.get("experiment_arm").is_none(),
        "no governing experiment, no stamp"
    );
}

#[tokio::test]
async fn a_missing_arm_version_fails_safe_to_the_active_version() {
    // candidate_version names v9; the registry has no such row. The
    // experiment is malformed — the packet must admit exactly as if
    // no experiment existed.
    let mut exp = experiment(serde_json::json!(100));
    exp.metadata["candidate_version"] = serde_json::json!(9);
    let (app, jobs) = app_with(
        vec![exp],
        vec![
            spec(2, WorkflowStatus::Active),
            spec(3, WorkflowStatus::Draft),
        ],
    )
    .await;

    let job = packet();
    create(&app, &job).await;

    let stored = jobs.get_job(&job.id).await.unwrap().expect("stored");
    assert_eq!(
        stored.workflow_version, 2,
        "an experiment must never break the kind it measures"
    );
    assert!(
        stored.metadata.get("experiment_arm").is_none(),
        "a packet that did not run under an arm carries no stamp"
    );
}

#[tokio::test]
async fn the_stamped_arm_and_the_pinned_version_agree_at_an_even_split() {
    // At split=50 the coin is the packet's own id — unpredictable per
    // packet, but the STAMP and the PIN must always agree, and the
    // same id must always land the same way.
    let (app, jobs) = app_with(
        vec![experiment(serde_json::json!(50))],
        vec![
            spec(2, WorkflowStatus::Active),
            spec(3, WorkflowStatus::Draft),
        ],
    )
    .await;

    for _ in 0..20 {
        let job = packet();
        create(&app, &job).await;
        let stored = jobs.get_job(&job.id).await.unwrap().expect("stored");
        let arm = stored
            .metadata
            .get("experiment_arm")
            .and_then(|v| v.as_str())
            .expect("every packet admitted during the window is stamped");
        let expected = match arm {
            a if a == ARM_CANDIDATE => 3,
            a if a == ARM_CONTROL => 2,
            other => panic!("unknown arm stamp {other:?}"),
        };
        assert_eq!(
            stored.workflow_version, expected,
            "the pin must be the stamped arm's version"
        );
        assert_eq!(
            boss_jobs::experiments::arm_for(&stored.id, 50),
            arm,
            "the arm is a pure function of the packet id — replay-deterministic"
        );
    }
}
