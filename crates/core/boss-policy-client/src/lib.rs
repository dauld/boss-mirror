//! Client-side surface for boss-policy. Every domain service depends
//! on this crate and holds an `Arc<dyn PolicyClient>` in its HTTP
//! state. Tests plug in `FakePolicyClient`; prod plugs in
//! `ReqwestPolicyClient`.
//!
//! The contract types + the in-memory engine live HERE in the
//! *-client crate, not in `boss-policy`. The service crate
//! (`boss-policy`) contains just the HTTP server + Postgres adapter
//! + the seeder binaries. This keeps hexagonal-port hygiene:
//! consumers of `boss-policy-client` don't transitively pull
//! `boss-policy`'s sqlx + axum service-side dep tree.
//!
//! Caching: `ReqwestPolicyClient` keeps a 60s TTL cache keyed on
//! `(user_id, role, access_tier, action, resource)` — every input the
//! decision reads (backlog 8878f85f; see `CacheKey`). Invalidation is TTL-only; NATS-
//! driven invalidation on top of the TTL is a planned addition (D4).
//!
//! Fail-closed: if the HTTP call fails (no connection, a timeout, a
//! 5xx) and no live cache entry is available, `check` and
//! `scope_predicate` return `Err(PolicyClientError::Unreachable)` —
//! never an Allow (D9), and never cached. It is an ERROR rather than a
//! Deny so a door can answer it as what it is, a 503 with Retry-After
//! (`impl IntoResponse for PolicyClientError`), instead of a 403 or an
//! empty page (backlog 45553536). Only decisions the service made are
//! cached.

pub mod defaults;
pub mod engine;
pub mod in_memory;
pub mod port;
pub mod predicates;
pub mod seed_loader;
pub mod types;

pub use engine::PolicyEngine;
pub use in_memory::InMemoryPolicy;
pub use port::{PolicyError, PolicyRepository, ReconcileStats};
pub use predicates::scope_to_predicate;
pub use types::{
    AccessTier, Action, Decision, PolicyRule, Predicate, Resource, Scope, User, UserOverride,
};

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde::Serialize;

/// Axum extractor that reads the `User` from the `X-Boss-User` header.
/// The gateway populates this header per-request from the session;
/// tests pass a manually-constructed `User` JSON. Missing header
/// yields [`User::anonymous`] — the `guest` role at user tier, which
/// only the defaults' workflow read grants anything and which no door
/// trusts (backlog e84de48e), so the default behaviour is "locked
/// down." A sibling service is not anonymous: it signs as its own
/// `automation:<x>`.
pub struct CurrentUser(pub User);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for CurrentUser {
    type Rejection = axum::response::Response;

    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _state: &S,
    ) -> Result<Self, Self::Rejection> {
        use axum::http::StatusCode;
        use axum::response::IntoResponse;

        if let Some(raw) = parts.headers.get("x-boss-user") {
            let s = raw.to_str().map_err(|_| {
                (StatusCode::BAD_REQUEST, "invalid X-Boss-User header").into_response()
            })?;
            let user: User = serde_json::from_str(s).map_err(|e| {
                (
                    StatusCode::BAD_REQUEST,
                    format!("invalid X-Boss-User JSON: {e}"),
                )
                    .into_response()
            })?;
            Ok(CurrentUser(user))
        } else {
            Ok(CurrentUser(User::anonymous()))
        }
    }
}

/// Shared per-request context middleware. Scopes **both** the
/// sim-origin flag (`x-sim-origin`) and the ambient request actor
/// (`x-boss-user` → [`User::ambient_actor`]) for the duration of the
/// handler.
///
/// Every service registers this single layer
/// (`app.layer(axum::middleware::from_fn(request_context_middleware))`),
/// replacing the per-binary `sim_origin_middleware` copies. The
/// [`DomainPublisher`](boss_core::publisher::DomainPublisher) then
/// attributes every emit to the request's authenticated actor — and
/// stamps `_simulated` — without any handler threading either down by
/// hand. Outside a request (CLI, bootstrap, background tasks) the
/// actor is unset and the publisher falls back to the service's own
/// `automation:<source>` identity.
///
/// The header opens a sim chain ONLY on an instance that runs a
/// simulator ([`sim_enabled`]); elsewhere it is ignored (backlog
/// 85e7f10f, 2026-09-25). Before, any caller that reached a service
/// port directly — the LAN machine door, an in-cluster pod — could
/// send `x-sim-origin: true` and have its real work admitted
/// `Simulated` and stamped `_simulated`, the set the cutover trims.
pub async fn request_context_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let sim = sim_chain_from_header(
        sim_enabled(),
        req.headers()
            .get(boss_core::sim_origin::SIM_ORIGIN_HEADER)
            .and_then(|v| v.to_str().ok()),
    );
    let actor = req
        .headers()
        .get("x-boss-user")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| serde_json::from_str::<User>(s).ok())
        .and_then(|u| u.ambient_actor());
    let run = boss_core::sim_origin::with_sim_chain(sim, next.run(req));
    match actor {
        Some(a) => boss_core::actor_context::with_actor(a, run).await,
        None => run.await,
    }
}
use tokio::sync::RwLock;

