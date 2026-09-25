//! HTTP surface for boss-policy-api.
//!
//! Three groups of endpoints (per the design doc):
//!   - hot path: POST /check, POST /check-batch
//!   - frontend read: GET /my-scope (takes ?user_id=X)
//!   - admin: list/upsert/deactivate rules + user-overrides
//!
//! Session 1 ships the hot path + minimal admin. The full admin matrix
//! UI lands in session 2 on top of the same endpoints.
//!
//! Every admin WRITE authorizes its caller against `policy-rule` and
//! records that caller as `changed_by` (backlog 42c25542; see
//! [`authorize`]).

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

use boss_policy_client::CurrentUser;
use boss_policy_client::engine::PolicyEngine;
use boss_policy_client::port::{PolicyError, PolicyRepository};
use boss_policy_client::types::{
    Action, Decision, PolicyRule, Resource, Scope, User, UserOverride,
};

pub struct PolicyApiState<R: PolicyRepository> {
    pub repo: Arc<R>,
    pub engine: Arc<PolicyEngine<R>>,
}

pub fn router<R: PolicyRepository + 'static>(state: PolicyApiState<R>) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/policy/health", get(health))
        .route("/api/policy/check", post(check::<R>))
        .route("/api/policy/check-batch", post(check_batch::<R>))
        .route("/api/policy/my-scope", post(my_scope::<R>))
        .route(
            "/api/policy/rules",
            get(list_rules::<R>).post(upsert_rule::<R>),
        )
        .route(
            "/api/policy/rules/{id}",
            get(get_rule::<R>)
                .put(upsert_rule::<R>)
                .delete(deactivate_rule::<R>),
        )
        .route(
            "/api/policy/user-overrides",
            post(upsert_user_override::<R>),
        )
        .route(
            "/api/policy/user-overrides/{user_id}",
            get(list_user_overrides::<R>).delete(deactivate_user_override::<R>),
        )
        .with_state(shared)
}

fn err_response(e: PolicyError) -> Response {
    match e {
        PolicyError::NotFound(m) => (StatusCode::NOT_FOUND, m).into_response(),
        PolicyError::Conflict(m) => (StatusCode::CONFLICT, m).into_response(),
        PolicyError::Storage(m) => (StatusCode::INTERNAL_SERVER_ERROR, m).into_response(),
    }
}

#[cfg(feature = "postgres")]
const STORAGE: &str = "postgres";
#[cfg(not(feature = "postgres"))]
const STORAGE: &str = "in-memory";

async fn health() -> Json<boss_core::startup::HealthResponse> {
    Json(boss_core::startup::health_response(
        "boss-policy-api",
        env!("CARGO_PKG_VERSION"),
        STORAGE,
    ))
}

// ----- check ---------------------------------------------------------------

#[derive(Deserialize)]
struct CheckBody {
    user: User,
    action: Action,
    resource: Resource,
}

async fn check<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    Json(body): Json<CheckBody>,
) -> Response {
    match state
        .engine
        .check(&body.user, body.action, body.resource)
        .await
    {
        Ok(d) => Json(d).into_response(),
        Err(e) => err_response(e),
    }
}

#[derive(Deserialize)]
struct CheckBatchBody {
    user: User,
    checks: Vec<CheckPair>,
}

#[derive(Deserialize, Serialize)]
struct CheckPair {
    action: Action,
    resource: Resource,
}

#[derive(Serialize)]
struct CheckBatchResult {
    action: Action,
    resource: Resource,
    decision: Decision,
}

async fn check_batch<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    Json(body): Json<CheckBatchBody>,
) -> Response {
    let mut out = Vec::with_capacity(body.checks.len());
    for c in body.checks {
        let resource = c.resource.clone();
        match state.engine.check(&body.user, c.action, resource).await {
            Ok(d) => out.push(CheckBatchResult {
                action: c.action,
                resource: c.resource,
                decision: d,
            }),
            Err(e) => return err_response(e),
        }
    }
    Json(out).into_response()
}

// ----- my-scope ------------------------------------------------------------

#[derive(Deserialize)]
struct MyScopeBody {
    user: User,
}

#[derive(Serialize)]
struct MyScopeResult {
    user_id: String,
    role: String,
    rules: Vec<ScopeEntry>,
}

