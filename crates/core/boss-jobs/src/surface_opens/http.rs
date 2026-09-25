//! Axum routes for surface opens: the SPA's one write, the roll-up read
//! the chore and the page share, and the retention sweep.
//!
//! **The write signs as the session.** `POST /api/surface-opens` takes
//! `{route, at}` and nothing else; the actor is the `x-boss-user` the
//! gateway injected from the cookie (`role_headers.rs` strips any copy
//! the client sent), the way every write on this service is signed. A
//! header-less caller has no actor to credit and is refused (400), and
//! a machine-shaped actor — an automation, an agent login the door
//! resolved — is refused (403) with the reason: an automation has no
//! surface to open, and a count that could include one would answer
//! "what does the operator open" with a number that is partly a bot's.
//!
//! **The roll-up is one read for two readers.** `GET
//! /api/surface-opens/rollup?since=…[&until=…]` answers the chore
//! (`infra/surface-usage.sh`, a 24-hour window, filed as `measured`)
//! and the Codebase page's Surfaces section (a 7-day window) with the
//! same GROUP BY, so the two cannot disagree by summing differently.
//! Readable by any signed-in session except the gateway's guest (a
//! read-only-floor role at user tier): the rows are an operator's own
//! attention, and the Codebase page that shows them is readable by any
//! operator already. Trusted internal callers and the auditor tier (the
//! recorded-probe reader) read too.
//!
//! **The sweep is operator machinery.** `POST /api/surface-opens/sweep`
//! deletes rows older than [`super::RETENTION_DAYS`] and answers with
//! the count and the cutoff, so the chore's row can state what the
//! sweep did rather than that it ran. Operator tier or trusted internal.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;

use boss_policy_client::{AccessTier, CurrentUser, User};

use crate::trust::is_trusted;

use super::port::{SurfaceOpens, SurfaceOpensError, sweep_retention};
use super::types::{NewSurfaceOpen, validate_route};
use crate::owner_resolution::is_automation_shaped;

pub struct SurfaceOpensApiState {
    pub repo: Arc<dyn SurfaceOpens>,
}

// Writes admit `crate::trust::is_trusted` — the operator machinery every
// operator door admits (839335b7). The read below is deliberately WIDER
// than `crate::trust::can_read`, and says why.

/// Everyone but the gateway's guest session, which arrives at USER tier
/// carrying a read-only-floor role (`POST /api/auth/guest`: `visitor`,
/// or `audit-readonly` where the instance opts in — design 2830b6b7).
/// The floor predicate, not the name `audit-readonly`, which the new
/// `visitor` passed. An employee's ordinary session (user tier, their
/// own role) reads: the page that renders this sits in a department any
/// operator opens.
fn can_read(user: &User) -> bool {
    !(user.access_tier == AccessTier::User && boss_core::roles::is_read_only_floor(&user.role))
}

pub fn router(state: SurfaceOpensApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/surface-opens", post(record_open))
        .route("/api/surface-opens/rollup", get(rollup))
        .route("/api/surface-opens/sweep", post(sweep))
        .with_state(shared)
}

fn err_response(e: SurfaceOpensError) -> Response {
    match e {
        SurfaceOpensError::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
        SurfaceOpensError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

async fn record_open(
    State(state): State<Arc<SurfaceOpensApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<NewSurfaceOpen>,
) -> Response {
    // A header-less caller is a sibling service or a harness: there is
    // no session to credit, and crediting the extractor's `anonymous`
    // would be a row about nobody.
    if user.role == "guest" {
        return (
            StatusCode::BAD_REQUEST,
            "no session actor — a surface open is credited to the signed-in session, \
             and this request carried none",
        )
            .into_response();
    }
    if is_automation_shaped(&user.id) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "a surface open is an operator's, not an automation's",
                "actor_id": user.id,
                "why": "the id is machine-shaped (an automation or an agent session); \
                        agents do not run the SPA, so nothing of theirs is counted here",
            })),
        )
            .into_response();
    }
    if let Err(why) = validate_route(&body.route) {
        return (StatusCode::BAD_REQUEST, why).into_response();
    }
    // A record stamp, on the one real clock: the SPA's own `at` when it
    // sent one, else now. This is not a business date, so it does not
    // route through the sim-aware clock port (no-wallclock.sh names
    // `wall_now` as the sanctioned source for exactly this).
    let at = body.at.unwrap_or_else(boss_clock_client::wall_now);
    match state.repo.record(&user.id, &body.route, at).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err_response(e),
    }
}

#[derive(Debug, Deserialize)]
struct RollupQuery {
    since: DateTime<Utc>,
    #[serde(default)]
    until: Option<DateTime<Utc>>,
}

