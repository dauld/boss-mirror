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

use boss_policy_client::CurrentUser;

use crate::trust::{can_read, is_trusted};

use super::port::{AgentRunError, AgentRunLog};
use super::types::{AgentRunView, NewAgentRun, RunFilter, RunSummary, summarize};

pub struct AgentRunsApiState {
    pub log: Arc<dyn AgentRunLog>,
}

// Who this door admits lives in `crate::trust` (839335b7): this door
// was the first to learn (2026-09-15) that its reads must admit the
// auditor tier the recorded-probe reader carries, and the other four
// operator doors had not — so the predicate pair moved out.

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
        // 409, the status every other refused-by-state write on this
        // service answers with (a terminal step, incomplete sign-offs):
        // the report was well-formed, and the record's state — the
        // actor's spend against its cap — is what refused it. The body
        // is the decision, not a bare string, so a caller can show it.
        AgentRunError::Denied { reason } => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "run refused against the actor's budget",
                "budget": { "kind": "deny", "reason": reason },
                "hint": "the refusal is on the log as agents.run.denied; \
                         the window rolls an hour after the spend it counted",
            })),
        )
            .into_response(),
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
    /// The held row with its derived basis — see [`AgentRunView`].
    pub run: AgentRunView,
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
        Ok(runs) => {
            Json(runs.into_iter().map(AgentRunView::from).collect::<Vec<_>>()).into_response()
        }
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
                run: out.run.into(),
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
    use boss_policy_client::{AccessTier, User};
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
            // The live row's declared ratio (design 91a9bfe7), so
            // these tests see the shape the surface actually serves: a
            // total-only run, priced at the blend, saying so.
            blended_input_share_ppm: Some(875_000),
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

    /// The recorded-probe reader (boss-cli prove.rs, the unattended door:
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
        // A bare total IS priced now, at the model's declared blend
        // (design 91a9bfe7): 134,392 tokens at $7.50/MTok. Until
        // 2026-09-20 this row read null, which is how 83 of 85 recorded
        // runs came to carry no cost at all. The figure is an estimate
        // and the record says so — the basis is derived from the two
        // fields on this very row, `input_tokens` null beside a
        // `usd_micros` that is not, and the roll-up at
        // `/api/agent-runs/cost` carries it as a word.
        assert_eq!(row["usd_micros"], 1_007_940, "body: {body}");
        assert_eq!(row["priced_by"], "opus-5[1m]", "body: {body}");
        assert!(row["input_tokens"].is_null(), "body: {body}");
        // And the row SAYS so, as the roll-up does (backlog 93fdb119):
        // until this key rode on the row, a per-row surface could tell
        // blended from measured only by re-deriving the server's rule
        // from `input_tokens` and `usd_micros` itself.
        assert_eq!(row["pricing_basis"], "blended", "body: {body}");
    }

    /// The POST's answer is a single run too, and a caller reading it
    /// (`boss dispatch --report`) must be able to take the basis off it
    /// rather than recompute it. All three answers, from one rule: a
    /// measured split, a blend, and no figure at all — which is `null`,
    /// never `split`, because there is no number to describe.
    #[tokio::test]
    async fn a_recorded_run_names_the_basis_of_its_own_figure() {
        let user = Some(header("platform-admin", AccessTier::Operator));
        let report = |run_id: &str, model: &str, tokens: serde_json::Value| {
            let mut body = serde_json::json!({
                "run_id": run_id,
                "actor_id": "agent-claude",
                "model": model,
                "started_at": "2026-09-15T22:00:00Z",
                "finished_at": "2026-09-15T22:10:00Z",
                "outcome": "success",
            });
            if let (Some(obj), Some(t)) = (body.as_object_mut(), tokens.as_object()) {
                obj.extend(t.clone());
            }
            body
        };
        let cases = [
            (
                report(
                    "run-split",
                    "opus-5[1m]",
                    serde_json::json!({"input_tokens": 1000, "output_tokens": 200}),
                ),
                serde_json::json!("split"),
            ),
            (
                report(
                    "run-blend",
                    "opus-5[1m]",
                    serde_json::json!({"total_tokens": 1000}),
                ),
                serde_json::json!("blended"),
            ),
            (
                report(
                    "run-unpriced",
                    "a-model-no-card-row-covers",
                    serde_json::json!({"input_tokens": 1000, "output_tokens": 200}),
                ),
                serde_json::Value::Null,
            ),
        ];
        for (body, want) in cases {
            let (status, out) = post("/api/agent-runs", body, user.clone()).await;
            assert_eq!(status, StatusCode::OK, "body: {out}");
            let out: serde_json::Value = serde_json::from_str(&out).expect("JSON");
            let run = &out["run"];
            assert!(
                run.as_object()
                    .is_some_and(|o| o.contains_key("pricing_basis")),
                "the key is always present, null included: {out}"
            );
            assert_eq!(run["pricing_basis"], want, "{out}");
        }
    }

    /// The roll-up a surface reads, saying what its figure rests on.
    #[tokio::test]
    async fn the_cost_roll_up_names_the_basis_beside_the_figure() {
        let (status, body) = get(
            "/api/agent-runs/cost",
            Some(header("platform-admin", AccessTier::Operator)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        let out: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        let summary = &out["summary"];
        assert_eq!(summary["usd_micros"], 1_007_940, "body: {body}");
        assert_eq!(
            summary["pricing_basis"], "blended",
            "a figure from a total must not read as a measurement: {body}"
        );
        assert_eq!(summary["blended_runs"], 1, "body: {body}");
        assert_eq!(summary["by_model"][0]["pricing_basis"], "blended");
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

    /// A refused run answers 409 with the decision in the body — the
    /// status every other refused-by-state write on this service uses
    /// — and an admitted one carries its decision on the run. Through
    /// the door, so the wire shape is what is pinned: a caller reads
    /// `budget.kind` off either answer.
    #[tokio::test]
    async fn a_refused_run_is_a_409_carrying_the_decision() {
        // A cap of zero is a declared cap: the agent is switched off.
        let log = InMemoryAgentRuns::new(card()).with_budgeted_agent(
            "agent-claude",
            "opus-5[1m]",
            boss_core::agent::AgentCaps {
                hourly_budget_usd_micros: Some(0),
                max_concurrent_runs: None,
            },
        );
        let app = router(AgentRunsApiState { log: Arc::new(log) });
        let req = Request::post("/api/agent-runs")
            .header("content-type", "application/json")
            .header(
                "x-boss-user",
                header("platform-admin", AccessTier::Operator),
            )
            .body(Body::from(
                serde_json::json!({
                    "run_id": "run-refused",
                    "actor_id": "agent-claude",
                    "started_at": "2026-09-15T22:00:00Z",
                    "finished_at": "2026-09-15T22:10:00Z",
                    "outcome": "success",
                    "input_tokens": 900,
                    "output_tokens": 100
                })
                .to_string(),
            ))
            .expect("request builds");
        let resp = app.oneshot(req).await.expect("the router answers");
        let status = resp.status();
        let body = String::from_utf8_lossy(
            &resp
                .into_body()
                .collect()
                .await
                .expect("body collects")
                .to_bytes(),
        )
        .into_owned();
        assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
        let out: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(out["budget"]["kind"], "deny", "body: {body}");
        assert!(
            out["budget"]["reason"]
                .as_str()
                .is_some_and(|r| r.contains("0 of 0")),
            "body: {body}"
        );
    }

    /// The unbudgeted case is the live one (agent-claude, both caps
    /// NULL): the run is admitted and the answer says so, with nothing
    /// to count down.
    #[tokio::test]
    async fn an_admitted_run_answers_with_its_decision() {
        let (status, body) = post(
            "/api/agent-runs",
            serde_json::json!({
                "run_id": "run-admitted",
                "actor_id": "agent-claude",
                "started_at": "2026-09-15T22:00:00Z",
                "finished_at": "2026-09-15T22:10:00Z",
                "outcome": "success",
                "total_tokens": 1000
            }),
            Some(header("platform-admin", AccessTier::Operator)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        let out: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(out["run"]["budget"]["kind"], "allow", "body: {body}");
        assert!(
            out["run"]["budget"]["remaining_usd_micros"].is_null(),
            "no cap declared: {body}"
        );
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
