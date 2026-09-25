//! In-memory adapter — for tests and the `FakePolicyClient` backbone.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::port::{Judge, PolicyError, PolicyRepository, ReconcileStats, id_taken};
use crate::types::{PolicyRule, UserOverride};

#[derive(Default)]
pub struct InMemoryPolicy {
    inner: Mutex<State>,
}

#[derive(Default)]
struct State {
    rules: HashMap<String, PolicyRule>,
    /// Tracks which rules came from a bootstrap seed. Mirrors the
    /// `updated_by = 'bootstrap'` discriminator the postgres adapter
    /// uses, so reconcile semantics match across adapters in tests.
    bootstrap_owned: std::collections::HashSet<String>,
    overrides: HashMap<String, UserOverride>,
}

impl InMemoryPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed with a fixed set of rules. Useful for tests that want a
    /// specific matrix without going through upsert_rule.
    pub fn with_rules(rules: impl IntoIterator<Item = PolicyRule>) -> Self {
        let me = Self::new();
        {
            let mut state = me.inner.lock().expect("poisoned lock");
            for r in rules {
                state.rules.insert(r.id.clone(), r);
            }
        }
        me
    }
}

#[async_trait]
impl PolicyRepository for InMemoryPolicy {
    async fn list_rules(&self) -> Result<Vec<PolicyRule>, PolicyError> {
        let state = self.inner.lock().expect("poisoned lock");
        Ok(state.rules.values().cloned().collect())
    }

    async fn rule_for(&self, id: &str) -> Result<Option<PolicyRule>, PolicyError> {
        let state = self.inner.lock().expect("poisoned lock");
        Ok(state.rules.get(id).cloned())
    }

    async fn upsert_rule_judged(
        &self,
        rule: &PolicyRule,
        changed_by: &str,
        judge: Judge<'_, PolicyRule>,
    ) -> Result<(), PolicyError> {
        // One lock across the read, the judgement and the write — this
        // adapter's transaction.
        let mut state = self.inner.lock().expect("poisoned lock");
        judge(state.rules.get(&rule.id)).map_err(PolicyError::Refused)?;
        state.rules.insert(rule.id.clone(), rule.clone());
        if changed_by == "bootstrap" {
            state.bootstrap_owned.insert(rule.id.clone());
        } else {
            state.bootstrap_owned.remove(&rule.id);
        }
        Ok(())
    }

    async fn deactivate_rule(&self, id: &str, _changed_by: &str) -> Result<(), PolicyError> {
        let mut state = self.inner.lock().expect("poisoned lock");
        match state.rules.get_mut(id) {
            Some(r) => {
                r.active = false;
                Ok(())
            }
            None => Err(PolicyError::NotFound(id.to_string())),
        }
    }

    async fn list_user_overrides(&self, user_id: &str) -> Result<Vec<UserOverride>, PolicyError> {
        // Live rows only, as the port says and the postgres adapter
        // answers. Returning expired rows too hid F5 of backlog b8e75382
        // from every in-memory test: the write door read this list to
        // tell a Create from an Update, and here it saw the expired row
        // postgres never showed it.
        let now = chrono::Utc::now();
        let state = self.inner.lock().expect("poisoned lock");
        Ok(state
            .overrides
            .values()
            .filter(|o| o.user_id == user_id && o.is_active_at(now))
            .cloned()
            .collect())
    }

    async fn user_override(&self, id: &str) -> Result<Option<UserOverride>, PolicyError> {
        let state = self.inner.lock().expect("poisoned lock");
        Ok(state.overrides.get(id).cloned())
    }

