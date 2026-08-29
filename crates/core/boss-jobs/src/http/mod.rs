//! Axum HTTP handlers for the jobs API.
//!
//! Handlers are grouped by concern into submodules; this module owns
//! the shared `JobsApiState`, the router that wires every route, and
//! the helpers used across more than one concern.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use boss_core::job::{Job, JobStatus, Step, StepStatus};
use boss_core::port::EventBus;
use boss_core::publisher::DomainPublisher;
use boss_policy_client::{Action, Decision, Resource};

use boss_policy_client::{CurrentUser, PolicyClient};
use serde::{Deserialize, Serialize};

use crate::events;
use crate::in_memory::compute_job_status;
use crate::policy_glue::scope_matches;
use crate::port::{JobFilter, JobScope, JobsRepository, LaunchCalendarRow};
use crate::registry::{WorkflowError, WorkflowRegistry, WorkflowSpec};
use crate::step_plugins::{StepPluginError, StepPluginRegistry, StepPluginSpec};
use crate::step_registry::StepRegistry;

pub mod machine_gate;

mod census;
mod jobs;
mod kinds;
mod plugins;
mod refusals;
mod sim_clock;
mod stations;
mod steps;
mod terminal_report;

use census::*;
use jobs::*;
use kinds::*;
use plugins::*;
use refusals::*;
use sim_clock::*;
use stations::*;
use steps::*;
use terminal_report::*;

const DEFAULT_LIMIT: i64 = 100;
const MAX_LIMIT: i64 = 1000;
/// Cap for the sim workforce's bulk assigned-backlog pull — higher than
/// the My-Day `MAX_LIMIT` because the workforce legitimately wants the
/// whole assigned backlog in one round-trip, not a page.
const BULK_ASSIGNED_LIMIT: i64 = 50_000;

