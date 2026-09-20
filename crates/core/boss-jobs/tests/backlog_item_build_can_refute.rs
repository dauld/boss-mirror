//! Layer 4: a `backlog-item` whose premise dies AT THE BUILD can still
//! be withdrawn.
//!
//! The defect (backlog 6c114a23, measured 2026-09-20 while closing
//! ef74fc12). The three withdrawal terminals — `duplicate`, `stale`,
//! `declined` — each read the TRIAGE step's disposition and nothing
//! else. So the moment triage routes a packet to `build`, all three
//! have every step they reference terminal with their predicate still
//! false, and the skip rule (registry.rs, `refs_all_terminal`) marks
//! them Skipped on the spot. Read off the live packet ef74fc12:
//!
//!   completed filed | completed triage | skipped measure |
//!   skipped draft-design | skipped design-review | active build |
//!   skipped duplicate | skipped stale | skipped declined |
//!   pending closed
//!
//! From `build` the only remaining terminal was `closed`, whose
//! outcome is `completed` — it asserts the work was done. Verify-the-
//! claim is a step the startup protocol REQUIRES of every builder, and
//! ef74fc12's builder did it, found the premise false, and refused to
//! build. The three options left were to complete `build` (asserting
//! work that never happened), to leave the packet open (residue), or
//! to close it out of band — which is what happened, a PUT plus a
//! `refuted` annotation. That closes the packet with NO terminal
//! outcome, so every count that reads terminals sees it as neither
//! stale nor built.
//!
//! THE FIX IS A PREDICATE, NOT A NEW STEP, and the shape was already
//! in the file: `declined` has carried a two-armed `ready_when`
//! (triage OR design-review) since af28e250. Each withdrawal terminal
//! now carries a third arm reading an OPTIONAL `disposition` on
//! `build`, and `closed`'s build disjunct is the NEGATIVE of those
//! three values — the same way it already states the design-review
//! fork — so a build with no disposition still closes as completed and
//! two terminals never go Ready off one write.
//!
//! These tests drive the REAL router against the REAL platform bundle,
//! for the reason `backlog_item_design_routes_onward` gives: a fixture
//! copy of the spec would keep passing while the shipped kind stayed
//! broken.

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

fn triage_build() -> serde_json::Value {
    serde_json::json!({
        "disposition": "build",
        "evidence": "The claim was measured against origin/main at triage time and held.",
    })
}

/// Route a fresh packet to `build`, returning it as read after triage.
async fn routed_to_build(app: &axum::Router) -> (String, serde_json::Value) {
    let job_id = open_item(app).await;
    let job = read(app, &job_id).await;
    complete(app, &job_id, &job, "triage", triage_build()).await;
    let after = read(app, &job_id).await;
    (job_id, after)
}

/// THE DEFECT ITSELF. Routing to `build` must leave the three
/// withdrawal terminals PENDING, not Skipped — a skipped step never
/// comes back, and skipping all three at triage time is what left
/// ef74fc12's builder with nowhere to land a refutation.
#[tokio::test]
async fn routing_to_build_keeps_the_withdrawal_terminals_alive() {
    let app = app();
    let (_id, after) = routed_to_build(&app).await;

    assert!(actionable(&after, "build"), "the build must be actionable");
    for slug in ["stale", "duplicate", "declined"] {
        assert_eq!(
            status_of(&after, slug),
            "pending",
            "`{slug}` must stay reachable from the build — it is `{}`. Steps: {:#?}",
            status_of(&after, slug),
            after["steps"]
        );
    }
}

/// A builder that refutes the premise completes `build` with a
/// disposition, and the packet lands on the terminal that disposition
/// names — WITH that outcome on the closed job, which is the whole
/// point: a count that reads terminals sees a withdrawal, not a
/// completion and not a hole.
#[tokio::test]
async fn a_refuting_build_reaches_the_terminal_its_disposition_names() {
    for (disposition, terminal, outcome) in [
        ("stale", "stale", "stale"),
        ("duplicate", "duplicate", "duplicate"),
        ("decline", "declined", "declined"),
    ] {
        let app = app();
        let (job_id, after) = routed_to_build(&app).await;

        complete(
            &app,
            &job_id,
            &after,
            "build",
            serde_json::json!({ "disposition": disposition }),
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

/// THE REGRESSION THIS MUST NOT CAUSE. A build that actually built
/// carries no disposition, and still closes the item as `completed`.
/// `closed`'s build disjunct states the three refutations NEGATIVELY
/// for exactly this: boss-expr resolves a missing identifier to
/// Absent (7b756357), false against every literal, so `NOT (false OR
/// false OR false)` is true and an ordinary build lands there.
#[tokio::test]
async fn a_build_that_built_still_closes_the_item_as_completed() {
    let app = app();
    let (job_id, after) = routed_to_build(&app).await;

    complete(&app, &job_id, &after, "build", serde_json::json!({})).await;
    let after = read(&app, &job_id).await;

    for slug in ["stale", "duplicate", "declined"] {
        assert_eq!(
            status_of(&after, slug),
            "skipped",
            "a build that built withdraws nothing — `{slug}` is `{}`",
            status_of(&after, slug)
        );
    }
    if actionable(&after, "closed") {
        complete(&app, &job_id, &after, "closed", serde_json::json!({})).await;
    }
    let done = read(&app, &job_id).await;
    assert_eq!(done["status"], "closed");
    assert_eq!(
        done["metadata"]["outcome"], "completed",
        "an ordinary build still closes as completed: {:#?}",
        done["metadata"]
    );
}

/// The disposition is ENUM-CHECKED on the step, so a value outside the
/// set is refused at the API rather than skipping every successor and
/// wedging the packet open. The enum deliberately omits `verify`,
/// `design` and `build`: a builder refutes a packet, it does not
/// re-route one.
#[tokio::test]
async fn a_disposition_outside_the_set_is_refused() {
    let app = app();
    let (job_id, after) = routed_to_build(&app).await;

    let (status, body) = try_complete(
        &app,
        &job_id,
        &after,
        "build",
        serde_json::json!({ "disposition": "design" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a disposition outside the set must be refused, not accepted: {body}"
    );
}
