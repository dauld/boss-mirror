//! The agent-run record, exercised THROUGH the port.
//!
//! Backlog 83344e16: "an agent run leaves no record: which model ran,
//! what it cost, how long it took." These tests ask the four questions
//! that packet says nobody could answer, using the four runs measured
//! on 2026-09-10 as the fixture — so the test data is the evidence the
//! packet was filed on, not invented numbers.
//!
//! In-memory adapter, not a mock: the Pg adapter calls the same
//! `price_run`, collapses on the same `run_id`, and orders by the same
//! key, so what passes here is the contract both sides owe.

use boss_core::actor::ActorId;
use boss_jobs::agent_runs::{
    AGENT_RUN_RECORDED, AgentRunLog, InMemoryAgentRuns, NewAgentRun, RateCardRow, RunFilter,
    RunOutcome, summarize,
};
use chrono::{DateTime, Duration, Utc};

fn card() -> Vec<RateCardRow> {
    vec![
        RateCardRow {
            model: "opus-5".into(),
            input_usd_micros_per_mtok: 5_000_000,
            output_usd_micros_per_mtok: 25_000_000,
            note: "Claude Opus 5 — $5.00/$25.00 per MTok".into(),
        },
        RateCardRow {
            model: "haiku-4-5".into(),
            input_usd_micros_per_mtok: 1_000_000,
            output_usd_micros_per_mtok: 5_000_000,
            note: "Claude Haiku 4.5 — $1.00/$5.00 per MTok".into(),
        },
    ]
}

fn at(s: &str) -> DateTime<Utc> {
    s.parse().expect("fixture timestamp parses")
}

/// One of the four runs measured on 2026-09-10, split 9:1
/// input:output (the harness reports a single total; the split is the
/// fixture's, not a claim about the real call mix).
fn measured(branch: &str, total_tokens: u64, tool_calls: u32, minutes: i64) -> NewAgentRun {
    let started = at("2026-09-10T01:00:00Z");
    NewAgentRun {
        run_id: format!("run-{branch}"),
        actor_id: ActorId::agent("claude", "opus-5"),
        started_at: started,
        finished_at: started + Duration::minutes(minutes),
        outcome: RunOutcome::Success,
        error: None,
        input_tokens: total_tokens * 9 / 10,
        output_tokens: total_tokens / 10,
        tool_calls,
        job_id: None,
        branch: Some(branch.to_string()),
        detail: serde_json::json!({"host": "dev-pod"}),
    }
}

fn the_four() -> Vec<NewAgentRun> {
    vec![
        measured(
            "fix/the-mirrors-own-merge-is-not-foreign-work",
            142_982,
            52,
            10,
        ),
        measured("fix/a-parked-car-triages-its-item", 218_218, 91, 14),
        measured("fix/ci-images-are-pruned-by-age", 204_346, 68, 15),
        measured("feat/a-verb-answers-which-rules-are-live", 196_036, 98, 18),
    ]
}

fn filer() -> ActorId {
    ActorId::agent("claude", "opus-5[1m]")
}

#[tokio::test]
async fn recording_a_run_answers_model_cost_and_duration() {
    let log = InMemoryAgentRuns::new(card());
    let out = log
        .record_run(&measured("feat/x", 142_982, 52, 10), &filer())
        .await
        .expect("a well-formed run records");

    assert!(out.recorded);
    // The three things the packet says nothing recorded.
    assert_eq!(out.run.model(), Some("opus-5"), "which model ran");
    assert_eq!(out.run.duration_secs(), 600, "how long it took");
    assert!(out.run.usd_micros.is_some(), "what it cost");
    assert_eq!(out.run.priced_by.as_deref(), Some("opus-5"));
    // 128,683 input * $5 + 14,298 output * $25 per MTok.
    assert_eq!(out.run.usd_micros, Some(1_000_865));
    // And the two the packet also names.
    assert_eq!(out.run.run.tool_calls, 52);
    assert_eq!(
        out.run.run.branch.as_deref(),
        Some("feat/x"),
        "which car it produced"
    );
}

