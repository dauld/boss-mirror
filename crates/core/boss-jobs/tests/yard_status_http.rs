//! `GET /api/yard/status` — the yard status read-model, end-to-end
//! through the real router against the in-memory adapters.
//!
//! The contracts this pins are the ones an operator used to SSH for:
//!
//! 1. **A blocked train names its block, prominently.** The deploy-block
//!    reason lived in `deployed.metadata.deploy_blocked` and was read by
//!    nobody (2026-09-02, four hours wedged). The payload surfaces it on
//!    the train row as a first-class `block`.
//! 2. **The boarding predicate is computed from the LIVE cadence rows** —
//!    the `min_dock_depth` and `at_times` come from `cadence_rules`, not
//!    a constant baked into the page. Change the rule, the line moves.
//! 3. **The dock, recent trains, stranded greens, and policy thresholds**
//!    all read from the record, never a guess.
//! 4. **The read is policy-scoped like every queue surface** — an
//!    unreadable caller gets an empty, well-formed yard, not a 403 and
//!    not a false-empty.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::cadence::{CadenceRepository, CadenceRuleRow, InMemoryCadence};
use boss_jobs::delivery::{
    DeliveryPolicyRepository, DeliveryPolicyRow, InMemoryDeliveryPolicy, StoredPolicy,
};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::step_registry::StepRegistry;
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, NaiveDate, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const NOW: &str = "2026-09-03T12:00:00Z";

fn t(rfc3339: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(rfc3339).unwrap().into()
}

fn user_header(role: &str) -> String {
    serde_json::to_string(&User {
        id: "emp-david".to_string(),
        role: role.to_string(),
        access_tier: AccessTier::User,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User always serialises")
}

fn depth_rule() -> CadenceRuleRow {
    CadenceRuleRow {
        name: "train-board-on-dock-depth".into(),
        verb: "board".into(),
        basis: "queue-depth".into(),
        every_minutes: None,
        at_times: None,
        min_dock_depth: Some(4),
        cooldown_minutes: Some(120),
        cadence: None,
        anchor_date: None,
        business_calendar: None,
    }
}

fn clock_rule() -> CadenceRuleRow {
    CadenceRuleRow {
        name: "train-window".into(),
        verb: "run".into(),
        basis: "clock".into(),
        every_minutes: None,
        at_times: Some(json!(["06:00", "18:00"])),
        min_dock_depth: None,
        cooldown_minutes: None,
        cadence: None,
        anchor_date: None,
        business_calendar: None,
    }
}

fn policy_row() -> StoredPolicy {
    StoredPolicy {
        row: DeliveryPolicyRow {
            name: "train-conductor".into(),
            version: 1,
            max_red_trains: 2,
            stall_hours: 6,
            consist_excluded_lints: json!([]),
            consist_budget_secs: 600,
            consist_output_budget: 2000,
            consist_files_named: 5,
            skip_reason_file_budget: 200,
            blip_cause_budget: 200,
            ci_host_floor_gb: 10,
            gate_max_concurrent: 4,
        },
        status: "active".into(),
    }
}

fn app_with(
    rules: Vec<CadenceRuleRow>,
    policy: Vec<StoredPolicy>,
) -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let policy_client: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let cadence: Arc<dyn CadenceRepository> = Arc::new(InMemoryCadence::new(rules));
    let delivery: Arc<dyn DeliveryPolicyRepository> = Arc::new(InMemoryDeliveryPolicy::new(policy));
    let state = JobsApiState {
        jobs: jobs.clone(),
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy: policy_client,
        kind_registry: None,
        plugin_registry: None,
        job_edges: None,
        stations: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::FixedClockClient::new(
            boss_clock_client::ClockNow {
                now: t(NOW),
                simulated: false,
                epoch_start: None,
                epoch_end: None,
                paused: false,
                restart_in_progress: false,
                warp_factor: None,
            },
        )),
        cadence: Some(cadence),
        delivery: Some(delivery),
    };
    (router(state), jobs)
}

