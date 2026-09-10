//! Axum routes for the agent-run record.
//!
//! Four reads and one write, all on the jobs API — the one door
//! `boss-api` already reaches, which is why the surface lives here and
//! not in `boss-cybernetics`: a record written somewhere nothing
//! deploys is a record nobody can file.
//!
//! Filing a run is a WRITE and it names its own actor, so there is no
//! guest-trusted path for the POST. The reads match the cadence
//! surface's stance: operator tier, or a trusted internal caller.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use boss_policy_client::{AccessTier, CurrentUser, User};

use super::port::{AgentRunError, AgentRunLog};
use super::types::{AgentRun, NewAgentRun, RunFilter, RunSummary, summarize};

pub struct AgentRunsApiState {
    pub log: Arc<dyn AgentRunLog>,
}

/// Same two categories the cadence door admits: an operator-tier
/// caller, or a trusted internal one (the extractor defaults to
/// `role=guest` when no `x-boss-user` header arrived, i.e. a loopback
/// sibling or a test harness; the gateway always injects the header for
/// external requests).
fn is_trusted(user: &User) -> bool {
    user.role == "guest" || user.access_tier == AccessTier::Operator
}

pub fn router(state: AgentRunsApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/agent-runs", get(list_runs).post(record_run))
        .route("/api/agent-runs/cost", get(cost))
        .route("/api/agent-rate-card", get(rate_card))
        .with_state(shared)
}

fn err_response(e: AgentRunError) -> Response {
    match e {
        AgentRunError::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
        AgentRunError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

/// Query string for both reads. Mirrors [`RunFilter`] but flat, because
/// a query string has no nesting.
#[derive(Debug, Default, Deserialize)]
pub struct RunQuery {
    #[serde(default)]
    pub job_id: Option<Uuid>,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub actor_id: Option<String>,
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
    #[serde(default)]
    pub limit: Option<i64>,
}

impl From<RunQuery> for RunFilter {
    fn from(q: RunQuery) -> Self {
        RunFilter {
            job_id: q.job_id,
            branch: q.branch,
            actor_id: q.actor_id,
            since: q.since,
            limit: q.limit,
        }
    }
}

/// What a POST answers with.
#[derive(Debug, Serialize)]
pub struct RecordResponse {
    /// `false` means this `run_id` was already held — a retried report.
    pub recorded: bool,
    pub run: AgentRun,
}

/// What `GET /api/agent-runs/cost` answers with: the roll-up plus the
/// filter it was taken over, so a reader can tell what the number
/// covers. A summary that did not say what it summed is a number
/// someone has to go re-derive.
#[derive(Debug, Serialize)]
pub struct CostResponse {
    pub summary: RunSummary,
    pub job_id: Option<Uuid>,
    pub branch: Option<String>,
    pub since: Option<DateTime<Utc>>,
}

async fn list_runs(
    State(state): State<Arc<AgentRunsApiState>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<RunQuery>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.log.list_runs(&q.into()).await {
        Ok(runs) => Json(runs).into_response(),
        Err(e) => err_response(e),
    }
}

async fn record_run(
    State(state): State<Arc<AgentRunsApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<NewAgentRun>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    // Who FILED the record. Unlike the run's own `actor_id`, this is
    // the caller — and an unidentified caller is attributed to the
    // named `platform` automation rather than to a fake human, the same
    // default every other write on this service takes.
    let recorded_by = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    match state.log.record_run(&body, &recorded_by).await {
        Ok(out) => (
            StatusCode::OK,
            Json(RecordResponse {
                recorded: out.recorded,
                run: out.run,
            }),
        )
            .into_response(),
        Err(e) => err_response(e),
    }
}

async fn cost(
    State(state): State<Arc<AgentRunsApiState>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<RunQuery>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let job_id = q.job_id;
    let branch = q.branch.clone();
    let since = q.since;
    let filter: RunFilter = q.into();
    match state.log.list_runs(&filter).await {
        Ok(runs) => Json(CostResponse {
            summary: summarize(&runs),
            job_id,
            branch,
            since,
        })
        .into_response(),
        Err(e) => err_response(e),
    }
}

async fn rate_card(
    State(state): State<Arc<AgentRunsApiState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.log.rate_card().await {
        Ok(card) => Json(card).into_response(),
        Err(e) => err_response(e),
    }
}
