//! Axum routes for the agent-run record.
//!
//! Three reads and one write, all on the jobs API — the one door
//! `boss-api` already reaches, which is why the surface lives here and
//! not in `boss-cybernetics`: a record written somewhere nothing
//! deploys is a record nobody can file.
//!
//! **Every route, the POST included, admits the same two categories as
//! the cadence surface: operator tier, or a trusted internal caller.**
//! An internal caller is one that arrived with no `x-boss-user` header
//! at all, which the extractor reports as `role=guest` — a loopback
//! sibling or a test harness, never a browser, because the gateway
//! injects the header for everything external and refuses a
//! session-less request before it forwards. A POST from such a caller
//! is attributed to `automation:platform` rather than to a person; see
//! [`record_run`].
//!
//! The two GET paths under `/api/agent-runs` are proxied at the human
//! door (`boss-gateway`, backlog 48bb0200); `/api/agent-rate-card` and
//! the POST are not, so those stay cluster-internal until something
//! needs them from a browser. [`is_trusted`] is therefore the whole
//! authorization story for a browser session reading per-actor spend,
//! which is why it is the thing the tests below pin first.

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

#[cfg(test)]
mod tests {
    //! The door's trust gate, pinned.
    //!
    //! Until the gateway grew `/api/agent-runs` (backlog 48bb0200) this
    //! surface was reachable only from inside the cluster, and
    //! [`is_trusted`] was the whole authorization story with no test
    //! under it. Now a browser session can arrive here, and what these
    //! rows say — which actor ran, for how long, at what price — is
    //! exactly the kind of read that is worse served than refused. So
    //! the gate gets a test before the door gets traffic.
    //!
    //! Note what is NOT here: a `PolicyClient` call. This surface
    //! consults no policy rule; it admits operator tier (and a
    //! header-less internal sibling) and refuses everything else. A
    //! guest session is the case that matters, because the gateway
    //! mints one for anyone who asks: it carries `role=audit-readonly`
    //! and `access_tier=user`, so it lands in `a_browser_guest_is_refused`
    //! below and gets a 403.

    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use boss_core::actor::ActorId;
    use chrono::Duration;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::agent_runs::{InMemoryAgentRuns, RateCardRow, RunOutcome, TokenUsage};

    /// Every read on this router, so a new one cannot be added without
    /// a decision about who may call it.
    const READS: &[&str] = &[
        "/api/agent-runs",
        "/api/agent-runs/cost",
        "/api/agent-rate-card",
    ];

    fn card() -> Vec<RateCardRow> {
        vec![RateCardRow {
            model: "opus-5[1m]".into(),
            input_usd_micros_per_mtok: 5_000_000,
            output_usd_micros_per_mtok: 25_000_000,
            note: "Claude Opus 5, 1M context".into(),
        }]
    }

    /// One run, shaped like the rows the live surface actually holds on
    /// 2026-09-11: an agent actor, a branch, and a bare total because
    /// the harness reports no input/output split.
    fn a_run() -> NewAgentRun {
        let finished = chrono::DateTime::parse_from_rfc3339("2026-09-10T15:06:40Z")
            .expect("fixture timestamp parses")
            .with_timezone(&Utc);
        NewAgentRun {
            run_id: "2026-09-10-branch-sweep".into(),
            actor_id: ActorId::Agent {
                mode: "claude".into(),
                model: "opus-5[1m]".into(),
            },
            started_at: finished - Duration::minutes(11),
            finished_at: finished,
            outcome: RunOutcome::Success,
            error: None,
            tokens: TokenUsage::TotalOnly { total: 134_392 },
            tool_calls: 70,
            job_id: None,
            branch: Some("fix/the-branch-sweep-inspects-every-landed-car".into()),
            detail: serde_json::Value::Null,
        }
    }

    async fn app() -> Router {
        let log = InMemoryAgentRuns::new(card());
        log.record_run(&a_run(), &ActorId::Automation("platform".into()))
            .await
            .expect("the fixture run records");
        router(AgentRunsApiState { log: Arc::new(log) })
    }

    fn header(role: &str, tier: AccessTier) -> String {
        serde_json::to_string(&User {
            id: "someone".into(),
            role: role.into(),
            access_tier: tier,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: None,
        })
        .expect("the user header serializes")
    }

