//! Layer 4: a `backlog-item` whose premise dies AT THE MEASUREMENT can
//! still be withdrawn.
//!
//! The sibling of backlog 6c114a23, one step over (backlog 2802ba8c).
//! That one gave `build` a way to refute its packet; `measure` never
//! got the same treatment, and it is the step MOST likely to need it —
//! a re-measurement exists precisely to test whether the claim still
//! holds, and "it does not" was the one answer it could not record.
//!
//! Measured on origin/main before this change: `build` declared an
//! optional `disposition` (stale|duplicate|decline) that all three
//! withdrawal terminals read, and `closed` excluded those three. The
//! `measure` step declared NO FIELDS AT ALL, and `closed` fired on a
//! bare `steps.measure.done`. So a measurement that disproved its
//! packet had exactly one exit, and it was the one that asserts the
//! work was COMPLETED — the same wrong-outcome hole ef74fc12 fell
//! into, reached by a different door.
//!
//! The fix is the same predicate shape, not a new step.

use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_jobs::http::{JobsApiState, router};
use boss_jobs::owner_resolution::RosterLookup;
use boss_jobs::registry::seedable_platform_workflows;
use boss_jobs::{InMemoryJobs, InMemoryWorkflows, WorkflowRegistry};
use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
use boss_testing::RecordingEventBus;
use http_body_util::BodyExt;
use tower::ServiceExt;

struct AdminRoster;

#[async_trait]
impl RosterLookup for AdminRoster {
    async fn active_holders(&self, role: &str) -> Result<Vec<String>, String> {
        Ok(match role {
            "platform-admin" => vec!["emp-bootstrap-admin".to_string()],
            _ => Vec::new(),
        })
    }
    async fn is_active_employee(&self, id: &str) -> Result<bool, String> {
        Ok(id == "emp-bootstrap-admin")
    }
}

fn admin_header() -> String {
    serde_json::json!({
        "id": "emp-bootstrap-admin",
        "role": "platform-admin",
        "access_tier": "operator",
        "territory_account_ids": [],
        "direct_report_ids": [],
        "department": "platform",
    })
    .to_string()
}

fn app() -> axum::Router {
    let kinds = Arc::new(InMemoryWorkflows::new());
    for spec in seedable_platform_workflows() {
        kinds.seed(spec).expect("seed platform kind");
    }
    let jobs = Arc::new(InMemoryJobs::new());
    let policy: Arc<dyn PolicyClient> = Arc::new(
        FakePolicyClient::builder()
            .allow(
                "platform-admin",
                Action::Create,
                Resource::job(),
                Scope::All,
            )
            .allow("platform-admin", Action::Read, Resource::job(), Scope::All)
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
        kind_registry: Some(kinds as Arc<dyn WorkflowRegistry>),
        roster: Some(Arc::new(AdminRoster)),
        ..JobsApiState::minimal(
            jobs,
            bus,
            DomainPublisher::new(bus_dyn, "jobs"),
            policy,
            Arc::new(boss_clock_client::WallClockClient),
        )
    };
    router(state)
}

async fn send(app: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = app.clone().oneshot(req).await.expect("router responds");
    let status = resp.status();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("collect body")
        .to_bytes();
    let json = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, json)
}

async fn open_item(app: &axum::Router) -> String {
    let body = serde_json::json!({
        "kind": "backlog-item",
        "subject": { "subject_kind": "custom", "id": "bosspipeline" },
        "title": "A packet routed to build cannot be closed stale",
        "owner_id": "emp-bootstrap-admin",
        "priority": "standard",
        "status": "open",
        "metadata": { "area": "platform" },
        "tags": ["protocol"],
    })
    .to_string();
    let (status, job) = send(
        app,
        Request::builder()
            .method("POST")
            .uri("/api/jobs")
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(body))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "create rejected: {job}");
    job["id"].as_str().expect("job id").to_string()
}

