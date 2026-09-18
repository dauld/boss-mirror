//! Axum routes for the cadence registry.
//!
//! This surface exists so the train conductor can run OUTSIDE the
//! cluster without a database connection. It reaches the same
//! `boss-jobs-internal` door it already uses for the dock probe, so
//! the conductor needs exactly one address to do its whole job.
//!
//! Since 2026-09-18 (backlog 13d1fff3) it is also the operator's
//! WRITE door on the registry: `POST /api/cadence/rules/{name}/retire`
//! and `POST /api/cadence/rules/{name}/publish`, in front of
//! `CadenceRegistry`. Before them a live rule could be re-versioned
//! only by a migration — refused since the cutover stamp in
//! `infra/lint/migrations-declare-schema-only.sh` — and a retire had
//! no path at all; the department-retro car left `protocol-retro-daily`
//! redundant with nothing to run. The two writes are policy-gated the
//! way the workflow and station routes are (`Action::Retire` /
//! `Action::Publish` on `Resource::workflow()`), not by the
//! machinery-trust predicate the conductor's claim uses: they are a
//! person's decision about the operating model, and the policy layer
//! is where a person's writes are granted.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};

use boss_policy_client::{Action, CurrentUser, Decision, PolicyClient, Resource};

use crate::registry::WorkflowStatus;
use crate::trust::{can_read, is_trusted};

use super::port::{CadenceError, CadenceRegistry, CadenceRepository};
use super::types::{CadenceRuleSpec, ClaimResult, FiringOutcome, NewFiring};

pub struct CadenceApiState {
    /// The conductor's half: active rules, firings, outcomes.
    pub repo: Arc<dyn CadenceRepository>,
    /// The registry half: the lineage, publish and retire. The same
    /// `PgCadence` behind both in production; the seed's writer too.
    pub registry: Arc<dyn CadenceRegistry>,
    /// Row-level authorization for the two writes — the same client
    /// the main router's workflow and station routes ask.
    pub policy: Arc<dyn PolicyClient>,
    /// The authoritative clock, stamping `created_at` on a publish
    /// (clock-routed, never wallclock — sim-mode edits stamp sim time).
    pub clock: Arc<dyn boss_clock_client::ClockClient>,
}

// Who this door admits lives in `crate::trust`, with the four other
// operator doors (839335b7): reads admit the auditor tier the
// recorded-probe reader carries; the conductor's writes are operator
// machinery only; the registry writes are policy-gated (below).

pub fn router(state: CadenceApiState) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/cadence/rules", get(list_rules))
        .route("/api/cadence/rules/{name}/last-firing", get(last_firing))
        // The lineage — every version of a name, any status — so a
        // retire or a publish can be read back the way stations' and
        // workflows' can.
        .route("/api/cadence/rules/{name}/versions", get(list_versions))
        .route("/api/cadence/rules/{name}/publish", post(publish_rule))
        .route("/api/cadence/rules/{name}/retire", post(retire_rule))
        .route("/api/cadence/firings", post(claim_firing))
        .route("/api/cadence/firings/{id}/outcome", post(record_outcome))
        .with_state(shared)
}

/// The workflow routes' gate, asked of the same resource: a cadence
/// rule is protocol data (protocol-cadence.md), and the shipped
/// defaults grant `platform-admin` every action on it.
async fn policy_check(
    state: &CadenceApiState,
    user: &boss_policy_client::User,
    action: Action,
) -> Result<(), Response> {
    match state.policy.check(user, action, Resource::workflow()).await {
        Ok(Decision::Allow { .. }) => Ok(()),
        Ok(Decision::Deny { reason }) => Err((StatusCode::FORBIDDEN, reason).into_response()),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("policy check failed: {e}"),
        )
            .into_response()),
    }
}

/// The who + when a registry write hands the adapter — the
/// `http::kinds::write_stamp` contract: the session's identity,
/// falling back to the platform automation for a header-less
/// internal call; `now` from the authoritative clock.
async fn write_stamp(
    state: &CadenceApiState,
    user: &boss_policy_client::User,
) -> (boss_core::actor::ActorId, chrono::DateTime<chrono::Utc>) {
    let actor = user
        .ambient_actor()
        .unwrap_or_else(|| boss_core::actor::ActorId::Automation("platform".into()));
    let now = boss_clock_client::now_from(&state.clock).await;
    (actor, now)
}