/// Shared state for jobs API handlers.
pub struct JobsApiState<R: JobsRepository, B: EventBus> {
    pub jobs: Arc<R>,
    pub bus: Arc<B>,
    pub publisher: DomainPublisher,
    pub step_registry: Arc<StepRegistry>,
    /// Cross-service client for row-level authorization. Plumb in a
    /// `ReqwestPolicyClient` in prod, `FakePolicyClient` in tests.
    pub policy: Arc<dyn PolicyClient>,
    /// Workflow registry — authored via /api/workflows. None until a
    /// caller wires the adapter in; endpoints respond with 503 in that
    /// case to keep the seam explicit.
    pub kind_registry: Option<Arc<dyn WorkflowRegistry>>,
    /// Step UX plugin registry — authored via /api/jobs/step-plugins.
    /// Same optionality semantics as `kind_registry`.
    pub plugin_registry: Option<Arc<dyn StepPluginRegistry>>,
    /// The job_edges registry (read-only; edges are declared in
    /// migrations). None → 503 on the read route.
    pub job_edges: Option<Arc<dyn crate::job_edges::JobEdgesRegistry>>,
    /// Station registry (stations.md) — the data-defined priority
    /// queues over packets. Same optionality semantics as
    /// `kind_registry`: None → 503 on the /api/stations routes and
    /// the claim path's station gate.
    pub stations: Option<Arc<dyn crate::stations::StationRegistry>>,
    /// Cross-service client for the global calendar primitive
    /// (`docs/architecture-decisions.md` §Calendar). When set, scheduling
    /// steps that transition `ready → active` with full
    /// metadata (`scheduled_at`, `duration_hours`, `assignee_id`)
    /// reserve the assignee's time; conflicts surface as 409.
    /// `None` keeps every existing test path working — the
    /// reservation hook is purely additive.
    pub calendar: Option<Arc<dyn boss_calendar_client::CalendarClient>>,
    /// SubjectKind registry client. When set, Job creates / updates
    /// with `subject = Subject::Custom { custom_kind }` validate
    /// `custom_kind` against the registry on writes. `None` skips the
    /// check — same opt-in shape as the calendar client; lets
    /// boss-jobs-api deploy independently of the subject-kinds rollout.
    pub subject_kinds: Option<Arc<dyn boss_subject_kinds_client::SubjectKindsClient>>,
    /// Subject-existence validator. When set, Job creates check the
    /// Subject's id against the relevant upstream service (people,
    /// assets, locations, inventory). `None` skips the check — same
    /// opt-in shape as the registry clients above; preserves the
    /// in-memory test path.
    pub subject_existence: Option<Arc<dyn crate::subject_existence::SubjectExistenceCheck>>,
    /// Human job-owner resolution (Q7). `None` skips — the
    /// in-memory test paths without an upstream roster.
    pub roster: Option<Arc<dyn crate::owner_resolution::RosterLookup>>,
    /// Authoritative clock. See `boss-clock-client`.
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

/// `GET /api/jobs/job-edges` — the declared job-to-job link fields.
/// Read-only: authoring an edge is a migration (it changes what the
/// write path refuses).
async fn list_job_edges<R: JobsRepository, B: EventBus>(
    State(state): State<Arc<JobsApiState<R, B>>>,
) -> Response {
    let Some(reg) = state.job_edges.as_ref() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "job_edges registry not configured",
        )
            .into_response();
    };
    match reg.list().await {
        Ok(edges) => Json(edges).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

/// Build the jobs API router.
pub fn router<R: JobsRepository + 'static, B: EventBus + 'static>(
    state: JobsApiState<R, B>,
) -> Router {
    let shared = Arc::new(state);
    // One layer over the whole router rather than a call at each of
    // `steps.rs`'s ~15 refusal sites, so a refusal added later cannot
    // silently go uncounted. It passes everything that is not a step
    // WRITE straight through without buffering. See http/refusals.rs.
    let refusals =
        axum::middleware::from_fn_with_state(shared.clone(), record_step_write_refusals::<R, B>);
    Router::new()
        .route("/api/jobs/health", get(health))
        .route(
            "/api/jobs/step-write-refusals",
            get(list_step_write_refusals::<R, B>),
        )
        .route("/api/jobs/summary", get(jobs_summary::<R, B>))
        .route("/api/jobs/live", get(jobs_live::<R, B>))
        .route("/api/jobs/sim-clock/pause", post(sim_clock_pause::<R, B>))
        .route("/api/jobs/sim-clock/resume", post(sim_clock_resume::<R, B>))
        .route(
            "/api/jobs/sim-clock/restart-epoch",
            post(sim_clock_restart_epoch::<R, B>),
        )
        .route("/api/jobs/sim-clock/stream", get(sim_clock_stream::<R, B>))
        .route(
            "/api/jobs/phase-distribution",
            get(jobs_phase_distribution::<R, B>),
        )
        .route("/api/jobs/launch-calendar", get(launch_calendar::<R, B>))
        .route("/api/jobs/assignments", get(list_assignments::<R, B>))
        .route("/api/jobs", get(list_jobs::<R, B>))
        .route("/api/jobs", post(create_job::<R, B>))
        .route("/api/jobs/{id}", get(get_job::<R, B>))
        .route("/api/jobs/{id}", put(update_job::<R, B>))
        // Top-level metadata merge — the atomic alternative to the
        // GET → spread → full PUT read-modify-write. `null` removes.
        .route("/api/jobs/{id}/metadata", patch(patch_job_metadata::<R, B>))
        .route("/api/jobs/{id}/stream", get(job_stream::<R, B>))
        .route("/api/jobs/step-types", get(list_step_types::<R, B>))
        // Station registry — data-defined priority queues over
        // packets (docs/design/stations.md). Append-only and
        // versioned like the Workflow registry, and editable at run
        // time: a queue is redrawn by publishing a new version, never
        // by a deploy.
        .route(
            "/api/stations",
            get(list_stations::<R, B>).post(create_station::<R, B>),
        )
        // The packet-loss census door (packet-loss.md Q3): the
        // dispatcher's `network.census` handler measures over the
        // read surfaces above and lands its counts here, one
        // `jobs.network.census` event per firing.
        .route("/api/network/census", post(record_network_census::<R, B>))
        .route("/api/stations/load", get(stations_load::<R, B>))
        // Author-time dry run: lint a spec without persisting, so the
        // editor surfaces the same `station_lint::gate_active` the
        // publish path enforces.
        .route("/api/stations/_validate", post(validate_station::<R, B>))
        .route("/api/stations/{name}/queue", get(station_queue::<R, B>))
        .route(
            "/api/stations/{name}/versions",
            get(list_station_versions::<R, B>),
        )
        .route(
            "/api/stations/{name}/versions/{version}",
            get(get_station_version::<R, B>),
        )
        .route(
            "/api/stations/{name}/publish",
            post(publish_station::<R, B>),
        )
        .route("/api/stations/{name}/retire", post(retire_station::<R, B>))
        .route("/api/jobs/{id}/steps", get(list_steps::<R, B>))
        .route("/api/jobs/{id}/steps", post(add_step::<R, B>))
        .route("/api/jobs/{id}/steps/{step_id}", put(update_step::<R, B>))
        .route(
            "/api/jobs/{id}/steps/{step_id}/claim",
            post(claim_step::<R, B>),
        )
        .route(
            "/api/jobs/{id}/steps/{step_id}/sign-offs",
            post(post_step_sign_off::<R, B>),
        )
        // Workflow registry — see docs/architecture-decisions.md
        // §Jobs, Workflows, Steps
        .route(
            "/api/workflows",
            get(list_kinds::<R, B>).post(create_kind::<R, B>),
        )
        // Author-time dry run: lint a draft spec without persisting,
        // so the editor surfaces the same `workflow_lint::gate_active`
        // the publish path enforces (live, on the graph). See
        // architecture-decisions.md §Jobs, Workflows, Steps.
        .route("/api/workflows/_validate", post(validate_kind::<R, B>))
        .route(
            "/api/workflows/{kind}",
            get(get_kind::<R, B>).put(update_kind::<R, B>),
        )
        .route(
            "/api/workflows/{kind}/versions",
            get(list_kind_versions::<R, B>),
        )
        .route(
            "/api/workflows/{kind}/versions/{version}",
            get(get_kind_version::<R, B>),
        )
        // Experiments Tier 1 (docs/design/network-experiments.md):
        // the per-version terminal report — measurement of what
        // version pinning already records.
        .route(
            "/api/workflows/{kind}/terminal-report",
            get(workflow_terminal_report::<R, B>),
        )
        .route("/api/workflows/{kind}/publish", post(publish_kind::<R, B>))
        .route("/api/workflows/{kind}/retire", post(retire_kind::<R, B>))
        // Step UX plugin registry — see docs/architecture-decisions.md
        // §Step UX & frontend
        .route("/api/jobs/job-edges", get(list_job_edges))
        .route(
            "/api/jobs/step-plugins",
            get(list_plugins::<R, B>).post(create_plugin::<R, B>),
        )
        .route(
            "/api/jobs/step-plugins/{kind}",
            get(get_plugin::<R, B>).put(update_plugin::<R, B>),
        )
        .route(
            "/api/jobs/step-plugins/{kind}/versions",
            get(list_plugin_versions::<R, B>),
        )
        .route(
            "/api/jobs/step-plugins/{kind}/versions/{version}",
            get(get_plugin_version::<R, B>),
        )
        .route(
            "/api/jobs/step-plugins/{kind}/publish",
            post(publish_plugin::<R, B>),
        )
        .route(
            "/api/jobs/step-plugins/{kind}/retire",
            post(retire_plugin::<R, B>),
        )
        .route(
            "/api/jobs/step-plugins/{kind}/in-flight-count",
            get(in_flight_plugin_count::<R, B>),
        )
        .with_state(shared)
        .layer(refusals)
}

// ---------------------------------------------------------------------------
// Health
// ---------------------------------------------------------------------------

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-jobs-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

// ---------------------------------------------------------------------------
// Subject validation (shared by create_job / update_job and the kinds path)
// ---------------------------------------------------------------------------

/// SubjectKind validator. The SubjectKind registry is the **single
/// source of truth** for the noun vocabulary — core enumerates no
/// kinds itself. When the registry is wired, every kind is validated
/// against it (the platform specializations — `asset`, `account`,
/// `purchase_order`, … — are seeded rows, so they pass; an unknown
/// kind 400s). When it isn't wired (tests) the check is a no-op and
/// all kinds pass.
///
/// 502 on registry-call failure rather than dropping the write
/// silently — a misconfigured registry is loud, not a silent
/// always-pass.
#[allow(
    clippy::result_large_err,
    reason = "idiomatic axum Response error; crate-wide Box<Response> cleanup tracked separately"
)]
pub(super) async fn check_custom_subject(
    registry: Option<&Arc<dyn boss_subject_kinds_client::SubjectKindsClient>>,
    subject: &boss_core::job::Subject,
) -> Result<(), Response> {
    let Some(reg) = registry else {
        return Ok(());
    };
    let kind = subject.kind.as_str();
    match reg.subject_kind_exists(kind).await {
        Ok(true) => Ok(()),
        Ok(false) => Err((
            StatusCode::BAD_REQUEST,
            format!(
                "unknown subject kind `{kind}` — register it in the subject_kinds registry first",
            ),
        )
            .into_response()),
        Err(e) => Err((
            StatusCode::BAD_GATEWAY,
            format!("subject-kinds registry unreachable: {e}"),
        )
            .into_response()),
    }
}