#[tokio::test]
async fn recording_a_run_puts_a_fact_on_the_log() {
    let log = InMemoryAgentRuns::new(card());
    log.record_run(&measured("feat/x", 1_000, 1, 1), &ActorId::human("emp-032"))
        .await
        .expect("records");

    let events = log.recorded_events().await;
    assert_eq!(events.len(), 1, "the log is the system of record");
    assert_eq!(events[0].kind, AGENT_RUN_RECORDED);
    // The filer rides as `_actor`; the run's own CPU is `actor_id`.
    assert_eq!(events[0].payload["_actor"], "emp-032");
    assert_eq!(events[0].payload["actor_id"], "claude:opus-5");
    // The payload carries the whole run, which is what the rebuilder
    // reconstructs the row from.
    assert_eq!(events[0].payload["tool_calls"], 1);
    assert_eq!(events[0].payload["usd_micros"], 7_000);
}

#[tokio::test]
async fn a_retried_report_records_the_run_once() {
    let log = InMemoryAgentRuns::new(card());
    let run = measured("feat/x", 1_000, 1, 1);

    let first = log.record_run(&run, &filer()).await.expect("records");
    let second = log.record_run(&run, &filer()).await.expect("collapses");

    assert!(first.recorded);
    assert!(!second.recorded, "the same run_id must not record twice");
    assert_eq!(second.run, first.run, "the retry sees the held record");
    assert_eq!(
        log.recorded_events().await.len(),
        1,
        "and puts no second event on the log"
    );
}

#[tokio::test]
async fn what_did_this_car_cost_to_build() {
    let log = InMemoryAgentRuns::new(card());
    for run in the_four() {
        log.record_run(&run, &filer()).await.expect("records");
    }

    let runs = log
        .list_runs(&RunFilter {
            branch: Some("fix/ci-images-are-pruned-by-age".into()),
            ..Default::default()
        })
        .await
        .expect("lists");
    assert_eq!(runs.len(), 1);

    let summary = summarize(&runs);
    assert_eq!(summary.runs, 1);
    assert_eq!(summary.tool_calls, 68);
    assert_eq!(summary.wall_secs, 900);
    assert_eq!(summary.unpriced_runs, 0);
    // 183,911 input * $5 + 20,434 output * $25 per MTok = $1.4304…
    assert_eq!(summary.usd_micros, Some(1_430_405));
    assert_eq!(summary.by_model.len(), 1);
    assert_eq!(summary.by_model[0].key, "opus-5");
}

#[tokio::test]
async fn what_did_the_whole_session_cost() {
    let log = InMemoryAgentRuns::new(card());
    for run in the_four() {
        log.record_run(&run, &filer()).await.expect("records");
    }

    let all = log.list_runs(&RunFilter::default()).await.expect("lists");
    let summary = summarize(&all);

    assert_eq!(summary.runs, 4);
    // The packet's headline number, now answerable: ~761,000 tokens.
    assert_eq!(summary.input_tokens + summary.output_tokens, 761_578);
    assert_eq!(summary.tool_calls, 52 + 91 + 68 + 98);
    assert_eq!(summary.wall_secs, (10 + 14 + 15 + 18) * 60);
    assert!(
        summary.usd_micros.expect("all four priced") > 0,
        "a cost-per-car figure exists at all"
    );
    assert_eq!(summary.by_branch.len(), 4, "one bucket per car");
}

#[tokio::test]
async fn an_unpriced_run_is_recorded_and_the_total_refuses_to_understate() {
    let log = InMemoryAgentRuns::new(card());
    let mut unknown = measured("feat/unknown-model", 1_000, 1, 1);
    unknown.actor_id = ActorId::agent("claude", "some-future-model");

    let out = log.record_run(&unknown, &filer()).await.expect("records");
    assert!(out.recorded, "an unpriced run is still a fact");
    assert_eq!(out.run.usd_micros, None, "unpriced, not free");
    assert_eq!(out.run.run.input_tokens, 900, "tokens are kept regardless");

    log.record_run(&measured("feat/x", 1_000, 1, 1), &filer())
        .await
        .expect("records");

    let summary = summarize(&log.list_runs(&RunFilter::default()).await.expect("lists"));
    assert_eq!(summary.runs, 2);
    assert_eq!(summary.unpriced_runs, 1);
    assert_eq!(
        summary.usd_micros, None,
        "a total that skipped the unpriced run would look complete and be wrong"
    );
}

