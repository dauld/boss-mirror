//! A station that omits a whole kind of work says so — backlog abda9ab4.
//!
//! THE DEFECT, measured 2026-09-22 on the live instance:
//! `a.platform-admin.opus-5-1m` answered its queue with 124 packets
//! while 57 open packets carrying a ready step the ACTIVE protocol
//! routes to that queue were ABSENT from it, because they were
//! admitted under a version that declared no agent block and so carry
//! none of the keys the predicate reads. Forty-seven were the
//! page-audit initiative, which sat three days where no agent could
//! find it. Every number the station reported was right about the rows
//! it could see; there was no error, no warning, and no figure that
//! looked wrong.
//!
//! The unit tests in `station_reach` prove the arithmetic. They cannot
//! prove the thing that matters to an operator: that the figure
//! reaches the surface that draws the queue, beside the depth it
//! corrects. That is what this exercises, through the real HTTP
//! handler, with the drift produced the way production produces it —
//! a packet admitted under v1, then v2 published over it.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::agent_spec::{AgentSpec, Effort};
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::registry::{StepSpec, WorkflowSpec};
use boss_jobs::station_queue::{StationPredicate, StepMatch};
use boss_jobs::{
    InMemoryJobs, InMemoryStations, InMemoryWorkflows, StationKind, StationRegistry, StationSpec,
    WorkflowRegistry,
};
use boss_policy_client::types::{AccessTier, User};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

const STATION: &str = "a.platform-admin.opus-5-1m";

fn user_header(id: &str, role: &str) -> String {
    serde_json::to_string(&User {
        id: id.to_string(),
        role: role.to_string(),
        access_tier: AccessTier::Operator,
        territory_account_ids: Vec::new(),
        direct_report_ids: Vec::new(),
        department: Some("it".to_string()),
    })
    .expect("a User always serialises")
}

/// v1: a `build` step with a role and NO agent block — the protocol
/// the drifted packets were admitted under.
fn protocol_v1() -> WorkflowSpec {
    WorkflowSpec::platform_seed(
        "backlog-item",
        "Backlog item",
        "it",
        vec!["custom".into()],
        vec![StepSpec {
            title: "build".into(),
            kind: "task".into(),
            ready_when: "true".into(),
            title_template: "Build the change".into(),
            authority_role: Some("platform-admin".into()),
            terminal: Some(boss_jobs::registry::Terminal {
                outcome: "succeeded".into(),
            }),
            ..Default::default()
        }],
    )
}

/// v2: the same step, now declaring the agent block — the ACTIVE
/// protocol, which routes this step to the agent's station.
fn protocol_v2() -> WorkflowSpec {
    let mut spec = protocol_v1();
    spec.steps[0].agent = Some(AgentSpec {
        profile: "builder".into(),
        model: "opus-5[1m]".into(),
        budget_usd: 5.0,
        effort: Effort::High,
    });
    spec
}

/// The live row, reduced to the two projected keys it reads.
fn agent_station() -> StationSpec {
    let mut spec = StationSpec::draft(
        STATION,
        "Agent queue",
        StationKind::Constraint,
        StationPredicate {
            status: Some(boss_core::job::JobStatus::Open),
            step: Some(StepMatch {
                status_in: vec![
                    boss_core::job::StepStatus::Ready,
                    boss_core::job::StepStatus::Active,
                ],
                metadata_equals: [
                    ("agent_model".to_string(), "opus-5[1m]".to_string()),
                    ("authority_role".to_string(), "platform-admin".to_string()),
                ]
                .into_iter()
                .collect(),
                ..Default::default()
            }),
            ..Default::default()
        },
        chrono::Utc::now(),
    );
    spec.status = boss_jobs::registry::WorkflowStatus::Active;
    spec
}

struct Harness {
    app: axum::Router,
    kinds: Arc<InMemoryWorkflows>,
}

fn harness() -> Harness {
    let kinds = Arc::new(InMemoryWorkflows::new());
    kinds.seed(protocol_v1()).expect("seed v1");
    let stations = Arc::new(InMemoryStations::new());
    stations.seed(agent_station()).expect("seed station");
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Create,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
            .build(),
    );
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let state = JobsApiState {
        stations: Some(stations as Arc<dyn StationRegistry>),
        kind_registry: Some(kinds.clone() as Arc<dyn WorkflowRegistry>),
        ..JobsApiState::minimal(
            Arc::new(InMemoryJobs::new()),
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    Harness {
        app: router(state),
        kinds,
    }
}

async fn open_packet(app: &axum::Router, id: &str) {
    let body = serde_json::json!({
        "kind": "backlog-item",
        "subject": { "subject_kind": "custom", "id": id },
        "title": format!("packet {id}"),
        "owner_id": "emp-david",
        "status": "open",
        "priority": "standard",
        "opened_on": "2026-09-22",
        "metadata": {},
        "tags": [],
    });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/api/jobs")
                .header("content-type", "application/json")
                .header("x-boss-user", user_header("emp-david", "platform-admin"))
                .body(Body::from(serde_json::to_vec(&body).expect("body")))
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
}

async fn load_row(app: &axum::Router) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::get("/api/stations/load")
                .header("x-boss-user", user_header("emp-david", "platform-admin"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let v: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    v["data"]
        .as_array()
        .expect("data array")
        .iter()
        .find(|r| r["station"] == STATION)
        .cloned()
        .unwrap_or_else(|| panic!("`{STATION}` absent from the load: {v:#}"))
}

/// THE MEASURED DEFECT, at the surface. A packet admitted under v1
/// carries neither projected key, so the station's depth omits it —
/// and the row now says so beside that depth, which is the whole
/// point: a count smaller than it should be is invisible unless the
/// difference is printed next to it.
#[tokio::test]
async fn a_drifted_packet_is_reported_beside_the_depth_it_is_missing_from() {
    let h = harness();
    open_packet(&h.app, "pinned-to-v1").await;

    let actor = boss_core::actor::ActorId::human("emp-david");
    let now = chrono::Utc::now();
    h.kinds
        .create_draft(protocol_v2(), &actor, now)
        .await
        .expect("draft v2");
    h.kinds
        .publish("backlog-item", &actor, now)
        .await
        .expect("publish v2");

    let row = load_row(&h.app).await;
    assert_eq!(
        row["depth"], 0,
        "the drifted packet must not be a member — that is the defect, not the fix: {row:#}"
    );
    assert_eq!(
        row["unreachable"], 1,
        "the omission must be REPORTED, or the station answers a correct-looking total: {row:#}"
    );
}

/// A packet admitted under the ACTIVE protocol carries both keys, so
/// it is a member and nothing is unreachable. Zero on a healthy
/// network is the property that makes the figure worth reading — a
/// number that is routinely non-zero for benign reasons is noise, and
/// this is the control that says it is not.
#[tokio::test]
async fn a_packet_admitted_under_the_active_protocol_reports_zero() {
    let h = harness();
    let actor = boss_core::actor::ActorId::human("emp-david");
    let now = chrono::Utc::now();
    h.kinds
        .create_draft(protocol_v2(), &actor, now)
        .await
        .expect("draft v2");
    h.kinds
        .publish("backlog-item", &actor, now)
        .await
        .expect("publish v2");

    open_packet(&h.app, "admitted-under-v2").await;

    let row = load_row(&h.app).await;
    assert_eq!(row["depth"], 1, "{row:#}");
    assert_eq!(
        row["unreachable"], 0,
        "a station with no drift must read zero: {row:#}"
    );
}
