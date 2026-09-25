//! What an authorized policy writer may write (backlog b8e75382,
//! decided 2026-09-25).
//!
//! Car e2209174 (backlog 42c25542) made every policy write authorize its
//! caller against `policy-rule`. Its adversarial review then proved that
//! was not a bound: holding one policy verb let a caller grant its own
//! role the rest, a rule id could disguise the grant it carried,
//! retiring a deny override widened access while being judged a delete,
//! and nothing kept policy authority from the identities anonymous
//! visitors carry. These are the rules that bound it, as pure functions
//! of three things: what the caller holds (read through the engine
//! BEFORE the write's transaction opens), the row the write would change
//! (read INSIDE it by the adapter, see `port::Judge`), and the write.
//!
//! 1. No grant beyond what the granter holds: granting (action, resource,
//!    scope) needs the caller's own decision on that action and resource
//!    to allow a scope that contains the one granted — and the grant may
//!    last no longer than that decision does. A decision a user override
//!    made ends when the override does, so it grants nothing that
//!    outlives it: no role rule (a rule never expires) and no override
//!    ending later or never (H3 of the hold review of car a8becd52 — an
//!    hour's cover was laundered into a permanent grant). The deploy
//!    superuser is named, not derived — `boss tenant publish` seeds a
//!    tenant's grants as platform-admin, and the example tenant's alone
//!    name twenty `step-signoff:<role>` resources no default gives it.
//! 2. Break-glass repairs, it does not invent: a rule it writes must
//!    equal a row core ships, and it writes no override at all — core
//!    ships none, so none is a repair (H2). It may retire one, which
//!    returns that user to the role rules, judged like anyone's
//!    retirement.
//! 4. A write is judged by its effect: a row that exists, live or not,
//!    makes an upsert an Update; ending a live override narrower than
//!    `all` hands back whatever the user's role grants — up to `all`,
//!    since this service does not know the role — so it is a Create and
//!    a grant at `all`, whether the write retires the row or only brings
//!    its expiry forward (H1: a deny re-POSTed to expire in a second is
//!    a retirement).
//! 5. An anonymous visitor never holds policy authority, as caller or as
//!    grantee.
//!
//! Rule 3 (the id is derived, and a role carries no `:` to derive
//! another grant's id with) is a malformed request, answered 422 at the
//! door before anything here runs; rule 6 (decide inside the
//! transaction) is the shape of `port::Judge`.
//!
//! Two limits, stated because each is a place the bound is looser than a
//! reader might assume (S3 and S4 of the same review):
//!
//! - **Rule 1 compares breadth, not reach.** `self`, `team` and
//!   `territory` are relative to whoever holds them: a granter holding
//!   `team` may grant another role `team`, and the grantee's team is its
//!   own reports, not the granter's. `Scope::contains` answers "is this
//!   shape no wider", never "are these rows ones I can see".
//! - **The caller's authority is read before the transaction.** The
//!   engine is async and the judge is not, so what the caller holds is
//!   read first and only the row being written is read and locked inside
//!   the write. A change to the caller's OWN grants that commits between
//!   the two — a revocation racing a write — is not seen: that one write
//!   is judged on the authority the caller held when it was read.

use boss_core::roles::{
    ANONYMOUS_VISITOR_IDS, BREAK_GLASS_ROLE, PLATFORM_ADMIN_ROLE, is_anonymous_visitor,
    is_anonymous_visitor_role,
};
use boss_policy_client::defaults::default_rules;
use boss_policy_client::types::{Action, Decision, PolicyRule, Resource, Scope, UserOverride};
use chrono::{DateTime, Utc};

/// The verbs a policy write can be judged as.
pub const POLICY_VERBS: [Action; 3] = [Action::Create, Action::Update, Action::Delete];

