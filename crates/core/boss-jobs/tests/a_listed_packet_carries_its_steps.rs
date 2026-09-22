//! `GET /api/jobs?…` returns each row WITH its steps embedded.
//!
//! WHY THIS FILE EXISTS (backlog d0ee20f8). This is a wire contract
//! live consumers depend on and nothing held. `infra/ops/ops-runner.sh`
//! lists its open `ops-request` packets once and reads the `execute`
//! step straight off each listed row —
//!
//!     ((.steps // []) | map(select(.spec_slug == "execute")) | .[0])
//!     // ((.steps // []) | map(select(.title == "execute")) | .[0])
//!
//! — and skips any packet where that selection is empty. Drop the
//! enrichment loop in `boss_jobs::http::jobs`' list handler and the
//! runner does not error: it logs "has no execute step — skipping" for
//! every request and the ops queue silently stops moving. A list read
//! that quietly stopped carrying steps is what this file refuses.
//!
//! IT IS ALSO THE REFUTATION OF A RECURRING SHORTCUT. The reflex behind
//! the duplicate exit that backlog 50fede8b collapsed was "write the
//! fact at the request level so a reader need not fetch steps". A
//! request-level reader never had to fetch them: the list has carried
//! them all along. That argument is only usable by the next author if
//! it is written where they look and held by something that fails —
//! which is this test, named from `boss-dispatcher-handlers`' crate doc
//! and from the enrichment site itself.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_clock_client::{ClockClient, ClockNow, FixedClockClient};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::JobsRepository;
use boss_jobs::http::{JobsApiState, router};
use boss_policy_client::{
    AccessTier, Action, FakePolicyClient, PolicyClient, Resource, Scope, User,
};
use boss_testing::RecordingEventBus;
use chrono::NaiveDate;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uuid::Uuid;

const FIRST: &str = "00000000-0000-0000-0000-00000000f001";
const SECOND: &str = "00000000-0000-0000-0000-00000000f002";

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
}

fn ceo() -> User {
    User {
        id: "emp-ceo".into(),
        role: "ceo".into(),
        access_tier: AccessTier::User,
        territory_account_ids: vec![],
        direct_report_ids: vec![],
        department: None,
    }
}

fn request(id: &str, verb: &str) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).expect("uuid")),
        kind: "ops-request".into(),
        workflow_version: 1,
        subject: Subject::new("asset", "BOSSNET"),
        title: format!("{verb} on forge"),
        owner_id: "emp-david".into(),
        status: JobStatus::Open,
        priority: Priority::Standard,
        opened_on: day(2026, 9, 20),
        opened_at: None,
        due_on: None,
        closed_on: None,
        metadata: serde_json::json!({ "verb": verb, "host": "forge" }),
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

/// The execute step as the ops-runner meets it: addressed by
/// `spec_slug`, gated on `status`, and carrying the verb's exit under
/// `exit_code` — the one spelling, on the step (backlog 50fede8b).
fn execute_step(job: &str, exit_code: &str) -> Step {
    Step {
        id: StepId::new(),
        job_id: JobId::from_uuid(Uuid::parse_str(job).expect("uuid")),
        kind: "task".into(),
        title: "execute".into(),
        spec_slug: Some("execute".into()),
        assignee_id: None,
        status: StepStatus::Ready,
        sort_order: 1,
        blocked_by: Vec::new(),
        sign_offs_required: Vec::new(),
        assurance_required: None,
        sign_offs: Vec::new(),
        fields: Vec::new(),
        completed_on: None,
        completed_by: None,
        completed_at: None,
        metadata: serde_json::json!({ "exit_code": exit_code }),
        notes: None,
        step_plugin_version: 0,
        embedded_job: None,
    }
}

async fn seed() -> Router {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let publisher = DomainPublisher::new(bus_dyn, "jobs");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("ceo", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let clock: Arc<dyn ClockClient> = Arc::new(FixedClockClient::new(ClockNow {
        now: day(2026, 9, 20)
            .and_hms_opt(12, 0, 0)
            .expect("noon")
            .and_utc(),
        simulated: true,
        epoch_start: None,
        epoch_end: None,
        paused: false,
        restart_in_progress: false,
        warp_factor: None,
    }));
    for (id, verb, exit) in [(FIRST, "converge", "0"), (SECOND, "publish-drift", "75")] {
        jobs.create_job(&request(id, verb)).await.expect("job");
        jobs.add_step(&execute_step(id, exit)).await.expect("step");
    }
    let state = JobsApiState::minimal(jobs, bus, publisher, policy, clock);
    router(state)
}

async fn list(app: &Router, query: &str) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/jobs?{query}"))
                .header("x-boss-user", serde_json::to_string(&ceo()).expect("user"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let body = resp.into_body().collect().await.expect("body").to_bytes();
    let body = String::from_utf8_lossy(&body).into_owned();
    assert_eq!(status, StatusCode::OK, "GET /api/jobs?{query}: {body}");
    serde_json::from_str(&body).expect("json")
}

/// The ops-runner's own selection, in Rust: slug first, title as
/// fallback, off the LISTED row — no second fetch anywhere.
fn execute_of(row: &serde_json::Value) -> &serde_json::Value {
    let steps = row["steps"]
        .as_array()
        .unwrap_or_else(|| panic!("listed row carries a steps array: {row}"));
    steps
        .iter()
        .find(|s| s["spec_slug"] == "execute")
        .or_else(|| steps.iter().find(|s| s["title"] == "execute"))
        .unwrap_or_else(|| panic!("listed row carries its execute step: {row}"))
}

#[tokio::test]
async fn a_listed_packet_carries_its_steps() {
    let app = seed().await;
    let body = list(&app, "kind=ops-request").await;
    let rows = body["data"].as_array().expect("data array");
    assert_eq!(rows.len(), 2, "both requests listed: {body}");

    for row in rows {
        let step = execute_of(row);
        // The fields the runner gates on before it acts.
        assert_eq!(step["status"], "ready", "step status rides the list: {row}");
        assert_eq!(step["job_id"], row["id"], "each row carries ITS OWN steps");
        assert!(
            step["metadata"]["exit_code"].is_string(),
            "step metadata rides the list: {row}"
        );
    }

    // Per-row, not one row's steps stamped on every row: the two
    // requests must answer with their own exits.
    let exits: Vec<&str> = rows
        .iter()
        .map(|r| {
            execute_of(r)["metadata"]["exit_code"]
                .as_str()
                .unwrap_or("")
        })
        .collect();
    assert!(
        exits.contains(&"0") && exits.contains(&"75"),
        "each listed row carries its own step metadata, got {exits:?}"
    );
}

/// The knowledge half of backlog d0ee20f8: the fact above is only
/// reachable by the next author if the places they read NAME this file.
/// Two do — the enrichment site and the handler crate's doc — and this
/// test is what stops either reference going quietly dead.
#[test]
fn the_contract_is_named_where_an_author_would_look() {
    let root = boss_testing::repo_root();
    for path in [
        "crates/core/boss-jobs/src/http/jobs.rs",
        "crates/orchestrators/boss-dispatcher-handlers/src/lib.rs",
    ] {
        let text =
            std::fs::read_to_string(root.join(path)).unwrap_or_else(|e| panic!("read {path}: {e}"));
        assert!(
            text.contains("a_listed_packet_carries_its_steps"),
            "{path} must name the test that holds the list-carries-steps contract"
        );
    }
}
