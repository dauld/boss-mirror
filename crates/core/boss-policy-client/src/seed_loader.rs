//! TOML loader for tenant `policy_rules.toml` files.
//!
//! Tenants seed their role-grant matrix declaratively at first
//! boot — see `examples/<tenant>/seeds/policy_rules.toml`. Core
//! ships only platform rules in [`default_rules`](crate::default_rules);
//! every business-shaped role (sales-rep, brewer, controller, …)
//! lives in tenant seed data.
//!
//! ## Schema
//!
//! The TOML carries a list of `[[grants]]` tables. Each grant
//! expands to one [`PolicyRule`] per `(role × resource × action)`
//! combination, so a single grant can replace dozens of
//! hand-written rows:
//!
//! ```toml
//! # Single-rule form.
//! [[grants]]
//! role = "ceo"
//! resource = "policy-rule"
//! action = "update"
//! scope = "all"
//!
//! # Fan-out form: 4 roles × 13 resources × 1 action = 52 rules.
//! [[grants]]
//! roles = ["ceo", "coo", "cfo", "cto"]
//! resources = ["job", "step", "account", "employee", "invoice",
//!              "agreement", "asset", "shipment", "part",
//!              "purchase-order", "policy-rule", "workflow",
//!              "step-plugin"]
//! action = "read"
//! scope = "all"
//!
//! # Department scope via the DB-string convention.
//! [[grants]]
//! role = "service-mgr"
//! resources = ["job", "step"]
//! actions = ["read", "update", "close"]
//! scope = "department:service"
//! ```
//!
//! `scope` accepts the same strings [`Scope::to_db_string`] emits:
//! `none` / `self` / `territory` / `team` / `all` /
//! `department:<name>`.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

use crate::types::{Action, PolicyRule, Resource, Scope};

#[derive(Debug, Deserialize)]
struct PolicySeed {
    #[serde(default)]
    grants: Vec<GrantSpec>,
}

#[derive(Debug, Deserialize)]
struct GrantSpec {
    /// Single-role shorthand. Mutually exclusive with `roles`.
    #[serde(default)]
    role: Option<String>,
    /// Multi-role fan-out. Mutually exclusive with `role`.
    #[serde(default)]
    roles: Option<Vec<String>>,

    /// Single-resource shorthand. Mutually exclusive with `resources`.
    #[serde(default)]
    resource: Option<String>,
    /// Multi-resource fan-out.
    #[serde(default)]
    resources: Option<Vec<String>>,

    /// Single-action shorthand.
    #[serde(default)]
    action: Option<String>,
    /// Multi-action fan-out.
    #[serde(default)]
    actions: Option<Vec<String>>,

    /// Scope in DB-string form (`all`, `self`, `team`, `territory`,
    /// `none`, `department:<name>`).
    scope: String,
}

impl GrantSpec {
    fn expand(&self) -> Result<Vec<PolicyRule>> {
        let roles = match (&self.role, &self.roles) {
            (Some(_), Some(_)) => return Err(anyhow!("grant cannot set both `role` and `roles`")),
            (Some(r), None) => vec![r.clone()],
            (None, Some(rs)) if rs.is_empty() => {
                return Err(anyhow!("grant `roles` cannot be empty"));
            }
            (None, Some(rs)) => rs.clone(),
            (None, None) => return Err(anyhow!("grant must set `role` or `roles`")),
        };
        let resources = match (&self.resource, &self.resources) {
            (Some(_), Some(_)) => {
                return Err(anyhow!("grant cannot set both `resource` and `resources`"));
            }
            (Some(r), None) => vec![r.clone()],
            (None, Some(rs)) if rs.is_empty() => {
                return Err(anyhow!("grant `resources` cannot be empty"));
            }
            (None, Some(rs)) => rs.clone(),
            (None, None) => return Err(anyhow!("grant must set `resource` or `resources`")),
        };
        let actions = match (&self.action, &self.actions) {
            (Some(_), Some(_)) => {
                return Err(anyhow!("grant cannot set both `action` and `actions`"));
            }
            (Some(a), None) => vec![a.clone()],
            (None, Some(as_)) if as_.is_empty() => {
                return Err(anyhow!("grant `actions` cannot be empty"));
            }
            (None, Some(as_)) => as_.clone(),
            (None, None) => return Err(anyhow!("grant must set `action` or `actions`")),
        };
        let scope = Scope::from_db_string(&self.scope)
            .map_err(|e| anyhow!("invalid scope `{}`: {}", self.scope, e))?;

        let mut out = Vec::with_capacity(roles.len() * resources.len() * actions.len());
        for role in &roles {
            for resource in &resources {
                for action in &actions {
                    let action: Action = action
                        .parse()
                        .map_err(|e| anyhow!("invalid action `{action}`: {e}"))?;
                    let resource = Resource::new(resource.as_str());
                    out.push(PolicyRule::new(role, resource, action, scope.clone()));
                }
            }
        }
        Ok(out)
    }
}