#[derive(Debug, thiserror::Error)]
pub enum PolicyClientError {
    /// The policy service could not be ASKED: no connection, a timeout,
    /// or a 5xx. Not a decision, so never cached and never a Deny — a
    /// reader must be able to tell "not allowed" from "could not ask"
    /// (backlog 45553536). Every caller still refuses on it. The
    /// rendered text carries `policy-unreachable`, the word boss-cli's
    /// `names_a_policy_outage` keys on, and a door's 503 body is that
    /// word alone (see `IntoResponse` below).
    #[error("policy-unreachable: {0}")]
    Unreachable(String),
    #[error("transport failure: {0}")]
    Transport(String),
}

/// Seconds a caller is told to wait after a policy outage. The outage
/// this was measured on (the #689 rollout, 2026-09-25) was a policy pod
/// rolling — seconds, not minutes.
pub const POLICY_OUTAGE_RETRY_AFTER_SECS: u64 = 5;

/// The ONE rendering of a failed policy check, so every door answers an
/// outage the same way: 503 + `Retry-After` for
/// [`PolicyClientError::Unreachable`], 500 for anything else. Both
/// refuse; neither is a 403, because neither is a permission fact.
///
/// The BODY is fixed words, never the detail: the detail is reqwest's
/// text, which names the policy service's internal URL, and it was
/// handed to every caller of every door until backlog fe9d212c
/// (2026-09-26). An outage's detail is logged where it is raised
/// (`ReqwestPolicyClient::check`); a transport failure's is logged
/// here, because nothing upstream of this logs it.
impl axum::response::IntoResponse for PolicyClientError {
    fn into_response(self) -> axum::response::Response {
        use axum::http::{StatusCode, header};
        match self {
            PolicyClientError::Unreachable(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                [(
                    header::RETRY_AFTER,
                    POLICY_OUTAGE_RETRY_AFTER_SECS.to_string(),
                )],
                "policy-unreachable",
            )
                .into_response(),
            PolicyClientError::Transport(detail) => {
                tracing::warn!(error = %detail, "policy check failed");
                (StatusCode::INTERNAL_SERVER_ERROR, "policy check failed").into_response()
            }
        }
    }
}

#[async_trait]
pub trait PolicyClient: Send + Sync {
    async fn check(
        &self,
        user: &User,
        action: Action,
        resource: Resource,
    ) -> Result<Decision, PolicyClientError>;

    /// Read-scope Predicate for a list endpoint. Denied access yields
    /// `Predicate::None`, which callers translate to "no rows." A
    /// policy service that could not be asked is an `Err`, NOT
    /// `Predicate::None`: an outage must not read as an empty list.
    async fn scope_predicate(
        &self,
        user: &User,
        resource: Resource,
    ) -> Result<Predicate, PolicyClientError>;
}

// ---------------------------------------------------------------------------
// Sim-origin bypass — the permissive auth handler for simulator traffic
// ---------------------------------------------------------------------------

/// The deployment switch that says this instance runs a simulator. The
/// launcher derives it from the tenant manifest's `sim` key (or the
/// deployment sets it — prod's boss.yaml says `"false"`, the
/// playground renders `"true"`) and every service inherits it.
pub const SIM_ENABLED_ENV: &str = "BOSS_SIM_ENABLED";

/// Whether this process runs beside a simulator: [`SIM_ENABLED_ENV`] is
/// `true` or `1`. UNSET IS OFF — the bypass is something an instance
/// asks for, never something it gets by saying nothing (backlog
/// 85e7f10f: until 2026-09-25 the bypass was wired unconditionally, so
/// prod, whose sim has been parked since 2026-09-05, carried it too).
pub fn sim_enabled() -> bool {
    sim_enabled_value(std::env::var(SIM_ENABLED_ENV).ok().as_deref())
}

fn sim_enabled_value(value: Option<&str>) -> bool {
    value.is_some_and(|v| {
        let v = v.trim();
        v == "1" || v.eq_ignore_ascii_case("true")
    })
}

/// What the request-context middleware scopes into the sim-chain
/// task-local: the `x-sim-origin` header's truth, and only on a sim
/// instance. Pure so both halves are testable without the environment.
fn sim_chain_from_header(sim_enabled: bool, header: Option<&str>) -> bool {
    sim_enabled && boss_core::sim_origin::header_is_truthy(header)
}