/// Adapter so handlers stay terse. Pulls the registry off
/// `JobsApiState` and delegates to `check_custom_subject`.
#[allow(
    clippy::result_large_err,
    reason = "idiomatic axum Response error; crate-wide Box<Response> cleanup tracked separately"
)]
pub(super) async fn validate_custom_subject<R: JobsRepository, B: EventBus>(
    state: &JobsApiState<R, B>,
    subject: &boss_core::job::Subject,
) -> Result<(), Response> {
    check_custom_subject(state.subject_kinds.as_ref(), subject).await
}

// ---------------------------------------------------------------------------
// Shared policy-scope translation
// ---------------------------------------------------------------------------

/// Translate the caller's read-scope `Predicate` into the `JobScope`
/// the adapter can push into SQL. `DepartmentIs` is the odd one out:
/// Jobs don't carry a department column, so it's either "all"
/// (caller's department matches) or "none" (it doesn't). Shared by
/// the /api/jobs list and the station queue lens so every packet
/// read surface passes through ONE policy path.
pub(super) fn job_scope_from_predicate(
    user: &boss_policy_client::User,
    predicate: &boss_policy_client::Predicate,
) -> JobScope {
    match predicate {
        boss_policy_client::Predicate::Unrestricted => JobScope::All,
        boss_policy_client::Predicate::None => JobScope::None,
        boss_policy_client::Predicate::OwnerIs { user_id } => JobScope::OwnerIs(user_id.clone()),
        boss_policy_client::Predicate::OwnerIn { user_ids } => JobScope::OwnerIn(user_ids.clone()),
        boss_policy_client::Predicate::AccountIn { account_ids } => {
            JobScope::AccountIn(account_ids.clone())
        }
        boss_policy_client::Predicate::DepartmentIs { department } => {
            if user.department.as_deref() == Some(department.as_str()) {
                JobScope::All
            } else {
                JobScope::None
            }
        }
    }
}