async fn rollup(
    State(state): State<Arc<SurfaceOpensApiState>>,
    CurrentUser(user): CurrentUser,
    Query(q): Query<RollupQuery>,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let until = q.until.unwrap_or_else(boss_clock_client::wall_now);
    match state.repo.rollup(q.since, until).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => err_response(e),
    }
}

async fn sweep(
    State(state): State<Arc<SurfaceOpensApiState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match sweep_retention(state.repo.as_ref(), boss_clock_client::wall_now()).await {
        Ok(s) => Json(s).into_response(),
        Err(e) => err_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use chrono::Duration;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use crate::surface_opens::InMemorySurfaceOpens;

    fn header(id: &str, role: &str, tier: AccessTier) -> String {
        serde_json::to_string(&User {
            id: id.into(),
            role: role.into(),
            access_tier: tier,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: None,
        })
        .expect("the user header serializes")
    }

    fn david() -> Option<String> {
        Some(header("emp-david", "platform-admin", AccessTier::User))
    }

    async fn send(
        app: Router,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
        user: Option<String>,
    ) -> (StatusCode, String) {
        let mut req = Request::builder().method(method).uri(path);
        if body.is_some() {
            req = req.header("content-type", "application/json");
        }
        if let Some(u) = user {
            req = req.header("x-boss-user", u);
        }
        let body = body.map(|b| Body::from(b.to_string())).unwrap_or_default();
        let resp = app
            .oneshot(req.body(body).expect("request builds"))
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

    fn app(repo: &Arc<InMemorySurfaceOpens>) -> Router {
        router(SurfaceOpensApiState {
            repo: repo.clone() as Arc<dyn SurfaceOpens>,
        })
    }

    fn open(route: &str) -> serde_json::Value {
        serde_json::json!({ "route": route, "at": "2026-09-16T12:00:00Z" })
    }

    /// The whole mechanism in one pass: a session posts a pattern, the
    /// row is credited to the SESSION's id (the body never named one),
    /// and the roll-up counts it.
    #[tokio::test]
    async fn a_session_open_is_credited_to_the_session_and_rolled_up() {
        let repo = Arc::new(InMemorySurfaceOpens::new());
        for route in ["/it", "/it", "/it/codebase"] {
            let (status, body) = send(
                app(&repo),
                "POST",
                "/api/surface-opens",
                Some(open(route)),
                david(),
            )
            .await;
            assert_eq!(status, StatusCode::NO_CONTENT, "{route}: {body}");
        }
        let rows = repo.rows().await;
        assert!(rows.iter().all(|r| r.actor_id == "emp-david"), "{rows:?}");

        let (status, body) = send(
            app(&repo),
            "GET",
            "/api/surface-opens/rollup?since=2026-09-16T00:00:00Z&until=2026-09-17T00:00:00Z",
            None,
            david(),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let r: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(r["rows"][0]["actor_id"], "emp-david");
        assert_eq!(r["rows"][0]["route"], "/it");
        assert_eq!(r["rows"][0]["opens"], 2);
        assert_eq!(r["rows"][1]["route"], "/it/codebase");
        assert_eq!(r["rows"][1]["opens"], 1);
    }

    /// The body cannot name the actor. A client that tries is not
    /// refused — the key is simply not read — and the row is still the
    /// session's, which is what makes the count trustworthy.
    #[tokio::test]
    async fn a_body_naming_an_actor_is_ignored_in_favour_of_the_session() {
        let repo = Arc::new(InMemorySurfaceOpens::new());
        let (status, body) = send(
            app(&repo),
            "POST",
            "/api/surface-opens",
            Some(serde_json::json!({ "route": "/it", "actor_id": "emp-somebody-else" })),
            david(),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert_eq!(repo.rows().await[0].actor_id, "emp-david");
    }

    /// An automation and an agent login (the id the login door resolves
    /// `claude@…` to) are refused BY SHAPE, with the reason on the body.
    #[tokio::test]
    async fn a_machine_shaped_actor_is_refused_with_the_reason() {
        let repo = Arc::new(InMemorySurfaceOpens::new());
        for id in [
            "automation:train-conductor",
            "agent-claude",
            "claude:opus-5",
        ] {
            let (status, body) = send(
                app(&repo),
                "POST",
                "/api/surface-opens",
                Some(open("/it")),
                Some(header(id, "platform-admin", AccessTier::Operator)),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{id}: {body}");
            assert!(body.contains("machine-shaped"), "{id}: {body}");
        }
        assert!(repo.rows().await.is_empty());
    }

    /// A header-less caller has no session to credit: 400, not a row
    /// about `anonymous`.
    #[tokio::test]
    async fn a_headerless_caller_cannot_record_an_open() {
        let repo = Arc::new(InMemorySurfaceOpens::new());
        let (status, body) = send(
            app(&repo),
            "POST",
            "/api/surface-opens",
            Some(open("/it")),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(repo.rows().await.is_empty());
    }

    #[tokio::test]
    async fn a_route_that_is_not_a_pattern_is_refused() {
        let repo = Arc::new(InMemorySurfaceOpens::new());
        let (status, body) = send(
            app(&repo),
            "POST",
            "/api/surface-opens",
            Some(open("/ux/jobs?kind=x")),
            david(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("query"), "{body}");
    }

    /// A client with no `at` still records — stamped by the door.
    #[tokio::test]
    async fn an_open_without_a_time_is_stamped_by_the_door() {
        let repo = Arc::new(InMemorySurfaceOpens::new());
        let before = boss_clock_client::wall_now() - Duration::seconds(5);
        let (status, body) = send(
            app(&repo),
            "POST",
            "/api/surface-opens",
            Some(serde_json::json!({ "route": "/it" })),
            david(),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
        assert!(repo.rows().await[0].at >= before);
    }

    /// The gateway's guest is the one reader refused; an employee at
    /// user tier, the auditor tier (the probe reader) and a header-less
    /// sibling all read.
    #[tokio::test]
    async fn the_rollup_reads_for_everyone_but_the_gateway_guest() {
        let repo = Arc::new(InMemorySurfaceOpens::new());
        let path = "/api/surface-opens/rollup?since=2026-09-16T00:00:00Z";
        let (status, _) = send(
            app(&repo),
            "GET",
            path,
            None,
            Some(header(
                "guest@algedonic.dev",
                "audit-readonly",
                AccessTier::User,
            )),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        for user in [
            david(),
            Some(header(
                "audit-readonly",
                "audit-readonly",
                AccessTier::Auditor,
            )),
            None,
        ] {
            let (status, body) = send(app(&repo), "GET", path, None, user.clone()).await;
            assert_eq!(status, StatusCode::OK, "{user:?}: {body}");
        }
    }

    /// Design 2830b6b7: the OSS guest arrives as `visitor` at user tier,
    /// and the refusal is the read-only floor predicate's, not the name
    /// `audit-readonly` — keyed on the name, a guest under the new role
    /// read an operator's own attention.
    #[tokio::test]
    async fn the_rollup_is_refused_to_every_read_only_floor_session() {
        let repo = Arc::new(InMemorySurfaceOpens::new());
        let path = "/api/surface-opens/rollup?since=2026-09-16T00:00:00Z";
        for role in boss_core::roles::READ_ONLY_FLOOR_ROLES {
            let (status, body) = send(
                app(&repo),
                "GET",
                path,
                None,
                Some(header("guest@algedonic.dev", role, AccessTier::User)),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{role}: {body}");
        }
    }

    /// `since` alone is a window ending now; a window that holds
    /// nothing is a 400, never an empty list.
    #[tokio::test]
    async fn a_window_that_holds_nothing_is_refused() {
        let repo = Arc::new(InMemorySurfaceOpens::new());
        let (status, body) = send(
            app(&repo),
            "GET",
            "/api/surface-opens/rollup?since=2099-01-01T00:00:00Z",
            None,
            david(),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.contains("holds nothing"), "{body}");
    }

    /// The sweep is operator machinery and answers with what it did:
    /// the count, the cutoff, and the retention it applied.
    #[tokio::test]
    async fn the_sweep_deletes_past_retention_and_reports_the_cutoff() {
        let repo = Arc::new(InMemorySurfaceOpens::new());
        let now = boss_clock_client::wall_now();
        repo.record("emp-david", "/it", now - Duration::days(31))
            .await
            .unwrap();
        repo.record("emp-david", "/it", now - Duration::days(1))
            .await
            .unwrap();

        let (status, _) = send(
            app(&repo),
            "POST",
            "/api/surface-opens/sweep",
            None,
            david(),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "a user-tier session is not the sweeper"
        );

        let (status, body) = send(
            app(&repo),
            "POST",
            "/api/surface-opens/sweep",
            None,
            Some(header(
                "automation:surface-usage",
                "platform-admin",
                AccessTier::Operator,
            )),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let s: serde_json::Value = serde_json::from_str(&body).expect("JSON");
        assert_eq!(s["deleted"], 1, "{body}");
        assert_eq!(s["retention_days"], super::super::RETENTION_DAYS, "{body}");
        assert!(s["before"].as_str().is_some(), "{body}");
        assert_eq!(repo.rows().await.len(), 1);
    }
}