    async fn upsert_user_override_judged(
        &self,
        ov: &UserOverride,
        _changed_by: &str,
        judge: Judge<'_, UserOverride>,
    ) -> Result<(), PolicyError> {
        let mut state = self.inner.lock().expect("poisoned lock");
        // The postgres conflict key is (user_id, resource, action), and a
        // conflict rewrites scope, reason and expiry on the row already
        // there, keeping its id. Mirrored, so a test through this
        // adapter rewrites the row postgres would.
        let existing = state
            .overrides
            .values()
            .find(|o| o.user_id == ov.user_id && o.resource == ov.resource && o.action == ov.action)
            .cloned();
        judge(existing.as_ref()).map_err(PolicyError::Refused)?;
        let row = match existing {
            Some(old) => UserOverride {
                scope: ov.scope.clone(),
                reason: ov.reason.clone(),
                expires_at: ov.expires_at,
                ..old
            },
            // A new row under an id another key already owns: postgres
            // refuses it on the primary key, and inserting it here would
            // silently replace that other row (S2 of the hold review of
            // car a8becd52).
            None => match state.overrides.get(&ov.id) {
                Some(other) => return Err(id_taken(ov, other)),
                None => ov.clone(),
            },
        };
        state.overrides.insert(row.id.clone(), row);
        Ok(())
    }

    async fn deactivate_user_override_judged(
        &self,
        id: &str,
        _changed_by: &str,
        judge: Judge<'_, UserOverride>,
    ) -> Result<(), PolicyError> {
        let mut state = self.inner.lock().expect("poisoned lock");
        match state.overrides.get_mut(id) {
            Some(o) => {
                judge(Some(&*o)).map_err(PolicyError::Refused)?;
                o.expires_at = Some(chrono::Utc::now());
                Ok(())
            }
            None => Err(PolicyError::NotFound(id.to_string())),
        }
    }

    async fn bootstrap_reconcile(
        &self,
        defaults: &[PolicyRule],
    ) -> Result<ReconcileStats, PolicyError> {
        let mut stats = ReconcileStats::default();
        let mut state = self.inner.lock().expect("poisoned lock");
        for rule in defaults {
            match state.rules.get(&rule.id) {
                None => {
                    state.rules.insert(rule.id.clone(), rule.clone());
                    state.bootstrap_owned.insert(rule.id.clone());
                    stats.inserted += 1;
                }
                Some(existing) => {
                    if state.bootstrap_owned.contains(&rule.id) {
                        if existing.scope != rule.scope || existing.active != rule.active {
                            state.rules.insert(rule.id.clone(), rule.clone());
                            stats.refreshed += 1;
                        } else {
                            stats.unchanged += 1;
                        }
                    } else {
                        stats.preserved += 1;
                    }
                }
            }
        }
        Ok(stats)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Action, Resource, Scope, UserOverride};