#[tokio::test]
async fn a_run_whose_actor_is_not_an_agent_is_refused() {
    let log = InMemoryAgentRuns::new(card());
    let mut run = measured("feat/x", 1_000, 1, 1);
    run.actor_id = ActorId::automation("train-conductor");

    let err = log
        .record_run(&run, &filer())
        .await
        .expect_err("an agent run must name an agent CPU")
        .to_string();
    assert!(
        err.contains("<mode>:<model>"),
        "the refusal names its fix: {err}"
    );
    assert!(
        log.recorded_events().await.is_empty(),
        "and records nothing"
    );
}

#[tokio::test]
async fn runs_list_newest_finish_first_with_a_total_order() {
    let log = InMemoryAgentRuns::new(card());
    for run in the_four() {
        log.record_run(&run, &filer()).await.expect("records");
    }

    let runs = log.list_runs(&RunFilter::default()).await.expect("lists");
    let minutes: Vec<i64> = runs.iter().map(|r| r.duration_secs() / 60).collect();
    assert_eq!(minutes, vec![18, 15, 14, 10]);

    // A limit is a window, and the window starts at the newest.
    let top = log
        .list_runs(&RunFilter {
            limit: Some(2),
            ..Default::default()
        })
        .await
        .expect("lists");
    assert_eq!(top.len(), 2);
    assert_eq!(top[0].duration_secs() / 60, 18);
}

#[tokio::test]
async fn runs_filter_by_actor_and_by_finish_time() {
    let log = InMemoryAgentRuns::new(card());
    let mut cheap = measured("feat/cheap", 1_000, 1, 1);
    cheap.actor_id = ActorId::agent("claude", "haiku-4-5");
    log.record_run(&cheap, &filer()).await.expect("records");
    log.record_run(&measured("feat/x", 1_000, 1, 30), &filer())
        .await
        .expect("records");

    let haiku = log
        .list_runs(&RunFilter {
            actor_id: Some("claude:haiku-4-5".into()),
            ..Default::default()
        })
        .await
        .expect("lists");
    assert_eq!(haiku.len(), 1);
    assert_eq!(haiku[0].model(), Some("haiku-4-5"));

    // The window a budget question asks over: runs finishing after T.
    let recent = log
        .list_runs(&RunFilter {
            since: Some(at("2026-09-10T01:05:00Z")),
            ..Default::default()
        })
        .await
        .expect("lists");
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].run.branch.as_deref(), Some("feat/x"));
}

#[tokio::test]
async fn a_packet_can_be_asked_what_its_agents_spent() {
    let log = InMemoryAgentRuns::new(card());
    let packet = uuid::Uuid::new_v4();
    let mut on_packet = measured("feat/x", 10_000, 5, 3);
    on_packet.job_id = Some(packet);
    log.record_run(&on_packet, &filer()).await.expect("records");
    log.record_run(&measured("feat/y", 10_000, 5, 3), &filer())
        .await
        .expect("records");

    let runs = log
        .list_runs(&RunFilter {
            job_id: Some(packet),
            ..Default::default()
        })
        .await
        .expect("lists");
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].run.job_id, Some(packet));
}

#[tokio::test]
async fn the_rate_card_is_readable_so_a_price_can_be_checked() {
    let log = InMemoryAgentRuns::new(card());
    let rows = log.rate_card().await.expect("reads");
    assert_eq!(
        rows.iter().map(|r| r.model.as_str()).collect::<Vec<_>>(),
        vec!["haiku-4-5", "opus-5"],
        "model-ordered"
    );
    assert!(
        rows.iter().all(|r| !r.note.is_empty()),
        "every price says where it came from"
    );
}
