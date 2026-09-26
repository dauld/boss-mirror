//! HTTP surface for boss-policy-api.
//!
//! Two groups of endpoints:
//!   - hot path: POST /check
//!   - admin: list/upsert/deactivate rules + user-overrides
//!
//! `check-batch` and `my-scope` were deleted with no caller left in the
//! tree (backlog 5a914364 S2).
//!
//! Every admin WRITE authorizes its caller against `policy-rule` and
//! records that caller as `changed_by` (backlog 42c25542; see
//! [`authorize`]).

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde::Deserialize;

use boss_policy_client::CurrentUser;
use boss_policy_client::engine::PolicyEngine;
use boss_policy_client::port::{PolicyError, PolicyRepository};
use boss_policy_client::types::{
    Action, PolicyRule, Resource, User, UserOverride, refuse_ambiguous_role, rule_id,
};

use crate::authority::{self, Holdings};

pub struct PolicyApiState<R: PolicyRepository> {
    pub repo: Arc<R>,
    pub engine: Arc<PolicyEngine<R>>,
}

pub fn router<R: PolicyRepository + 'static>(state: PolicyApiState<R>) -> Router {
    let shared = Arc::new(state);
    Router::new()
        .route("/api/policy/health", get(health))
        .route("/api/policy/check", post(check::<R>))
        .route(
            "/api/policy/rules",
            get(list_rules::<R>).post(post_rule::<R>),
        )
        .route(
            "/api/policy/rules/{id}",
            get(get_rule::<R>)
                .put(put_rule::<R>)
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
        PolicyError::Refused(m) => (StatusCode::FORBIDDEN, m).into_response(),
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

// ----- who may ask about whom ---------------------------------------------
//
// Until backlog b8e75382's rule 7 (F7) the reads about a person answered
// any caller about any user, and an override's row and the deny it
// decides both carry its `reason` — free text an operator wrote about a
// person. The override list now answers the caller about itself, and
// anyone else only to a holder of Read on `policy-rule` at scope all,
// the authority to read the table those answers come from.
//
// `/check` takes the same bound WHENEVER the request is signed (backlog
// 5a914364 S1): the gateway always sets `x-boss-user` on a session's
// request, so a session asking about someone else is judged like the
// list. An UNSIGNED `/check` stays open, because every service asks it
// through `ReqwestPolicyClient` with no `x-boss-user` of its own, and a
// bound there would deny every policy check in the estate. That arm
// closes when callers sign their policy calls (e84de48e, and the
// machine token of design 6805c764); until then a caller reaching the
// port directly is the gap, pinned by
// `an_unsigned_check_still_answers_about_anyone_until_callers_sign`. So
// do the rule reads (`GET /api/policy/rules`), which the headerless
// bootstrap reads.

/// Allow a read about `id` (holding `role`, when the read names one),
/// or the refusal to return.
async fn may_read_for<R: PolicyRepository + 'static>(
    state: &PolicyApiState<R>,
    caller: &User,
    id: &str,
    role: Option<&str>,
) -> Result<(), Response> {
    // Its own authority, as itself: a body naming the caller's id with
    // another role asks what that role would get, which is a question
    // about someone else.
    if caller.id == id && role.is_none_or(|r| r == caller.role) {
        return Ok(());
    }
    let about = format!("{} asks about {id}", caller.id);
    // The guest session's role holds Read on policy-rule by shipped
    // default; the identity never reads another's authority (rule 5).
    if boss_core::roles::ANONYMOUS_VISITOR_IDS.contains(&caller.id.as_str()) {
        return Err(forbidden(format!(
            "{about}; an anonymous visitor reads only its own authority"
        )));
    }
    let decision = state
        .engine
        .check(caller, Action::Read, Resource::policy_rule())
        .await
        .map_err(err_response)?;
    authority::may(&caller.role, Action::Read, &decision)
        .map_err(|reason| forbidden(format!("{about}, and only reads its own: {reason}")))
}

// ----- check ---------------------------------------------------------------

#[derive(Deserialize)]
struct CheckBody {
    user: User,
    action: Action,
    resource: Resource,
}

/// A signed caller is judged by [`may_read_for`]; an unsigned one — every
/// service's `ReqwestPolicyClient` today — is answered, until callers
/// sign (e84de48e). Presence of the header is the test, not the id the
/// extractor defaults to, because a signed request may carry any id.
async fn check<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    headers: HeaderMap,
    CurrentUser(caller): CurrentUser,
    Json(body): Json<CheckBody>,
) -> Response {
    if headers.contains_key("x-boss-user")
        && let Err(refused) =
            may_read_for(&state, &caller, &body.user.id, Some(&body.user.role)).await
    {
        return refused;
    }
    match state
        .engine
        .check(&body.user, body.action, body.resource)
        .await
    {
        Ok(d) => Json(d).into_response(),
        Err(e) => err_response(e),
    }
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
//
// Authorizing the caller did not bound what it could write (backlog
// b8e75382): what an authorized writer may grant is `crate::authority`,
// judged by the adapter on the row the write changes, inside the write's
// transaction.

fn forbidden(reason: String) -> Response {
    (StatusCode::FORBIDDEN, reason).into_response()
}

/// Allow a deactivation of a rule, or the refusal to return. A missing
/// rule already denies, so retiring one only ever narrows: it is judged
/// as the Delete it is, whoever the caller.
async fn authorize<R: PolicyRepository + 'static>(
    state: &PolicyApiState<R>,
    user: &User,
    action: Action,
) -> Result<(), Response> {
    authority::refuse_anonymous_caller(&user.id, &user.role).map_err(forbidden)?;
    let decision = state
        .engine
        .check(user, action, Resource::policy_rule())
        .await
        .map_err(err_response)?;
    authority::may(&user.role, action, &decision).map_err(forbidden)
}

