//! Tenant policy-rule seeding, shared by the
//! `boss-policy-bootstrap` binary and tenant `prepare` steps (the
//! brewery's converged prepare calls this directly so the bin and
//! the live demo seed policy through one impl).
//!
//! [`publish_policy_rules`] POSTs each rule in a tenant's
//! `policy_rules.toml` to `/api/policy/rules`. Core ships only
//! platform rules (`platform-admin` / `audit-readonly` /
//! `smoke-tester` / `guest`) — see [`crate::default_rules`]. Tenant
//! role grants (ceo / coo / sales-rep / brewer / controller / …)
//! live in tenant seed data and arrive via this fn.
//!
//! Idempotent: each rule is GETd from `/api/policy/rules/{id}`
//! first; existing rows (operator-tuned or seeded by a prior run)
//! are left alone unless `force` — and a kept row whose `scope` or
//! `active` differs from the file's is NAMED in the outcome (design
//! e187198f, 2026-09-18: the instance is the truth, and a repo edit
//! that does not land is named, never silent; until then an edited
//! rule under the same id was skipped without a word). Under `force`
//! only the rows that differ are re-POSTed, each change named.
//! Hard-fails on any non-2xx response, so a partial failure resumes
//! cleanly on re-run.

use std::path::Path;

use anyhow::{Context, Result};
use boss_core::publish::{FieldChange, KeptRow, UpdatedRow};
use boss_policy_client::PolicyRule;
use reqwest::blocking::Client;
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::seed_loader::load_policy_rules;

fn policy_url(api_base: &str, path: &str) -> String {
    format!("{}{}", api_base.trim_end_matches('/'), path)
}

/// What a policy publish did: rules POSTed new, rules kept as the
/// instance holds them with the differing fields named, rules a
/// `force` overwrote with each change named, and rules already as
/// declared.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolicyPublishOutcome {
    pub posted: usize,
    pub kept: Vec<KeptRow>,
    pub updated: Vec<UpdatedRow>,
    pub unchanged: usize,
}

impl PolicyPublishOutcome {
    /// One line for a publish report: counts, then each overwritten
    /// rule field by field, then each kept-but-differing rule with the
    /// flag that would overwrite it.
    pub fn summary(&self) -> String {
        let mut s = format!("{} posted", self.posted);
        if !self.updated.is_empty() {
            s.push_str(", ");
            s.push_str(&boss_core::publish::render_updated(&self.updated));
        }
        if self.unchanged > 0 {
            s.push_str(&format!(", {} already as declared", self.unchanged));
        }
        let kept = boss_core::publish::render_kept(&self.kept, Some("policy"));
        if !kept.is_empty() {
            s.push_str("; ");
            s.push_str(&kept);
        }
        s
    }
}

/// The fields the live rule (as `GET /api/policy/rules/{id}` answers
/// it) disagrees with the file's rule on. The id is
/// `role:resource:action`, so those three cannot differ under one id;
/// `scope` and `active` can. Compared as JSON so a scope carrying a
/// department compares whole.
pub fn rule_changes(live: &Value, declared: &PolicyRule) -> Vec<FieldChange> {
    let want = serde_json::to_value(declared).unwrap_or(Value::Null);
    ["scope", "active"]
        .into_iter()
        .filter_map(|f| {
            let have = live.get(f).cloned().unwrap_or(Value::Null);
            let to = want.get(f).cloned().unwrap_or(Value::Null);
            (have != to).then(|| FieldChange {
                field: f.to_string(),
                from: have,
                to,
            })
        })
        .collect()
}