fn job(kind: &str, id: &str, title: &str, status: JobStatus, metadata: Value) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: kind.into(),
        workflow_version: 16,
        subject: Subject::new("custom", "s"),
        title: title.into(),
        owner_id: "emp-david".into(),
        status,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 3).unwrap(),
        due_on: None,
        closed_on: if status == JobStatus::Closed {
            Some(NaiveDate::from_ymd_opt(2026, 9, 3).unwrap())
        } else {
            None
        },
        metadata,
        tags: vec![],
        simulated: false,
    }
}

fn step(job_id: &JobId, slug: &str, title: &str, status: StepStatus, metadata: Value) -> Step {
    let mut s = Step::new(*job_id, "task", title, 0);
    s.id = StepId::new();
    s.spec_slug = Some(slug.into());
    s.status = status;
    s.metadata = metadata;
    s
}

async fn seed_full(jobs: &InMemoryJobs) {
    let now = t(NOW);
    // A blocked, merged, mid-deploy train.
    let train = job(
        "pr-train",
        "11111111-1111-1111-1111-111111111111",
        "train #200",
        JobStatus::Open,
        json!({ "boarded_jobs": ["22222222-2222-2222-2222-222222222222"] }),
    );
    jobs.create_job_at(&train, now, &[]).await.unwrap();
    for s in [
        step(
            &train.id,
            "merged",
            "Merged into main",
            StepStatus::Completed,
            json!({ "completed_at": "2026-09-03T06:45:00Z", "merge_ref": "abcdef123456" }),
        ),
        step(
            &train.id,
            "deployed",
            "Deployed to the playground",
            StepStatus::Ready,
            json!({
                "deploy_blocked": "deploy tree busy (branch=main, dirty=True) — will retry",
                "deploy_blocked_since": "2026-09-03T06:46:00Z",
            }),
        ),
    ] {
        jobs.add_step_at(&s, now, &[]).await.unwrap();
    }

    // An arrived train (recent).
    let arrived = job(
        "pr-train",
        "33333333-3333-3333-3333-333333333333",
        "train #199",
        JobStatus::Closed,
        json!({ "outcome": "arrived" }),
    );
    jobs.create_job_at(&arrived, now, &[]).await.unwrap();
    for s in [
        step(
            &arrived.id,
            "collect",
            "Collect what is ready to board",
            StepStatus::Completed,
            json!({ "completed_at": "2026-09-03T05:00:00Z" }),
        ),
        step(
            &arrived.id,
            "arrived",
            "Train arrived",
            StepStatus::Completed,
            json!({ "completed_at": "2026-09-03T05:30:00Z" }),
        ),
    ] {
        jobs.add_step_at(&s, now, &[]).await.unwrap();
    }

    // Two parked cars on the dock (open ship-a-change, branch, no train).
    for (id, branch, title) in [
        ("22222222-2222-2222-2222-222222222222", "feat/a", "A fix"),
        ("44444444-4444-4444-4444-444444444444", "feat/b", "B fix"),
    ] {
        let car = job(
            "ship-a-change",
            id,
            title,
            JobStatus::Open,
            json!({ "branch": branch }),
        );
        jobs.create_job_at(&car, now, &[]).await.unwrap();
    }
    // Note: feat/a IS a boarded car (on the train above) but stays open;
    // the fallback dock predicate excludes cars with a `train` marker,
    // and this one has none, so both read as parked here — the station
    // registry is not wired in this test, so the fallback runs.

    // A stranded green gate-run: a branch with a green verdict, no car.
    let gr = job(
        "gate-run",
        "55555555-5555-5555-5555-555555555555",
        "gate feat/stranded",
        JobStatus::Closed,
        json!({ "branch": "feat/stranded" }),
    );
    jobs.create_job_at(&gr, now, &[]).await.unwrap();
    jobs.add_step_at(
        &step(
            &gr.id,
            "gate",
            "Gate",
            StepStatus::Completed,
            json!({ "verdict": "green" }),
        ),
        now,
        &[],
    )
    .await
    .unwrap();

    // An IN-FLIGHT gate-run: open, no verdict yet — occupies a slot.
    let gating = job(
        "gate-run",
        "66666666-6666-6666-6666-666666666666",
        "gate feat/gating",
        JobStatus::Open,
        json!({ "branch": "feat/gating" }),
    );
    jobs.create_job_at(&gating, now, &[]).await.unwrap();
    jobs.add_step_at(
        &step(
            &gating.id,
            "record-verdict",
            "Record the receipt",
            StepStatus::Active,
            json!({}),
        ),
        now,
        &[],
    )
    .await
    .unwrap();

    // A RED gate-run: a failed verdict naming the check — the garage. Its
    // receipt carries the checks array the runner writes.
    let red = job(
        "gate-run",
        "77777777-7777-7777-7777-777777777777",
        "gate feat/broken",
        JobStatus::Closed,
        json!({ "branch": "feat/broken" }),
    );
    jobs.create_job_at(&red, now, &[]).await.unwrap();
    let red_receipt = json!({
        "verdict": "failed",
        "head": "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8",
        "checks": [
            {"name": "clippy", "result": "pass"},
            {"name": "test", "result": "fail"},
        ],
    })
    .to_string();
    jobs.add_step_at(
        &step(
            &red.id,
            "record-verdict",
            "Record the receipt",
            StepStatus::Completed,
            json!({ "verdict": "failed", "receipt": red_receipt }),
        ),
        now,
        &[],
    )
    .await
    .unwrap();
}