    #[tokio::test]
    async fn bootstrap_reconcile_inserts_missing_rules() {
        let repo = InMemoryPolicy::new();
        let defaults = vec![PolicyRule::new(
            "guest",
            Resource::workflow(),
            Action::Read,
            Scope::All,
        )];
        let stats = repo.bootstrap_reconcile(&defaults).await.unwrap();
        assert_eq!(stats.inserted, 1);
        assert_eq!(stats.refreshed, 0);
        assert_eq!(stats.preserved, 0);
        assert!(
            repo.rule_for("guest:workflow:read")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn bootstrap_reconcile_refreshes_drifted_bootstrap_rows() {
        let repo = InMemoryPolicy::new();
        // Seed an old bootstrap rule with the wrong scope.
        let stale = PolicyRule::new("cto", Resource::workflow(), Action::Read, Scope::Self_);
        repo.upsert_rule(&stale, "bootstrap").await.unwrap();

        // Defaults now say Scope::All — reconcile should refresh.
        let defaults = vec![PolicyRule::new(
            "cto",
            Resource::workflow(),
            Action::Read,
            Scope::All,
        )];
        let stats = repo.bootstrap_reconcile(&defaults).await.unwrap();
        assert_eq!(stats.inserted, 0);
        assert_eq!(stats.refreshed, 1);
        assert_eq!(stats.preserved, 0);
        let live = repo.rule_for("cto:workflow:read").await.unwrap().unwrap();
        assert_eq!(live.scope, Scope::All);
    }

    #[tokio::test]
    async fn bootstrap_reconcile_preserves_operator_edits() {
        let repo = InMemoryPolicy::new();
        // Operator-tuned rule (changed_by != "bootstrap").
        let custom = PolicyRule::new("cto", Resource::workflow(), Action::Read, Scope::Self_);
        repo.upsert_rule(&custom, "emp-cto").await.unwrap();

        let defaults = vec![PolicyRule::new(
            "cto",
            Resource::workflow(),
            Action::Read,
            Scope::All,
        )];
        let stats = repo.bootstrap_reconcile(&defaults).await.unwrap();
        assert_eq!(stats.inserted, 0);
        assert_eq!(stats.refreshed, 0);
        assert_eq!(stats.preserved, 1);
        let live = repo.rule_for("cto:workflow:read").await.unwrap().unwrap();
        assert_eq!(live.scope, Scope::Self_, "operator edit must survive");
    }

    /// The port promises live overrides only; the postgres adapter
    /// filters `expires_at > NOW()`, and this stand-in must answer the
    /// same or a test through it measures a different system.
    #[tokio::test]
    async fn listing_overrides_leaves_out_the_expired() {
        let repo = InMemoryPolicy::new();
        let lapsed = UserOverride {
            id: "ov-lapsed".to_string(),
            user_id: "emp-1".to_string(),
            resource: Resource::job(),
            action: Action::Close,
            scope: Scope::All,
            reason: "last quarter".to_string(),
            expires_at: Some(chrono::Utc::now() - chrono::Duration::hours(1)),
        };
        repo.upsert_user_override(&lapsed, "t").await.unwrap();
        assert_eq!(repo.list_user_overrides("emp-1").await.unwrap(), vec![]);
    }

    /// S2 of the hold review of car a8becd52 (backlog b8e75382): a new
    /// override was stored under the body's id, so an id another
    /// (user, resource, action) already owned silently replaced that
    /// row — the other user's grant or deny vanished with no audit of
    /// it. Postgres refuses the same write (the id is its primary key),
    /// and so does this stand-in, as a Conflict, leaving both rows.
    #[tokio::test]
    async fn an_override_id_owned_by_another_key_is_a_conflict() {
        let repo = InMemoryPolicy::new();
        let deny = UserOverride {
            id: "ov-1".to_string(),
            user_id: "emp-1".to_string(),
            resource: Resource::ledger(),
            action: Action::Read,
            scope: Scope::None,
            reason: "suspended".to_string(),
            expires_at: None,
        };
        repo.upsert_user_override(&deny, "t").await.unwrap();

        let squatter = UserOverride {
            user_id: "emp-2".to_string(),
            scope: Scope::All,
            reason: "same id, another user".to_string(),
            ..deny.clone()
        };
        let err = repo
            .upsert_user_override(&squatter, "t")
            .await
            .expect_err("the id is taken");
        assert!(matches!(err, PolicyError::Conflict(_)), "{err:?}");
        assert_eq!(
            repo.list_user_overrides("emp-1").await.unwrap(),
            vec![deny],
            "the first row stands"
        );
        assert_eq!(repo.list_user_overrides("emp-2").await.unwrap(), vec![]);
    }

    #[tokio::test]
    async fn bootstrap_reconcile_no_op_when_already_matching() {
        let repo = InMemoryPolicy::new();
        let rule = PolicyRule::new("cto", Resource::workflow(), Action::Read, Scope::All);
        repo.upsert_rule(&rule, "bootstrap").await.unwrap();

        let stats = repo.bootstrap_reconcile(&[rule]).await.unwrap();
        assert_eq!(stats.inserted, 0);
        assert_eq!(stats.refreshed, 0);
        assert_eq!(stats.preserved, 0);
        assert_eq!(stats.unchanged, 1);
    }
}