#[derive(Serialize)]
struct ScopeEntry {
    resource: Resource,
    action: Action,
    scope: Scope,
}

async fn my_scope<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    Json(body): Json<MyScopeBody>,
) -> Response {
    let mut entries = Vec::new();
    // Iterate the platform's shipped resources (defaults::shipped_resources)
    // so the discovery endpoint covers everything `default_rules` enumerates
    // Read access against — including the registry resources (workflow,
    // step_plugin) the SPA needs for /workflows + /system/step-plugins
    // nav-gating.
    for resource in boss_policy_client::defaults::shipped_resources() {
        for action in [
            Action::Read,
            Action::Create,
            Action::Update,
            Action::Close,
            Action::SignOff,
            Action::Delete,
        ] {
            match state
                .engine
                .check(&body.user, action, resource.clone())
                .await
            {
                Ok(Decision::Allow { scope }) => entries.push(ScopeEntry {
                    resource: resource.clone(),
                    action,
                    scope,
                }),
                Ok(Decision::Deny { .. }) => {}
                Err(e) => return err_response(e),
            }
        }
    }

    Json(MyScopeResult {
        user_id: body.user.id.clone(),
        role: body.user.role.clone(),
        rules: entries,
    })
    .into_response()
}

// ----- rules admin ---------------------------------------------------------

async fn list_rules<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
) -> Response {
    match state.repo.list_rules().await {
        Ok(rules) => Json(rules).into_response(),
        Err(e) => err_response(e),
    }
}

async fn get_rule<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    Path(id): Path<String>,
) -> Response {
    match state.repo.rule_for(&id).await {
        Ok(Some(r)) => Json(r).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, format!("rule {id} not found")).into_response(),
        Err(e) => err_response(e),
    }
}

// ----- who may write the policy table ---------------------------------------
//
// Until backlog 42c25542 (2026-09-25) these four writes authorized nobody:
// each took the rule from the body and wrote it, and took `changed_by` from
// the body too. The gateway proxies `/api/policy/*` for any valid session —
// a guest session is one — so on a guest-enabled deployment an anonymous
// visitor could grant itself any authority at every policy-gated upstream,
// and sign the audit row with someone else's name.
//
// The caller is the gateway-set `x-boss-user` (the edge strips any client
// copy, role_headers.rs), read through the same `CurrentUser` extractor
// every other core service reads; no header is the locked-down `guest`.
// It is checked against `policy-rule` on THIS service's own engine — the
// policy service asking itself, so there is no network hop to fail open.
// A body may still carry `changed_by` (the SPA and older seeders send
// one); it is ignored, because an attribution the caller writes is a
// claim, and the audit row records who the request authenticated as.

/// Allow the write, or the refusal to return. `action` is the write's
/// EFFECT — create a row that is not there, update one that is, delete —
/// as jobs.rs takes `close` for a cancel rather than reading the method.
async fn authorize<R: PolicyRepository + 'static>(
    state: &PolicyApiState<R>,
    user: &User,
    action: Action,
) -> Result<(), Response> {
    match state
        .engine
        .check(user, action, Resource::policy_rule())
        .await
    {
        Ok(Decision::Allow { scope: Scope::All }) => Ok(()),
        // The row-in-scope check jobs.rs makes after its role check. A
        // policy rule has no owner, team, territory or department — it
        // belongs to the whole deployment — so only `all` contains one.
        Ok(Decision::Allow { scope }) => Err((
            StatusCode::FORBIDDEN,
            format!(
                "role {} holds {} on policy-rule only at scope {}; a policy rule belongs to \
                 the whole deployment, so only scope all writes one",
                user.role,
                action.as_str(),
                scope.to_db_string(),
            ),
        )
            .into_response()),
        Ok(Decision::Deny { reason }) => Err((StatusCode::FORBIDDEN, reason).into_response()),
        Err(e) => Err(err_response(e)),
    }
}

#[derive(Deserialize)]
struct UpsertRuleBody {
    rule: PolicyRule,
}

async fn upsert_rule<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<UpsertRuleBody>,
) -> Response {
    let action = match state.repo.rule_for(&body.rule.id).await {
        Ok(Some(_)) => Action::Update,
        Ok(None) => Action::Create,
        Err(e) => return err_response(e),
    };
    if let Err(refused) = authorize(&state, &user, action).await {
        return refused;
    }
    match state.repo.upsert_rule(&body.rule, &user.id).await {
        Ok(()) => (StatusCode::OK, Json(body.rule)).into_response(),
        Err(e) => err_response(e),
    }
}

