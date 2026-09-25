//! Every sign-off a brewery Workflow demands can be given.
//!
//! The sign-off door (`boss-jobs::http::steps::post_step_sign_off`)
//! accepts a role only if it is in the step's `sign_offs_required`,
//! then checks `SignOff` on `step-signoff:<role>` — an exact resource
//! match. A role a Workflow names there with no such grant in
//! `policy_rules.toml` makes the step unsignable by anyone.
//!
//! Until the sim-origin bypass was bound (car ab1dba92) the simulator
//! signed through it, so the missing `step-signoff:ceo` grant behind
//! weekly-leadership-review's ceo-review step never showed: the bypass
//! answered in the grant's place (backlog 292c00c2, 2026-09-25). This
//! pins the two seed files to each other, naming the missing role.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn brewery_seeds_dir() -> PathBuf {
    boss_testing::repo_root().join("examples/brewery/seeds")
}

fn read_toml(name: &str) -> toml::Value {
    let path = brewery_seeds_dir().join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
}

/// Every role named in any `sign_offs_required` array, wherever it
/// sits in the document — walked rather than addressed, so a step
/// nested under a new table shape is still counted.
fn sign_off_roles(value: &toml::Value, out: &mut BTreeSet<String>) {
    match value {
        toml::Value::Table(t) => t.iter().for_each(|(k, v)| {
            if k == "sign_offs_required" {
                v.as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|r| r.as_str())
                    .for_each(|r| {
                        out.insert(r.to_string());
                    });
            } else {
                sign_off_roles(v, out);
            }
        }),
        toml::Value::Array(a) => a.iter().for_each(|v| sign_off_roles(v, out)),
        _ => {}
    }
}

#[test]
fn every_sign_offs_required_role_holds_its_step_signoff_grant() {
    let mut required = BTreeSet::new();
    sign_off_roles(&read_toml("workflows.toml"), &mut required);
    assert!(
        !required.is_empty(),
        "no sign_offs_required found in workflows.toml — the walk is reading the wrong shape"
    );

    let policy = read_toml("policy_rules.toml");
    let granted: BTreeSet<String> = policy
        .get("grants")
        .and_then(|g| g.as_array())
        .into_iter()
        .flatten()
        .filter(|g| g.get("action").and_then(|a| a.as_str()) == Some("sign-off"))
        .filter_map(|g| {
            let role = g.get("role")?.as_str()?;
            let resource = g.get("resource")?.as_str()?;
            (resource == format!("step-signoff:{role}")).then(|| role.to_string())
        })
        .collect();

    let missing: Vec<&String> = required.difference(&granted).collect();
    assert!(
        missing.is_empty(),
        "roles named in sign_offs_required with no `sign-off` grant on \
         step-signoff:<role> for that role in policy_rules.toml: {missing:?}"
    );
}