/// Whether `user` is an identity a sim chain is driven by:
///
/// - `automation:sim` — boss-sim's `LiveApiOutput` and workforce
///   clients sign every call with it;
/// - role `system-sim` — the same clients' `put_as`, which names the
///   simulated EMPLOYEE as `id` (so attribution lands on the person)
///   and marks itself automation by role;
/// - the dispatcher continuing a chain it inherited from a simulated
///   event — `automation:dispatcher` on its reads and `rule:<name>` on
///   its writes (`boss_dispatcher::rules::actor`).
///
/// HOW THAT IS PROVEN TODAY: it is not. `x-boss-user` is ASSERTED by the
/// caller on the LAN machine door (the gateway replaces it from the
/// session; nothing behind the gateway verifies it), so on a sim
/// instance a caller can still claim one of these ids. What this closes
/// is the header ALONE — an anonymous or ordinary caller that adds
/// `x-sim-origin` — and, with [`sim_enabled`], every instance without a
/// sim. Proof of the identity arrives with the machine token (design
/// 6805c764); until then a sim instance is a playground whose data the
/// cutover trims, not a store of record.
pub fn is_sim_identity(user: &User) -> bool {
    user.role == "system-sim"
        || user.id == "automation:sim"
        || user.id == "automation:dispatcher"
        || user.id.starts_with("rule:")
}

/// The ONE predicate every sim-bypass site asks (backlog 85e7f10f):
/// this instance runs a sim, the request is on a sim chain, AND the
/// caller is a sim identity. The operator-tier doors (classes,
/// locations, calendar, the ledger's chart and tax registry) write
/// `sim_bypass_allowed(&user) || tier_ok`; no site reads the chain flag
/// or the switch for authorization on its own, which is what let
/// `sim || tier_ok` open each of them to a header.
pub fn sim_bypass_allowed(user: &User) -> bool {
    sim_bypass_admits(sim_enabled(), user)
}

fn sim_bypass_admits(sim_enabled: bool, user: &User) -> bool {
    sim_enabled && sim_chain_admits(user)
}

fn sim_chain_admits(user: &User) -> bool {
    boss_core::sim_origin::is_in_sim_chain() && is_sim_identity(user)
}

/// Wraps a [`PolicyClient`] and short-circuits to `Allow` for a sim
/// caller on a simulated event chain — see [`sim_bypass_allowed`] for
/// the three conditions.
///
/// The simulator masquerades as the real employees whose work it
/// stands in for; every event it drives is stamped `_simulated=true`
/// by the request-context middleware. Rather than seed a per-role
/// grant matrix for the simulator (or let it claim a superuser role),
/// sim traffic is authorized here, at the boundary, with a single
/// permissive decision — while every other caller flows through the
/// wrapped client unchanged and is enforced per-role.
///
/// INSTALLED ONLY ON A SIM INSTANCE. There is no public `new`: a
/// binary gets the bypass through [`SimBypassPolicyClient::from_env`],
/// which hands back the inner client untouched unless
/// [`SIM_ENABLED_ENV`] is on. Until 2026-09-25 five binaries (jobs,
/// search, views, ledger, people) wrapped unconditionally and the
/// bypass trusted the header alone, so any caller reaching a service
/// port directly passed every policy check by sending
/// `x-sim-origin: true` (backlog 85e7f10f). The gateway strips that
/// header; the LAN machine door and in-cluster callers do not pass the
/// gateway.
pub struct SimBypassPolicyClient {
    inner: Arc<dyn PolicyClient>,
}

impl SimBypassPolicyClient {
    /// The bypass around `inner` when this instance runs a sim, and
    /// `inner` itself when it does not. What every service binary calls.
    pub fn from_env(inner: Arc<dyn PolicyClient>) -> Arc<dyn PolicyClient> {
        Self::wrap(inner, sim_enabled())
    }

    /// [`Self::from_env`] with the switch passed in — the door a test
    /// uses to build both instances in one process.
    pub fn wrap(inner: Arc<dyn PolicyClient>, sim_enabled: bool) -> Arc<dyn PolicyClient> {
        if sim_enabled {
            Arc::new(Self { inner })
        } else {
            inner
        }
    }
}

#[async_trait]
impl PolicyClient for SimBypassPolicyClient {
    async fn check(
        &self,
        user: &User,
        action: Action,
        resource: Resource,
    ) -> Result<Decision, PolicyClientError> {
        // Constructed only on a sim instance (`wrap`), so the switch is
        // already decided; the chain and the identity are per request.
        if sim_chain_admits(user) {
            return Ok(Decision::Allow { scope: Scope::All });
        }
        self.inner.check(user, action, resource).await
    }

    async fn scope_predicate(
        &self,
        user: &User,
        resource: Resource,
    ) -> Result<Predicate, PolicyClientError> {
        if sim_chain_admits(user) {
            return Ok(Predicate::Unrestricted);
        }
        self.inner.scope_predicate(user, resource).await
    }
}

// ---------------------------------------------------------------------------
// Reqwest adapter — the prod client
// ---------------------------------------------------------------------------