    async fn get(path: &str, user: Option<String>) -> (StatusCode, String) {
        let mut req = Request::get(path);
        if let Some(u) = user {
            req = req.header("x-boss-user", u);
        }
        let resp = (app().await)
            .oneshot(req.body(Body::empty()).expect("request builds"))
            .await
            .expect("the router answers");
        let status = resp.status();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .expect("body collects")
            .to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn an_operator_reads_every_surface() {
        for path in READS {
            let (status, body) =
                get(path, Some(header("platform-admin", AccessTier::Operator))).await;
            assert_eq!(status, StatusCode::OK, "`{path}`: {body}");
        }
    }

    /// The leak this door exists to refuse. A guest session is what the
    /// gateway hands anyone who asks (`POST /api/auth/guest`), and it
    /// arrives here as `audit-readonly` at user tier — NOT as the
    /// `guest` role, which only a header-less internal sibling
    /// produces. `audit-readonly` holds Read at `Scope::All` on every
    /// resource the policy defaults declare, and per-actor spend is not
    /// one of them; whether it should be is a privilege decision, and
    /// until it is made the answer is 403 rather than the rows.
    #[tokio::test]
    async fn a_browser_guest_is_refused() {
        for path in READS {
            let (status, body) = get(path, Some(header("audit-readonly", AccessTier::User))).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "`{path}` served per-actor cost to a guest session: {body}"
            );
            assert!(
                !body.contains("opus-5"),
                "`{path}` leaked a row body to a refused caller: {body}"
            );
        }
    }

    #[tokio::test]
    async fn a_user_tier_employee_is_refused() {
        for path in READS {
            let (status, body) = get(path, Some(header("member", AccessTier::User))).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "`{path}`: {body}");
        }
    }

    /// A header-less caller is a loopback sibling or a test harness —
    /// the gateway always injects `x-boss-user` for anything arriving
    /// from outside, and `proxy::handle` refuses a session-less request
    /// with a 401 before it forwards. Same stance as the cadence and
    /// credential doors.
    #[tokio::test]
    async fn a_headerless_internal_caller_is_trusted() {
        for path in READS {
            let (status, body) = get(path, None).await;
            assert_eq!(status, StatusCode::OK, "`{path}`: {body}");
        }
    }

    /// The shape a reader gets off the list — the fields the Crew Board
    /// went looking for and could not reach.
    #[tokio::test]
    async fn the_list_carries_actor_branch_and_the_priced_total() {
        let (status, body) = get(
            "/api/agent-runs",
            Some(header("platform-admin", AccessTier::Operator)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        let rows: serde_json::Value = serde_json::from_str(&body).expect("the list is JSON");
        let row = &rows[0];
        assert_eq!(row["actor_id"], "claude:opus-5[1m]");
        assert_eq!(
            row["branch"],
            "fix/the-branch-sweep-inspects-every-landed-car"
        );
        assert_eq!(row["total_tokens"], 134_392);
        // A bare total cannot be priced, and the row says so rather
        // than reporting a dollar figure it does not have.
        assert!(row["usd_micros"].is_null(), "body: {body}");
    }

    /// The roll-up a cost surface would read. It already groups by
    /// model and by branch; `by_actor` is absent on purpose — the model
    /// key IS parsed out of `actor_id`, so under today's
    /// `<mode>:<model>` vocabulary the two groupings have the same
    /// buckets, and `actor_id` is a filter parameter on the list for
    /// the narrower question.
    #[tokio::test]
    async fn the_cost_read_names_the_filter_it_summed() {
        let (status, body) = get(
            "/api/agent-runs/cost?branch=fix/the-branch-sweep-inspects-every-landed-car",
            Some(header("platform-admin", AccessTier::Operator)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        let out: serde_json::Value = serde_json::from_str(&body).expect("the cost read is JSON");
        assert_eq!(
            out["branch"], "fix/the-branch-sweep-inspects-every-landed-car",
            "a summary must say what it summed: {body}"
        );
        assert_eq!(out["summary"]["runs"], 1);
        assert_eq!(out["summary"]["by_model"][0]["key"], "opus-5[1m]");
        assert!(out["summary"].get("by_actor").is_none(), "body: {body}");
    }

    /// `actor_id` is the filter the `(actor_id, finished_at)` index was
    /// built for, and the reason the raw rows answer "what is this
    /// actor building and what did it cost" without a new grouping.
    #[tokio::test]
    async fn the_list_filters_by_actor() {
        let user = Some(header("platform-admin", AccessTier::Operator));
        let (_, mine) = get("/api/agent-runs?actor_id=claude:opus-5[1m]", user.clone()).await;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&mine)
                .expect("JSON")
                .as_array()
                .map(Vec::len),
            Some(1),
            "body: {mine}"
        );
        let (_, theirs) = get("/api/agent-runs?actor_id=claude:haiku-4-5", user).await;
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&theirs)
                .expect("JSON")
                .as_array()
                .map(Vec::len),
            Some(0),
            "body: {theirs}"
        );
    }
}
