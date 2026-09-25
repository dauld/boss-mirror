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
//! `(user_id, action, resource)`. Invalidation is TTL-only; NATS-
//! driven invalidation on top of the TTL is a planned addition (D4).
//!
//! Fail-closed: if the HTTP call fails and no cache entry is
//! available, we return a Deny with reason="policy-unreachable" (D9).

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
pub async fn request_context_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let sim = req
        .headers()
        .get(boss_core::sim_origin::SIM_ORIGIN_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
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
    #[error("policy service unreachable: {0}")]
    Unreachable(String),
    #[error("transport failure: {0}")]
    Transport(String),
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
    /// `Predicate::None`, which callers translate to "no rows."
    async fn scope_predicate(
        &self,
        user: &User,
        resource: Resource,
    ) -> Result<Predicate, PolicyClientError>;
}

// ---------------------------------------------------------------------------
// Sim-origin bypass — the permissive auth handler for simulator traffic
// ---------------------------------------------------------------------------

/// Wraps a [`PolicyClient`] and short-circuits to `Allow` for requests
/// that are part of a simulated event chain — i.e. when
/// [`boss_core::sim_origin::is_in_sim_chain`] is true because the
/// caller sent `x-sim-origin: true`.
///
/// The simulator runs on a fully trusted box and masquerades as the
/// real employees whose work it stands in for; every event it drives
/// is already stamped `_simulated=true` by the SimOrigin middleware.
/// Rather than seed a per-role grant matrix for the simulator (or let
/// it claim a superuser role), we authorize sim traffic here, at the
/// boundary, with a single permissive decision — while real traffic
/// flows through the wrapped client unchanged and is enforced per-role.
///
/// The invariant *"no audit write without a policy allow"* still holds:
/// every write consults policy; sim writes are allowed *because they
/// are the trusted, clearly-marked simulator*, not because the check
/// was skipped.
///
/// SECURITY: this trusts `x-sim-origin`. A production gateway MUST
/// strip or reject that header from untrusted ingress, or the bypass
/// is forgeable. It is safe on the regen/demo box, where only the
/// simulator sets it.
pub struct SimBypassPolicyClient {
    inner: Arc<dyn PolicyClient>,
}

impl SimBypassPolicyClient {
    pub fn new(inner: Arc<dyn PolicyClient>) -> Self {
        Self { inner }
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
        if boss_core::sim_origin::is_in_sim_chain() {
            return Ok(Decision::Allow { scope: Scope::All });
        }
        self.inner.check(user, action, resource).await
    }

    async fn scope_predicate(
        &self,
        user: &User,
        resource: Resource,
    ) -> Result<Predicate, PolicyClientError> {
        if boss_core::sim_origin::is_in_sim_chain() {
            return Ok(Predicate::Unrestricted);
        }
        self.inner.scope_predicate(user, resource).await
    }
}

// ---------------------------------------------------------------------------
// Reqwest adapter — the prod client
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct CacheKey {
    user_id_hash: u64,
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
        Self {
            base_url: base_url.into(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
                .expect("reqwest client"),
            cache: RwLock::new(HashMap::new()),
            ttl: std::time::Duration::from_secs(60),
        }
    }

    fn cache_key(user_id: &str, action: Action, resource: Resource) -> CacheKey {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        user_id.hash(&mut h);
        CacheKey {
            user_id_hash: h.finish(),
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
        let key = Self::cache_key(&user.id, action, resource.clone());
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

        let decision = match resp {
            Ok(r) if r.status().is_success() => r
                .json::<Decision>()
                .await
                .map_err(|e| PolicyClientError::Transport(e.to_string()))?,
            Ok(r) => {
                // HTTP error from the server: fail closed.
                let status = r.status();
                tracing::warn!(%status, "policy service returned non-2xx; deny");
                Decision::Deny {
                    reason: format!("policy service returned {status}"),
                }
            }
            Err(e) => {
                // Service unreachable and no warm cache: fail closed per D9.
                tracing::warn!(error = %e, "policy service unreachable; deny");
                Decision::Deny {
                    reason: "policy-unreachable".to_string(),
                }
            }
        };

        self.cache_put(key, decision.clone()).await;
        Ok(decision)
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