/// Parse `policy_rules.toml` at `path` and expand every grant
/// into its `(role × resource × action)` Cartesian product.
pub fn load_policy_rules(path: &Path) -> Result<Vec<PolicyRule>> {
    let txt =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let seed: PolicySeed = toml::from_str(&txt)
        .with_context(|| format!("parsing {} as policy_rules.toml", path.display()))?;
    let mut rules = Vec::new();
    for (idx, grant) in seed.grants.iter().enumerate() {
        let expanded = grant
            .expand()
            .with_context(|| format!("grant #{} in {}", idx + 1, path.display()))?;
        rules.extend(expanded);
    }
    Ok(rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Vec<PolicyRule>> {
        let seed: PolicySeed = toml::from_str(s)?;
        seed.grants.iter().try_fold(Vec::new(), |mut acc, g| {
            acc.extend(g.expand()?);
            Ok(acc)
        })
    }

    #[test]
    fn single_rule_form() {
        let rules = parse(
            r#"
[[grants]]
role = "ceo"
resource = "policy-rule"
action = "update"
scope = "all"
"#,
        )
        .unwrap();
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].role, "ceo");
        assert_eq!(rules[0].resource.as_str(), "policy-rule");
        assert_eq!(rules[0].action, Action::Update);
        assert_eq!(rules[0].scope, Scope::All);
    }

    #[test]
    fn fan_out_roles_resources_actions() {
        let rules = parse(
            r#"
[[grants]]
roles = ["ceo", "coo"]
resources = ["job", "step"]
actions = ["read", "update"]
scope = "all"
"#,
        )
        .unwrap();
        assert_eq!(rules.len(), 2 * 2 * 2);
    }

    #[test]
    fn department_scope_parses() {
        let rules = parse(
            r#"
[[grants]]
role = "service-mgr"
resource = "job"
action = "read"
scope = "department:service"
"#,
        )
        .unwrap();
        assert_eq!(rules[0].scope, Scope::Department("service".into()));
    }

    #[test]
    fn rejects_mixing_role_and_roles() {
        let err = parse(
            r#"
[[grants]]
role = "ceo"
roles = ["coo"]
resource = "job"
action = "read"
scope = "all"
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("both"), "got: {err}");
    }

    #[test]
    fn rejects_invalid_scope() {
        let err = parse(
            r#"
[[grants]]
role = "ceo"
resource = "job"
action = "read"
scope = "bogus"
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("scope"), "got: {err}");
    }

    #[test]
    fn rejects_invalid_action() {
        let err = parse(
            r#"
[[grants]]
role = "ceo"
resource = "job"
action = "explode"
scope = "all"
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("action"), "got: {err}");
    }

    #[test]
    fn rejects_missing_role() {
        let err = parse(
            r#"
[[grants]]
resource = "job"
action = "read"
scope = "all"
"#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("role"), "got: {err}");
    }
}

#[cfg(test)]
mod tenant_seed_tests {
    use super::*;
    use std::path::PathBuf;

    fn repo_root() -> PathBuf {
        // CARGO_MANIFEST_DIR is crates/core/boss-policy; up 3.
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("..")
    }

    #[test]
    fn brewery_seed_parses() {
        let path = repo_root().join("examples/brewery/seeds/policy_rules.toml");
        let rules =
            load_policy_rules(&path).unwrap_or_else(|e| panic!("brewery policy_rules.toml: {e:#}"));
        assert!(
            rules.len() > 100,
            "expected >100 rules, got {}",
            rules.len()
        );
        // Every brewery role from employees.json should have at
        // least one grant. (Spot-check the load-bearing ones.)
        let roles: std::collections::HashSet<&str> =
            rules.iter().map(|r| r.role.as_str()).collect();
        for must in [
            "ceo",
            "coo",
            "cfo",
            "cto",
            "head-brewer",
            "brewer",
            "cellar-tech",
            "qa-supervisor",
            "lab-tech",
            "packaging-mgr",
            "warehouse-mgr",
            "head-of-sales",
            "sales-rep",
            "controller",
            "head-of-people",
            "hr-generalist",
        ] {
            assert!(roles.contains(must), "brewery seed missing role `{must}`");
        }
        // Every rule's id should be unique.
        let mut ids = std::collections::HashSet::new();
        for r in &rules {
            assert!(ids.insert(r.id.clone()), "duplicate id `{}`", r.id);
        }
    }
}
