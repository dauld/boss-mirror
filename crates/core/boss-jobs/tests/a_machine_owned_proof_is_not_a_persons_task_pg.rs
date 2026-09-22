//! The operator's task station does not list a proof the machine owns
//! (backlog b725e860).
//!
//! `q.platform-admin.task` was a projected constraint station — every
//! `task` step carrying `authority_role = platform-admin`, ready or
//! active — and the ship-a-change `proven` step is one of those, so a
//! person CAN prove a car by hand. But a landed car that recorded a
//! `proof_probe` at park time is proven by the forge on arrival and
//! re-run by the daily recheck; nothing about it is a person's task, and
//! eight of them sat on David's queue beside real decisions on
//! 2026-09-15 ("many of the jobs were buggy"). The authored row this
//! test reads (migration 20260915023614) shadows the projection with
//! the same step clause plus one Job-level exclusion.
//!
//! Read against the ROW THE SCHEMA SHIPS, through the same `/queue`
//! endpoint `boss orient` and the backlog page read, with the Pg
//! adapters — a hand-copied predicate here would test the copy, not the
//! migration (CLAUDE.md 9a).
//!
//! Contracts:
//! 1. A landed car carrying `proof_probe` is NOT a member, even with
//!    its `proven` step ready under platform-admin authority.
//! 2. A landed car with NO recorded probe IS a member — a person must
//!    prove it, so it is a person's task.
//! 3. The row still serves the constraint it shadows: a platform-admin
//!    `task` on any other packet is a member. This is the hole
//!    `station_lint::check_derived_namespace` names — an authored `q.`
//!    row that did not cover its (kind, role) pair would hide that
//!    queue while the list still advertised it — so the row is also
//!    linted here, as the publish path would lint it.

use std::sync::Arc;

use axum::http::StatusCode;
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{JobsRepository, PgJobs, PgStations, StationRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::{RecordingEventBus, TestDb, TestRequest};
use chrono::NaiveDate;
use uuid::Uuid;

const STATION: &str = "q.platform-admin.task";
const WITH_PROBE: &str = "aaaaaaaa-0000-4000-8000-000000000001";
const WITHOUT_PROBE: &str = "aaaaaaaa-0000-4000-8000-000000000002";
const BACKLOG: &str = "aaaaaaaa-0000-4000-8000-000000000003";

fn job(id: &str, kind: &str, metadata: serde_json::Value) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: kind.into(),
        workflow_version: 1,
        subject: Subject::new("custom", "fix/x"),
        title: format!("{kind} {id}"),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 14).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

/// A ready `task` step under platform-admin authority. `authority_role`
/// sits in step metadata because materialisation surfaces it there for
/// the sign-off gate, and that is where the station clause reads it.
fn ready_task(job_id: &str, slug: &str) -> Step {
    Step {
        id: StepId::from_uuid(Uuid::new_v4()),
        job_id: JobId::from_uuid(Uuid::parse_str(job_id).unwrap()),
        kind: "task".into(),
        title: slug.into(),
        spec_slug: Some(slug.into()),
        assignee_id: None,
        status: StepStatus::Ready,
        sort_order: 0,
        blocked_by: vec![],
        sign_offs_required: Vec::new(),
        assurance_required: None,
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: None,
        completed_by: None,
        completed_at: None,
        metadata: serde_json::json!({ "authority_role": "platform-admin" }),
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

/// A landed car: merged, `proven` ready. `probe` is the park-time
/// intent the forge runs; `None` is a car a person must prove.
fn landed_car(id: &str, probe: Option<&str>) -> Job {
    let mut md = serde_json::json!({ "branch": format!("fix/{}", &id[..8]), "merged": "true" });
    if let Some(p) = probe {
        md["proof_probe"] = serde_json::Value::String(p.into());
        md["proof_expect"] = serde_json::Value::String("ok".into());
    }
    job(id, "ship-a-change", md)
}

async fn app(db: &TestDb) -> axum::Router {
    let jobs = Arc::new(PgJobs::new(db.pool.clone()));
    for (car, slug) in [
        (landed_car(WITH_PROBE, Some("bash x.sh")), "proven"),
        (landed_car(WITHOUT_PROBE, None), "proven"),
        (
            job(BACKLOG, "backlog-item", serde_json::json!({})),
            "triage",
        ),
    ] {
        jobs.create_job(&car).await.expect("packet lands");
        jobs.add_step(&ready_task(&car.id.to_string(), slug))
            .await
            .expect("step lands");
    }
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    router(JobsApiState {
        stations: Some(Arc::new(PgStations::new(db.pool.clone()))),
        ..JobsApiState::minimal(
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    })
}

fn members(body: &serde_json::Value) -> Vec<String> {
    let mut ids: Vec<String> = body["data"]
        .as_array()
        .expect("data is a collection")
        .iter()
        .map(|j| j["id"].as_str().expect("packet id").to_string())
        .collect();
    ids.sort();
    ids
}

#[tokio::test(flavor = "multi_thread")]
async fn a_landed_car_with_a_recorded_probe_leaves_the_operators_task_queue() {
    let db = TestDb::new().await;
    let app = app(&db).await;

    let resp = TestRequest::get(format!("/api/stations/{STATION}/queue"))
        .as_user("emp-david", "platform-admin")
        .send(&app)
        .await;
    resp.assert_status(StatusCode::OK);
    let body: serde_json::Value = resp.assert_json();

    assert_eq!(body["station"], STATION);
    let mut expected = vec![WITHOUT_PROBE.to_string(), BACKLOG.to_string()];
    expected.sort();
    assert_eq!(
        members(&body),
        expected,
        "the probe-carrying car is the machine's; the unprobed car and the \
         triage are a person's — got {body}"
    );
    assert_eq!(
        body["total"], 2,
        "the envelope's depth agrees with its rows"
    );
}

/// The authored row is the projection's predicate plus one exclusion,
/// and it passes the lint the publish path applies — in particular the
/// derived-namespace check, which is what stops an authored `q.` name
/// from shadowing a constraint queue it does not serve.
#[tokio::test(flavor = "multi_thread")]
async fn the_authored_row_covers_the_constraint_it_shadows() {
    let db = TestDb::new().await;
    let registry = PgStations::new(db.pool.clone());
    let row = registry
        .get_active(STATION)
        .await
        .expect("the schema ships the authored row");

    assert_eq!(
        row.predicate.metadata_absent,
        vec!["proof_probe".to_string()],
        "one exclusion: the park-time intent the forge runs"
    );
    let step = row
        .predicate
        .step
        .as_ref()
        .expect("the covering step clause");
    assert_eq!(step.kind.as_deref(), Some("task"));
    assert_eq!(
        step.metadata_equals
            .get("authority_role")
            .map(String::as_str),
        Some("platform-admin")
    );
    assert_eq!(step.status_in, vec![StepStatus::Ready, StepStatus::Active]);
    assert_eq!(
        row.capability.as_ref().map(|c| c.roles.clone()),
        Some(vec!["platform-admin".to_string()]),
        "the role still gates the claim"
    );
    let problems = boss_jobs::station_lint::validate_station(&row);
    assert!(problems.is_empty(), "lint-clean, got {problems:?}");
}