async fn deactivate_rule<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    if let Err(refused) = authorize(&state, &user, Action::Delete).await {
        return refused;
    }
    match state.repo.deactivate_rule(&id, &user.id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err_response(e),
    }
}

// ----- user-overrides admin -----------------------------------------------

async fn list_user_overrides<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    Path(user_id): Path<String>,
) -> Response {
    match state.repo.list_user_overrides(&user_id).await {
        Ok(ovs) => Json(ovs).into_response(),
        Err(e) => err_response(e),
    }
}

#[derive(Deserialize)]
struct UpsertOverrideBody {
    #[serde(rename = "override")]
    ov: UserOverride,
}

async fn upsert_user_override<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<UpsertOverrideBody>,
) -> Response {
    // The adapter upserts on (user_id, resource, action), so a live
    // override on that pair is what this write would change.
    let action = match state.repo.list_user_overrides(&body.ov.user_id).await {
        Ok(live)
            if live.iter().any(|o| {
                o.id == body.ov.id || (o.resource == body.ov.resource && o.action == body.ov.action)
            }) =>
        {
            Action::Update
        }
        Ok(_) => Action::Create,
        Err(e) => return err_response(e),
    };
    if let Err(refused) = authorize(&state, &user, action).await {
        return refused;
    }
    match state.repo.upsert_user_override(&body.ov, &user.id).await {
        Ok(()) => (StatusCode::CREATED, Json(body.ov)).into_response(),
        Err(e) => err_response(e),
    }
}

async fn deactivate_user_override<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    if let Err(refused) = authorize(&state, &user, Action::Delete).await {
        return refused;
    }
    match state.repo.deactivate_user_override(&id, &user.id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err_response(e),
    }
}

#[cfg(test)]
mod tests {
    //! The policy table's write doors, driven through the router with the
    //! identity header the gateway sets (backlog 42c25542). Every caller
    //! is a real `x-boss-user` shape — or none at all, which is what a
    //! request that reached this port with no session carries.

    use super::*;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use boss_policy_client::InMemoryPolicy;
    use boss_policy_client::defaults::default_rules;
    use boss_policy_client::port::ReconcileStats;
    use tower::ServiceExt;

    /// The in-memory adapter plus a record of every write that reached
    /// the port and the `changed_by` it carried — "nothing changed" is
    /// asserted on this, not inferred from a status code.
    #[derive(Default)]
    struct Recording {
        inner: InMemoryPolicy,
        writes: Mutex<Vec<(String, String)>>,
    }

    impl Recording {
        fn note(&self, op: &str, by: &str) {
            self.writes
                .lock()
                .expect("poisoned lock")
                .push((op.to_string(), by.to_string()));
        }
        fn writes(&self) -> Vec<(String, String)> {
            self.writes.lock().expect("poisoned lock").clone()
        }
    }

    #[async_trait]
    impl PolicyRepository for Recording {
        async fn list_rules(&self) -> Result<Vec<PolicyRule>, PolicyError> {
            self.inner.list_rules().await
        }
        async fn rule_for(&self, id: &str) -> Result<Option<PolicyRule>, PolicyError> {
            self.inner.rule_for(id).await
        }
        async fn upsert_rule(&self, rule: &PolicyRule, by: &str) -> Result<(), PolicyError> {
            self.note("rule.upsert", by);
            self.inner.upsert_rule(rule, by).await
        }
        async fn deactivate_rule(&self, id: &str, by: &str) -> Result<(), PolicyError> {
            self.note("rule.deactivate", by);
            self.inner.deactivate_rule(id, by).await
        }
        async fn list_user_overrides(
            &self,
            user_id: &str,
        ) -> Result<Vec<UserOverride>, PolicyError> {
            self.inner.list_user_overrides(user_id).await
        }
        async fn upsert_user_override(
            &self,
            ov: &UserOverride,
            by: &str,
        ) -> Result<(), PolicyError> {
            self.note("override.upsert", by);
            self.inner.upsert_user_override(ov, by).await
        }
        async fn deactivate_user_override(&self, id: &str, by: &str) -> Result<(), PolicyError> {
            self.note("override.deactivate", by);
            self.inner.deactivate_user_override(id, by).await
        }
        async fn bootstrap_reconcile(
            &self,
            defaults: &[PolicyRule],
        ) -> Result<ReconcileStats, PolicyError> {
            self.inner.bootstrap_reconcile(defaults).await
        }
    }