async fn list_versions(
    State(state): State<Arc<CadenceApiState>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.registry.live_versions(&name).await {
        // `[]` for a name never held — a read, like the stations'.
        Ok(rows) => Json(rows).into_response(),
        Err(e) => err_response(e),
    }
}

/// `POST /api/cadence/rules/{name}/publish` — body = the row in the
/// bundle file's shape (`infra/platform/cadence/<name>.toml`'s
/// `[[cadence_rule]]` as JSON; `created_at` ignored, stamped here).
/// Retire-then-insert at the declared version through
/// `CadenceRegistry::publish_declared`; 409 when the version is not
/// above the newest of the lineage; 200 with the row written.
async fn publish_rule(
    State(state): State<Arc<CadenceApiState>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
    Json(spec): Json<CadenceRuleSpec>,
) -> Response {
    if let Err(r) = policy_check(&state, &user, Action::Publish).await {
        return r;
    }
    if spec.name() != name {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "the body declares `{}` but the path names `{name}`",
                spec.name()
            ),
        )
            .into_response();
    }
    if spec.status != WorkflowStatus::Active {
        // The seed loader refuses the same file; a body that arrived
        // another way gets the same answer.
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "a published row is active; the body declares `{}` — retiring is \
                 POST /api/cadence/rules/{name}/retire",
                spec.status.as_str()
            ),
        )
            .into_response();
    }
    let (actor, now) = write_stamp(&state, &user).await;
    match state.registry.publish_declared(spec, &actor, now).await {
        Ok(written) => Json(written).into_response(),
        Err(e) => err_response(e),
    }
}

/// `POST /api/cadence/rules/{name}/retire` — retire the active
/// version; 200 with the row retired, 404 when nothing of the name is
/// active (the operator retiring by name is told it was not live —
/// never a silent 204).
async fn retire_rule(
    State(state): State<Arc<CadenceApiState>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    if let Err(r) = policy_check(&state, &user, Action::Retire).await {
        return r;
    }
    let (actor, now) = write_stamp(&state, &user).await;
    match state.registry.retire(&name, &actor, now).await {
        Ok(Some(retired)) => Json(retired).into_response(),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            format!("no active cadence rule named `{name}` — nothing retired"),
        )
            .into_response(),
        Err(e) => err_response(e),
    }
}