async fn get(app: &axum::Router, role: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/yard/status")
                .header("x-boss-user", user_header(role))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, v)
}

#[tokio::test]
async fn the_status_names_the_buried_block_reason() {
    let (app, jobs) = app_with(vec![depth_rule(), clock_rule()], vec![policy_row()]);
    seed_full(&jobs).await;
    let (status, body) = get(&app, "operator").await;
    assert_eq!(status, StatusCode::OK);

    let trains = body["trains"].as_array().unwrap();
    assert_eq!(trains.len(), 1, "one open train");
    let train = &trains[0];
    assert_eq!(train["phase"], "deploying");
    // The block that used to be buried is now a first-class field.
    assert_eq!(train["block"]["kind"], "deploy-blocked");
    assert_eq!(
        train["block"]["reason"],
        "deploy tree busy (branch=main, dirty=True) — will retry"
    );
    assert_eq!(train["block"]["since"], "2026-09-03T06:46:00Z");
    assert_eq!(train["car_count"], 1);
}

#[tokio::test]
async fn the_boarding_predicate_comes_from_the_live_cadence_rules() {
    let (app, jobs) = app_with(vec![depth_rule(), clock_rule()], vec![policy_row()]);
    seed_full(&jobs).await;
    let (_, body) = get(&app, "operator").await;

    let b = &body["boarding"];
    assert_eq!(b["dock_threshold"], 4);
    assert_eq!(b["cooldown_minutes"], 120);
    assert_eq!(b["at_times"], json!(["06:00", "18:00"]));
    assert_eq!(b["dock_depth"], 2);
    assert_eq!(b["threshold_met"], false);
    let summary = b["summary"].as_str().unwrap();
    assert!(summary.contains("4 parked cars"), "{summary}");
    assert!(summary.contains("06:00 / 18:00 UTC"), "{summary}");
    assert!(summary.contains("2 car(s) parked now"), "{summary}");
}

#[tokio::test]
async fn a_changed_cadence_rule_moves_the_line() {
    // The whole point of reading the registry: change the threshold, the
    // page changes — no folklore, no redeploy.
    let mut d = depth_rule();
    d.min_dock_depth = Some(2);
    let (app, jobs) = app_with(vec![d], vec![policy_row()]);
    seed_full(&jobs).await;
    let (_, body) = get(&app, "operator").await;
    assert_eq!(body["boarding"]["dock_threshold"], 2);
    // Two parked, threshold two → met.
    assert_eq!(body["boarding"]["threshold_met"], true);
}

