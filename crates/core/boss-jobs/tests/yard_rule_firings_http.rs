//! `GET /api/yard/rule-firings` — every dispatcher rule's newest firing
//! and its dead-letters, end-to-end through the real router against the
//! in-memory adapters (backlog 43c4451a, found by page-audit 08a444bc).
//!
//! The rules list at `/it/registry/rules` could not say when a rule last
//! fired or whether its handler was failing, so a stalled
//! `auto-park-on-gate-green` and an idle one painted the same row. What
//! this pins:
//!
//! 1. **Every rule the firing record holds is answered once**, with its
//!    newest instant — the rules list's second reading of the record the
//!    world map's borders already read.
//! 2. **The dead-letters roll up per rule** from the packets carrying
//!    one, inside the firing record's own retention window, naming the
//!    packet that holds the newest.
//! 3. **Unread is not empty.** A jobs API with no firing record wired,
//!    or a caller whose scope reads no packets, gets `null` with the
//!    reason — never an empty list a page would paint as "never fired"
//!    or "no failures".

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::job::{Job, JobId, JobStatus, Priority, Subject};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::dispatcher_firings::{
    DispatcherFiringsRepository, InMemoryDispatcherFirings, LastFiring, RETENTION_DAYS,
};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::{InMemoryJobs, JobsRepository};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use chrono::{DateTime, NaiveDate, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;

const NOW: &str = "2026-09-24T12:00:00Z";

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

fn firing(rule: &str, at: &str) -> (String, LastFiring) {
    (
        rule.to_string(),
        LastFiring {
            firing_id: format!("dispatcher:{rule}:{at}"),
            fired_on: "step.done.gate-verdict".into(),
            fired_at: t(at),
        },
    )
}

fn app(firings: Option<InMemoryDispatcherFirings>) -> (axum::Router, Arc<InMemoryJobs>) {
    let jobs = Arc::new(InMemoryJobs::new());
    let policy_client: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow("operator", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let dispatcher_firings = firings.map(|f| Arc::new(f) as Arc<dyn DispatcherFiringsRepository>);
    let state = JobsApiState {
        dispatcher_firings,
        ..JobsApiState::minimal(
            jobs.clone(),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy_client,
            Arc::new(boss_clock_client::FixedClockClient::new(
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
        )
    };
    (router(state), jobs)
}

fn packet(id: &str, status: JobStatus, metadata: Value) -> Job {
    Job {
        id: JobId::from_uuid(Uuid::parse_str(id).unwrap()),
        kind: "maintenance-sweep".into(),
        workflow_version: 1,
        subject: Subject::new("custom", "s"),
        title: "a sweep".into(),
        owner_id: "emp-david".into(),
        status,
        priority: Priority::Standard,
        opened_on: NaiveDate::from_ymd_opt(2026, 9, 24).unwrap(),
        opened_at: None,
        due_on: None,
        closed_on: (status == JobStatus::Closed)
            .then(|| NaiveDate::from_ymd_opt(2026, 9, 24).unwrap()),
        metadata,
        tags: vec![],
        partition: boss_core::partition::Partition::Real,
    }
}

fn dead_letter(rule: &str, recorded_at: &str) -> Value {
    json!({ "dead_letter": {
        "rule": rule,
        "handler": "jobs.auto-park",
        "error": "POST /api/jobs returned 503",
        "failures": [format!("{rule}/jobs.auto-park: POST /api/jobs returned 503")],
        "attempts": 8,
        "class": "budget-exhausted",
        "topic": "step.done.gate-verdict",
        "event_id": "evt-1",
        "recorded_at": recorded_at,
    }})
}

async fn get(app: &axum::Router, role: &str) -> (StatusCode, Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/yard/rule-firings")
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
async fn every_rule_answers_its_newest_firing_and_its_dead_letters() {
    let (app, jobs) = app(Some(
        InMemoryDispatcherFirings::new(vec![
            firing("auto-park-on-gate-green", "2026-09-24T09:00:00Z"),
            firing("auto-park-on-gate-green", "2026-09-24T11:00:00Z"),
            firing("complete-marker-on-step-ready", "2026-09-23T08:00:00Z"),
        ])
        // 4b175523: a dead-letter on a topic that names no packet is a
        // row in the firing record, and the rollup counts it.
        .with_unrouted_dead_letters(vec![
            ("issue-invoice".into(), t("2026-09-24T10:00:00Z")),
            ("issue-invoice".into(), t("2026-09-24T10:30:00Z")),
        ]),
    ));
    let now = t(NOW);
    // The stalled shape: the rule fired at 11:00 and dead-lettered
    // after it, on two packets — one of them since closed by hand.
    for (id, status, md) in [
        (
            "11111111-1111-1111-1111-111111111111",
            JobStatus::Open,
            dead_letter("auto-park-on-gate-green", "2026-09-24T11:30:00Z"),
        ),
        (
            "22222222-2222-2222-2222-222222222222",
            JobStatus::Closed,
            dead_letter("auto-park-on-gate-green", "2026-09-24T11:45:00Z"),
        ),
        // Outside the firing record's window: not this period's failure.
        (
            "33333333-3333-3333-3333-333333333333",
            JobStatus::Open,
            dead_letter("complete-marker-on-step-ready", "2026-07-01T00:00:00Z"),
        ),
        (
            "44444444-4444-4444-4444-444444444444",
            JobStatus::Open,
            json!({ "channel": "monitoring" }),
        ),
    ] {
        jobs.create_job_at(&packet(id, status, md), now, &[])
            .await
            .unwrap();
    }

    let (status, v) = get(&app, "operator").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["retention_days"], RETENTION_DAYS);
    assert_eq!(v["now"], "2026-09-24T12:00:00Z");

    let firings = v["firings"].as_array().expect("firings read: {v}");
    assert_eq!(firings.len(), 2, "one entry per rule, never every firing");
    assert_eq!(firings[0]["rule"], "auto-park-on-gate-green");
    assert_eq!(firings[0]["fired_at"], "2026-09-24T11:00:00Z");
    assert_eq!(firings[0]["fired_on"], "step.done.gate-verdict");
    assert_eq!(firings[1]["rule"], "complete-marker-on-step-ready");
    assert_eq!(v["firings_error"], Value::Null);

    let dead = v["dead_letters"]
        .as_array()
        .expect("dead letters read: {v}");
    assert_eq!(dead.len(), 2, "{v}");
    assert_eq!(dead[0]["rule"], "auto-park-on-gate-green");
    assert_eq!(dead[0]["packets"], 2);
    assert_eq!(dead[0]["unrouted"], 0);
    assert_eq!(dead[0]["newest_at"], "2026-09-24T11:45:00Z");
    assert_eq!(
        dead[0]["newest_job_id"],
        "22222222-2222-2222-2222-222222222222"
    );
    assert_eq!(dead[1]["rule"], "issue-invoice");
    assert_eq!(dead[1]["packets"], 0);
    assert_eq!(dead[1]["unrouted"], 2, "{v}");
    assert_eq!(dead[1]["newest_at"], "2026-09-24T10:30:00Z");
    assert_eq!(dead[1]["newest_job_id"], Value::Null, "no packet to open");
    assert_eq!(v["dead_letters_error"], Value::Null);
}

#[tokio::test]
async fn an_unwired_record_and_an_unscoped_caller_answer_null_with_the_reason() {
    // No firing record wired to this jobs API: unread, and said so —
    // an empty list here would paint every rule "no firing recorded".
    let (unwired, _) = app(None);
    let (status, v) = get(&unwired, "operator").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["firings"], Value::Null, "{v}");
    assert!(
        v["firings_error"]
            .as_str()
            .is_some_and(|e| e.contains("not wired")),
        "{v}"
    );
    // The dead-letters that name no packet live in that same record, so
    // the count is unread too — never "none" missing a half (4b175523).
    assert_eq!(v["dead_letters"], Value::Null, "{v}");
    assert!(
        v["dead_letters_error"]
            .as_str()
            .is_some_and(|e| e.contains("name no packet")),
        "{v}"
    );

    // A caller whose policy reads no packets cannot be told there are no
    // dead-letters on them: unknown, with the reason.
    let (wired, _) = app(Some(InMemoryDispatcherFirings::new(vec![])));
    let (status, v) = get(&wired, "nobody").await;
    assert_eq!(status, StatusCode::OK, "{v}");
    assert_eq!(v["firings"], json!([]));
    assert_eq!(v["dead_letters"], Value::Null, "{v}");
    assert!(
        v["dead_letters_error"]
            .as_str()
            .is_some_and(|e| e.contains("scope")),
        "{v}"
    );
}
