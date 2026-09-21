//! Core vocabulary — actions, resources, scope, rules, decisions.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Action — what the caller is trying to do.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Read,
    Create,
    Update,
    Close,
    SignOff,
    Delete,
    /// Flip a draft Workflow (or similar lifecycle resource) to active.
    /// Separate from Update so organizations can let many roles draft
    /// while restricting who can actually make a draft live.
    Publish,
    /// Flip an active resource to retired. Pairs with Publish.
    Retire,
}

impl Action {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Create => "create",
            Self::Update => "update",
            Self::Close => "close",
            Self::SignOff => "sign-off",
            Self::Delete => "delete",
            Self::Publish => "publish",
            Self::Retire => "retire",
        }
    }
}

impl std::str::FromStr for Action {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read" => Ok(Self::Read),
            "create" => Ok(Self::Create),
            "update" => Ok(Self::Update),
            "close" => Ok(Self::Close),
            "sign-off" => Ok(Self::SignOff),
            "delete" => Ok(Self::Delete),
            "publish" => Ok(Self::Publish),
            "retire" => Ok(Self::Retire),
            other => Err(format!("unknown action: {other}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Resource — the noun being acted on. Tier-extensible string newtype;
// tenants introduce new kinds via `Resource::new(...)` without forking core.
// ---------------------------------------------------------------------------

/// Resource is a policy-gated kind of thing — what a rule says "Read /
/// Update / Close / SignOff is allowed on." Tier-1 (state-machine OS)
/// kinds are `job`, `step`, `policy-rule`, `workflow`, `step-plugin`;
/// module + tenant crates introduce their own kinds (`invoice`,
/// `account`, `specimen`, …) by calling `Resource::new("specimen")`
/// without modifying core. Helper constructors are provided for the
/// 13 kinds the platform ships out of the box; they're shorthand —
/// not the gating list.
///
/// Wire + DB format is the bare kebab-case string ("policy-rule",
/// "workflow", "purchase-order"). `#[serde(transparent)]` makes the
/// newtype invisible on the wire — a v1 policy_rules row with
/// `resource = 'invoice'` deserializes the same after this lift.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Resource(String);

impl Resource {
    /// Construct from any kebab-case-ish string. No validation: callers
    /// own the namespace.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_inner(self) -> String {
        self.0
    }

    // ---- Platform-level (Tier 1) resources -------------------------
    pub fn job() -> Self {
        Self::new("job")
    }
    pub fn step() -> Self {
        Self::new("step")
    }
    pub fn policy_rule() -> Self {
        Self::new("policy-rule")
    }
    pub fn workflow() -> Self {
        Self::new("workflow")
    }
    /// Raw audit-log rows, wherever they are surfaced as results —
    /// global search, a View over `events`, any future reader.
    ///
    /// The log itself is still written and tailed out-of-band; this
    /// governs who may be handed its rows back as a *result set*,
    /// which is a different question and one that used to have no
    /// answer at all.
    pub fn event() -> Self {
        Self::new("event")
    }

    /// The general ledger as a readable surface: accounts, the trial
    /// balance, the three statements, journal entries, tax liability,
    /// bills.
    ///
    /// All-or-nothing by nature. A financial statement is a
    /// company-wide aggregate with no owner to narrow by — there is no
    /// such thing as "my share of the balance sheet" — so the only
    /// meaningful question is whether this caller may see the
    /// company's finances at all.
    pub fn ledger() -> Self {
        Self::new("ledger")
    }

    /// Identity rows — the `subjects` table, across every kind.
    ///
    /// Coarser than the per-kind resources (`account`, `employee`,
    /// `asset`) on purpose: those govern a Subject's *detail*, this
    /// governs whether you may enumerate identity at all. A surface
    /// that lists Subjects of mixed kinds has no single per-kind
    /// resource to ask about, and asking none was the previous
    /// answer.
    pub fn subject() -> Self {
        Self::new("subject")
    }

    pub fn step_plugin() -> Self {
        Self::new("step-plugin")
    }

    // ---- Module-tier shorthands ------------------------------------
    // Helpers stay in core for call-site ergonomics; the tier-purity
    // property is that `Resource::new("specimen")` works for anything
    // else without a core change.
    pub fn account() -> Self {
        Self::new("account")
    }
    pub fn employee() -> Self {
        Self::new("employee")
    }
    pub fn invoice() -> Self {
        Self::new("invoice")
    }
    pub fn agreement() -> Self {
        Self::new("agreement")
    }
    pub fn asset() -> Self {
        Self::new("asset")
    }
    pub fn shipment() -> Self {
        Self::new("shipment")
    }
    pub fn part() -> Self {
        Self::new("part")
    }
    pub fn purchase_order() -> Self {
        Self::new("purchase-order")
    }
}

impl std::fmt::Display for Resource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::str::FromStr for Resource {
    type Err = std::convert::Infallible;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Scope — the filter applied by a rule. v1 has six variants (per D1).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// Never allowed.
    None,
    /// Rows the caller owns directly (owner_id match).
    #[serde(rename = "self")]
    Self_,
    /// Accounts on the caller's account team, and rows whose subject
    /// references one of those accounts (per D2: multi-rep).
    Territory,
    /// Rows whose owner is the caller OR one of the caller's direct reports.
    Team,
    /// Rows scoped to the named department (per D3).
    Department(String),
    /// No restriction.
    All,
}

impl Scope {
    /// Serialize to the DB `scope` column format.
    /// 'none' | 'self' | 'territory' | 'team' | '`department:<name>`' | 'all'
    pub fn to_db_string(&self) -> String {
        match self {
            Self::None => "none".to_string(),
            Self::Self_ => "self".to_string(),
            Self::Territory => "territory".to_string(),
            Self::Team => "team".to_string(),
            Self::Department(d) => format!("department:{d}"),
            Self::All => "all".to_string(),
        }
    }

    pub fn from_db_string(s: &str) -> Result<Self, String> {
        match s {
            "none" => Ok(Self::None),
            "self" => Ok(Self::Self_),
            "territory" => Ok(Self::Territory),
            "team" => Ok(Self::Team),
            "all" => Ok(Self::All),
            other if other.starts_with("department:") => {
                Ok(Self::Department(other[11..].to_string()))
            }
            other => Err(format!("unknown scope: {other}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Rule — one (role, resource, action) → scope binding.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRule {
    /// `role:resource:action`.
    pub id: String,
    pub role: String,
    pub resource: Resource,
    pub action: Action,
    pub scope: Scope,
    pub active: bool,
}

impl PolicyRule {
    pub fn new(role: impl Into<String>, resource: Resource, action: Action, scope: Scope) -> Self {
        let role = role.into();
        let id = format!("{}:{}:{}", role, resource.as_str(), action.as_str());
        Self {
            id,
            role,
            resource,
            action,
            scope,
            active: true,
        }
    }
}

// ---------------------------------------------------------------------------
// User override — per-user exception to the role rule. Delegation,
// coverage, temporary elevation.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserOverride {
    pub id: String,
    pub user_id: String,
    pub resource: Resource,
    pub action: Action,
    pub scope: Scope,
    pub reason: String,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl UserOverride {
    pub fn is_active_at(&self, now: chrono::DateTime<chrono::Utc>) -> bool {
        self.expires_at.is_none_or(|exp| exp > now)
    }
}

// ---------------------------------------------------------------------------
// User context passed into every policy check.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub role: String,
    #[serde(default = "default_tier")]
    pub access_tier: AccessTier,
    /// Accounts on the user's account team (per D2). Pre-computed at
    /// session start and refreshed every few minutes.
    #[serde(default)]
    pub territory_account_ids: Vec<String>,
    /// Employees reporting to this user. Pre-computed at session start.
    #[serde(default)]
    pub direct_report_ids: Vec<String>,
    #[serde(default)]
    pub department: Option<String>,
}

impl User {
    /// The ambient [`ActorId`](boss_core::actor::ActorId) this request
    /// acts as — used by the request-context middleware as the default
    /// actor for any event a handler emits without naming one. Keyed on
    /// `id`, not `role` (a `system-sim` role with a real employee id is
    /// still a human; the role only marks the event simulated).
    ///
    /// Returns `None` for an anonymous request (no `x-boss-user`); the
    /// emit then falls back to the emitting service's own automation
    /// identity. There is no `"system"` actor — every automated id maps
    /// to a named `Automation`.
    ///
    /// The legacy automation spellings below are checked BEFORE
    /// delegating to `ActorId::from_str`, because they are
    /// colon-bearing ids that predate the agent wire form:
    /// `rule:bill-approve` is a firing dispatch rule, not a `rule`-mode
    /// agent. Everything left over goes through the type's own parse,
    /// which sorts `<mode>:<model>` agents from bare employee ids.
    pub fn ambient_actor(&self) -> Option<boss_core::actor::ActorId> {
        use boss_core::actor::ActorId;
        let id = self.id.as_str();
        if id == "anonymous" {
            return None;
        }
        // Already a typed automation (`automation:<slug>`).
        if let Some(slug) = id.strip_prefix("automation:") {
            return Some(ActorId::Automation(slug.to_string()));
        }
        // Non-prefixed automation identities → a named automation:
        //   rule:<name>     → automation:rule:<name>  (the firing rule)
        //   system:<proc>   → automation:<proc>       (e.g. dispatcher)
        //   system          → automation:platform     (legacy catch-all)
        //   *-sim / *-runner→ automation:<id>
        if id == "system"
            || id.starts_with("rule:")
            || id.starts_with("system:")
            || id.ends_with("-sim")
            || id.ends_with("-runner")
        {
            let slug = id.strip_prefix("system:").unwrap_or(id);
            let slug = if slug == "system" { "platform" } else { slug };
            return Some(ActorId::Automation(slug.to_string()));
        }
        // `<mode>:<model>` → Agent; anything else → Human. Infallible.
        id.parse().ok()
    }
}

fn default_tier() -> AccessTier {
    AccessTier::User
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccessTier {
    User,
    Operator,
    Auditor,
}

// ---------------------------------------------------------------------------
// Decision — what the engine returns.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "kebab-case")]
pub enum Decision {
    Allow { scope: Scope },
    Deny { reason: String },
}

impl Decision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow { .. })
    }
}

// ---------------------------------------------------------------------------
// Predicate — resource-agnostic intent produced by scope_predicate.
// Each repository adapter translates this into its own WHERE clause.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Predicate {
    /// Scope::All → always true.
    Unrestricted,
    /// Scope::None → always false; caller should short-circuit.
    None,
    /// Scope::Self_ → WHERE owner_id = $1.
    OwnerIs { user_id: String },
    /// Scope::Territory → WHERE account_id IN (...) (or resource-
    /// specific join for subjects).
    AccountIn { account_ids: Vec<String> },
    /// Scope::Team → WHERE owner_id IN (self + direct reports).
    OwnerIn { user_ids: Vec<String> },
    /// Scope::Department(d) → resource-specific department filter.
    DepartmentIs { department: String },
}

impl Predicate {
    pub fn is_unrestricted(&self) -> bool {
        matches!(self, Self::Unrestricted)
    }

    /// Collapse this predicate to an **owner allow-list** for a
    /// surface whose only narrowing column is `owner_id`.
    ///
    /// `None` means unrestricted; `Some(list)` means "owner must be
    /// in this list", and an empty list therefore means nothing
    /// matches. That asymmetry is the trap this method exists to
    /// stop being retyped: `None` reads like "no access" and means
    /// the opposite, so every copy of the translation is a chance to
    /// turn a denial into full access.
    ///
    /// `AccountIn` narrows by a Subject rather than an owner and so
    /// cannot be expressed here; it collapses to deny rather than
    /// widening to allow. `DepartmentIs` is all-or-nothing on whether
    /// the caller is in that department, since these surfaces carry
    /// no department column.
    ///
    /// boss-search and boss-views had byte-identical copies of this,
    /// differing only in the wrapper type they returned. jobs-api
    /// keeps its own translation: it maps `AccountIn` onto a real
    /// subject join, so it genuinely does more than this.
    pub fn owner_allow_list(&self, user: &User) -> Option<Vec<String>> {
        match self {
            Self::Unrestricted => Option::None,
            Self::None => Some(Vec::new()),
            Self::OwnerIs { user_id } => Some(vec![user_id.clone()]),
            Self::OwnerIn { user_ids } => Some(user_ids.clone()),
            Self::DepartmentIs { department } => {
                if user.department.as_deref() == Some(department.as_str()) {
                    Option::None
                } else {
                    Some(Vec::new())
                }
            }
            Self::AccountIn { .. } => Some(Vec::new()),
        }
    }

    pub fn matches_none(&self) -> bool {
        matches!(self, Self::None)
    }
}

#[cfg(test)]
mod owner_allow_list_tests {
    use super::*;

    fn user(id: &str, department: Option<&str>) -> User {
        User {
            id: id.to_string(),
            role: "r".to_string(),
            access_tier: AccessTier::User,
            territory_account_ids: vec![],
            direct_report_ids: vec![],
            department: department.map(str::to_string),
        }
    }

    #[test]
    fn unrestricted_is_none_and_deny_is_empty_not_the_other_way_round() {
        // The whole reason this lives in one place.
        let u = user("emp-1", None);
        assert_eq!(Predicate::Unrestricted.owner_allow_list(&u), None);
        assert_eq!(Predicate::None.owner_allow_list(&u), Some(vec![]));
    }

    #[test]
    fn owner_predicates_become_the_allow_list() {
        let u = user("emp-1", None);
        assert_eq!(
            Predicate::OwnerIs {
                user_id: "emp-1".into()
            }
            .owner_allow_list(&u),
            Some(vec!["emp-1".to_string()])
        );
        assert_eq!(
            Predicate::OwnerIn {
                user_ids: vec!["a".into(), "b".into()]
            }
            .owner_allow_list(&u),
            Some(vec!["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn department_is_all_or_nothing_on_membership() {
        let inside = user("emp-1", Some("brewhouse"));
        let outside = user("emp-2", Some("sales"));
        let p = Predicate::DepartmentIs {
            department: "brewhouse".into(),
        };
        assert_eq!(p.owner_allow_list(&inside), None, "in the department → all");
        assert_eq!(p.owner_allow_list(&outside), Some(vec![]));
        // No department at all is not membership.
        assert_eq!(p.owner_allow_list(&user("emp-3", None)), Some(vec![]));
    }

    #[test]
    fn account_scope_collapses_to_deny_never_to_allow() {
        // It narrows by Subject, which these surfaces cannot express.
        // Widening would hand an account-scoped caller everything.
        let u = user("emp-1", None);
        assert_eq!(
            Predicate::AccountIn {
                account_ids: vec!["acc-1".into()]
            }
            .owner_allow_list(&u),
            Some(vec![])
        );
    }
}

#[cfg(test)]
mod ambient_actor_tests {
    use super::*;
    use boss_core::actor::ActorId;

    fn user(id: &str) -> User {
        User {
            id: id.to_string(),
            role: "r".to_string(),
            access_tier: AccessTier::User,
            territory_account_ids: vec![],
            direct_report_ids: vec![],
            department: None,
        }
    }

    /// An agent session authenticates as `<mode>:<model>`. It must
    /// land on the Agent arm — not Human, which is what made 104 step
    /// assignments read as an unknown employee.
    #[test]
    fn an_agent_session_is_an_agent_not_a_human() {
        for id in ["claude:fable", "claude:opus-5"] {
            let a = user(id).ambient_actor().unwrap();
            assert_eq!(a.to_string(), id);
            assert!(a.is_agent(), "{id} should be an agent");
            assert!(!a.is_human(), "{id} must not count as staff");
        }
    }

    /// The pre-existing automation shapes keep winning over the agent
    /// colon-split — `rule:<name>` is a firing rule, not a `rule`-mode
    /// agent, and this ordering is the reason it stays that way.
    #[test]
    fn automation_shapes_still_beat_the_agent_split() {
        let cases = [
            ("automation:dispatcher", "automation:dispatcher"),
            ("rule:bill-approve", "automation:rule:bill-approve"),
            ("system:dispatcher", "automation:dispatcher"),
            ("system", "automation:platform"),
            ("brewery-sim", "automation:brewery-sim"),
        ];
        for (id, want) in cases {
            let a = user(id).ambient_actor().unwrap();
            assert_eq!(a.to_string(), want, "for x-boss-user id {id}");
            assert!(!a.is_agent(), "{id} is an automation, not an agent");
        }
    }

    #[test]
    fn a_plain_employee_id_is_still_human_and_anonymous_is_still_none() {
        assert_eq!(
            user("emp-032").ambient_actor(),
            Some(ActorId::human("emp-032"))
        );
        assert_eq!(user("anonymous").ambient_actor(), None);
    }
}