/// What the caller holds on `policy-rule` and on the action and resource
/// a write concerns. Read here, before the adapter opens the write's
/// transaction, because the judge inside it is synchronous — and a
/// caller's authority is about the caller, not about the row it writes.
async fn holdings<R: PolicyRepository + 'static>(
    state: &PolicyApiState<R>,
    user: &User,
    action: Action,
    resource: &Resource,
) -> Result<Holdings, Response> {
    authority::refuse_anonymous_caller(&user.id, &user.role).map_err(forbidden)?;
    let mut policy_rule = Vec::with_capacity(authority::POLICY_VERBS.len());
    for verb in authority::POLICY_VERBS {
        let decision = state
            .engine
            .check(user, verb, Resource::policy_rule())
            .await
            .map_err(err_response)?;
        policy_rule.push((verb, decision));
    }
    // With when it stops holding, because a grant made from it may last
    // no longer (H3 of the hold review of car a8becd52).
    let (decision, until) = state
        .engine
        .check_until(user, action, resource.clone())
        .await
        .map_err(err_response)?;
    Ok(Holdings {
        id: user.id.clone(),
        role: user.role.clone(),
        policy_rule,
        action,
        resource: resource.clone(),
        decision,
        until,
    })
}

#[derive(Deserialize)]
struct UpsertRuleBody {
    rule: PolicyRule,
}

async fn post_rule<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Json(body): Json<UpsertRuleBody>,
) -> Response {
    write_rule(&state, &user, body.rule).await
}

/// A PUT writes the one rule its path names (backlog b8e75382, F2).
async fn put_rule<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
    Json(body): Json<UpsertRuleBody>,
) -> Response {
    if id != body.rule.id {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "the path names rule {id} and the body names rule {}; a PUT writes the one rule \
                 its path names",
                body.rule.id
            ),
        )
            .into_response();
    }
    write_rule(&state, &user, body.rule).await
}

async fn write_rule<R: PolicyRepository + 'static>(
    state: &PolicyApiState<R>,
    user: &User,
    rule: PolicyRule,
) -> Response {
    // The engine finds a rule ONLY by `role:resource:action`, and the
    // adapter's conflict key is the id, so an id the body chooses is a
    // grant displayed as one thing and enforced as another (F2) — and so
    // is a role that carries the separator, whose honestly derived id is
    // also another grant's (S1).
    if let Err(reason) = refuse_ambiguous_role(&rule.role) {
        return (StatusCode::UNPROCESSABLE_ENTITY, reason).into_response();
    }
    let derived = rule_id(&rule.role, &rule.resource, rule.action);
    if rule.id != derived {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "rule id {} is not {derived}, the role:resource:action it carries; the engine \
                 enforces a rule by its id, so the id is derived, never chosen",
                rule.id
            ),
        )
            .into_response();
    }
    let held = match holdings(state, user, rule.action, &rule.resource).await {
        Ok(held) => held,
        Err(refused) => return refused,
    };
    let judge = |existing: Option<&PolicyRule>| authority::judge_rule(&held, existing, &rule);
    match state.repo.upsert_rule_judged(&rule, &user.id, &judge).await {
        Ok(()) => (StatusCode::OK, Json(rule)).into_response(),
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
    CurrentUser(caller): CurrentUser,
    Path(user_id): Path<String>,
) -> Response {
    if let Err(refused) = may_read_for(&state, &caller, &user_id, None).await {
        return refused;
    }
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
    let held = match holdings(&state, &user, body.ov.action, &body.ov.resource).await {
        Ok(held) => held,
        Err(refused) => return refused,
    };
    let now = boss_policy_client::engine::expiry_now();
    let judge = |existing: Option<&UserOverride>| {
        authority::judge_override(&held, existing, Some(&body.ov), now)
    };
    match state
        .repo
        .upsert_user_override_judged(&body.ov, &user.id, &judge)
        .await
    {
        Ok(()) => (StatusCode::CREATED, Json(body.ov)).into_response(),
        Err(e) => err_response(e),
    }
}