/// What the caller holds, read before the write's transaction opens.
#[derive(Debug, Clone)]
pub struct Holdings {
    pub id: String,
    pub role: String,
    /// The caller's decision on each of [`POLICY_VERBS`] on `policy-rule`.
    pub policy_rule: Vec<(Action, Decision)>,
    /// The action and resource the write grants or withholds, and the
    /// caller's own decision on them.
    pub action: Action,
    pub resource: Resource,
    pub decision: Decision,
    /// When `decision` stops holding: the expiry of the user override
    /// that made it, `None` when nothing in it expires.
    pub until: Option<DateTime<Utc>>,
}

/// A caller carrying an anonymous visitor's id or role holds no policy
/// authority, whatever the table says (rule 5).
pub fn refuse_anonymous_caller(id: &str, role: &str) -> Result<(), String> {
    if is_anonymous_visitor(id, role) {
        return Err(format!(
            "{id} (role {role}) is an identity an anonymous visitor carries, and those never \
             write policy"
        ));
    }
    Ok(())
}

/// Allow `verb` on `policy-rule` only at scope `all`. A policy rule has
/// no owner, team, territory or department — it belongs to the whole
/// deployment — so only `all` contains one (the row-in-scope check
/// jobs.rs makes after its role check).
pub fn may(role: &str, verb: Action, decision: &Decision) -> Result<(), String> {
    match decision {
        Decision::Allow { scope: Scope::All } => Ok(()),
        Decision::Allow { scope } => Err(format!(
            "role {role} holds {} on policy-rule only at scope {}; a policy rule belongs to the \
             whole deployment, so only scope all writes one",
            verb.as_str(),
            scope.to_db_string(),
        )),
        Decision::Deny { reason } => Err(reason.clone()),
    }
}

/// True when something ending at `end` ends before something ending at
/// `other`, where `None` is never.
fn ends_before(end: Option<DateTime<Utc>>, other: Option<DateTime<Utc>>) -> bool {
    match (end, other) {
        (Some(end), Some(other)) => end < other,
        (Some(_), None) => true,
        (None, _) => false,
    }
}

fn when(t: Option<DateTime<Utc>>) -> String {
    t.map_or_else(|| "never".to_string(), |t| t.to_rfc3339())
}

impl Holdings {
    fn may(&self, verb: Action) -> Result<(), String> {
        match self.policy_rule.iter().find(|(v, _)| *v == verb) {
            Some((_, decision)) => may(&self.role, verb, decision),
            None => Err(format!(
                "{} on policy-rule was not read for {}",
                verb.as_str(),
                self.id
            )),
        }
    }

    /// Rule 1: a grant at `scope` whose effect ends at `ends` (`None`,
    /// never).
    fn holds(&self, scope: &Scope, ends: Option<DateTime<Utc>>) -> Result<(), String> {
        if self.role == PLATFORM_ADMIN_ROLE {
            return Ok(());
        }
        let what = format!("{} on {}", self.action.as_str(), self.resource.as_str());
        match &self.decision {
            Decision::Allow { scope: held } if held.contains(scope) => {}
            Decision::Allow { scope: held } => {
                return Err(format!(
                    "role {} grants {what} at scope {} only if it holds that much itself; it \
                     holds scope {}",
                    self.role,
                    scope.to_db_string(),
                    held.to_db_string(),
                ));
            }
            Decision::Deny { reason } => {
                return Err(format!(
                    "role {} cannot grant {what}, which it does not hold: {reason}",
                    self.role
                ));
            }
        }
        if ends_before(self.until, ends) {
            return Err(format!(
                "{} holds {what} through a user override that ends {}, and a grant made from it \
                 ends no later; this one ends {}",
                self.id,
                when(self.until),
                when(ends),
            ));
        }
        Ok(())
    }

    /// The authority was read for the action and resource the judged
    /// row is about — a guard, since the door reads one and the adapter
    /// hands over the other.
    fn about(&self, action: Action, resource: &Resource) -> Result<(), String> {
        if self.action == action && &self.resource == resource {
            return Ok(());
        }
        Err(format!(
            "authority was read for {} on {}, but the row is {} on {}",
            self.action.as_str(),
            self.resource.as_str(),
            action.as_str(),
            resource.as_str()
        ))
    }
}