fn err_response(e: CadenceError) -> Response {
    match e {
        CadenceError::BadRequest(m) => (StatusCode::BAD_REQUEST, m).into_response(),
        CadenceError::Conflict(m) => (StatusCode::CONFLICT, m).into_response(),
        CadenceError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

async fn list_rules(
    State(state): State<Arc<CadenceApiState>>,
    CurrentUser(user): CurrentUser,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.repo.active_rules().await {
        Ok(rules) => Json(rules).into_response(),
        Err(e) => err_response(e),
    }
}

async fn last_firing(
    State(state): State<Arc<CadenceApiState>>,
    CurrentUser(user): CurrentUser,
    Path(name): Path<String>,
) -> Response {
    if !can_read(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state.repo.last_firing(&name).await {
        // `null` for "never fired" — the conductor treats it as
        // "every window is a candidate", so a 404 would be wrong.
        Ok(f) => Json(f).into_response(),
        Err(e) => err_response(e),
    }
}

async fn claim_firing(
    State(state): State<Arc<CadenceApiState>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<NewFiring>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if body.firing_id.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "firing_id is required").into_response();
    }
    match state.repo.claim_firing(&body).await {
        // A lost claim is a normal, expected outcome — not an error.
        // It is reported as 200 + `{"claimed": false}` rather than 409
        // so the conductor's retry policy (which retries 5xx and
        // treats 4xx as fatal) never sees a race as a failure.
        Ok(claimed) => Json(ClaimResult { claimed }).into_response(),
        Err(e) => err_response(e),
    }
}

async fn record_outcome(
    State(state): State<Arc<CadenceApiState>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<FiringOutcome>,
) -> Response {
    if !is_trusted(&user) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state
        .repo
        .record_outcome(&id, body.rc, body.runtime_secs)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err_response(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cadence::InMemoryCadence;
    use axum::body::Body;
    use axum::http::Request;
    use boss_policy_client::{AccessTier, User};
    use tower::ServiceExt;

    use crate::cadence::{CadenceRuleRow, CadenceRuleSpec};
    use crate::registry::WorkflowStatus;
    use boss_clock_client::{ClockNow, FixedClockClient};
    use boss_policy_client::{Action, FakePolicyClient, PolicyClient, Resource, Scope};
    use chrono::{DateTime, TimeZone, Utc};
    use http_body_util::BodyExt;

    /// The door's clock — what every publish stamps as `created_at`.
    fn door_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 18, 17, 30, 0).unwrap()
    }

    /// The real router over the in-memory adapters: one registry
    /// backing both halves (as `PgCadence` does), the platform
    /// operator granted Publish + Retire on `workflow` as the shipped
    /// defaults grant `platform-admin`, and nothing granted to anyone
    /// else.
    fn app_with(cadence: Arc<InMemoryCadence>) -> Router {
        let policy: Arc<dyn PolicyClient> = Arc::new(
            FakePolicyClient::builder()
                .allow(
                    "platform-admin",
                    Action::Publish,
                    Resource::workflow(),
                    Scope::All,
                )
                .allow(
                    "platform-admin",
                    Action::Retire,
                    Resource::workflow(),
                    Scope::All,
                )
                .build(),
        );
        router(CadenceApiState {
            repo: cadence.clone(),
            registry: cadence,
            policy,
            clock: Arc::new(FixedClockClient::new(ClockNow {
                now: door_now(),
                simulated: false,
                epoch_start: None,
                epoch_end: None,
                paused: false,
                restart_in_progress: false,
                warp_factor: None,
            })),
        })
    }

    fn app() -> Router {
        app_with(Arc::new(InMemoryCadence::default()))
    }

    fn reconcile_row(every: i32) -> CadenceRuleRow {
        CadenceRuleRow {
            name: "train-reconcile".into(),
            verb: "reconcile".into(),
            basis: "wall".into(),
            every_minutes: Some(every),
            at_times: None,
            min_dock_depth: None,
            cooldown_minutes: None,
            cadence: None,
            anchor_date: None,
            business_calendar: None,
        }
    }

    /// The operator, as `boss cadence …` signs: platform-admin at
    /// operator tier, id = the actor running the verb.
    fn operator() -> String {
        serde_json::to_string(&User {
            id: "emp-david".into(),
            role: "platform-admin".into(),
            access_tier: AccessTier::Operator,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: Some("platform".into()),
        })
        .unwrap()
    }

    async fn call(
        app: &Router,
        method: &str,
        path: &str,
        user: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder()
            .method(method)
            .uri(path)
            .header("x-boss-user", user);
        let body = match body {
            Some(b) => {
                req = req.header("content-type", "application/json");
                Body::from(b.to_string())
            }
            None => Body::empty(),
        };
        let resp = app.clone().oneshot(req.body(body).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
        (status, json)
    }

    /// The bundle file's shape on the wire — `[[cadence_rule]]`'s
    /// columns as JSON, no `created_at`: what `boss cadence publish
    /// <file.toml>` sends after the seed loader's parse.
    fn publish_body(version: i32, every: i32) -> serde_json::Value {
        serde_json::json!({
            "name": "train-reconcile",
            "version": version,
            "status": "active",
            "verb": "reconcile",
            "basis": "wall",
            "every_minutes": every,
        })
    }

    // ------------------------------------------------------------------
    // The write door (backlog 13d1fff3).
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn a_retire_answers_the_retired_row_then_404_and_records_once() {
        let cadence = Arc::new(InMemoryCadence::new(vec![reconcile_row(10)]));
        let app = app_with(cadence.clone());
        let (st, body) = call(
            &app,
            "POST",
            "/api/cadence/rules/train-reconcile/retire",
            &operator(),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["name"], "train-reconcile");
        assert_eq!(body["version"], 1);
        assert_eq!(body["status"], "retired");

        // The conductor's read no longer serves it …
        let (st, rules) = call(&app, "GET", "/api/cadence/rules", &operator(), None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(rules, serde_json::json!([]));
        // … the lineage still holds it, retired …
        let (st, lineage) = call(
            &app,
            "GET",
            "/api/cadence/rules/train-reconcile/versions",
            &operator(),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(lineage[0]["status"], "retired");
        // … a second retire finds nothing active — 404, not a silent
        // 204: the operator retiring by name is told the name was not
        // live …
        let (st, body) = call(
            &app,
            "POST",
            "/api/cadence/rules/train-reconcile/retire",
            &operator(),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{body}");
        assert!(
            body.as_str()
                .unwrap_or_default()
                .contains("train-reconcile"),
            "the refusal names the rule: {body}"
        );
        let (st, _) = call(
            &app,
            "POST",
            "/api/cadence/rules/never-declared/retire",
            &operator(),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::NOT_FOUND);
        // … and exactly one jobs.cadence.retired was recorded, signed
        // by the operator, not the platform automation.
        let events = cadence.recorded_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, crate::events::CADENCE_RETIRED);
        assert_eq!(events[0].payload["_actor"], "emp-david");
    }

    #[tokio::test]
    async fn a_publish_lands_the_declared_version_over_the_active_row() {
        let cadence = Arc::new(InMemoryCadence::new(vec![reconcile_row(10)]));
        let app = app_with(cadence.clone());
        let (st, body) = call(
            &app,
            "POST",
            "/api/cadence/rules/train-reconcile/publish",
            &operator(),
            Some(publish_body(2, 5)),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["version"], 2);
        assert_eq!(body["status"], "active");
        assert_eq!(body["every_minutes"], 5);
        assert_eq!(
            body["created_at"],
            serde_json::to_value(door_now()).unwrap(),
            "stamped by the door's clock, never by the body"
        );
        let (_, rules) = call(&app, "GET", "/api/cadence/rules", &operator(), None).await;
        assert_eq!(rules[0]["every_minutes"], 5, "the conductor reads v2");
        let (_, lineage) = call(
            &app,
            "GET",
            "/api/cadence/rules/train-reconcile/versions",
            &operator(),
            None,
        )
        .await;
        assert_eq!(
            lineage
                .as_array()
                .unwrap()
                .iter()
                .map(|r| (
                    r["version"].as_i64().unwrap(),
                    r["status"].as_str().unwrap()
                ))
                .collect::<Vec<_>>(),
            vec![(1, "retired"), (2, "active")]
        );
        let events = cadence.recorded_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, crate::events::CADENCE_PUBLISHED);
        assert_eq!(events[0].payload["version"], 2);
        assert_eq!(events[0].payload["_actor"], "emp-david");
    }

    #[tokio::test]
    async fn a_publish_not_above_the_newest_version_is_409() {
        let cadence = Arc::new(InMemoryCadence::new(vec![reconcile_row(10)]));
        let app = app_with(cadence.clone());
        // v1 is live: v1 again (an overwrite) and v0 are both 409 —
        // the `CadenceError::Conflict` arm car 3 declared, reachable.
        for v in [1, 0] {
            let (st, body) = call(
                &app,
                "POST",
                "/api/cadence/rules/train-reconcile/publish",
                &operator(),
                Some(publish_body(v, 5)),
            )
            .await;
            assert_eq!(st, StatusCode::CONFLICT, "v{v}: {body}");
            assert!(
                body.as_str().unwrap_or_default().contains("v1"),
                "names the newest version: {body}"
            );
        }
        assert!(
            cadence.recorded_events().is_empty(),
            "a refusal records nothing"
        );
        let (_, rules) = call(&app, "GET", "/api/cadence/rules", &operator(), None).await;
        assert_eq!(rules[0]["every_minutes"], 10, "v1 stands");
    }

    #[tokio::test]
    async fn a_publish_body_must_name_the_path_and_declare_an_active_row() {
        let app = app();
        let mut wrong_name = publish_body(1, 5);
        wrong_name["name"] = "train-window".into();
        let (st, body) = call(
            &app,
            "POST",
            "/api/cadence/rules/train-reconcile/publish",
            &operator(),
            Some(wrong_name),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.as_str().unwrap_or_default().contains("train-window"),
            "{body}"
        );
        let mut retired = publish_body(1, 5);
        retired["status"] = "retired".into();
        let (st, body) = call(
            &app,
            "POST",
            "/api/cadence/rules/train-reconcile/publish",
            &operator(),
            Some(retired),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
        assert!(
            body.as_str().unwrap_or_default().contains("retire"),
            "{body}"
        );
    }

    /// The write door is policy-gated like the workflow routes
    /// (`Action::Publish` / `Action::Retire` on `Resource::workflow()`):
    /// a caller the policy does not grant is refused, operator tier or
    /// not, and the probe reader's auditor tier reads the lineage and
    /// writes nothing.
    #[tokio::test]
    async fn the_writes_are_policy_gated_and_the_reader_reads_the_lineage() {
        let cadence = Arc::new(InMemoryCadence::new(vec![reconcile_row(10)]));
        let app = app_with(cadence.clone());
        let ungranted = serde_json::to_string(&User {
            id: "emp-someone".into(),
            role: "reporter".into(),
            access_tier: AccessTier::Operator,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: Some("finance".into()),
        })
        .unwrap();
        let reader = header("audit-readonly", AccessTier::Auditor);
        for user in [&ungranted, &reader] {
            let (st, _) = call(
                &app,
                "POST",
                "/api/cadence/rules/train-reconcile/retire",
                user,
                None,
            )
            .await;
            assert_eq!(st, StatusCode::FORBIDDEN);
            let (st, _) = call(
                &app,
                "POST",
                "/api/cadence/rules/train-reconcile/publish",
                user,
                Some(publish_body(2, 5)),
            )
            .await;
            assert_eq!(st, StatusCode::FORBIDDEN);
        }
        assert!(cadence.recorded_events().is_empty());
        let (st, lineage) = call(
            &app,
            "GET",
            "/api/cadence/rules/train-reconcile/versions",
            &reader,
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(lineage[0]["status"], "active");
        let (st, _) = call(
            &app,
            "GET",
            "/api/cadence/rules/train-reconcile/versions",
            &header("audit-readonly", AccessTier::User),
            None,
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN, "the gateway's guest session");
    }

    /// The spec the seed loader parses is the body the door reads —
    /// one wire shape, so `boss cadence publish <file.toml>` sends
    /// what it parsed and a hand-written body may omit `created_at`.
    #[test]
    fn the_publish_body_is_a_cadence_rule_spec_without_created_at() {
        let spec: CadenceRuleSpec = serde_json::from_value(publish_body(2, 5)).unwrap();
        assert_eq!(spec.version, 2);
        assert_eq!(spec.status, WorkflowStatus::Active);
        assert_eq!(spec.row.every_minutes, Some(5));
        assert_eq!(spec.created_at, DateTime::<Utc>::UNIX_EPOCH);
    }

    fn header(role: &str, tier: AccessTier) -> String {
        serde_json::to_string(&User {
            id: "x".into(),
            role: role.into(),
            access_tier: tier,
            territory_account_ids: Vec::new(),
            direct_report_ids: Vec::new(),
            department: Some("platform".into()),
        })
        .unwrap()
    }

    async fn status(req: Request<Body>) -> StatusCode {
        app().oneshot(req).await.unwrap().status()
    }

    /// Measured 2026-09-16 (839335b7): `/api/cadence/rules` and
    /// `/api/cadence/rules/{name}/last-firing` answered the
    /// recorded-probe reader (`audit-readonly` at auditor) 403 on the
    /// live system of record, so no car about a cadence could be
    /// proved. The reads admit the auditor tier; the two writes stay
    /// operator machinery.
    #[tokio::test]
    async fn the_probe_reader_reads_the_cadence_and_cannot_claim_a_firing() {
        let reader = header("audit-readonly", AccessTier::Auditor);
        for path in ["/api/cadence/rules", "/api/cadence/rules/board/last-firing"] {
            let st = status(
                Request::get(path)
                    .header("x-boss-user", reader.clone())
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert_eq!(st, StatusCode::OK, "{path}");
        }
        let st = status(
            Request::post("/api/cadence/firings")
                .header("x-boss-user", reader.clone())
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"firing_id":"f1","rule_name":"board","verb":"board","basis":"queue-depth","fired_at":"2026-09-16T00:00:00Z"}"#,
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(
            st,
            StatusCode::FORBIDDEN,
            "a read-only reader claims nothing"
        );
        // The gateway's guest session (audit-readonly at USER tier)
        // stays refused on the reads too.
        let st = status(
            Request::get("/api/cadence/rules")
                .header("x-boss-user", header("audit-readonly", AccessTier::User))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(st, StatusCode::FORBIDDEN);
    }
}