#[tokio::test]
async fn the_dock_recent_stranded_and_policy_all_read_from_the_record() {
    let (app, jobs) = app_with(vec![depth_rule(), clock_rule()], vec![policy_row()]);
    seed_full(&jobs).await;
    let (_, body) = get(&app, "operator").await;

    // Dock: two parked cars, each with a branch.
    let dock = body["dock"].as_array().unwrap();
    assert_eq!(dock.len(), 2);
    let branches: Vec<&str> = dock.iter().filter_map(|c| c["branch"].as_str()).collect();
    assert!(branches.contains(&"feat/a"));
    assert!(branches.contains(&"feat/b"));

    // Recent: one arrived train with a journey time.
    let recent = body["recent"].as_array().unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0]["outcome"], "arrived");
    assert_eq!(recent[0]["journey_seconds"], 1800);

    // Stranded: the green gate-run whose branch is no car.
    let stranded = body["stranded"].as_array().unwrap();
    assert_eq!(stranded.len(), 1);
    assert_eq!(stranded[0]["branch"], "feat/stranded");

    // Policy thresholds from the active row.
    assert_eq!(body["policy"]["stall_hours"], 6);
    assert_eq!(body["policy"]["max_red_trains"], 2);

    // The server clock rides along so the client dates elapsed times
    // against it, not its own wallclock.
    assert!(body["now"].is_string());
}

#[tokio::test]
async fn the_gate_slots_and_garage_read_from_the_gate_runs() {
    let (app, jobs) = app_with(vec![depth_rule(), clock_rule()], vec![policy_row()]);
    seed_full(&jobs).await;
    let (_, body) = get(&app, "operator").await;

    // Capacity is the policy's gate_max_concurrent, not a constant.
    let gates = &body["gates"];
    assert_eq!(gates["capacity"], 4);
    // One in-flight gate-run occupies a slot; the green and red runs do
    // not (they have verdicts).
    let active = gates["active"].as_array().unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0]["branch"], "feat/gating");
    assert_eq!(
        active[0]["packet_id"],
        "66666666-6666-6666-6666-666666666666"
    );
    assert!(active[0]["since"].is_string());

    // The garage holds the branch whose latest gate is red, named with
    // its failing check.
    let garage = body["garage"].as_array().unwrap();
    assert_eq!(garage.len(), 1);
    assert_eq!(garage[0]["branch"], "feat/broken");
    assert_eq!(garage[0]["failed_check"], "test");
}

#[tokio::test]
async fn an_unreadable_caller_gets_an_empty_well_formed_yard_not_a_403() {
    let (app, jobs) = app_with(vec![depth_rule()], vec![policy_row()]);
    seed_full(&jobs).await;
    // A role with no job-read grant → Predicate::None → empty yard.
    let (status, body) = get(&app, "outsider").await;
    assert_eq!(status, StatusCode::OK, "empty, not forbidden");
    assert_eq!(body["trains"].as_array().unwrap().len(), 0);
    assert_eq!(body["dock"].as_array().unwrap().len(), 0);
    // Still a well-formed payload — the boarding block renders "nothing",
    // never a false-empty error.
    assert!(body["boarding"].is_object());
}

#[tokio::test]
async fn no_cadence_or_policy_wired_degrades_gracefully() {
    // The trains and dock are what the operator came for; a yard with no
    // cadence configured still answers, saying so plainly.
    let jobs = Arc::new(InMemoryJobs::new());
    let policy_client: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        jobs: jobs.clone(),
        bus,
        publisher: DomainPublisher::new(bus_dyn, "jobs"),
        step_registry: Arc::new(StepRegistry::v1()),
        policy: policy_client,
        kind_registry: None,
        plugin_registry: None,
        job_edges: None,
        stations: None,
        calendar: None,
        subject_kinds: None,
        subject_existence: None,
        roster: None,
        clock: Arc::new(boss_clock_client::WallClockClient),
        cadence: None,
        delivery: None,
    };
    let app = router(state);
    seed_full(&jobs).await;
    let (status, body) = get(&app, "operator").await;
    assert_eq!(status, StatusCode::OK);
    // Trains still surface.
    assert_eq!(body["trains"].as_array().unwrap().len(), 1);
    // No cadence → the honest "no configured cadence" line.
    assert!(body["boarding"]["dock_threshold"].is_null());
    assert!(
        body["boarding"]["summary"]
            .as_str()
            .unwrap()
            .contains("No boarding cadence is configured")
    );
    // No policy → thresholds null, never a fabricated default.
    assert!(body["policy"]["stall_hours"].is_null());
    // No policy → gate capacity is the compiled fallback (3), the same
    // bound a gate obeys against an unreachable registry.
    assert_eq!(body["gates"]["capacity"], 3);
}