/// Every input the decision is a function of — so a cached decision is
/// served only to a caller who would have been given the same one.
///
/// The engine reads the caller's `id` (its user overrides) and `role`
/// (the rule id). Until backlog 8878f85f (2026-09-26) the key held a
/// hash of the id ALONE, while the role arrives with the caller in
/// `x-boss-user` — asserted, on the LAN machine door — so for the TTL a
/// decision made for one role was served to the same id presenting
/// another: a narrower session got a wider cached Allow, or the reverse.
/// boss-sim's `put_as` is an in-tree shape of it: the simulated
/// employee's id with role `system-sim`. `access_tier` is keyed too: the
/// service receives it, and keying it is free.
///
/// NOT keyed, deliberately: territory, direct reports and department.
/// The decision does not read them — the cache holds the `Scope`, and
/// [`scope_to_predicate`] turns it into rows from the LIVE caller on
/// every request, after the cache. Key one of them only if the engine
/// ever starts deciding on it.
///
/// The id and role are held as written, not hashed: a key that decides
/// authorization must not be able to collide.
#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct CacheKey {
    user_id: String,
    role: String,
    access_tier: AccessTier,
    action: Action,
    resource: Resource,
}

struct CacheEntry {
    decision: Decision,
    expires_at: std::time::Instant,
}

pub struct ReqwestPolicyClient {
    base_url: String,
    http: reqwest::Client,
    cache: RwLock<HashMap<CacheKey, CacheEntry>>,
    ttl: std::time::Duration,
}

impl ReqwestPolicyClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self::with_timeout(base_url, std::time::Duration::from_secs(5))
    }

    /// `new` with the whole-request timeout named. Private: production
    /// takes 5 s through `new`; the loopback tests take a wider one,
    /// because a host too starved to answer in 5 s turned their status
    /// assertions into transport errors (train 40256c45, 2026-09-26).
    fn with_timeout(base_url: impl Into<String>, timeout: std::time::Duration) -> Self {
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::builder()
                .timeout(timeout)
                .build()
                .expect("reqwest client"),
            cache: RwLock::new(HashMap::new()),
            ttl: std::time::Duration::from_secs(60),
        }
    }

    fn cache_key(user: &User, action: Action, resource: Resource) -> CacheKey {
        CacheKey {
            user_id: user.id.clone(),
            role: user.role.clone(),
            access_tier: user.access_tier,
            action,
            resource,
        }
    }

    async fn cached(&self, key: &CacheKey) -> Option<Decision> {
        let cache = self.cache.read().await;
        cache
            .get(key)
            .filter(|e| e.expires_at > std::time::Instant::now())
            .map(|e| e.decision.clone())
    }

    async fn cache_put(&self, key: CacheKey, decision: Decision) {
        let mut cache = self.cache.write().await;
        cache.insert(
            key,
            CacheEntry {
                decision,
                expires_at: std::time::Instant::now() + self.ttl,
            },
        );
    }
}

#[derive(Serialize)]
struct CheckBody<'a> {
    user: &'a User,
    action: Action,
    resource: Resource,
}

#[async_trait]
impl PolicyClient for ReqwestPolicyClient {
    async fn check(
        &self,
        user: &User,
        action: Action,
        resource: Resource,
    ) -> Result<Decision, PolicyClientError> {
        let key = Self::cache_key(user, action, resource.clone());
        if let Some(d) = self.cached(&key).await {
            return Ok(d);
        }

        let url = format!("{}/api/policy/check", self.base_url);
        let resp = self
            .http
            .post(&url)
            .json(&CheckBody {
                user,
                action,
                resource,
            })
            .send()
            .await;

        // ONLY a decision the service made is cached. Until backlog
        // 45553536 (2026-09-26) an outage became a Deny and was cached
        // for the full TTL, so a policy pod rolling for seconds refused
        // every key that asked during it for a minute after it was back
        // — as 403s, a permission fact where the fact was an outage.
        // Both failure arms still fail closed (D9): no caller gets an
        // Allow out of either.
        match resp {
            Ok(r) if r.status().is_success() => {
                let decision = r
                    .json::<Decision>()
                    .await
                    .map_err(|e| PolicyClientError::Transport(e.to_string()))?;
                self.cache_put(key, decision.clone()).await;
                Ok(decision)
            }
            Ok(r) if r.status().is_server_error() => {
                // The service failed to decide: an outage, not an answer.
                let status = r.status();
                tracing::warn!(%status, "policy service returned 5xx; refusing as unreachable");
                Err(PolicyClientError::Unreachable(format!(
                    "policy service returned {status}"
                )))
            }
            Ok(r) => {
                // A 4xx: the service refused the QUESTION (this client
                // asked it wrongly). Fail closed as a deny, since asking
                // again asks wrongly again — but not cached, because it
                // is not a decision about this caller either.
                let status = r.status();
                tracing::warn!(%status, "policy service returned non-2xx; deny");
                Ok(Decision::Deny {
                    reason: format!("policy service returned {status}"),
                })
            }
            Err(e) => {
                tracing::warn!(error = %e, "policy service unreachable; refusing");
                Err(PolicyClientError::Unreachable(e.to_string()))
            }
        }
    }