async fn read(app: &axum::Router, job_id: &str) -> serde_json::Value {
    let (status, body) = send(
        app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/jobs/{job_id}"))
            .header("x-boss-user", admin_header())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "read failed: {body}");
    body
}

fn step_of<'a>(job: &'a serde_json::Value, slug: &str) -> &'a serde_json::Value {
    job["steps"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["spec_slug"] == slug)
        .unwrap_or_else(|| panic!("no step `{slug}` on the packet: {job:#?}"))
}

fn status_of(job: &serde_json::Value, slug: &str) -> String {
    step_of(job, slug)["status"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn actionable(job: &serde_json::Value, slug: &str) -> bool {
    matches!(status_of(job, slug).as_str(), "ready" | "active")
}

/// Complete a step, merging `extra` over its current metadata —
/// never replacing, because `authority_role` shares that object.
async fn try_complete(
    app: &axum::Router,
    job_id: &str,
    job: &serde_json::Value,
    slug: &str,
    extra: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let step = step_of(job, slug);
    let mut metadata = step["metadata"].clone();
    for (k, v) in extra.as_object().into_iter().flatten() {
        metadata[k] = v.clone();
    }
    send(
        app,
        Request::builder()
            .method("PUT")
            .uri(format!(
                "/api/jobs/{job_id}/steps/{}",
                step["id"].as_str().expect("step id")
            ))
            .header("content-type", "application/json")
            .header("x-boss-user", admin_header())
            .body(Body::from(
                serde_json::json!({ "status": "completed", "metadata": metadata }).to_string(),
            ))
            .unwrap(),
    )
    .await
}

async fn complete(
    app: &axum::Router,
    job_id: &str,
    job: &serde_json::Value,
    slug: &str,
    extra: serde_json::Value,
) {
    assert!(
        actionable(job, slug),
        "step `{slug}` is `{}`, not actionable",
        status_of(job, slug)
    );
    let (status, body) = try_complete(app, job_id, job, slug, extra).await;
    assert!(
        status.is_success(),
        "completing `{slug}` failed {status}: {body}"
    );
}

fn triage_verify() -> serde_json::Value {
    serde_json::json!({
        "disposition": "verify",
        "evidence": "The claim needs re-measuring against origin/main before anything is built.",
    })
}

/// Route a fresh packet to `measure`, returning it as read after triage.
async fn routed_to_measure(app: &axum::Router) -> (String, serde_json::Value) {
    let job_id = open_item(app).await;
    let job = read(app, &job_id).await;
    complete(app, &job_id, &job, "triage", triage_verify()).await;
    let after = read(app, &job_id).await;
    (job_id, after)
}

/// Routing to `measure` must leave the three withdrawal terminals
/// PENDING. A skipped step never comes back, so skipping all three at
/// triage time is what leaves a measurement with nowhere to land a
/// refutation.
#[tokio::test]
async fn routing_to_measure_keeps_the_withdrawal_terminals_alive() {
    let app = app();
    let (_id, after) = routed_to_measure(&app).await;

    assert!(
        actionable(&after, "measure"),
        "the measure must be actionable"
    );
    for slug in ["stale", "duplicate", "declined"] {
        assert_eq!(
            status_of(&after, slug),
            "pending",
            "`{slug}` must stay reachable from the measurement — it is `{}`. Steps: {:#?}",
            status_of(&after, slug),
            after["steps"]
        );
    }
}

/// A measurement that disproves its packet completes with a
/// disposition, and the packet lands on the terminal that disposition
/// names — carrying that outcome, so a count reading terminals sees a
/// withdrawal rather than a completion.
#[tokio::test]
async fn a_refuting_measurement_reaches_the_terminal_its_disposition_names() {
    for (disposition, terminal, outcome) in [
        ("stale", "stale", "stale"),
        ("duplicate", "duplicate", "duplicate"),
        ("decline", "declined", "declined"),
    ] {
        let app = app();
        let (job_id, after) = routed_to_measure(&app).await;

        complete(
            &app,
            &job_id,
            &after,
            "measure",
            serde_json::json!({
                "disposition": disposition,
                "evidence": "Re-measured on origin/main: the claim no longer holds.",
            }),
        )
        .await;
        let after = read(&app, &job_id).await;

        assert!(
            actionable(&after, terminal) || after["status"] == "closed",
            "`disposition = {disposition}` must reach `{terminal}` — it is `{}`. Steps: {:#?}",
            status_of(&after, terminal),
            after["steps"]
        );
        assert_ne!(
            status_of(&after, "closed"),
            "ready",
            "`closed` must not race `{terminal}` — one refutation, one terminal"
        );

        if actionable(&after, terminal) {
            complete(&app, &job_id, &after, terminal, serde_json::json!({})).await;
        }
        let done = read(&app, &job_id).await;
        assert_eq!(done["status"], "closed", "a refuted packet closes");
        assert_eq!(
            done["metadata"]["outcome"], outcome,
            "the closed packet must carry the withdrawal outcome, not a hole: {:#?}",
            done["metadata"]
        );
    }
}

/// THE REGRESSION THIS MUST NOT CAUSE. A measurement that CONFIRMED
/// the claim carries no disposition and still closes the item as
/// `completed` — which is what `closed`'s new measure disjunct states
/// negatively, since boss-expr resolves a missing identifier to
/// Absent and `NOT (false OR false OR false)` is true.
#[tokio::test]
async fn a_measurement_that_held_still_closes_the_item_as_completed() {
    let app = app();
    let (job_id, after) = routed_to_measure(&app).await;

    complete(
        &app,
        &job_id,
        &after,
        "measure",
        serde_json::json!({ "evidence": "Re-measured on origin/main: the claim holds." }),
    )
    .await;
    let after = read(&app, &job_id).await;

    for slug in ["stale", "duplicate", "declined"] {
        assert_eq!(
            status_of(&after, slug),
            "skipped",
            "a measurement that held withdraws nothing — `{slug}` is `{}`",
            status_of(&after, slug)
        );
    }
    let done = if actionable(&after, "closed") {
        complete(&app, &job_id, &after, "closed", serde_json::json!({})).await;
        read(&app, &job_id).await
    } else {
        after
    };
    assert_eq!(done["status"], "closed");
    assert_eq!(
        done["metadata"]["outcome"], "completed",
        "a confirmed measurement closes the packet as completed: {:#?}",
        done["metadata"]
    );
}

/// NO EVIDENCE IS NOT A PASS, at the step that exists to produce it
/// (backlog 828381c0). Measured on 2026-09-22: `boss step complete
/// dd92d961 --step measure` with no fields completed the step and the
/// packet went straight to `closed`, holding nothing measured — the
/// row declared only an optional `disposition`, so a bare completion
/// was the ONLY one it permitted. A second packet (648a68a9) had a real
/// finding the same night and nowhere on the step to put it.
///
/// So `measure` requires `evidence`, the name `triage` already uses,
/// and the validator refuses a bare completion exactly as it refuses a
/// bare triage: the step stays open and the refusal names the field.
#[tokio::test]
async fn a_measurement_with_nothing_measured_is_refused() {
    let app = app();
    let (job_id, after) = routed_to_measure(&app).await;

    for extra in [
        serde_json::json!({}),
        // A refutation is a measurement too: a disposition alone is
        // still nothing measured.
        serde_json::json!({ "disposition": "stale" }),
    ] {
        let (status, body) = try_complete(&app, &job_id, &after, "measure", extra.clone()).await;
        assert!(
            status.is_client_error(),
            "completing `measure` with {extra} must be refused, got {status}: {body}"
        );
        assert!(
            body.to_string().contains("evidence"),
            "the refusal must name the field the step lacks: {body}"
        );
    }

    let still = read(&app, &job_id).await;
    assert!(
        actionable(&still, "measure"),
        "the refused measurement stays open — it is `{}`",
        status_of(&still, "measure")
    );
    assert_eq!(still["status"], "open", "nothing measured closes nothing");
}