/// Write authority on the policy table itself — every verb but Read.
fn is_policy_authority(resource: &Resource, action: Action) -> bool {
    resource == &Resource::policy_rule() && action != Action::Read
}

/// Judge a rule write against the row under its id (active or not).
pub fn judge_rule(
    held: &Holdings,
    existing: Option<&PolicyRule>,
    rule: &PolicyRule,
) -> Result<(), String> {
    refuse_anonymous_caller(&held.id, &held.role)?;
    held.about(rule.action, &rule.resource)?;
    let verb = match existing {
        Some(old) if old.active && !rule.active => Action::Delete,
        Some(_) => Action::Update,
        None => Action::Create,
    };
    held.may(verb)?;
    // A missing rule already denies, so a deny or an inactive row
    // grants nothing and cannot widen anyone's access.
    let grants = rule.active && rule.scope != Scope::None;
    if grants
        && is_policy_authority(&rule.resource, rule.action)
        && is_anonymous_visitor_role(&rule.role)
    {
        return Err(format!(
            "role {} is one an anonymous visitor carries, and it is never granted policy \
             authority",
            rule.role
        ));
    }
    if held.role == BREAK_GLASS_ROLE {
        return if default_rules().contains(rule) {
            Ok(())
        } else {
            Err(format!(
                "break-glass restores a rule core ships, exactly; {} as written is not one",
                rule.id
            ))
        };
    }
    if grants {
        // A rule never expires.
        held.holds(&rule.scope, None)?;
    }
    Ok(())
}

/// Judge an override write — `write` is `None` for a retirement —
/// against the row on its (user, resource, action), expired or not.
pub fn judge_override(
    held: &Holdings,
    existing: Option<&UserOverride>,
    write: Option<&UserOverride>,
    now: DateTime<Utc>,
) -> Result<(), String> {
    refuse_anonymous_caller(&held.id, &held.role)?;
    let row = write
        .or(existing)
        .ok_or_else(|| "no override to judge".to_string())?;
    held.about(row.action, &row.resource)?;
    if held.role == BREAK_GLASS_ROLE && write.is_some() {
        return Err(format!(
            "break-glass repairs what core ships, and core ships no override; it may retire {}'s \
             override on {} {}, which returns that user to the role rules, and write none",
            row.user_id,
            row.action.as_str(),
            row.resource.as_str(),
        ));
    }
    let live_before = existing.filter(|o| o.is_active_at(now));
    let live_after = write.filter(|o| o.is_active_at(now));
    // A live override narrower than `all` that this write ends sooner
    // than it would have ended — a retirement ends it now — hands its
    // user back to the role, which may grant `all`, from then until it
    // would have ended anyway (H1).
    let ends = write.map_or(Some(now), |w| w.expires_at);
    let lifted = live_before.filter(|o| o.scope != Scope::All && ends_before(ends, o.expires_at));
    // The widest access the write can make live for this user, and when
    // that effect ends.
    let granted = match lifted {
        Some(old) => Some((Scope::All, old.expires_at)),
        None => live_after
            .filter(|o| o.scope != Scope::None)
            .map(|o| (o.scope.clone(), o.expires_at)),
    };
    let verbs: &[Action] = match (write, existing, lifted) {
        (None, _, Some(_)) => &[Action::Create],
        (None, _, None) => &[Action::Delete],
        (Some(_), Some(_), Some(_)) => &[Action::Update, Action::Create],
        (Some(_), Some(_), None) => &[Action::Update],
        (Some(_), None, _) => &[Action::Create],
    };
    for verb in verbs {
        held.may(*verb)?;
    }
    let Some((scope, ends)) = granted else {
        return Ok(());
    };
    if is_policy_authority(&row.resource, row.action)
        && ANONYMOUS_VISITOR_IDS.contains(&row.user_id.as_str())
    {
        return Err(format!(
            "{} is an identity an anonymous visitor carries, and it is never granted policy \
             authority",
            row.user_id
        ));
    }
    held.holds(&scope, ends)
}
