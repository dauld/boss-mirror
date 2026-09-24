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
//! THE REPAIR, backlog 51aef4dd (2026-09-23). Reporting the omission
//! was half of it; the other half is that the station now SERVES those
//! packets. A step carrying no agent projection is resolved against
//! its kind's ACTIVE Workflow row before any station clause reads it
//! (`boss_jobs::agent_spec::resolved`), in memory, writing nothing to
//! the packet — so the pinned packet is a member, its queue lists it,
//! the claim door admits it through that station, and the omission
//! figure falls to zero because there is no longer an omission.
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
            // The claim door's own policy check, so a claim through the
            // station reaches the membership gate this file is about.
            .allow(
                "platform-admin",
                Action::Update,
                Resource::step(),
                Scope::All,
            )
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

async fn open_packet(app: &axum::Router, id: &str) -> String {
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
    let job: serde_json::Value = serde_json::from_slice(&bytes).expect("job json");
    job["id"].as_str().expect("job id").to_string()
}

async fn get_json(app: &axum::Router, path: &str) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::get(path)
                .header("x-boss-user", user_header("emp-david", "platform-admin"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    assert_eq!(
        status,
        StatusCode::OK,
        "{path}: {}",
        String::from_utf8_lossy(&bytes)
    );
    serde_json::from_slice(&bytes).expect("json")
}

/// Admit a packet under v1, then publish v2 over it — the drift
/// produced the way production produced it for the 47 page-audit
/// packets (pinned at v1, active row v3).
async fn a_packet_pinned_before_the_block(h: &Harness) -> String {
    let id = open_packet(&h.app, "pinned-to-v1").await;
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
    id
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

/// THE MEASURED DEFECT, repaired at the surface. A packet admitted
/// under v1 carries no agent projection; its kind's ACTIVE row routes
/// the step to this station, so it is a member — depth counts it — and
/// nothing is unreachable. Until 51aef4dd this read depth 0 and
/// unreachable 1: the omission reported, not repaired.
#[tokio::test]
async fn a_packet_pinned_before_its_block_is_a_member_of_the_station_its_active_row_names() {
    let h = harness();
    let _ = a_packet_pinned_before_the_block(&h).await;

    let row = load_row(&h.app).await;
    assert_eq!(
        row["depth"], 1,
        "the pinned packet must be a member — its active row routes it here: {row:#}"
    );
    assert_eq!(
        row["unreachable"], 0,
        "a member is not an omission, so the figure must fall to zero: {row:#}"
    );
}

/// The queue an agent READS lists it. The load row counting it is not
/// enough if the door an agent takes work from still cannot see it.
#[tokio::test]
async fn the_agent_queue_lists_a_packet_pinned_before_its_block() {
    let h = harness();
    let id = a_packet_pinned_before_the_block(&h).await;

    let queue = get_json(&h.app, &format!("/api/stations/{STATION}/queue")).await;
    assert_eq!(queue["total"], 1, "{queue:#}");
    assert_eq!(queue["data"][0]["id"], id.as_str(), "{queue:#}");
}

/// Every object anywhere in `v` whose `key` is `value` — the yard's
/// reads nest a station several levels down, and this file asks only
/// whether the station is drawn and what it says, not where.
fn find_all<'a>(
    v: &'a serde_json::Value,
    key: &str,
    value: &str,
    out: &mut Vec<&'a serde_json::Value>,
) {
    match v {
        serde_json::Value::Object(m) => {
            if m.get(key).and_then(|x| x.as_str()) == Some(value) {
                out.push(v);
            }
            m.values().for_each(|x| find_all(x, key, value, out));
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| find_all(x, key, value, out)),
        _ => {}
    }
}

/// The yard's two readers count the SAME members. Measured 2026-09-23
/// 23:40Z (backlog 6c06ef65): this station answered 213 in the load
/// and its own queue, and 170 in `/api/yard/regions` and
/// `/api/yard/borders`, because the map's marshalling read listed steps
/// raw while the load and queue resolved them against the active row.
/// One membership question, so one answer on every surface that draws
/// it: the marshalling machine says one standing, and the border names
/// the station holding one packet.
#[tokio::test]
async fn the_yard_regions_and_borders_count_a_packet_pinned_before_its_block() {
    let h = harness();
    let _ = a_packet_pinned_before_the_block(&h).await;
    assert_eq!(
        load_row(&h.app).await["depth"],
        1,
        "the control: the load counts it"
    );

    let regions = get_json(&h.app, "/api/yard/regions").await;
    let mut machines = Vec::new();
    // The regions themselves: the HUD's machine cell (design 00774ca8)
    // names the same unjudged machine again, with its region, as a
    // click-through — the machine is drawn once, in its territory.
    find_all(
        &regions["regions"],
        "id",
        &format!("station:{STATION}"),
        &mut machines,
    );
    let [machine] = machines.as_slice() else {
        panic!("one `station:{STATION}` machine on the regions map: {regions:#}");
    };
    let why = machine["why"].as_str().unwrap_or_default();
    assert!(
        why.starts_with("1 standing"),
        "the regions map must count the member the load counts, read: {why:?}"
    );

    let borders = get_json(&h.app, "/api/yard/borders").await;
    let mut holds = Vec::new();
    find_all(&borders, "what", STATION, &mut holds);
    assert!(
        !holds.is_empty(),
        "the borders must show `{STATION}` holding the member the load counts: {borders:#}"
    );
    for hold in holds {
        let why = hold["why"].as_str().unwrap_or_default();
        assert!(why.starts_with("1 packet standing"), "{hold:#}");
    }
}

/// The claim door asks the SAME membership question when a claim names
/// its station; answered from the packet's copy alone it would refuse
/// "packet is not at this station" for a packet the queue just listed.
/// And the resolution answers a question, it writes nothing: the
/// claimed step still carries no `agent_model` of its own.
#[tokio::test]
async fn a_claim_through_the_station_admits_a_packet_pinned_before_its_block() {
    let h = harness();
    let id = a_packet_pinned_before_the_block(&h).await;
    let job = get_json(&h.app, &format!("/api/jobs/{id}")).await;
    let step_id = job["steps"]
        .as_array()
        .and_then(|steps| steps.iter().find(|s| s["spec_slug"] == "build"))
        .and_then(|s| s["id"].as_str())
        .unwrap_or_else(|| panic!("a build step on the packet: {job:#}"))
        .to_string();

    let resp = h
        .app
        .clone()
        .oneshot(
            Request::post(format!(
                "/api/jobs/{id}/steps/{step_id}/claim?station={STATION}"
            ))
            .header("x-boss-user", user_header("emp-david", "platform-admin"))
            .body(Body::empty())
            .expect("request"),
        )
        .await
        .expect("response");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let claimed: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
    assert!(
        claimed["metadata"].get("agent_model").is_none(),
        "resolving must never write the active row's declaration onto a packet pinned to a \
         version that made none: {claimed:#}"
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