    const SEEDED_OVERRIDE: &str = "ov-seeded";
    /// What a forger writes into the body's `changed_by`: a human's id.
    const FORGED: &str = "emp-david";

    /// Core's shipped rules, plus `extra`, plus one live override.
    async fn repo(extra: Vec<PolicyRule>) -> Arc<Recording> {
        let inner = InMemoryPolicy::with_rules(default_rules().into_iter().chain(extra));
        inner
            .upsert_user_override(
                &UserOverride {
                    id: SEEDED_OVERRIDE.to_string(),
                    user_id: "emp-cover".to_string(),
                    resource: Resource::job(),
                    action: Action::Close,
                    scope: Scope::All,
                    reason: "covering a leave".to_string(),
                    expires_at: None,
                },
                "seed",
            )
            .await
            .expect("seed override");
        Arc::new(Recording {
            inner,
            writes: Mutex::default(),
        })
    }

    fn app(repo: &Arc<Recording>) -> Router {
        router(PolicyApiState {
            repo: repo.clone(),
            engine: Arc::new(PolicyEngine::new(repo.clone())),
        })
    }

    fn user(id: &str, role: &str) -> String {
        serde_json::json!({"id": id, "role": role, "access_tier": "user"}).to_string()
    }

    /// Every write door, each shaped as an escalation: the caller grants
    /// ITSELF policy authority, rewrites a rule, deactivates the admin's
    /// own grant, hands itself an override, retires someone else's.
    fn writes() -> Vec<(Method, String, Option<serde_json::Value>)> {
        let grant_self =
            PolicyRule::new("guest", Resource::policy_rule(), Action::Create, Scope::All);
        let rewrite = PolicyRule::new("audit-readonly", Resource::job(), Action::Read, Scope::None);
        let elevate = UserOverride {
            id: "ov-escalate".to_string(),
            user_id: "anonymous".to_string(),
            resource: Resource::policy_rule(),
            action: Action::Create,
            scope: Scope::All,
            reason: "self-granted".to_string(),
            expires_at: None,
        };
        vec![
            (
                Method::POST,
                "/api/policy/rules".to_string(),
                Some(serde_json::json!({"rule": grant_self, "changed_by": FORGED})),
            ),
            (
                Method::PUT,
                format!("/api/policy/rules/{}", rewrite.id),
                Some(serde_json::json!({"rule": rewrite, "changed_by": FORGED})),
            ),
            (
                Method::DELETE,
                format!("/api/policy/rules/platform-admin:policy-rule:update?changed_by={FORGED}"),
                None,
            ),
            (
                Method::POST,
                "/api/policy/user-overrides".to_string(),
                Some(serde_json::json!({"override": elevate, "changed_by": FORGED})),
            ),
            (
                Method::DELETE,
                format!("/api/policy/user-overrides/{SEEDED_OVERRIDE}?changed_by={FORGED}"),
                None,
            ),
        ]
    }

    async fn send(
        app: Router,
        method: Method,
        uri: &str,
        body: Option<&serde_json::Value>,
        caller: Option<&str>,
    ) -> StatusCode {
        let mut req = Request::builder().method(method).uri(uri);
        if let Some(c) = caller {
            req = req.header("x-boss-user", c);
        }
        let req = match body {
            Some(b) => req
                .header("content-type", "application/json")
                .body(Body::from(b.to_string())),
            None => req.body(Body::empty()),
        }
        .expect("request");
        app.oneshot(req).await.expect("infallible").status()
    }

    async fn snapshot(repo: &Recording) -> (Vec<PolicyRule>, Vec<UserOverride>, Vec<UserOverride>) {
        let mut rules = repo.list_rules().await.expect("rules");
        rules.sort_by(|a, b| a.id.cmp(&b.id));
        (
            rules,
            repo.list_user_overrides("emp-cover").await.expect("ovs"),
            repo.list_user_overrides("anonymous").await.expect("ovs"),
        )
    }