    async fn scope_predicate(
        &self,
        user: &User,
        resource: Resource,
    ) -> Result<Predicate, PolicyClientError> {
        match self.check(user, Action::Read, resource).await? {
            Decision::Deny { .. } => Ok(Predicate::None),
            Decision::Allow { scope } => Ok(crate::scope_to_predicate(&scope, user)),
        }
    }
}

// ---------------------------------------------------------------------------
// Permissive fake — for tests that aren't exercising policy themselves
// ---------------------------------------------------------------------------

/// A PolicyClient that returns `Allow { scope: All }` for every check,
/// regardless of role. Use this in domain-crate tests whose subject is
/// the handler logic, not the policy gate — it exercises the full
/// plumbing (extractor, async call, decision branch) without requiring
/// every test to seed a role matrix.
///
/// For tests that need scope-based filtering (e.g. territory-scoped
/// queries), use [`FakePolicyClient::builder`] and seed specific
/// `(role, action, resource, scope)` rules instead.
pub struct PermissivePolicyClient;

#[async_trait]
impl PolicyClient for PermissivePolicyClient {
    async fn check(
        &self,
        _user: &User,
        _action: Action,
        _resource: Resource,
    ) -> Result<Decision, PolicyClientError> {
        Ok(Decision::Allow { scope: Scope::All })
    }

    async fn scope_predicate(
        &self,
        _user: &User,
        _resource: Resource,
    ) -> Result<Predicate, PolicyClientError> {
        Ok(Predicate::Unrestricted)
    }
}

// ---------------------------------------------------------------------------
// Fake — in-process client for tests
// ---------------------------------------------------------------------------

/// In-process PolicyClient backed by an InMemoryPolicy + PolicyEngine.
/// Tests seed rules via the builder and pass the resulting client into
/// the service under test.
pub struct FakePolicyClient {
    engine: Arc<PolicyEngine<crate::InMemoryPolicy>>,
}

impl FakePolicyClient {
    pub fn builder() -> FakePolicyClientBuilder {
        FakePolicyClientBuilder::default()
    }

    /// Fully restrictive — every check returns Deny.
    pub fn deny_all() -> Self {
        let repo = Arc::new(crate::InMemoryPolicy::new());
        let engine = Arc::new(PolicyEngine::new(repo));
        Self { engine }
    }
}

#[derive(Default)]
pub struct FakePolicyClientBuilder {
    rules: Vec<crate::PolicyRule>,
    overrides: Vec<crate::UserOverride>,
}

impl FakePolicyClientBuilder {
    pub fn allow(
        mut self,
        role: impl Into<String>,
        action: Action,
        resource: Resource,
        scope: Scope,
    ) -> Self {
        self.rules
            .push(crate::PolicyRule::new(role, resource, action, scope));
        self
    }

    pub fn with_override(mut self, ov: crate::UserOverride) -> Self {
        self.overrides.push(ov);
        self
    }

    pub fn build(self) -> FakePolicyClient {
        let repo = Arc::new(crate::InMemoryPolicy::new());
        self.build_with(repo)
    }

    fn build_with(self, repo: Arc<crate::InMemoryPolicy>) -> FakePolicyClient {
        // Seed via the runtime-agnostic executor. `InMemoryPolicy` is
        // Mutex-backed; every future here resolves immediately, so
        // block_on is safe from either a tokio or non-tokio context.
        let seed = async {
            for r in &self.rules {
                repo.upsert_rule(r, "fake").await.unwrap();
            }
            for o in &self.overrides {
                repo.upsert_user_override(o, "fake").await.unwrap();
            }
        };
        futures::executor::block_on(seed);
        let engine = Arc::new(PolicyEngine::new(repo));
        FakePolicyClient { engine }
    }
}

#[async_trait]
impl PolicyClient for FakePolicyClient {
    async fn check(
        &self,
        user: &User,
        action: Action,
        resource: Resource,
    ) -> Result<Decision, PolicyClientError> {
        self.engine
            .check(user, action, resource)
            .await
            .map_err(|e| PolicyClientError::Transport(e.to_string()))
    }

