//! `PolicyEngine::check` + `scope_predicate`. Stateless over the repo —
//! all caching lives in the client.

use std::sync::Arc;

use crate::port::{PolicyError, PolicyRepository};
use crate::predicates::scope_to_predicate;
use crate::types::{Action, Decision, Predicate, Resource, Scope, User};

pub struct PolicyEngine<R: PolicyRepository> {
    repo: Arc<R>,
}

/// The instant an override's expiry is judged at. Expiry is local
/// decision logic that stamps no record, which is why this file may read
/// the wall clock (infra/lint/no-wallclock.sh). One reading, so the
/// engine deciding whether an override applies and the write door
/// deciding whether retiring one widens access (backlog b8e75382) judge
/// "expired" the same way.
pub fn expiry_now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

impl<R: PolicyRepository> PolicyEngine<R> {
    pub fn new(repo: Arc<R>) -> Self {
        Self { repo }
    }

    /// Resolve (user, action, resource) into a Decision.
    ///
    /// Evaluation order (per the design doc):
    /// 1. User overrides (non-expired) take priority over role rules.
    /// 2. Role rule lookup for (user.role, resource, action).
    /// 3. Missing or inactive rule → Deny.
    pub async fn check(
        &self,
        user: &User,
        action: Action,
        resource: Resource,
    ) -> Result<Decision, PolicyError> {
        self.check_until(user, action, resource)
            .await
            .map(|(decision, _)| decision)
    }

    /// [`Self::check`], and when the decision stops holding: the expiry
    /// of the user override that decided it, or `None` when nothing in
    /// it expires — a role rule, or an override with no expiry. The
    /// policy service caps what a caller grants at this, so an override
    /// that ends in an hour is not authority for a grant that never
    /// does (backlog b8e75382, H3 of the hold review of car a8becd52).
    pub async fn check_until(
        &self,
        user: &User,
        action: Action,
        resource: Resource,
    ) -> Result<(Decision, Option<chrono::DateTime<chrono::Utc>>), PolicyError> {
        let now = expiry_now();

        // 1. User overrides first.
        let overrides = self.repo.list_user_overrides(&user.id).await?;
        for ov in &overrides {
            if ov.resource == resource && ov.action == action && ov.is_active_at(now) {
                let decision = match &ov.scope {
                    Scope::None => Decision::Deny {
                        reason: format!("user override: {}", ov.reason),
                    },
                    other => Decision::Allow {
                        scope: other.clone(),
                    },
                };
                return Ok((decision, ov.expires_at));
            }
        }

        // 2. Role rule.
        let rule_id = crate::types::rule_id(&user.role, &resource, action);
        let rule = self.repo.rule_for(&rule_id).await?;

        let decision = match rule {
            Some(r) if r.active => match r.scope {
                Scope::None => Decision::Deny {
                    reason: format!(
                        "role {} is denied {} on {}",
                        user.role,
                        action.as_str(),
                        resource.as_str()
                    ),
                },
                other => Decision::Allow { scope: other },
            },
            _ => Decision::Deny {
                reason: format!(
                    "no active rule for role {} on {}:{}",
                    user.role,
                    resource.as_str(),
                    action.as_str(),
                ),
            },
        };
        Ok((decision, None))
    }

