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
//! 5. **The yard tells the time in instants, from the stamps the record
//!    holds.** A gate's `since` is its `opened_at`; an open train carries
//!    `boarded_at`; a closed train's `journey_seconds` reads the
//!    terminal instant from the job's `closed_at` — the shape the server
//!    actually writes, where the outcome step itself is never stamped.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Step, StepId, StepStatus, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::cadence::{CadenceRepository, CadenceRuleRow, InMemoryCadence, NewFiring};
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

/// The conductor's heartbeat rule: an interval basis, verb `reconcile`.
fn reconcile_rule() -> CadenceRuleRow {
    CadenceRuleRow {
        name: "train-reconcile".into(),
        verb: "reconcile".into(),
        basis: "interval".into(),
        every_minutes: Some(10),
        at_times: None,
        min_dock_depth: None,
        cooldown_minutes: None,
        cadence: None,
        anchor_date: None,
        business_calendar: None,
    }
}

/// Record a firing the way the conductor does: claim the window, then
/// (when the verb has finished) merge its exit code in. `rc: None`
/// leaves the run in flight.
async fn fire(cadence: &InMemoryCadence, rule: &str, verb: &str, at: &str, rc: Option<i32>) {
    let firing_id = format!("{rule}@{at}");
    cadence
        .claim_firing(&NewFiring {
            firing_id: firing_id.clone(),
            rule_name: rule.into(),
            verb: verb.into(),
            basis: "test".into(),
            fired_at: t(at),
            detail: json!({}),
        })
        .await
        .unwrap();
    if let Some(rc) = rc {
        cadence.record_outcome(&firing_id, rc, 30).await.unwrap();
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
    app_with_cadence(InMemoryCadence::new(rules), policy)
}

/// `app_with`, but over a cadence repository the test has already
/// written firings into.
fn app_with_cadence(
    cadence: InMemoryCadence,
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
    let cadence: Arc<dyn CadenceRepository> = Arc::new(cadence);
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
            "collect",
            "Collect what is ready to board",
            StepStatus::Completed,
            json!({ "completed_at": "2026-09-03T06:00:00Z" }),
        ),
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

    // An arrived train (recent), in the shape the server writes: the
    // conductor stamps `collect`, the `arrived` OUTCOME step is completed
    // bare by the terminal machinery, and the close instant lands on the
    // job as `closed_at` (`close_job_on_terminal`). Measured 2026-09-08
    // on every closed train; a fixture that stamped the step instead
    // passed while the live board read null.
    let arrived = job(
        "pr-train",
        "33333333-3333-3333-3333-333333333333",
        "train #199",
        JobStatus::Closed,
        json!({ "outcome": "arrived", "closed_at": "2026-09-03T05:30:00+00:00" }),
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
            json!({ "outcome_kind": "completed" }),
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

    // An IN-FLIGHT gate-run: open, no verdict yet — occupies a slot. It
    // carries the `opened_at` instant `boss gate` stamps.
    let gating = job(
        "gate-run",
        "66666666-6666-6666-6666-666666666666",
        "gate feat/gating",
        JobStatus::Open,
        json!({ "branch": "feat/gating", "opened_at": "2026-09-03T11:40:00Z" }),
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

    // A QUEUED gate-run: open, no verdict, and stamped `queued_at` —
    // `boss gate --wait` took a place in line because the build node was
    // at its bound. It holds NO bay, and until the queue lane existed the
    // floor showed nothing at all for it.
    let waiting = job(
        "gate-run",
        "99999999-9999-9999-9999-999999999999",
        "gate feat/waiting",
        JobStatus::Open,
        json!({ "branch": "feat/waiting", "queued_at": "2026-09-03T11:45:00Z" }),
    );
    jobs.create_job_at(&waiting, now, &[]).await.unwrap();
    jobs.add_step_at(
        &step(
            &waiting.id,
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

/// Two parked cars and nothing else — a dock with no train on the track,
/// so the board decision is about the dock and the cooldown alone.
async fn seed_dock_only(jobs: &InMemoryJobs) {
    let now = t(NOW);
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
    // "Aboard since": the collect stamp, an instant, additive on the row.
    assert_eq!(train["boarded_at"], "2026-09-03T06:00:00Z");
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

/// 2026-09-07, twice: the operator watched a threshold-met dock not
/// board and asked why. The answer lived only in the conductor's journal
/// — a cooldown with minutes left. Now the payload says so, with the
/// minutes, read from the board rule's own last firing.
#[tokio::test]
async fn a_recent_board_firing_reads_as_a_cooldown_hold_with_the_minutes_left() {
    // Threshold 2 so the two-car dock is met; no train on the track (an
    // open train would rank first — the next test).
    let mut d = depth_rule();
    d.min_dock_depth = Some(2);
    let cadence = InMemoryCadence::new(vec![d]);
    // Boarded 33 minutes before NOW, cleanly (rc 0): 120 − 33 = 87 left.
    fire(
        &cadence,
        "train-board-on-dock-depth",
        "board",
        "2026-09-03T11:27:00Z",
        Some(0),
    )
    .await;
    let (app, jobs) = app_with_cadence(cadence, vec![policy_row()]);
    seed_dock_only(&jobs).await;
    let (status, body) = get(&app, "operator").await;
    assert_eq!(status, StatusCode::OK);

    let b = &body["boarding"];
    assert_eq!(b["dock_depth"], 2);
    assert_eq!(b["threshold_met"], true);
    assert_eq!(b["held_because"], "cooldown — 87 min left");
    assert_eq!(b["cooldown_remaining_minutes"], 87);
    assert_eq!(b["last_board_at"], "2026-09-03T11:27:00Z");
    assert_eq!(
        b["next_board"],
        "boards on the next tick once the cooldown clears (87 min)"
    );
}

/// The single track is the hold the conductor checks first: a train
/// still before its merge holds the dock before its depth does, and the
/// sentence still names the depth that has to follow. The seed's own
/// open train is merged (mid-deploy) and does NOT count — two trains are
/// open, the track reads one.
#[tokio::test]
async fn an_open_train_reads_as_track_occupied_before_anything_else() {
    let (app, jobs) = app_with(vec![depth_rule(), clock_rule()], vec![policy_row()]);
    seed_full(&jobs).await;
    let now = t(NOW);
    // A fresh id: `seed_full` already owns 1111…7777, and the in-memory
    // `create_job_at` mirrors the Pg replay guard — an existing id is a
    // silent no-op, which is how a first draft of this test seeded
    // nothing and read one train.
    let pre_merge = job(
        "pr-train",
        "88888888-8888-8888-8888-888888888888",
        "train #201",
        JobStatus::Open,
        json!({}),
    );
    jobs.create_job_at(&pre_merge, now, &[]).await.unwrap();
    for s in [
        step(
            &pre_merge.id,
            "pr",
            "Open the batched PR",
            StepStatus::Completed,
            json!({ "completed_at": "2026-09-03T11:00:00Z" }),
        ),
        step(
            &pre_merge.id,
            "ci",
            "CI verdict",
            StepStatus::Ready,
            json!({}),
        ),
        step(
            &pre_merge.id,
            "merged",
            "Merged into main",
            StepStatus::Ready,
            json!({}),
        ),
    ] {
        jobs.add_step_at(&s, now, &[]).await.unwrap();
    }
    let (_, body) = get(&app, "operator").await;

    assert_eq!(body["trains"].as_array().map(Vec::len), Some(2));
    let b = &body["boarding"];
    assert_eq!(b["held_because"], "track occupied (1 open train)");
    assert!(b["cooldown_remaining_minutes"].is_null());
    assert!(b["last_board_at"].is_null());
    assert_eq!(
        b["next_board"],
        "boards on the next tick once the track clears and the dock reaches 4"
    );
}

/// 2026-09-07 (f3796323), twice: a merged train whose sha bricked its
/// boot sat at `converged`, and the board said the fix-forward car was
/// held by the track — the very train it would have converged. A merged
/// train's content is on main; the next consist merges on top of it and
/// converges it by ancestry, so it holds nothing. The seed's one open
/// train is merged and mid-deploy: the dock reads its depth, not a track.
#[tokio::test]
async fn a_merged_train_waiting_to_deploy_does_not_hold_the_track() {
    let (app, jobs) = app_with(vec![depth_rule(), clock_rule()], vec![policy_row()]);
    seed_full(&jobs).await;
    let (_, body) = get(&app, "operator").await;

    assert_eq!(body["trains"][0]["phase"], "deploying");
    let b = &body["boarding"];
    assert_eq!(b["held_because"], "below threshold (depth 2 of 4)");
    assert_eq!(
        b["next_board"],
        "boards on the next tick once the dock reaches 4"
    );
}

/// `conductor.last_verb` said `train-reconcile` — the RULE's name, passed
/// where the verb belonged. The verb is on the rule row; it is read there.
#[tokio::test]
async fn the_conductor_block_names_the_verb_it_ran_not_the_rule() {
    let cadence = InMemoryCadence::new(vec![depth_rule(), reconcile_rule()]);
    fire(
        &cadence,
        "train-reconcile",
        "reconcile",
        "2026-09-03T11:57:00Z",
        Some(0),
    )
    .await;
    let (app, jobs) = app_with_cadence(cadence, vec![policy_row()]);
    seed_full(&jobs).await;
    let (_, body) = get(&app, "operator").await;

    let c = &body["conductor"];
    assert_eq!(c["last_verb"], "reconcile");
    assert_eq!(c["last_rc"], 0);
    assert_eq!(c["silent_for_minutes"], 3);
    assert_eq!(c["expected_every_minutes"], 10);
    assert_eq!(c["silent"], false);
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

    // Recent: one arrived train with a journey time — collect 05:00 to
    // the job's `closed_at` 05:30, the outcome step itself unstamped.
    let recent = body["recent"].as_array().unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0]["outcome"], "arrived");
    assert_eq!(recent[0]["journey_seconds"], 1800);

    // Stranded: the green gate-run whose branch is no car. It names its
    // PACKET as well as its branch — the approach lane draws a wagon per
    // row and opens that packet, so recovering it client-side from a
    // window of gate-runs is what grew a second copy of "is this green
    // spent?" in the web lens (CLAUDE.md §9a, 2026-09-10).
    let stranded = body["stranded"].as_array().unwrap();
    assert_eq!(stranded.len(), 1);
    assert_eq!(stranded[0]["branch"], "feat/stranded");
    assert!(
        stranded[0]["packet_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "a stranded row names its gate-run packet: {}",
        stranded[0]
    );

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
    // The QUEUED run rides beside the active ones — its branch, its
    // packet and its place in line — and takes no bay. A queue an
    // operator cannot see reads as a gate that never launched.
    let queued = gates["queued"].as_array().unwrap();
    assert_eq!(queued.len(), 1);
    assert_eq!(queued[0]["branch"], "feat/waiting");
    assert_eq!(
        queued[0]["packet_id"],
        "99999999-9999-9999-9999-999999999999"
    );
    assert_eq!(queued[0]["position"], 1);
    assert_eq!(queued[0]["queued_at"], "2026-09-03T11:45:00Z");
    // The instant the run opened, so a bay can draw elapsed time — not
    // the day it opened on.
    assert_eq!(active[0]["since"], "2026-09-03T11:40:00Z");

    // The garage holds the branch whose latest gate is red, named with
    // its failing check.
    let garage = body["garage"].as_array().unwrap();
    assert_eq!(garage.len(), 1);
    assert_eq!(garage[0]["branch"], "feat/broken");
    assert_eq!(garage[0]["failed_check"], "test");
    assert!(
        garage[0]["packet_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "a garaged row names its gate-run packet: {}",
        garage[0]
    );
    // The gate EXIT is its own lane, always present: nothing here was
    // settled unjudged, so it is empty rather than absent.
    assert_eq!(body["limbo"].as_array().unwrap().len(), 0);
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
    // No cadence → the honest "no configured cadence" line, and no hold
    // invented for a depth rule that does not exist.
    assert!(body["boarding"]["dock_threshold"].is_null());
    assert!(body["boarding"]["held_because"].is_null());
    assert_eq!(
        body["boarding"]["next_board"],
        "no depth rule is configured — nothing boards on dock depth"
    );
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

/// A day of arrivals must not push the open train off the board.
///
/// The trains read was ONE query — "open OR closed since" — ordered by
/// `opened_on` (a DATE) and cut at `TRAIN_WINDOW`. On 2026-09-07 the yard
/// opened 82 trains in a day; the window sliced that same-day tie
/// wherever Postgres chose, the one open train fell out, and
/// `/api/yard/status` drew `trains: []` while
/// `/api/jobs?kind=pr-train&status=open` showed it — measured three
/// times at the yard's exact query (`limit=60`), absent every time, and
/// present at `limit=100`. The open trains are read on their own now.
///
/// The open train here is a day OLDER than the flood, which is when
/// `opened_on desc` puts it last deterministically in both adapters —
/// the same-day case is merely arbitrary, this one is certain. Red first:
/// with the single windowed read, `trains` comes back empty.
#[tokio::test]
async fn a_day_of_arrivals_does_not_push_the_open_train_off_the_board() {
    use boss_jobs::yard::{RECENT_LIMIT, TRAIN_WINDOW};

    let (app, jobs) = app_with(vec![depth_rule(), clock_rule()], vec![policy_row()]);
    let now = t(NOW);

    let mut in_flight = job(
        "pr-train",
        "11111111-1111-1111-1111-111111111111",
        "train #200",
        JobStatus::Open,
        json!({}),
    );
    in_flight.opened_on = NaiveDate::from_ymd_opt(2026, 9, 2).unwrap();
    jobs.create_job_at(&in_flight, now, &[]).await.unwrap();

    // Exactly one window of same-day arrivals, every one newer than the
    // open train and closed inside the retention window.
    for i in 0..TRAIN_WINDOW {
        let arrived = job(
            "pr-train",
            &format!("33333333-3333-3333-3333-{i:012}"),
            &format!("train #{}", 201 + i),
            JobStatus::Closed,
            json!({}),
        );
        jobs.create_job_at(&arrived, now, &[]).await.unwrap();
    }

    let (status, body) = get(&app, "operator").await;
    assert_eq!(status, StatusCode::OK);

    let trains = body["trains"].as_array().unwrap();
    assert_eq!(
        trains.len(),
        1,
        "the open train is on the board whatever arrived after it: {body}"
    );
    assert_eq!(trains[0]["id"], in_flight.id.to_string());
    assert_eq!(trains[0]["title"], "train #200");

    // The recent tail still reads the arrivals, capped as before.
    assert_eq!(body["recent"].as_array().unwrap().len(), RECENT_LIMIT);
}

/// An arrived train as the conductor records one: the outcome stamped,
/// the arrival report's timings, and the `closed_at` the server writes.
/// `board_to_merge_s` and the merge→`closed_at` gap are the two legs the
/// ETA measures.
fn arrived_train(i: i64, board_to_merge_s: i64, merge_to_arrival_s: i64) -> Job {
    let merged = t("2026-09-01T10:00:00Z");
    let closed = merged + chrono::Duration::seconds(merge_to_arrival_s);
    let mut j = job(
        "pr-train",
        &format!("44444444-4444-4444-4444-{i:012}"),
        &format!("arrived train #{i}"),
        JobStatus::Closed,
        json!({
            "outcome": "arrived",
            "closed_at": closed.to_rfc3339(),
            "arrival_report": { "timings": {
                "board_to_merge_s": board_to_merge_s,
                "merged_at": merged.to_rfc3339(),
            }},
        }),
    );
    // OLDER than the refusal flood, so `opened_on desc` puts every one of
    // these behind the noise — the position that makes a recency-only
    // read miss them entirely.
    j.opened_on = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
    j
}

/// A board the consist check refused: the Job opened, cancelled, carried
/// no cars and measured nothing. 696 of the 1,014 pr-trains on the live
/// record are this (measured 2026-09-10).
fn refused_train(i: i64) -> Job {
    job(
        "pr-train",
        &format!("55555555-5555-5555-5555-{i:012}"),
        &format!("refused train #{i}"),
        JobStatus::Closed,
        json!({ "outcome": "cancelled", "boarded_jobs": [] }),
    )
}

/// An open train that has boarded but not merged, 5 minutes before `NOW`.
fn boarding_train() -> (Job, Vec<Step>) {
    let mut j = job(
        "pr-train",
        "66666666-6666-6666-6666-666666666666",
        "PR train in flight",
        JobStatus::Open,
        json!({ "boarded_jobs": ["a", "b", "c"] }),
    );
    j.opened_on = NaiveDate::from_ymd_opt(2026, 9, 2).unwrap();
    let boarded = json!({ "completed_at": "2026-09-03T11:55:00Z" });
    let steps = vec![
        step(
            &j.id,
            "collect",
            "Collect what is ready to board",
            StepStatus::Completed,
            boarded.clone(),
        ),
        step(
            &j.id,
            "pr",
            "Open the batched PR",
            StepStatus::Completed,
            boarded,
        ),
        step(&j.id, "ci", "CI verdict", StepStatus::Active, json!({})),
        step(
            &j.id,
            "merged",
            "Merged into main",
            StepStatus::Ready,
            json!({}),
        ),
    ];
    (j, steps)
}

/// A TRAIN IN FLIGHT STATES ITS ETA — and the estimate reaches PAST the
/// refusal flood to find the arrivals it is measured from.
///
/// THE TRAP THIS PINS. A board the consist check refuses still opens a
/// pr-train Job and cancels it, so the recent population is
/// overwhelmingly zero-length cancellations: measured 2026-09-10, 696 of
/// 1,014 pr-trains are `cancelled`, and of the 40 most recent — the
/// window the departure board fetches client-side — exactly ONE had
/// arrived. Reaching 10 measurable arrivals from the newest end needs a
/// window 583 trains deep. So the arrived population is narrowed IN THE
/// QUERY (`metadata_contains`), not sampled by recency and filtered
/// after. This test puts a full `TRAIN_WINDOW` of refusals NEWER than
/// every arrival: a read that samples on recency sees nothing but zeros
/// and the ETA comes back unknown (or worse, near-instant).
#[tokio::test]
async fn a_train_in_flight_states_an_eta_measured_past_the_refusal_flood() {
    use boss_jobs::yard::{MIN_ETA_ARRIVALS, TRAIN_WINDOW};

    let (app, jobs) = app_with(vec![depth_rule(), clock_rule()], vec![policy_row()]);
    let now = t(NOW);

    let (in_flight, steps) = boarding_train();
    jobs.create_job_at(&in_flight, now, &[]).await.unwrap();
    for s in &steps {
        jobs.add_step_at(s, now, &[]).await.unwrap();
    }

    // A whole window of refusals, all NEWER than the arrivals below.
    for i in 0..TRAIN_WINDOW {
        jobs.create_job_at(&refused_train(i), now, &[])
            .await
            .unwrap();
    }
    // Exactly the floor's worth of measurable arrivals, spread so the
    // 10th/90th percentiles are distinct observations: board→merge
    // 900..1800, merge→arrival 600..1500.
    for i in 0..MIN_ETA_ARRIVALS {
        let k = i64::try_from(i).unwrap();
        jobs.create_job_at(&arrived_train(k, 900 + k * 100, 600 + k * 100), now, &[])
            .await
            .unwrap();
    }

    let (status, body) = get(&app, "operator").await;
    assert_eq!(status, StatusCode::OK);

    // The recency window really is drowned — this is the noise a naive
    // sample would have averaged.
    let recent = body["recent"].as_array().unwrap();
    assert!(
        recent.iter().all(|r| r["outcome"] == "cancelled"),
        "the recent tail is all refusals, which is the point: {recent:?}"
    );

    let eta = &body["trains"][0]["eta"];
    assert_eq!(
        eta["kind"], "estimate",
        "the arrivals are behind a window of refusals — only a filter in \
         the query reaches them: {body}"
    );
    assert_eq!(
        eta["leg"], "boarding → arrival",
        "the leg is named on the wire, so a reader never guesses which one"
    );
    assert_eq!(eta["sample_size"], MIN_ETA_ARRIVALS);
    // 5 minutes (300s) into the board→merge leg. median 1400/1100,
    // p10 1000/700, p90 1800/1500.
    assert_eq!(eta["remaining_seconds"], (1400 - 300) + 1100);
    assert_eq!(eta["remaining_low_seconds"], (1000 - 300) + 700);
    assert_eq!(eta["remaining_high_seconds"], (1800 - 300) + 1500);
    assert_eq!(eta["overdue"], false);
    assert!(
        eta["basis"].as_str().unwrap().contains("arrivals"),
        "the number arrives with its provenance: {eta}"
    );
}

/// Thin history REFUSES, and names what it was short of. A number drawn
/// from three arrivals reads as a promise; "3 arrived, 10 needed" sends
/// nobody to re-derive why there is no figure.
#[tokio::test]
async fn too_few_arrivals_refuse_with_a_stated_reason() {
    use boss_jobs::yard::MIN_ETA_ARRIVALS;

    let (app, jobs) = app_with(vec![depth_rule(), clock_rule()], vec![policy_row()]);
    let now = t(NOW);

    let (in_flight, steps) = boarding_train();
    jobs.create_job_at(&in_flight, now, &[]).await.unwrap();
    for s in &steps {
        jobs.add_step_at(s, now, &[]).await.unwrap();
    }
    for i in 0..20 {
        jobs.create_job_at(&refused_train(i), now, &[])
            .await
            .unwrap();
    }
    for i in 0..3 {
        jobs.create_job_at(&arrived_train(i, 1200, 1100), now, &[])
            .await
            .unwrap();
    }

    let (status, body) = get(&app, "operator").await;
    assert_eq!(status, StatusCode::OK);

    let eta = &body["trains"][0]["eta"];
    assert_eq!(eta["kind"], "unknown", "3 arrivals is an anecdote: {body}");
    let reason = eta["reason"].as_str().unwrap();
    assert!(reason.contains('3'), "name what it found: {reason}");
    assert!(
        reason.contains(&MIN_ETA_ARRIVALS.to_string()),
        "name the floor it fell short of: {reason}"
    );
    assert!(
        eta.get("remaining_seconds").is_none(),
        "no number rides alongside a refusal: {eta}"
    );
}

/// A MERGED train is told the remaining leg only. merge→arrival measured
/// a median of 1,183s — HALF the journey — so handing a merged train the
/// whole-journey figure roughly doubles what it has left.
#[tokio::test]
async fn a_merged_train_is_estimated_on_the_remaining_leg_only() {
    use boss_jobs::yard::MIN_ETA_ARRIVALS;

    let (app, jobs) = app_with(vec![depth_rule(), clock_rule()], vec![policy_row()]);
    let now = t(NOW);

    let mut merged_train = job(
        "pr-train",
        "77777777-7777-7777-7777-777777777777",
        "PR train past the merge",
        JobStatus::Open,
        json!({ "boarded_jobs": ["a"] }),
    );
    merged_train.opened_on = NaiveDate::from_ymd_opt(2026, 9, 2).unwrap();
    let merged_at = json!({ "completed_at": "2026-09-03T11:55:00Z" });
    let steps = vec![
        step(
            &merged_train.id,
            "collect",
            "Collect what is ready to board",
            StepStatus::Completed,
            json!({ "completed_at": "2026-09-03T11:00:00Z" }),
        ),
        step(
            &merged_train.id,
            "pr",
            "Open the batched PR",
            StepStatus::Completed,
            json!({ "completed_at": "2026-09-03T11:00:00Z" }),
        ),
        step(
            &merged_train.id,
            "ci",
            "CI verdict",
            StepStatus::Completed,
            merged_at.clone(),
        ),
        step(
            &merged_train.id,
            "merged",
            "Merged into main",
            StepStatus::Completed,
            merged_at,
        ),
        step(
            &merged_train.id,
            "deployed",
            "Deployed to the playground",
            StepStatus::Ready,
            json!({}),
        ),
        step(
            &merged_train.id,
            "converged",
            "Cluster converged",
            StepStatus::Pending,
            json!({}),
        ),
    ];
    jobs.create_job_at(&merged_train, now, &[]).await.unwrap();
    for s in &steps {
        jobs.add_step_at(s, now, &[]).await.unwrap();
    }
    for i in 0..MIN_ETA_ARRIVALS {
        let k = i64::try_from(i).unwrap();
        jobs.create_job_at(&arrived_train(k, 900 + k * 100, 600 + k * 100), now, &[])
            .await
            .unwrap();
    }

    let (_, body) = get(&app, "operator").await;
    let eta = &body["trains"][0]["eta"];
    assert_eq!(eta["kind"], "estimate");
    assert_eq!(
        eta["leg"], "merge → arrival",
        "a merged train is estimated on the leg it is actually on: {body}"
    );
    // 5 minutes past the merge, against the merge→arrival median of 1100.
    assert_eq!(eta["remaining_seconds"], 1100 - 300);
    assert!(
        eta["remaining_seconds"].as_i64().unwrap() < (1400 - 300) + 1100,
        "less than the whole-journey figure — mixing the legs is the \
         defect this pins"
    );
}