    async fn scope_predicate(
        &self,
        user: &User,
        resource: Resource,
    ) -> Result<Predicate, PolicyClientError> {
        self.engine
            .scope_predicate(user, resource)
            .await
            .map_err(|e| PolicyClientError::Transport(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user() -> User {
        User {
            id: "emp-test".to_string(),
            role: "sales-rep".to_string(),
            access_tier: crate::AccessTier::User,
            territory_account_ids: vec!["p-1".into(), "p-2".into()],
            direct_report_ids: vec![],
            department: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deny_all_denies() {
        let c = FakePolicyClient::deny_all();
        let d = c
            .check(&user(), Action::Read, Resource::job())
            .await
            .unwrap();
        assert!(!d.is_allowed());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn allow_specific_rule() {
        let c = FakePolicyClient::builder()
            .allow("sales-rep", Action::Read, Resource::job(), Scope::Territory)
            .build();
        let d = c
            .check(&user(), Action::Read, Resource::job())
            .await
            .unwrap();
        assert!(d.is_allowed());
    }

    // -- The sim bypass (backlog 85e7f10f, 2026-09-25) -------------------
    //
    // None of these read the environment: the switch is passed in, so
    // the sim-on and sim-off instances are both built in this process.

    fn caller(id: &str, role: &str) -> User {
        User {
            id: id.to_string(),
            role: role.to_string(),
            access_tier: crate::AccessTier::User,
            territory_account_ids: vec![],
            direct_report_ids: vec![],
            department: None,
        }
    }

    /// What the `CurrentUser` extractor yields for a headerless request.
    fn anonymous() -> User {
        caller("anonymous", "guest")
    }

    #[test]
    fn only_an_explicit_yes_turns_the_sim_on() {
        for on in ["true", "TRUE", "True", "1", " true "] {
            assert!(sim_enabled_value(Some(on)), "{on:?} is on");
        }
        for off in [
            None,
            Some(""),
            Some("false"),
            Some("0"),
            Some("no"),
            Some("yes"),
        ] {
            assert!(!sim_enabled_value(off), "{off:?} is off");
        }
    }

    #[test]
    fn the_header_opens_a_chain_only_on_a_sim_instance() {
        assert!(!sim_chain_from_header(false, Some("true")));
        assert!(!sim_chain_from_header(false, Some("1")));
        assert!(sim_chain_from_header(true, Some("true")));
        assert!(sim_chain_from_header(true, Some("1")));
        assert!(!sim_chain_from_header(true, Some("false")));
        assert!(!sim_chain_from_header(true, None));
    }

    #[test]
    fn the_sim_identities_are_the_sim_and_the_dispatcher() {
        assert!(is_sim_identity(&caller("automation:sim", "system-sim")));
        // put_as: the simulated employee's id, the sim's role.
        assert!(is_sim_identity(&caller("emp-042", "system-sim")));
        assert!(is_sim_identity(&caller(
            "automation:dispatcher",
            "platform-admin"
        )));
        assert!(is_sim_identity(&caller(
            "rule:people-hire",
            "platform-admin"
        )));
        assert!(!is_sim_identity(&anonymous()));
        assert!(!is_sim_identity(&caller("emp-042", "clerk")));
        assert!(!is_sim_identity(&caller("claude:opus-5", "platform-admin")));
        assert!(!is_sim_identity(&caller(
            "automation:classes-seed",
            "platform-admin"
        )));
    }

    #[tokio::test]
    async fn a_sim_header_alone_admits_nobody_with_the_sim_off_or_on() {
        let admitted = boss_core::sim_origin::with_sim_chain(true, async {
            [false, true]
                .into_iter()
                .flat_map(|on| {
                    [anonymous(), caller("emp-042", "clerk")]
                        .into_iter()
                        .map(move |u| (on, u.id.clone(), sim_bypass_admits(on, &u)))
                })
                .filter(|(_, _, admitted)| *admitted)
                .collect::<Vec<_>>()
        })
        .await;
        assert!(
            admitted.is_empty(),
            "admitted on a header alone: {admitted:?}"
        );
    }

    #[tokio::test]
    async fn a_sim_caller_is_admitted_only_on_a_sim_instance_and_a_sim_chain() {
        let sim = caller("automation:sim", "system-sim");
        let on_chain = boss_core::sim_origin::with_sim_chain(true, async {
            (
                sim_bypass_admits(true, &sim),
                sim_bypass_admits(false, &sim),
            )
        })
        .await;
        assert_eq!(on_chain, (true, false));
        // Off a chain the identity alone is nothing either.
        assert!(!sim_bypass_admits(true, &sim));
    }

    #[tokio::test]
    async fn the_bypass_is_not_installed_on_an_instance_without_a_sim() {
        let sim = caller("automation:sim", "system-sim");
        let off = SimBypassPolicyClient::wrap(Arc::new(FakePolicyClient::deny_all()), false);
        let on = SimBypassPolicyClient::wrap(Arc::new(FakePolicyClient::deny_all()), true);
        let (off_sim, on_sim, on_anon, on_anon_scope) =
            boss_core::sim_origin::with_sim_chain(true, async {
                (
                    off.check(&sim, Action::Update, Resource::step()).await,
                    on.check(&sim, Action::Update, Resource::step()).await,
                    on.check(&anonymous(), Action::Update, Resource::step())
                        .await,
                    on.scope_predicate(&anonymous(), Resource::job()).await,
                )
            })
            .await;
        assert!(!off_sim.unwrap().is_allowed(), "no sim, no bypass");
        assert!(on_sim.unwrap().is_allowed(), "the sim on a sim instance");
        assert!(!on_anon.unwrap().is_allowed(), "the header is not a caller");
        assert!(
            !matches!(on_anon_scope.unwrap(), Predicate::Unrestricted),
            "a header alone reads nothing unrestricted"
        );
    }

    // -- A policy outage is not a decision (backlog 45553536) ----------
    //
    // The #689 rollout, 2026-09-25: the policy service was dark for
    // seconds and every packet read answered 403 for about a minute,
    // because the outage became a Deny and the Deny was CACHED for the
    // 60 s TTL like any decision. These stand up a real policy stub on
    // loopback, so the reqwest adapter is exercised on the wire.

    /// A policy service that answers `first` for its first `failures`
    /// checks, then `Allow { All }`; and how many checks it has seen.
    async fn policy_stub(
        failures: usize,
        first: axum::http::StatusCode,
    ) -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use axum::response::IntoResponse;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = seen.clone();
        let app = axum::Router::new().route(
            "/api/policy/check",
            axum::routing::post(move || {
                let counter = counter.clone();
                async move {
                    if counter.fetch_add(1, Ordering::SeqCst) < failures {
                        first.into_response()
                    } else {
                        axum::Json(Decision::Allow { scope: Scope::All }).into_response()
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        (format!("http://{addr}"), seen)
    }

    /// The client every loopback-stub test asks through: the production
    /// adapter with a 60 s timeout in place of `new`'s 5 s.
    ///
    /// These tests judge how the client maps an ANSWER (a 5xx, a 4xx, a
    /// decision per role), so the transport must not be what decides
    /// them. Train 40256c45's gate, 2026-09-26: the lib binary took
    /// 19.25 s (0.02 s on the dev pod), pure tests finished after the
    /// HTTP ones had failed, and the two outage tests red on
    /// "error sending request" — the 5 s timeout firing on a starved
    /// host before the stub answered. The same tree was green on the
    /// car's own gate. A stub made to answer after 6 s reproduces both
    /// failures verbatim through `new`, and passes through this.
    fn loopback_client(url: String) -> ReqwestPolicyClient {
        ReqwestPolicyClient::with_timeout(url, std::time::Duration::from_secs(60))
    }

    /// A base URL nothing listens on: bound, read, and released.
    async fn dark_policy_url() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        format!("http://{addr}")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_policy_5xx_is_an_outage_and_the_next_check_asks_again() {
        let (url, seen) = policy_stub(1, axum::http::StatusCode::SERVICE_UNAVAILABLE).await;
        let c = loopback_client(url);

        let first = c.check(&user(), Action::Read, Resource::job()).await;
        match &first {
            Err(PolicyClientError::Unreachable(detail)) => {
                assert!(detail.contains("503"), "names the status: {detail}")
            }
            other => panic!("a 5xx is an outage, never a decision: {other:?}"),
        }
        assert!(
            first
                .unwrap_err()
                .to_string()
                .contains("policy-unreachable"),
            "the rendered error keeps the word every reader keys on"
        );

        // Recovered: the same key is ASKED again, not served a cached deny.
        let second = c
            .check(&user(), Action::Read, Resource::job())
            .await
            .unwrap();
        assert!(second.is_allowed(), "{second:?}");
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 2);

        // A decision the service MADE is cached: a third ask stays local.
        let third = c
            .check(&user(), Action::Read, Resource::job())
            .await
            .unwrap();
        assert!(third.is_allowed());
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dark_policy_service_is_an_error_on_check_and_on_scope() {
        let c = ReqwestPolicyClient::new(dark_policy_url().await);
        let check = c.check(&user(), Action::Read, Resource::job()).await;
        assert!(
            matches!(check, Err(PolicyClientError::Unreachable(_))),
            "{check:?}"
        );
        // A list asks through scope_predicate: an outage is an ERROR
        // there too, never Predicate::None — which a list renders as an
        // empty page, total 0, the shape of data loss.
        let scope = c.scope_predicate(&user(), Resource::job()).await;
        assert!(
            matches!(scope, Err(PolicyClientError::Unreachable(_))),
            "{scope:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_policy_4xx_still_refuses_and_is_not_cached() {
        let (url, seen) = policy_stub(1, axum::http::StatusCode::BAD_REQUEST).await;
        let c = loopback_client(url);
        let first = c
            .check(&user(), Action::Read, Resource::job())
            .await
            .unwrap();
        assert!(!first.is_allowed(), "a 4xx fails closed: {first:?}");
        let second = c
            .check(&user(), Action::Read, Resource::job())
            .await
            .unwrap();
        assert!(second.is_allowed(), "not a decision the service made");
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    // -- The cache is keyed on what the decision reads (backlog 8878f85f)
    //
    // The engine decides on the caller's id (its overrides) AND its role
    // (the rule id), and the role arrives with the caller in x-boss-user.
    // Until 2026-09-26 the cache was keyed on the id alone, so for 60 s
    // one role's decision was served to the same id presenting another:
    // a narrower session got a wider cached Allow, or the reverse.

    /// A policy service that decides by the caller's ROLE, as the real
    /// engine's rule lookup does: `platform-admin` is allowed everything,
    /// every other role is denied. And how many checks it has seen.
    async fn role_policy_stub() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = seen.clone();
        let app = axum::Router::new().route(
            "/api/policy/check",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    let role = body["user"]["role"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string();
                    axum::Json(if role == "platform-admin" {
                        Decision::Allow { scope: Scope::All }
                    } else {
                        Decision::Deny {
                            reason: format!("no active rule for role {role}"),
                        }
                    })
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await });
        (format!("http://{addr}"), seen)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_same_id_with_two_roles_gets_two_decisions() {
        use std::sync::atomic::Ordering;
        let (url, seen) = role_policy_stub().await;
        let c = loopback_client(url);
        let admin = caller("emp-042", "platform-admin");
        let clerk = caller("emp-042", "clerk");

        // Wider first: the admin's Allow must not reach the clerk.
        let wide = c.check(&admin, Action::Update, Resource::step()).await;
        assert!(wide.unwrap().is_allowed(), "platform-admin is allowed");
        let narrow = c.check(&clerk, Action::Update, Resource::step()).await;
        assert!(
            !narrow.unwrap().is_allowed(),
            "the same id as a clerk inside the TTL is served the admin's Allow"
        );
        assert_eq!(seen.load(Ordering::SeqCst), 2, "each role is asked");

        // And the reverse: a fresh client, narrower first.
        let (url, seen) = role_policy_stub().await;
        let c = loopback_client(url);
        assert!(
            !c.check(&clerk, Action::Update, Resource::step())
                .await
                .unwrap()
                .is_allowed()
        );
        assert!(
            c.check(&admin, Action::Update, Resource::step())
                .await
                .unwrap()
                .is_allowed(),
            "the admin is not served the clerk's cached Deny"
        );
        assert_eq!(seen.load(Ordering::SeqCst), 2);

        // The same id, role and tier inside the TTL is still one ask.
        assert!(
            c.check(&admin, Action::Update, Resource::step())
                .await
                .unwrap()
                .is_allowed()
        );
        assert_eq!(seen.load(Ordering::SeqCst), 2, "a repeat stays cached");

        // The access tier is part of the key too: the service receives
        // it, so a caller presenting another tier is asked again.
        let operator = User {
            access_tier: crate::AccessTier::Operator,
            ..admin.clone()
        };
        c.check(&operator, Action::Update, Resource::step())
            .await
            .unwrap();
        assert_eq!(seen.load(Ordering::SeqCst), 3, "another tier is asked");
    }

    #[tokio::test]
    async fn an_outage_answers_503_with_retry_after_and_keeps_its_word() {
        use axum::response::IntoResponse;
        let resp = PolicyClientError::Unreachable("connection refused".into()).into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            resp.headers()
                .get(axum::http::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok()),
            Some(POLICY_OUTAGE_RETRY_AFTER_SECS.to_string().as_str())
        );
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        // The fixed word and nothing else: the detail is the operator's
        // (it is in the log), not the caller's (backlog fe9d212c).
        assert_eq!(String::from_utf8_lossy(&body), "policy-unreachable");

        // Any other client failure stays a 500: not an outage we can
        // promise will pass, and not a permission answer either. Its
        // body is fixed words too — a reqwest decode error names the
        // URL just as a connect error does.
        let other = PolicyClientError::Transport(
            "error decoding response body for url (http://policy.internal:7700/api/policy/check)"
                .into(),
        )
        .into_response();
        assert_eq!(
            other.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        let body = axum::body::to_bytes(other.into_body(), 4096).await.unwrap();
        assert_eq!(String::from_utf8_lossy(&body), "policy check failed");
    }

    /// Backlog fe9d212c (review of the 45553536 car): the body was
    /// `self.to_string()`, and a real outage's detail is reqwest's own
    /// text — "error sending request for url (http://<policy-host>:
    /// <port>/api/policy/check)" — so every door handed its callers the
    /// policy service's internal address. Measured on the wire, with
    /// the real adapter, so the detail is the one production produces.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_real_outage_answers_without_the_policy_services_address() {
        use axum::response::IntoResponse;
        let url = dark_policy_url().await;
        let c = ReqwestPolicyClient::new(url.clone());
        let err = c
            .check(&user(), Action::Read, Resource::job())
            .await
            .unwrap_err();
        // The detail still exists — for the log, where it is read.
        assert!(
            err.to_string().contains("policy-unreachable"),
            "the rendered error keeps its word: {err}"
        );
        let resp = err.into_response();
        assert_eq!(resp.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body = String::from_utf8_lossy(&body);
        let host = url.trim_start_matches("http://");
        assert!(!body.contains("http"), "no URL in the body: {body}");
        assert!(!body.contains(host), "no address in the body: {body}");
        assert_eq!(body, "policy-unreachable");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn scope_predicate_territory_lists_account_ids() {
        let c = FakePolicyClient::builder()
            .allow("sales-rep", Action::Read, Resource::job(), Scope::Territory)
            .build();
        let p = c.scope_predicate(&user(), Resource::job()).await.unwrap();
        match p {
            Predicate::AccountIn { account_ids } => {
                assert_eq!(account_ids, vec!["p-1", "p-2"]);
            }
            other => panic!("expected AccountIn, got {other:?}"),
        }
    }
}