/// The id a station predicate's [`crate::station_queue::SELF`]
/// placeholder binds to for this request, or `None` when the caller is
/// not an identified actor.
///
/// `ambient_actor()` is already the platform's answer to "is there
/// somebody behind this request" — it returns `None` for an anonymous
/// caller — so the guest case is settled by the existing identity rule
/// rather than by a second hardcoded sentinel here. The id itself is
/// `user.id`, because that is what a packet records when it names who
/// filed it.
///
/// Shared by the station queue read and the claim gate: both ask a
/// per-actor station the same question, so both must bind the same id.
pub(super) fn self_id(user: &boss_policy_client::User) -> Option<&str> {
    user.ambient_actor().is_some().then_some(user.id.as_str())
}

// ---------------------------------------------------------------------------
// Shared id parsers
// ---------------------------------------------------------------------------

pub(super) fn parse_job_id(s: &str) -> Option<boss_core::job::JobId> {
    let uuid = uuid::Uuid::parse_str(s).ok()?;
    Some(boss_core::job::JobId::from_uuid(uuid))
}

pub(super) fn parse_step_id(s: &str) -> Option<boss_core::job::StepId> {
    let uuid = uuid::Uuid::parse_str(s).ok()?;
    Some(boss_core::job::StepId::from_uuid(uuid))
}
