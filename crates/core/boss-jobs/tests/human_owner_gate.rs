//! Q7 (subject-model design, approved 2026-07-15): every Job names a
//! responsible HUMAN owner. The create handler resolves
//! automation-shaped owners (`system-sim`, `automation:*`, `rule:*`)
//! to a holder of the kind's `metadata.owner_role`, falls back to
//! the first role-bearing step's `authority_role`, keeps a
//! human-shaped owner, and REJECTS when no human resolves. Before
//! this gate, the live playground held 18,762 `system-sim`-owned
//! jobs and zero human-owned ones.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, JobsRepository, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;

struct FixedRoster;

#[async_trait]
impl RosterLookup for FixedRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "bookkeeper" => vec!["emp-bk-1".to_string(), "emp-bk-2".to_string()],
            "workflow-approver" => vec!["emp-lead-1".to_string()],
            _ => Vec::new(),
        })
    }
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(matches!(
            id,
            "emp-bk-1" | "emp-bk-2" | "emp-lead-1" | "emp-ceo"
        ))
    }
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

fn kind_with_owner_role(
    kind: &str,
    owner_role: Option<&str>,
    step_role: Option<&str>,
) -> WorkflowSpec {
    let steps = match step_role {
        Some(role) => vec![StepSpec {
            title: "approve".into(),
            kind: "sign-off".into(),
            ready_when: "true".into(),
            title_template: "Approve".into(),
            authority_role: Some(role.to_string()),
            ..Default::default()
        }],
        None => Vec::new(),
    };
    let mut s = WorkflowSpec::platform_seed(kind, kind, "test", vec!["custom".into()], steps);
    if let Some(role) = owner_role {
        s.metadata = serde_json::json!({ "owner_role": role });
    }
    s
}

async fn stored_owner(jobs: &InMemoryJobs) -> String {
    let (all, _) = jobs.list_jobs(&Default::default(), 50, 0).await.unwrap();
    assert_eq!(all.len(), 1, "exactly one job expected");
    all[0].owner_id.clone()
}

fn app_with(specs: Vec<WorkflowSpec>) -> (axum::Router, Arc<InMemoryJobs>) {
    let kinds = Arc::new(InMemoryWorkflows::new());
    for s in specs {
        kinds.seed(s).unwrap();
    }
    let jobs = Arc::new(InMemoryJobs::new());
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
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        plugin_registry: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: Some(Arc::new(FixedRoster)),
        clock: Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
    };
    (router(state), jobs)
}

fn job(kind: &str, owner: &str) -> Job {
    Job::new(
        kind,
        Subject::new("custom", "x"),
        "t",
        owner,
        Priority::Standard,
        NaiveDate::from_ymd_opt(2026, 7, 17).unwrap(),
    )
}

async fn post_job(app: &axum::Router, j: &Job) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", ceo_header())
                .body(Body::from(serde_json::to_vec(j).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::json!(null));
    (status, body)
}

#[tokio::test]
async fn automation_owner_resolves_to_owner_role_holder() {
    let (app, jobs) = app_with(vec![kind_with_owner_role(
        "payroll-run",
        Some("bookkeeper"),
        None,
    )]);
    let (status, body) = post_job(&app, &job("payroll-run", "system-sim")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let owner = stored_owner(&jobs).await;
    assert!(
        owner == "emp-bk-1" || owner == "emp-bk-2",
        "system-sim must resolve to a bookkeeper, got {owner}"
    );
}

#[tokio::test]
async fn human_owner_is_kept_verbatim() {
    let (app, jobs) = app_with(vec![kind_with_owner_role(
        "payroll-run",
        Some("bookkeeper"),
        None,
    )]);
    let (status, body) = post_job(&app, &job("payroll-run", "emp-ceo")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(stored_owner(&jobs).await, "emp-ceo");
}

#[tokio::test]
async fn step_authority_role_is_the_fallback_for_meta_kinds() {
    let (app, jobs) = app_with(vec![kind_with_owner_role(
        "workflow-design",
        None,
        Some("workflow-approver"),
    )]);
    let (status, body) = post_job(&app, &job("workflow-design", "automation:prepare")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    assert_eq!(stored_owner(&jobs).await, "emp-lead-1");
}

#[tokio::test]
async fn unresolvable_owner_rejects_the_create() {
    // A kind with a role nobody holds and no step fallback.
    let (app, jobs) = app_with(vec![kind_with_owner_role(
        "orphan-kind",
        Some("role-nobody-holds"),
        None,
    )]);
    let (status, _) = post_job(&app, &job("orphan-kind", "system-sim")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (all, _) = jobs.list_jobs(&Default::default(), 50, 0).await.unwrap();
    assert!(all.is_empty(), "a rejected create must not land a row");
}

#[tokio::test]
async fn platform_meta_kinds_declare_a_resolvable_owner_role() {
    // The 2026-07-17 smoke red: workflow-design's approve authority
    // (`workflow-approver`) is a policy CAPABILITY, not an
    // employees.role — the step fallback found zero holders, every
    // design-Job create was rejected, no brewery kind ever
    // published, and the whole from-empty stack starved. The
    // platform meta-kinds must name a real role explicitly.
    for spec in boss_jobs::registry::platform_workflows() {
        let owner_role = spec.metadata.get("owner_role").and_then(|v| v.as_str());
        assert_eq!(
            owner_role,
            Some("platform-admin"),
            "{}: platform meta-work is owned by the operator baseline",
            spec.kind
        );
    }
}