    /// The Predicate a list endpoint should apply to filter rows to
    /// what this user may Read on this resource. Shorthand for
    /// `check(Read)` + predicate translation.
    pub async fn scope_predicate(
        &self,
        user: &User,
        resource: Resource,
    ) -> Result<Predicate, PolicyError> {
        match self.check(user, Action::Read, resource).await? {
            Decision::Deny { .. } => Ok(Predicate::None),
            Decision::Allow { scope } => Ok(scope_to_predicate(&scope, user)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::in_memory::InMemoryPolicy;
    use crate::types::{AccessTier, PolicyRule, UserOverride};

    fn user(role: &str, id: &str) -> User {
        User {
            id: id.to_string(),
            role: role.to_string(),
            access_tier: AccessTier::User,
            territory_account_ids: vec![],
            direct_report_ids: vec![],
            department: None,
        }
    }

    async fn engine_with_rules(rules: Vec<PolicyRule>) -> PolicyEngine<InMemoryPolicy> {
        let repo = Arc::new(InMemoryPolicy::new());
        for r in rules {
            repo.upsert_rule(&r, "test").await.unwrap();
        }
        PolicyEngine::new(repo)
    }

    #[tokio::test]
    async fn missing_rule_denies() {
        let engine = engine_with_rules(vec![]).await;
        let u = user("sales-rep", "emp-1");
        let d = engine
            .check(&u, Action::Close, Resource::job())
            .await
            .unwrap();
        assert!(!d.is_allowed());
    }

    #[tokio::test]
    async fn active_rule_allows() {
        let rule = PolicyRule::new("sales-rep", Resource::job(), Action::Read, Scope::Territory);
        let engine = engine_with_rules(vec![rule]).await;
        let u = user("sales-rep", "emp-1");
        let d = engine
            .check(&u, Action::Read, Resource::job())
            .await
            .unwrap();
        match d {
            Decision::Allow { scope } => assert_eq!(scope, Scope::Territory),
            Decision::Deny { .. } => panic!("expected allow"),
        }
    }

    #[tokio::test]
    async fn scope_none_rule_denies_with_role_reason() {
        let rule = PolicyRule::new("sales-rep", Resource::employee(), Action::Read, Scope::None);
        let engine = engine_with_rules(vec![rule]).await;
        let u = user("sales-rep", "emp-1");
        let d = engine
            .check(&u, Action::Read, Resource::employee())
            .await
            .unwrap();
        match d {
            Decision::Deny { reason } => assert!(reason.contains("sales-rep")),
            Decision::Allow { .. } => panic!("expected deny"),
        }
    }

    #[tokio::test]
    async fn user_override_beats_role_rule() {
        let rule = PolicyRule::new("sales-rep", Resource::employee(), Action::Read, Scope::None);
        let repo = Arc::new(InMemoryPolicy::new());
        repo.upsert_rule(&rule, "test").await.unwrap();

        let ov = UserOverride {
            id: "ov-1".to_string(),
            user_id: "emp-7".to_string(),
            resource: Resource::employee(),
            action: Action::Read,
            scope: Scope::All,
            reason: "covering HR temporarily".into(),
            expires_at: None,
        };
        repo.upsert_user_override(&ov, "test").await.unwrap();

        let engine = PolicyEngine::new(repo);
        let u = user("sales-rep", "emp-7");
        let d = engine
            .check(&u, Action::Read, Resource::employee())
            .await
            .unwrap();
        assert!(d.is_allowed());
    }

    #[tokio::test]
    async fn expired_override_is_ignored() {
        let rule = PolicyRule::new("sales-rep", Resource::employee(), Action::Read, Scope::None);
        let repo = Arc::new(InMemoryPolicy::new());
        repo.upsert_rule(&rule, "test").await.unwrap();

        let past = chrono::Utc::now() - chrono::Duration::hours(1);
        let ov = UserOverride {
            id: "ov-expired".to_string(),
            user_id: "emp-7".to_string(),
            resource: Resource::employee(),
            action: Action::Read,
            scope: Scope::All,
            reason: "past delegation".into(),
            expires_at: Some(past),
        };
        repo.upsert_user_override(&ov, "test").await.unwrap();

        let engine = PolicyEngine::new(repo);
        let u = user("sales-rep", "emp-7");
        let d = engine
            .check(&u, Action::Read, Resource::employee())
            .await
            .unwrap();
        assert!(!d.is_allowed());
    }

    #[tokio::test]
    async fn inactive_rule_denies() {
        let mut rule =
            PolicyRule::new("sales-rep", Resource::job(), Action::Read, Scope::Territory);
        rule.active = false;
        let engine = engine_with_rules(vec![rule]).await;
        let u = user("sales-rep", "emp-1");
        let d = engine
            .check(&u, Action::Read, Resource::job())
            .await
            .unwrap();
        assert!(!d.is_allowed());
    }

    #[tokio::test]
    async fn scope_predicate_denied_returns_none() {
        let engine = engine_with_rules(vec![]).await;
        let u = user("sales-rep", "emp-1");
        let p = engine.scope_predicate(&u, Resource::job()).await.unwrap();
        assert!(p.matches_none());
    }

    #[tokio::test]
    async fn scope_predicate_all_returns_unrestricted() {
        let rule = PolicyRule::new("ceo", Resource::job(), Action::Read, Scope::All);
        let engine = engine_with_rules(vec![rule]).await;
        let u = user("ceo", "emp-ceo");
        let p = engine.scope_predicate(&u, Resource::job()).await.unwrap();
        assert!(p.is_unrestricted());
    }
}
