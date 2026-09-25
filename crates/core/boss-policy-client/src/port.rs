//! Hexagonal port: `PolicyRepository` — what the engine needs from
//! persistence. Adapters implement it.

use async_trait::async_trait;

use crate::types::{PolicyRule, UserOverride};

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("storage failure: {0}")]
    Storage(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    /// A [`Judge`] refused the write, having seen the row it would
    /// change inside the write's own transaction. Nothing was written.
    #[error("refused: {0}")]
    Refused(String),
}

/// A decision on a write, taken by the adapter INSIDE the write's
/// transaction on the row the write would change — locked, and read
/// live or not (an inactive rule, an expired override) — or on `None`
/// when there is no such row. `Err` carries the refusal and writes
/// nothing.
///
/// Backlog b8e75382 (F5): the write doors used to read the row through
/// one port call and write through another, so the judgement and the
/// write saw different tables — an expired override was judged a Create
/// and then revived by the upsert, and a concurrent create slipped
/// between the read and the write. The judge is synchronous on purpose:
/// what the CALLER holds is read before the transaction opens, and only
/// the row itself is read inside it.
pub type Judge<'a, T> = &'a (dyn Fn(Option<&T>) -> Result<(), String> + Send + Sync);

/// The refusal of a new override whose id another (user, resource,
/// action) already owns. The id is the row's primary key and the triple
/// its conflict key, so the write can neither create a second row under
/// that id nor rewrite the other one; every adapter answers it the same,
/// a Conflict (backlog b8e75382, S2 of the hold review of car a8becd52 —
/// the in-memory adapter replaced the other row, postgres answered 500).
pub fn id_taken(ov: &UserOverride, owner: &UserOverride) -> PolicyError {
    PolicyError::Conflict(format!(
        "override id {} already names {}'s override of {} on {}; a new override of {} on {} for \
         {} needs an id of its own",
        ov.id,
        owner.user_id,
        owner.action.as_str(),
        owner.resource.as_str(),
        ov.action.as_str(),
        ov.resource.as_str(),
        ov.user_id,
    ))
}

/// The judge of a write nobody made through a door — bootstrap
/// reconcile, a seeded test fixture — which has no caller to judge.
pub fn unjudged<T>(_: Option<&T>) -> Result<(), String> {
    Ok(())
}

#[async_trait]
pub trait PolicyRepository: Send + Sync {
    /// Every active rule in the store.
    async fn list_rules(&self) -> Result<Vec<PolicyRule>, PolicyError>;

    /// Single rule lookup. Returns None if no rule exists for this
    /// (role, resource, action) triple.
    async fn rule_for(&self, id: &str) -> Result<Option<PolicyRule>, PolicyError>;

    /// Upsert a rule. Idempotent: if a row with the same id exists,
    /// update it; otherwise insert. Writes a `rule.upsert` audit row.
    /// `judge` sees the row under that id, active or not, inside the
    /// transaction; if it saw none and another writer creates one first,
    /// the write fails `Conflict` rather than update a row judged absent.
    async fn upsert_rule_judged(
        &self,
        rule: &PolicyRule,
        changed_by: &str,
        judge: Judge<'_, PolicyRule>,
    ) -> Result<(), PolicyError>;

    /// [`Self::upsert_rule_judged`] with no caller to judge.
    async fn upsert_rule(&self, rule: &PolicyRule, changed_by: &str) -> Result<(), PolicyError> {
        self.upsert_rule_judged(rule, changed_by, &unjudged::<PolicyRule>)
            .await
    }

    /// Soft-delete (sets `active=false`). Writes `rule.deactivate` audit.
    async fn deactivate_rule(&self, id: &str, changed_by: &str) -> Result<(), PolicyError>;

    /// Active (non-expired) overrides for one user.
    async fn list_user_overrides(&self, user_id: &str) -> Result<Vec<UserOverride>, PolicyError>;

    /// One override by id, expired or not. Its user, resource and
    /// action never change once written — an upsert rewrites only the
    /// scope, reason and expiry — which is what lets a write door learn
    /// what an override is ABOUT before the transaction that judges it.
    async fn user_override(&self, id: &str) -> Result<Option<UserOverride>, PolicyError>;

    /// Upsert a user override on (user_id, resource, action) — the
    /// adapter's conflict key, so an expired row there is rewritten, not
    /// duplicated. Writes `override.upsert` audit. `judge` sees that
    /// row, expired or not, inside the transaction.
    async fn upsert_user_override_judged(
        &self,
        ov: &UserOverride,
        changed_by: &str,
        judge: Judge<'_, UserOverride>,
    ) -> Result<(), PolicyError>;

    /// [`Self::upsert_user_override_judged`] with no caller to judge.
    async fn upsert_user_override(
        &self,
        ov: &UserOverride,
        changed_by: &str,
    ) -> Result<(), PolicyError> {
        self.upsert_user_override_judged(ov, changed_by, &unjudged::<UserOverride>)
            .await
    }

    /// Remove a user override (by setting expires_at to now). Writes
    /// `override.deactivate` audit. `judge` sees the row by id inside
    /// the transaction; a missing id is `NotFound` before it is asked.
    async fn deactivate_user_override_judged(
        &self,
        id: &str,
        changed_by: &str,
        judge: Judge<'_, UserOverride>,
    ) -> Result<(), PolicyError>;

    /// [`Self::deactivate_user_override_judged`] with no caller to judge.
    async fn deactivate_user_override(
        &self,
        id: &str,
        changed_by: &str,
    ) -> Result<(), PolicyError> {
        self.deactivate_user_override_judged(id, changed_by, &unjudged::<UserOverride>)
            .await
    }

    /// Reconcile the in-DB rules against a set of code-defined defaults.
    ///
    /// For each rule:
    ///   - Missing (no row with that id) → insert as `updated_by =
    ///     'bootstrap'`.
    ///   - Present and bootstrap-owned (`updated_by = 'bootstrap'`) but
    ///     drifted from the default body (scope or active changed) →
    ///     upsert, restamping `updated_by = 'bootstrap'`.
    ///   - Present and operator-owned (any other `updated_by`) →
    ///     preserve the operator edit untouched.
    ///
    /// This is the design's escape valve from the original "seed only
    /// if missing" loop, which silently let a default's scope drift
    /// out of the live DB whenever the code-side default changed
    /// after first boot. Bootstrap rules now self-heal on every
    /// service restart; operator-tuned rules still survive.
    async fn bootstrap_reconcile(
        &self,
        defaults: &[PolicyRule],
    ) -> Result<ReconcileStats, PolicyError>;
}

/// Result of a `bootstrap_reconcile` call. Counts each branch so the
/// service log records how much drift was healed on this boot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileStats {
    /// New rule rows inserted.
    pub inserted: usize,
    /// Bootstrap-owned rows whose scope or active flag was refreshed
    /// to match the current code default.
    pub refreshed: usize,
    /// Operator-edited rows left untouched.
    pub preserved: usize,
    /// Bootstrap-owned rows already matching the default — no write.
    pub unchanged: usize,
}