/// POST each rule in `seeds` (a tenant's `policy_rules.toml`) to
/// `/api/policy/rules` on `api_base` (a policy-api or gateway base
/// URL), idempotently.
///
/// `force` overwrites existing rules that differ instead of keeping
/// them (the `--take policy` of `boss tenant publish`). `x_boss_user`
/// overrides the default platform-bootstrap header when `Some`. That
/// header is both the authority the service checks each write against
/// and the `changed_by` it records: the service attributes a write to
/// the caller it authorized, never to a name in the body (backlog
/// 42c25542 — this took a `changed_by` argument until then, and the
/// body field it filled was how any caller signed any name). Hard-fails
/// on any non-2xx response.
pub fn publish_policy_rules(
    api_base: &str,
    seeds: &Path,
    force: bool,
    x_boss_user: Option<&str>,
) -> Result<PolicyPublishOutcome> {
    let user_header = x_boss_user.map(|s| s.to_string()).unwrap_or_else(|| {
        json!({
            "id": "automation:bootstrap",
            "role": "platform-admin",
            "access_tier": "operator",
            "territory_account_ids": [],
            "direct_report_ids": [],
            "department": "platform",
        })
        .to_string()
    });
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-boss-user",
        reqwest::header::HeaderValue::from_str(&user_header).context("x-boss-user header value")?,
    );
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let rules = load_policy_rules(seeds).with_context(|| format!("loading {}", seeds.display()))?;

    info!(
        seeds = %seeds.display(),
        api_base = %api_base,
        rule_count = rules.len(),
        force = force,
        "starting policy bootstrap"
    );

    let mut out = PolicyPublishOutcome::default();
    for rule in &rules {
        let get_url = policy_url(api_base, &format!("/api/policy/rules/{}", rule.id));
        let resp = client.get(&get_url).headers(headers.clone()).send()?;
        let live: Option<Value> = match resp.status().as_u16() {
            // The live row, compared below. A body that does not parse
            // compares as differing on both fields and is NAMED — a
            // door that cannot show its row is a finding, not a pass.
            200 => Some(resp.json().unwrap_or(Value::Null)),
            404 => None,
            other => {
                anyhow::bail!(
                    "GET {get_url} → {other} {}",
                    resp.text().unwrap_or_default()
                );
            }
        };
        let changes = live.as_ref().map(|l| rule_changes(l, rule));
        match (&changes, force) {
            (Some(c), _) if c.is_empty() => {
                out.unchanged += 1;
                continue;
            }
            (Some(c), false) => {
                out.kept.push(KeptRow {
                    id: rule.id.clone(),
                    differs: c.iter().map(|ch| ch.field.clone()).collect(),
                });
                continue;
            }
            _ => {}
        }
        let post_url = policy_url(api_base, "/api/policy/rules");
        let body = json!({ "rule": rule });
        let resp = client
            .post(&post_url)
            .headers(headers.clone())
            .json(&body)
            .send()?;
        if !resp.status().is_success() {
            anyhow::bail!(
                "POST {post_url} ({}) → {} {}",
                rule.id,
                resp.status(),
                resp.text().unwrap_or_default()
            );
        }
        match changes {
            Some(c) => out.updated.push(UpdatedRow {
                id: rule.id.clone(),
                changes: c,
            }),
            None => out.posted += 1,
        }
        if (out.posted + out.updated.len()).is_multiple_of(25) {
            info!(
                posted = out.posted,
                updated = out.updated.len(),
                total = rules.len(),
                "progress"
            );
        }
    }

    if out.posted == 0 && out.updated.is_empty() && !out.kept.is_empty() {
        warn!(
            kept = out.kept.len(),
            total = rules.len(),
            "every rule already exists and some differ from the file; the instance is the truth — force to overwrite"
        );
    }

    info!(
        posted = out.posted,
        updated = out.updated.len(),
        kept = out.kept.len(),
        unchanged = out.unchanged,
        total = rules.len(),
        "policy bootstrap complete"
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_policy_client::{Action, Resource, Scope};

    /// A kept rule names `scope` and `active` when they differ and
    /// nothing else — the id fixes role, resource and action.
    #[test]
    fn a_live_rule_differs_on_scope_or_active_only() {
        let declared = PolicyRule::new("founder", Resource::new("job"), Action::Read, Scope::All);
        let same = serde_json::to_value(&declared).unwrap();
        assert!(rule_changes(&same, &declared).is_empty());

        let mut edited = same.clone();
        edited["scope"] = json!("team");
        edited["active"] = json!(false);
        let changes = rule_changes(&edited, &declared);
        let fields: Vec<&str> = changes.iter().map(|c| c.field.as_str()).collect();
        assert_eq!(fields, ["scope", "active"]);
        assert_eq!(changes[0].render(), "scope team → all");
        assert_eq!(changes[1].render(), "active false → true");
        // An older door answering `{}` compares as differing on both,
        // and names them — never a silent skip.
        assert_eq!(rule_changes(&json!({}), &declared).len(), 2);

        let out = PolicyPublishOutcome {
            posted: 3,
            kept: vec![KeptRow {
                id: declared.id.clone(),
                differs: vec!["scope".into()],
            }],
            updated: vec![],
            unchanged: 1,
        };
        assert_eq!(
            out.summary(),
            "3 posted, 1 already as declared; kept: founder:job:read differs on scope (the \
             instance is the truth; --take policy overwrites)"
        );
    }
}