async fn deactivate_user_override<R: PolicyRepository + 'static>(
    State(state): State<Arc<PolicyApiState<R>>>,
    CurrentUser(user): CurrentUser,
    Path(id): Path<String>,
) -> Response {
    if let Err(refused) = authority::refuse_anonymous_caller(&user.id, &user.role) {
        return forbidden(refused);
    }
    // What the override is ABOUT, so the right authority is read; its
    // user, resource and action never change once written. Whether
    // retiring it widens anyone's access is judged on the row itself,
    // inside the transaction.
    let about = match state.repo.user_override(&id).await {
        Ok(Some(ov)) => ov,
        Ok(None) => return err_response(PolicyError::NotFound(id)),
        Err(e) => return err_response(e),
    };
    let held = match holdings(&state, &user, about.action, &about.resource).await {
        Ok(held) => held,
        Err(refused) => return refused,
    };
    let now = boss_policy_client::engine::expiry_now();
    let judge =
        |existing: Option<&UserOverride>| authority::judge_override(&held, existing, None, now);
    match state
        .repo
        .deactivate_user_override_judged(&id, &user.id, &judge)
        .await
    {
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
    use boss_policy_client::port::{Judge, ReconcileStats};
    use boss_policy_client::types::Scope;
    use tower::ServiceExt;

    /// The in-memory adapter plus a record of every write the port
    /// COMMITTED and the `changed_by` it carried — "nothing changed" is
    /// asserted on this, not inferred from a status code. Noted after
    /// the adapter answers, because since backlog b8e75382 a write can
    /// reach the port and be refused there, by its judge.
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
        async fn upsert_rule_judged(
            &self,
            rule: &PolicyRule,
            by: &str,
            judge: Judge<'_, PolicyRule>,
        ) -> Result<(), PolicyError> {
            self.inner.upsert_rule_judged(rule, by, judge).await?;
            self.note("rule.upsert", by);
            Ok(())
        }
        async fn deactivate_rule(&self, id: &str, by: &str) -> Result<(), PolicyError> {
            self.inner.deactivate_rule(id, by).await?;
            self.note("rule.deactivate", by);
            Ok(())
        }
        async fn list_user_overrides(
            &self,
            user_id: &str,
        ) -> Result<Vec<UserOverride>, PolicyError> {
            self.inner.list_user_overrides(user_id).await
        }
        async fn user_override(&self, id: &str) -> Result<Option<UserOverride>, PolicyError> {
            self.inner.user_override(id).await
        }
        async fn upsert_user_override_judged(
            &self,
            ov: &UserOverride,
            by: &str,
            judge: Judge<'_, UserOverride>,
        ) -> Result<(), PolicyError> {
            self.inner
                .upsert_user_override_judged(ov, by, judge)
                .await?;
            self.note("override.upsert", by);
            Ok(())
        }
        async fn deactivate_user_override_judged(
            &self,
            id: &str,
            by: &str,
            judge: Judge<'_, UserOverride>,
        ) -> Result<(), PolicyError> {
            self.inner
                .deactivate_user_override_judged(id, by, judge)
                .await?;
            self.note("override.deactivate", by);
            Ok(())
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

    /// Every write door, each shaped as an escalation: a drafter's role
    /// is granted policy authority, a rule is rewritten, the admin's own
    /// grant is deactivated, the drafter is handed an override, someone
    /// else's is retired. (Until backlog b8e75382 the grant and the
    /// override named the guest role and the anonymous id; those are now
    /// refused to every caller, the operator included, which
    /// `an_anonymous_identity_is_never_granted_policy_authority` pins.)
    fn writes() -> Vec<(Method, String, Option<serde_json::Value>)> {
        let grant_self = PolicyRule::new(
            "rule-drafter",
            Resource::policy_rule(),
            Action::Create,
            Scope::All,
        );
        let rewrite = PolicyRule::new("audit-readonly", Resource::job(), Action::Read, Scope::None);
        let elevate = UserOverride {
            id: "ov-escalate".to_string(),
            user_id: "emp-drafter".to_string(),
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
            repo.list_user_overrides("emp-drafter").await.expect("ovs"),
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
    ///
    /// Each body carries the forged `changed_by` the pre-42c25542 door
    /// REQUIRED, so on that tree the request deserializes and the test
    /// fails for the reason it names rather than on a missing field
    /// (backlog b8e75382, the review's nit). The drafter holds the job
    /// read it grants, because since b8e75382 a granter hands out only
    /// what it holds.
    #[tokio::test]
    async fn an_upsert_over_an_existing_rule_needs_update() {
        let creator = PolicyRule::new(
            "rule-drafter",
            Resource::policy_rule(),
            Action::Create,
            Scope::All,
        );
        let holds = PolicyRule::new("rule-drafter", Resource::job(), Action::Read, Scope::All);
        let repo = repo(vec![creator, holds]).await;
        let drafter = user("emp-drafter", "rule-drafter");

        let fresh = PolicyRule::new("reviewer", Resource::job(), Action::Read, Scope::Self_);
        let body = serde_json::json!({"rule": fresh, "changed_by": FORGED});
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
        let body = serde_json::json!({"rule": existing, "changed_by": FORGED});
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
        // `changed_by` for the same reason as the test above.
        let body = serde_json::json!({"rule": existing, "changed_by": FORGED});
        let uri = format!("/api/policy/rules/{}", existing.id);
        let status = send(app(&repo), Method::PUT, &uri, Some(&body), Some(&lead)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(repo.writes(), vec![]);
    }

    // ----- backlog b8e75382: what an authorized writer may write ---------
    //
    // The adversarial review of car e2209174 proved that authorizing the
    // caller was not enough: a caller holding one policy verb could grant
    // itself the rest, a rule id could disguise its grant, retiring a
    // deny widened access as a "delete", and nothing kept policy
    // authority from the identities anonymous visitors carry.

    async fn post_rule(repo: &Arc<Recording>, rule: &PolicyRule, caller: &str) -> StatusCode {
        let body = serde_json::json!({"rule": rule});
        send(
            app(repo),
            Method::POST,
            "/api/policy/rules",
            Some(&body),
            Some(caller),
        )
        .await
    }

    async fn post_override(repo: &Arc<Recording>, ov: &UserOverride, caller: &str) -> StatusCode {
        let body = serde_json::json!({"override": ov});
        send(
            app(repo),
            Method::POST,
            "/api/policy/user-overrides",
            Some(&body),
            Some(caller),
        )
        .await
    }

    async fn delete(repo: &Arc<Recording>, uri: &str, caller: &str) -> StatusCode {
        send(app(repo), Method::DELETE, uri, None, Some(caller)).await
    }

    fn grant(user_id: &str, resource: Resource, action: Action, scope: Scope) -> UserOverride {
        UserOverride {
            id: format!("ov-{user_id}-{}-{}", resource.as_str(), action.as_str()),
            user_id: user_id.to_string(),
            resource,
            action,
            scope,
            reason: "test".to_string(),
            expires_at: None,
        }
    }

    /// Rule 1 (F1). Create on policy-rule used to be enough to take
    /// everything: the reviewer's Create-only role granted itself
    /// Delete. A grant is now refused unless the caller's own decision
    /// on the same action and resource is an allow whose scope contains
    /// the one granted — and an override is a grant like a rule is.
    #[tokio::test]
    async fn a_granter_grants_nothing_it_does_not_hold() {
        let repo = repo(vec![
            PolicyRule::new(
                "rule-drafter",
                Resource::policy_rule(),
                Action::Create,
                Scope::All,
            ),
            PolicyRule::new("rule-drafter", Resource::job(), Action::Read, Scope::Team),
        ])
        .await;
        let drafter = user("emp-drafter", "rule-drafter");

        let beyond = [
            // The proven escalation: Create alone minting Delete.
            PolicyRule::new(
                "rule-drafter",
                Resource::policy_rule(),
                Action::Delete,
                Scope::All,
            ),
            // A resource the drafter holds nothing on.
            PolicyRule::new("reviewer", Resource::ledger(), Action::Read, Scope::All),
            // Wider than the drafter's own team scope.
            PolicyRule::new("reviewer", Resource::job(), Action::Read, Scope::All),
        ];
        for rule in beyond {
            let status = post_rule(&repo, &rule, &drafter).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{} must be refused", rule.id);
        }
        let mint = grant(
            "emp-drafter",
            Resource::policy_rule(),
            Action::Delete,
            Scope::All,
        );
        assert_eq!(
            post_override(&repo, &mint, &drafter).await,
            StatusCode::FORBIDDEN,
            "an override is a grant"
        );
        assert_eq!(repo.writes(), vec![], "nothing beyond reached the port");

        // Within what it holds: its own shape, and a narrower one.
        let within = [
            PolicyRule::new("reviewer", Resource::job(), Action::Read, Scope::Team),
            PolicyRule::new("intern", Resource::job(), Action::Read, Scope::Self_),
        ];
        for rule in within {
            let status = post_rule(&repo, &rule, &drafter).await;
            assert_eq!(
                status,
                StatusCode::OK,
                "{} is within the drafter's",
                rule.id
            );
        }
        assert_eq!(repo.writes().len(), 2);
    }

    /// Rule 2. Break-glass holds Create and Update on policy-rule for
    /// its auth-administration lever, and under rule 1 alone that would
    /// still let it mint what it holds. It repairs instead: a rule it
    /// writes must equal a row core ships, and deactivating stays a
    /// Delete, which it does not hold.
    #[tokio::test]
    async fn break_glass_restores_a_shipped_default_and_invents_nothing() {
        let repo = repo(vec![]).await;
        let shipped = default_rules()
            .into_iter()
            .find(|r| r.id == "platform-admin:policy-rule:update")
            .expect("core ships the operator's policy update");
        // The lockout the key exists for: the operator's grant, broken.
        let broken = PolicyRule {
            scope: Scope::None,
            ..shipped.clone()
        };
        repo.inner
            .upsert_rule(&broken, "emp-mistake")
            .await
            .expect("break it");
        let key = user("emp-oncall", "break-glass");

        let body = serde_json::json!({"rule": shipped});
        let uri = format!("/api/policy/rules/{}", shipped.id);
        let status = send(app(&repo), Method::PUT, &uri, Some(&body), Some(&key)).await;
        assert_eq!(status, StatusCode::OK, "restoring the shipped row");

        let invented = [
            // Itself, a verb the narrow role was never given.
            PolicyRule::new(
                "break-glass",
                Resource::policy_rule(),
                Action::Delete,
                Scope::All,
            ),
            // A shipped id with a scope core never shipped.
            PolicyRule::new(
                "platform-admin",
                Resource::ledger(),
                Action::Read,
                Scope::Self_,
            ),
            // Something it holds, which is still not a repair.
            PolicyRule::new("on-call", Resource::job(), Action::Read, Scope::All),
        ];
        for rule in invented {
            let status = post_rule(&repo, &rule, &key).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{} is not a repair", rule.id);
        }
        let status = delete(&repo, "/api/policy/rules/platform-admin:ledger:read", &key).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "deactivating is a Delete");
        assert_eq!(repo.writes().len(), 1, "{:?}", repo.writes());
    }

    /// Rule 3 (F2). The engine finds a rule only by its id, and the
    /// adapter's conflict key is the id, so a row displayed as the
    /// auditor's job read but stored as `guest:ledger:read` granted
    /// guests the ledger. The id is derived from the row, never taken
    /// from it — and a PUT names one rule, in its path and its body.
    #[tokio::test]
    async fn a_rule_id_is_derived_never_trusted() {
        let repo = repo(vec![]).await;
        let admin = user("emp-founder", "platform-admin");

        let disguised = PolicyRule {
            id: "guest:ledger:read".to_string(),
            ..PolicyRule::new("audit-readonly", Resource::job(), Action::Read, Scope::All)
        };
        assert_eq!(
            post_rule(&repo, &disguised, &admin).await,
            StatusCode::UNPROCESSABLE_ENTITY
        );

        let honest = PolicyRule::new("reviewer", Resource::job(), Action::Read, Scope::Self_);
        let body = serde_json::json!({"rule": honest});
        let status = send(
            app(&repo),
            Method::PUT,
            "/api/policy/rules/reviewer:job:update",
            Some(&body),
            Some(&admin),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::UNPROCESSABLE_ENTITY,
            "path and body differ"
        );
        assert_eq!(repo.writes(), vec![]);
    }

    /// Rule 4 (F3). Retiring a scope-none override lifts a deny: the
    /// user gets back whatever their role grants, which can be
    /// everything. That is a grant, judged as Create plus rule 1 against
    /// the widest access it can restore — the service does not know the
    /// user's role, so `all`. Retiring an `all` grant only narrows, and
    /// stays a Delete.
    #[tokio::test]
    async fn retiring_a_deny_override_is_judged_as_the_grant_it_restores() {
        let repo = repo(vec![
            PolicyRule::new(
                "override-clerk",
                Resource::policy_rule(),
                Action::Create,
                Scope::All,
            ),
            PolicyRule::new(
                "override-clerk",
                Resource::policy_rule(),
                Action::Delete,
                Scope::All,
            ),
            // Holds the access the deny withholds, but not Create.
            PolicyRule::new(
                "finance-keeper",
                Resource::policy_rule(),
                Action::Delete,
                Scope::All,
            ),
            PolicyRule::new(
                "finance-keeper",
                Resource::ledger(),
                Action::Read,
                Scope::All,
            ),
        ])
        .await;
        let deny = UserOverride {
            id: "ov-deny".to_string(),
            scope: Scope::None,
            reason: "suspended from the books".to_string(),
            ..grant("emp-cover", Resource::ledger(), Action::Read, Scope::None)
        };
        repo.inner
            .upsert_user_override(&deny, "seed")
            .await
            .expect("seed deny");

        let clerk = user("emp-clerk", "override-clerk");
        let keeper = user("emp-keeper", "finance-keeper");
        let admin = user("emp-founder", "platform-admin");
        let uri = "/api/policy/user-overrides/ov-deny";
        assert_eq!(
            delete(&repo, uri, &clerk).await,
            StatusCode::FORBIDDEN,
            "the clerk holds nothing on the ledger"
        );
        assert_eq!(
            delete(&repo, uri, &keeper).await,
            StatusCode::FORBIDDEN,
            "lifting a deny is a Create, which the keeper does not hold"
        );
        assert_eq!(repo.writes(), vec![]);

        let seeded = format!("/api/policy/user-overrides/{SEEDED_OVERRIDE}");
        assert_eq!(
            delete(&repo, &seeded, &clerk).await,
            StatusCode::NO_CONTENT,
            "retiring an all-scope grant narrows"
        );
        assert_eq!(delete(&repo, uri, &admin).await, StatusCode::NO_CONTENT);
        assert_eq!(repo.writes().len(), 2);
    }

    /// Rule 4 (F5's first half). The adapter upserts on
    /// (user_id, resource, action) and revives an expired row there, but
    /// the door read live rows only, so the revival was judged a Create.
    /// A row that exists, live or expired, makes the write an Update.
    #[tokio::test]
    async fn rewriting_an_expired_override_is_an_update() {
        let repo = repo(vec![
            PolicyRule::new(
                "delegator",
                Resource::policy_rule(),
                Action::Create,
                Scope::All,
            ),
            PolicyRule::new("delegator", Resource::job(), Action::Close, Scope::All),
        ])
        .await;
        let lapsed = UserOverride {
            expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
            ..grant("emp-returning", Resource::job(), Action::Close, Scope::All)
        };
        repo.inner
            .upsert_user_override(&lapsed, "seed")
            .await
            .expect("seed lapsed");
        let delegator = user("emp-delegator", "delegator");

        let revive = UserOverride {
            id: "ov-revive".to_string(),
            ..grant("emp-returning", Resource::job(), Action::Close, Scope::All)
        };
        assert_eq!(
            post_override(&repo, &revive, &delegator).await,
            StatusCode::FORBIDDEN,
            "an expired row is still the row this write rewrites"
        );
        let fresh = grant("emp-new", Resource::job(), Action::Close, Scope::All);
        assert_eq!(
            post_override(&repo, &fresh, &delegator).await,
            StatusCode::CREATED
        );
        assert_eq!(repo.writes().len(), 1);
    }

    /// Rule 5 (F4). An override on `guest@algedonic.dev` let a guest
    /// session write rules. No rule grants policy authority to a role an
    /// anonymous visitor carries, no override grants it to an id one
    /// carries — whoever writes it — and a caller carrying one is
    /// refused even if such a grant reached the table some other way.
    #[tokio::test]
    async fn an_anonymous_identity_is_never_granted_policy_authority() {
        use boss_core::roles::{ANONYMOUS_USER_ID, GUEST_EMAIL};
        let table = repo(vec![]).await;
        let admin = user("emp-founder", "platform-admin");

        for role in ["guest", "audit-readonly"] {
            let rule = PolicyRule::new(role, Resource::policy_rule(), Action::Update, Scope::All);
            let status = post_rule(&table, &rule, &admin).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{}", rule.id);
        }
        for id in [GUEST_EMAIL, ANONYMOUS_USER_ID] {
            let ov = grant(id, Resource::policy_rule(), Action::Create, Scope::All);
            let status = post_override(&table, &ov, &admin).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "an override on {id}");
        }
        assert_eq!(table.writes(), vec![]);

        // Reading the table stays the auditor's shipped default.
        let read = PolicyRule::new(
            "audit-readonly",
            Resource::policy_rule(),
            Action::Read,
            Scope::All,
        );
        assert_eq!(post_rule(&table, &read, &admin).await, StatusCode::OK);

        // A grant that reached the table anyway authorizes no guest.
        let planted = repo(vec![
            PolicyRule::new(
                "audit-readonly",
                Resource::policy_rule(),
                Action::Create,
                Scope::All,
            ),
            PolicyRule::new("audit-readonly", Resource::job(), Action::Read, Scope::All),
        ])
        .await;
        planted
            .inner
            .upsert_user_override(
                &grant(
                    GUEST_EMAIL,
                    Resource::policy_rule(),
                    Action::Create,
                    Scope::All,
                ),
                "seed",
            )
            .await
            .expect("plant");
        let guest = user(GUEST_EMAIL, "audit-readonly");
        let rule = PolicyRule::new("reviewer", Resource::job(), Action::Read, Scope::Self_);
        assert_eq!(
            post_rule(&planted, &rule, &guest).await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(planted.writes(), vec![]);
    }

    // ----- the hold review of car a8becd52 (2026-09-25) ------------------
    //
    // The car above was held with three findings a rewrite could walk
    // through (H1-H3) and two shapes the adapter or the door let pass
    // (S1, S2). Each test below failed on d63e2e45 for the reason it
    // names.

    fn expiring(ov: UserOverride, at: chrono::DateTime<chrono::Utc>) -> UserOverride {
        UserOverride {
            expires_at: Some(at),
            ..ov
        }
    }

    /// H1. Retiring a deny was a Create and a grant at `all`, but the
    /// same effect was on offer as an Update: re-POST the deny with an
    /// expiry a second away and it lapses into whatever the role grants.
    /// Ending a live narrowing override SOONER than it would have ended
    /// is retiring it early, and is judged as retiring it.
    #[tokio::test]
    async fn shortening_a_deny_override_is_judged_as_retiring_it() {
        let repo = repo(vec![
            PolicyRule::new(
                "override-clerk",
                Resource::policy_rule(),
                Action::Create,
                Scope::All,
            ),
            PolicyRule::new(
                "override-clerk",
                Resource::policy_rule(),
                Action::Update,
                Scope::All,
            ),
        ])
        .await;
        let now = chrono::Utc::now();
        let deny = UserOverride {
            id: "ov-deny".to_string(),
            reason: "suspended from the books".to_string(),
            ..grant("emp-cover", Resource::ledger(), Action::Read, Scope::None)
        };
        let dated = UserOverride {
            id: "ov-dated".to_string(),
            ..expiring(
                grant("emp-other", Resource::ledger(), Action::Read, Scope::None),
                now + chrono::Duration::hours(4),
            )
        };
        for seed in [&deny, &dated] {
            repo.inner
                .upsert_user_override(seed, "seed")
                .await
                .expect("seed deny");
        }
        let clerk = user("emp-clerk", "override-clerk");

        let lapses = [
            // A permanent deny given an expiry a second away.
            expiring(deny.clone(), now + chrono::Duration::seconds(1)),
            // A dated deny brought forward.
            expiring(dated.clone(), now + chrono::Duration::minutes(1)),
            // A deny given an expiry already past.
            expiring(deny.clone(), now - chrono::Duration::seconds(1)),
        ];
        for ov in &lapses {
            assert_eq!(
                post_override(&repo, ov, &clerk).await,
                StatusCode::FORBIDDEN,
                "{} ending at {:?} lifts the deny early",
                ov.id,
                ov.expires_at
            );
        }
        assert_eq!(repo.writes(), vec![], "no lapse reached the table");

        // The control: a rewrite that ends the deny no sooner is the
        // Update it looks like.
        let reworded = UserOverride {
            reason: "suspended, pending the audit".to_string(),
            ..deny.clone()
        };
        let extended = expiring(dated.clone(), now + chrono::Duration::hours(8));
        for ov in [&reworded, &extended] {
            assert_eq!(
                post_override(&repo, ov, &clerk).await,
                StatusCode::CREATED,
                "{} narrows nothing it did not already",
                ov.id
            );
        }
        let admin = user("emp-founder", "platform-admin");
        assert_eq!(
            post_override(&repo, &lapses[0], &admin).await,
            StatusCode::CREATED,
            "the operator holds what the lapse restores"
        );
        assert_eq!(repo.writes().len(), 3);
    }

    /// H2. Rule 2 bounded the rules break-glass writes and nothing
    /// bounded its overrides: holding Create and Update on policy-rule
    /// at `all`, it could hand any user — itself included — a permanent
    /// grant of what it holds, outliving the emergency. An override is
    /// never a shipped row, so break-glass writes none; it may retire
    /// one, which returns that user to the role rules.
    #[tokio::test]
    async fn break_glass_writes_no_override_and_may_retire_one() {
        let repo = repo(vec![]).await;
        // The lockout: the operator denied its own policy updates.
        let lockout = UserOverride {
            id: "ov-lockout".to_string(),
            ..grant(
                "emp-founder",
                Resource::policy_rule(),
                Action::Update,
                Scope::None,
            )
        };
        repo.inner
            .upsert_user_override(&lockout, "emp-mistake")
            .await
            .expect("seed lockout");
        let key = user("emp-oncall", "break-glass");

        let minted = [
            // Itself, permanently, the policy verb it holds.
            grant(
                "emp-oncall",
                Resource::policy_rule(),
                Action::Create,
                Scope::All,
            ),
            // Anyone, the platform stamp it holds.
            grant(
                "emp-friend",
                Resource::new("step-signoff:platform-admin"),
                Action::SignOff,
                Scope::All,
            ),
            // A narrowing is not a repair either.
            grant("emp-friend", Resource::job(), Action::Close, Scope::None),
        ];
        for ov in &minted {
            assert_eq!(
                post_override(&repo, ov, &key).await,
                StatusCode::FORBIDDEN,
                "{} is not a repair",
                ov.id
            );
        }
        assert_eq!(repo.writes(), vec![]);

        let status = delete(&repo, "/api/policy/user-overrides/ov-lockout", &key).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "lifting the lockout");
        assert_eq!(repo.writes().len(), 1);
    }

    /// H3. Rule 1 read the caller's decision through its own overrides,
    /// so an override expiring in an hour was authority for a permanent
    /// grant — to someone else, as a role rule, or to the caller itself
    /// by re-POSTing its own override with no expiry. A grant ends no
    /// later than the authority it is granted from.
    #[tokio::test]
    async fn a_grant_ends_no_later_than_the_authority_it_is_granted_from() {
        let repo = repo(vec![
            PolicyRule::new(
                "delegator",
                Resource::policy_rule(),
                Action::Create,
                Scope::All,
            ),
            PolicyRule::new(
                "delegator",
                Resource::policy_rule(),
                Action::Update,
                Scope::All,
            ),
            PolicyRule::new("delegator", Resource::job(), Action::Read, Scope::All),
        ])
        .await;
        let now = chrono::Utc::now();
        let until = now + chrono::Duration::hours(1);
        let cover = expiring(
            grant("emp-delegator", Resource::job(), Action::Close, Scope::All),
            until,
        );
        repo.inner
            .upsert_user_override(&cover, "seed")
            .await
            .expect("seed the temporary grant");
        let delegator = user("emp-delegator", "delegator");

        let laundered = [
            // Onward, with no expiry.
            grant("emp-other", Resource::job(), Action::Close, Scope::All),
            // Onward, past the delegator's own hour.
            expiring(
                grant("emp-other", Resource::job(), Action::Close, Scope::All),
                until + chrono::Duration::minutes(1),
            ),
            // Its own override, re-POSTed with no expiry.
            UserOverride {
                expires_at: None,
                ..cover.clone()
            },
        ];
        for ov in &laundered {
            assert_eq!(
                post_override(&repo, ov, &delegator).await,
                StatusCode::FORBIDDEN,
                "{} ending {:?} outlives {until}",
                ov.id,
                ov.expires_at
            );
        }
        let rule = PolicyRule::new("reviewer", Resource::job(), Action::Close, Scope::All);
        assert_eq!(
            post_rule(&repo, &rule, &delegator).await,
            StatusCode::FORBIDDEN,
            "a role rule never expires"
        );
        assert_eq!(repo.writes(), vec![]);

        // Within the hour, onward, is a delegation; and what the role
        // itself grants is not bounded by any override.
        let onward = expiring(
            grant("emp-other", Resource::job(), Action::Close, Scope::All),
            until - chrono::Duration::minutes(1),
        );
        assert_eq!(
            post_override(&repo, &onward, &delegator).await,
            StatusCode::CREATED
        );
        let read = PolicyRule::new("reviewer", Resource::job(), Action::Read, Scope::Team);
        assert_eq!(post_rule(&repo, &read, &delegator).await, StatusCode::OK);
        assert_eq!(repo.writes().len(), 2);
    }

    /// S1. A rule's id joins role, resource and action with `:`, and a
    /// resource may carry one (`step-signoff:<role>`), so a role that
    /// carries one derives the id of a different grant: the row below is
    /// displayed as role `reviewer:step-signoff` on `x`, and stored — and
    /// enforced — as `reviewer` on `step-signoff:x`.
    #[tokio::test]
    async fn a_role_carrying_the_id_separator_is_refused() {
        let repo = repo(vec![]).await;
        let admin = user("emp-founder", "platform-admin");
        let forged = PolicyRule::new(
            "reviewer:step-signoff",
            Resource::new("x"),
            Action::SignOff,
            Scope::All,
        );
        let enforced = PolicyRule::new(
            "reviewer",
            Resource::new("step-signoff:x"),
            Action::SignOff,
            Scope::All,
        );
        assert_eq!(forged.id, enforced.id, "the two derive one id");
        assert_eq!(
            post_rule(&repo, &forged, &admin).await,
            StatusCode::UNPROCESSABLE_ENTITY
        );
        assert_eq!(repo.writes(), vec![]);
    }

    // ----- backlog b8e75382 rule 7 (F7), and 5a914364: a read answers for
    // its caller ---------------------------------------------------------
    //
    // `/check` and the override list answered any caller about any user,
    // and an override's decision and row both carry its `reason` — free
    // text an operator wrote about a person. Rule 7 bounded the list (and
    // `check-batch` and `my-scope`, since deleted); its adversarial review
    // (5a914364, S1) showed `/check` still handed the same reason to any
    // session through the gateway, one question at a time. A SIGNED
    // `/check` now takes the same bound; an unsigned one stays open until
    // callers sign (e84de48e), and a pin below keeps that gap visible.

    /// A deny override's reason: what a read about emp-cover must not
    /// hand to a caller who may not read it.
    const DENY_REASON: &str = "suspended from the books";

    /// The shipped rules and the seeded grant, plus a deny on emp-cover
    /// whose decision carries [`DENY_REASON`].
    async fn reads_repo() -> Arc<Recording> {
        let repo = repo(vec![]).await;
        let deny = UserOverride {
            id: "ov-deny".to_string(),
            reason: DENY_REASON.to_string(),
            ..grant("emp-cover", Resource::ledger(), Action::Read, Scope::None)
        };
        repo.inner
            .upsert_user_override(&deny, "seed")
            .await
            .expect("seed deny");
        repo
    }

    /// The read doors about `id` holding `role`: the one question whose
    /// decision is emp-cover's deny, and the override list.
    fn reads_about(id: &str, role: &str) -> Vec<(Method, String, Option<serde_json::Value>)> {
        let who = serde_json::json!({"id": id, "role": role, "access_tier": "user"});
        vec![
            (
                Method::POST,
                "/api/policy/check".to_string(),
                Some(serde_json::json!({"user": who, "action": "read", "resource": "ledger"})),
            ),
            (
                Method::GET,
                format!("/api/policy/user-overrides/{id}"),
                None,
            ),
        ]
    }

    async fn ask(
        repo: &Arc<Recording>,
        method: Method,
        uri: &str,
        body: Option<&serde_json::Value>,
        caller: Option<&str>,
    ) -> (StatusCode, String) {
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
        let resp = app(repo).oneshot(req).await.expect("infallible");
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// THE DEFECT: a signed caller without Read on `policy-rule` — an
    /// employee drafting rules, a guest session (whose role holds that
    /// Read by shipped default, so the refusal is by the identity, rule
    /// 5) — learned another user's authority and the reason on their
    /// overrides, from `/check` as well as from the list (5a914364 S1).
    /// Each is refused, and no reason leaves in the refusal.
    #[tokio::test]
    async fn a_read_about_someone_else_is_refused_without_policy_read() {
        let repo = reads_repo().await;
        let drafter = user("emp-drafter", "rule-drafter");
        let guest = user("guest@algedonic.dev", "audit-readonly");
        for (who, caller) in [("a drafter", &drafter), ("a guest session", &guest)] {
            for (method, uri, body) in reads_about("emp-cover", "reviewer") {
                let (status, text) =
                    ask(&repo, method.clone(), &uri, body.as_ref(), Some(caller)).await;
                assert_eq!(
                    status,
                    StatusCode::FORBIDDEN,
                    "{who}: {method} {uri} must be refused: {text}"
                );
                assert!(!text.contains(DENY_REASON), "{who}: {text}");
                assert!(!text.contains("covering a leave"), "{who}: {text}");
            }
        }
        // The list has no service caller, so no identity is refused too.
        let (status, text) = ask(
            &repo,
            Method::GET,
            "/api/policy/user-overrides/emp-cover",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{text}");
    }

    /// A caller always reads its OWN authority — but as itself: the
    /// role in the body is the one the question is about, so naming
    /// its own id with a wider role is asking about someone else.
    #[tokio::test]
    async fn a_caller_reads_its_own_authority_and_only_as_itself() {
        let repo = reads_repo().await;
        let cover = user("emp-cover", "reviewer");
        for (method, uri, body) in reads_about("emp-cover", "reviewer") {
            let (status, text) =
                ask(&repo, method.clone(), &uri, body.as_ref(), Some(&cover)).await;
            assert_eq!(status, StatusCode::OK, "{method} {uri}: {text}");
            assert!(text.contains(DENY_REASON), "its own reason: {text}");
        }
        for (method, uri, body) in reads_about("emp-cover", "platform-admin") {
            if method == Method::GET {
                continue; // the override list names no role
            }
            let (status, text) =
                ask(&repo, method.clone(), &uri, body.as_ref(), Some(&cover)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}: {text}");
        }
    }

    /// The control: a holder of Read on `policy-rule` still reads
    /// anyone's — the operator, and the seeded auditor login, whose
    /// Read on override reasons design 2830b6b7 decides, not this car.
    #[tokio::test]
    async fn a_policy_reader_reads_anyones_authority() {
        let repo = reads_repo().await;
        let admin = user("emp-founder", "platform-admin");
        let auditor = user("emp-audit", "audit-readonly");
        for caller in [&admin, &auditor] {
            for (method, uri, body) in reads_about("emp-cover", "reviewer") {
                let (status, text) =
                    ask(&repo, method.clone(), &uri, body.as_ref(), Some(caller)).await;
                assert_eq!(status, StatusCode::OK, "{caller}: {method} {uri}: {text}");
                assert!(text.contains(DENY_REASON), "{text}");
            }
        }
    }

    /// The control for every service in the estate: `ReqwestPolicyClient`
    /// asks `/check` about the user it is serving and sends no
    /// `x-boss-user` of its own, so an unsigned question is answered —
    /// a bound on it would deny every policy check there is.
    #[tokio::test]
    async fn an_unsigned_service_check_is_still_answered() {
        let repo = reads_repo().await;
        let body = serde_json::json!({
            "user": {"id": "emp-founder", "role": "platform-admin", "access_tier": "operator"},
            "action": "create",
            "resource": "job",
        });
        let (status, text) = ask(&repo, Method::POST, "/api/policy/check", Some(&body), None).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert!(text.contains("allow"), "{text}");
    }

    /// THE GAP, PINNED (5a914364 S4): an unsigned `/check` still answers
    /// about anyone, reason and all. Every browser session reaches this
    /// port through the gateway, which always signs, so the gap is a
    /// caller that reaches the port directly — the machine door with
    /// its gate mode off (2710c8fc/6805c764). This test is meant to go
    /// RED when services sign their policy calls (e84de48e) and the
    /// unsigned arm is closed; invert it then, do not delete it.
    #[tokio::test]
    async fn an_unsigned_check_still_answers_about_anyone_until_callers_sign() {
        let repo = reads_repo().await;
        let (method, uri, body) = reads_about("emp-cover", "reviewer").remove(0);
        let (status, text) = ask(&repo, method, &uri, body.as_ref(), None).await;
        assert_eq!(status, StatusCode::OK, "{text}");
        assert!(text.contains(DENY_REASON), "{text}");
    }

    /// `check-batch` and `my-scope` had no caller in the tree — the web
    /// stopped fetching `my-scope`, nothing ever sent `check-batch` — so
    /// they were deleted rather than guarded (5a914364 S2). A door that
    /// returns is a door someone must bound again.
    #[tokio::test]
    async fn the_uncalled_scope_reads_are_gone() {
        let repo = reads_repo().await;
        let admin = user("emp-founder", "platform-admin");
        for uri in ["/api/policy/check-batch", "/api/policy/my-scope"] {
            let body = serde_json::json!({"user": {"id": "emp-founder", "role": "platform-admin"}});
            let (status, text) = ask(&repo, Method::POST, uri, Some(&body), Some(&admin)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri}: {text}");
        }
    }
}