    /// THE DEFECT (backlog 42c25542): a guest session, the read-only
    /// auditor, and a request carrying no identity at all could each
    /// rewrite the policy table. Every one is refused 403 at every write
    /// door, and nothing reaches the port.
    #[tokio::test]
    async fn a_caller_without_policy_authority_is_refused_every_write() {
        let guest = user("guest@algedonic.dev", "guest");
        let auditor = user("emp-audit", "audit-readonly");
        let callers: [(&str, Option<&str>); 3] = [
            ("guest session", Some(&guest)),
            ("audit-readonly", Some(&auditor)),
            ("no identity", None),
        ];
        for (who, caller) in callers {
            let repo = repo(vec![]).await;
            let before = snapshot(&repo).await;
            for (method, uri, body) in writes() {
                let status = send(app(&repo), method.clone(), &uri, body.as_ref(), caller).await;
                assert_eq!(
                    status,
                    StatusCode::FORBIDDEN,
                    "{who}: {method} {uri} must be refused"
                );
            }
            assert_eq!(repo.writes(), vec![], "{who}: no write may reach the port");
            assert!(snapshot(&repo).await == before, "{who}: the table changed");
        }
    }

    /// The control: the operator still writes through every door, and
    /// each write is attributed to the AUTHENTICATED caller — the body's
    /// `changed_by` is a claim, and a forged one never lands.
    #[tokio::test]
    async fn an_operator_writes_every_door_as_itself() {
        let repo = repo(vec![]).await;
        let admin = user("emp-founder", "platform-admin");
        for (method, uri, body) in writes() {
            let status = send(
                app(&repo),
                method.clone(),
                &uri,
                body.as_ref(),
                Some(&admin),
            )
            .await;
            assert!(status.is_success(), "{method} {uri}: {status}");
        }
        let writes = repo.writes();
        assert_eq!(writes.len(), 5, "{writes:?}");
        for (op, by) in writes {
            assert_eq!(by, "emp-founder", "{op} was attributed to {by}");
        }
    }

    /// The action is the write's EFFECT, not its method: `POST` is an
    /// upsert, so a role granted `create` but not `update` must not
    /// overwrite a rule that already exists by POSTing it — the shape
    /// `jobs.rs` uses to take `close` for a cancel.
    #[tokio::test]
    async fn an_upsert_over_an_existing_rule_needs_update() {
        let creator = PolicyRule::new(
            "rule-drafter",
            Resource::policy_rule(),
            Action::Create,
            Scope::All,
        );
        let repo = repo(vec![creator]).await;
        let drafter = user("emp-drafter", "rule-drafter");

        let fresh = PolicyRule::new("reviewer", Resource::job(), Action::Read, Scope::Self_);
        let body = serde_json::json!({"rule": fresh});
        let status = send(
            app(&repo),
            Method::POST,
            "/api/policy/rules",
            Some(&body),
            Some(&drafter),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "a new rule is a create");

        let existing =
            PolicyRule::new("audit-readonly", Resource::job(), Action::Read, Scope::None);
        let body = serde_json::json!({"rule": existing});
        let status = send(
            app(&repo),
            Method::POST,
            "/api/policy/rules",
            Some(&body),
            Some(&drafter),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::FORBIDDEN,
            "overwriting a live rule is an update"
        );
        assert_eq!(repo.writes().len(), 1);
    }

    /// A policy rule has no owner, team, territory or department, so the
    /// only scope that contains one is `all` — the row-in-scope check
    /// every jobs write makes after its role check, applied to a row that
    /// belongs to the whole deployment.
    #[tokio::test]
    async fn a_grant_narrower_than_all_writes_no_policy() {
        let narrow = PolicyRule::new(
            "team-lead",
            Resource::policy_rule(),
            Action::Update,
            Scope::Team,
        );
        let repo = repo(vec![narrow]).await;
        let lead = user("emp-lead", "team-lead");
        let existing =
            PolicyRule::new("audit-readonly", Resource::job(), Action::Read, Scope::None);
        let body = serde_json::json!({"rule": existing});
        let uri = format!("/api/policy/rules/{}", existing.id);
        let status = send(app(&repo), Method::PUT, &uri, Some(&body), Some(&lead)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(repo.writes(), vec![]);
    }
}
