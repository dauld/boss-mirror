//! Axum routes for the agent-run record.
//!
//! Three reads and one write, all on the jobs API — the one door
//! `boss-api` already reaches, which is why the surface lives here and
//! not in `boss-cybernetics`: a record written somewhere nothing
//! deploys is a record nobody can file.
//!
//! **Every route, the POST included, admits the same two categories as
//! the cadence surface: operator tier, or a trusted internal caller** —
//! and the three reads admit the auditor tier besides ([`can_read`]).
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

/// The reads admit one more caller than the POST: the auditor tier —
/// the door `/api/events/*` already opens to it, and the tier the
/// recorded-probe reader carries (`infra/forge/run-car-probe.sh`,
/// `audit-readonly` at `auditor`). Without it no car can prove a
/// claim about a run through `boss-sor-read`, which is the one reader
/// a probe may use — found 2026-09-15 rehearsing this surface's own
/// probe: 403. The gateway's guest session is NOT this: it is
/// `audit-readonly` at USER tier, and stays refused.
fn can_read(user: &User) -> bool {
    is_trusted(user) || user.access_tier == AccessTier::Auditor
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
    if !can_read(&user) {
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
    if !can_read(&user) {
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
    if !can_read(&user) {
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
            model: None,
            tokens: TokenUsage::TotalOnly { total: 134_392 },
            tool_calls: 70,
            job_id: None,
            branch: Some("fix/the-branch-sweep-inspects-every-landed-car".into()),
            detail: serde_json::Value::Null,
        }
    }

    async fn app() -> Router {
        // The registry's one live row, as 20260915212644 seeds it.
        let log =
            InMemoryAgentRuns::new(card()).with_registered_agent("agent-claude", "opus-5[1m]");
        log.record_run(&a_run(), &ActorId::Automation("platform".into()))
            .await
            .expect("the fixture run records");
        router(AgentRunsApiState { log: Arc::new(log) })
    }

    async fn post(
        path: &str,
        body: serde_json::Value,
        user: Option<String>,
    ) -> (StatusCode, String) {
        let mut req = Request::post(path).header("content-type", "application/json");
        if let Some(u) = user {
            req = req.header("x-boss-user", u);
        }
        let resp = (app().await)
            .oneshot(
                req.body(Body::from(body.to_string()))
                    .expect("request builds"),
            )
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

    /// The recorded-probe reader (`infra/forge/run-car-probe.sh`:
    /// role `audit-readonly`, tier `auditor`) reads every surface and
    /// writes none — the same door `/api/events/*` already opens to
    /// that tier. Found 2026-09-15 by rehearsing this car's probe:
    /// `boss-sor-read /api/agent-runs` answered 403, so no car could
    /// ever prove a claim about a run through the one reader a probe
    /// is allowed to use.
    #[tokio::test]
    async fn the_probe_reader_reads_every_surface_and_cannot_write() {
        let auditor = header("audit-readonly", AccessTier::Auditor);
        for path in READS {
            let (status, body) = get(path, Some(auditor.clone())).await;
            assert_eq!(status, StatusCode::OK, "`{path}`: {body}");
        }
        let (status, body) = post(
            "/api/agent-runs",
            serde_json::json!({
                "run_id": "run-auditor",
                "actor_id": "agent-claude",
                "started_at": "2026-09-15T22:00:00Z",
                "finished_at": "2026-09-15T22:10:00Z",
                "outcome": "success",
                "total_tokens": 1
            }),
            Some(auditor),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "an auditor is a reader; the record is written by operators: {body}"
        );
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
        // The model is its own key on every row the list serves — this
        // one resolved out of the legacy colon form at record time.
        assert_eq!(row["model"], "opus-5[1m]", "body: {body}");
        // A bare total cannot be priced, and the row says so rather
        // than reporting a dollar figure it does not have.
        assert!(row["usd_micros"].is_null(), "body: {body}");
    }

    /// The shape the design decided (6fda05ae): a registered agent's
    /// id, model-free, with the model a fact about the run — from the
    /// body when it says, from the agent row's default when it does
    /// not. Posted through the door, read back off the answer.
    #[tokio::test]
    async fn a_registered_agents_report_carries_its_model_or_takes_the_default() {
        let user = Some(header("platform-admin", AccessTier::Operator));
        let report = |run_id: &str, model: Option<&str>| {
            serde_json::json!({
                "run_id": run_id,
                "actor_id": "agent-claude",
                "model": model,
                "started_at": "2026-09-15T22:00:00Z",
                "finished_at": "2026-09-15T22:10:00Z",
                "outcome": "success",
                "total_tokens": 1000,
                "tool_calls": 1
            })
        };

        let (status, body) = post(
            "/api/agent-runs",
            report("run-says", Some("haiku-4-5")),
            user.clone(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        let out: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(out["run"]["actor_id"], "agent-claude");
        assert_eq!(
            out["run"]["model"], "haiku-4-5",
            "the report's own word: {body}"
        );

        let (status, body) = post("/api/agent-runs", report("run-silent", None), user).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        let out: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(
            out["run"]["model"], "opus-5[1m]",
            "the agent row's default: {body}"
        );
    }

    /// A registered id no `agents` row backs, reporting no model, is a
    /// 400 that names both fixes — not a row with a NULL model.
    #[tokio::test]
    async fn an_unregistered_agents_silent_report_is_refused_naming_the_fixes() {
        let (status, body) = post(
            "/api/agent-runs",
            serde_json::json!({
                "run_id": "run-nobody",
                "actor_id": "agent-nobody",
                "started_at": "2026-09-15T22:00:00Z",
                "finished_at": "2026-09-15T22:10:00Z",
                "outcome": "success",
                "total_tokens": 1000
            }),
            Some(header("platform-admin", AccessTier::Operator)),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
        assert!(body.contains("agent-nobody"), "{body}");
        assert!(body.contains("register the agent"), "{body}");
    }

    /// The roll-up a cost surface would read. It already groups by
    /// model and by branch; `by_actor` is absent on purpose — every
    /// live row is one actor (`agent-claude`, or its colon-form
    /// spelling on older rows), so an actor grouping would be one
    /// bucket, and `actor_id` is a filter parameter on the list for
    /// the narrower question. The model bucket reads the run's own
    /// `model` column since design 6fda05ae.
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
