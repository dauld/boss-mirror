//! `JobsApiState::minimal` is the test wiring's one definition of the
//! optional ports (backlog 26a856d4).
//!
//! Every optional dependency added to `JobsApiState` used to cost ~65
//! struct-literal edits, because each test spelled every field. Car
//! b14afc48 added one port and shipped 73 files, 63 of them the single
//! line `dispatcher_firings: None,` — a review cost as much as a typing
//! one, since the ten files that mattered were buried among them.
//!
//! This pins what `minimal` promises, so a test can use it as the base
//! of a functional-update literal and trust the rest: the required
//! ports are the ones passed in, and every optional port is absent —
//! which the routes that need one answer with 503, exactly as the
//! hand-written `None` did.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::InMemoryJobs;
use boss_jobs::http::{JobsApiState, router};
use boss_policy_client::{FakePolicyClient, PolicyClient};
use boss_testing::RecordingEventBus;
use tower::ServiceExt;

#[tokio::test]
async fn minimal_wires_the_required_ports_and_leaves_every_optional_one_absent() {
    let jobs = Arc::new(InMemoryJobs::new());
    let bus = RecordingEventBus::new();
    let bus_dyn: Arc<dyn EventBus> = bus.clone();
    let policy: Arc<dyn PolicyClient> = Arc::new(FakePolicyClient::builder().build());
    let state = JobsApiState::minimal(
        jobs,
        bus,
        DomainPublisher::new(bus_dyn, "jobs"),
        policy,
        Arc::new(boss_clock_client::WallClockClient),
    );

    // The optional ports are absent — asserted on the state itself, not
    // only through a route, so a port that is added to the struct and
    // forgotten in `minimal` cannot compile past here.
    assert!(state.kind_registry.is_none());
    assert!(state.plugin_registry.is_none());
    assert!(state.job_edges.is_none());
    assert!(state.stations.is_none());
    assert!(state.calendar.is_none());
    assert!(state.subject_kinds.is_none());
    assert!(state.subject_existence.is_none());
    assert!(state.roster.is_none());
    assert!(state.cadence.is_none());
    assert!(state.dispatcher_firings.is_none());
    assert!(state.delivery.is_none());
    assert!(state.agent_budget.is_none());
    assert!(state.schema_ledger.is_none());
    assert!(state.yard_moves.is_none());

    let app = router(state);

    // The required ports are live: health answers from the wired state.
    let health = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/jobs/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);

    // An absent optional port keeps the seam explicit: 503, never a
    // quiet empty answer. (CLAUDE.md §Doors: a wrong target answers
    // instead of erroring — this route refuses instead.)
    let edges = app
        .oneshot(
            Request::builder()
                .uri("/api/jobs/job-edges")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(edges.status(), StatusCode::SERVICE_UNAVAILABLE);
}
