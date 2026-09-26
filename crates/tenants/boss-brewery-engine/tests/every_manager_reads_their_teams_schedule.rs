//! Every brewery manager reads their team's schedule, and a `team`
//! schedule grant goes to managers only (backlog a621d091).
//!
//! The scheduling reads ask Read on `schedule` in a scope that covers
//! the employee (`boss-jobs::scheduling::access`); an employee reads
//! their own with no grant. So who else reads a schedule is exactly
//! the `schedule` grants in `policy_rules.toml`, and a `team` grant
//! means the holder and their DIRECT REPORTS — the employees whose
//! `manager_id` names them in `employees.json`.
//!
//! The first version of these grants was typed from role names: it
//! granted `payroll-mgr`, who manages nobody, and missed the seven
//! lead roles and the controller who do (adversarial review,
//! 2026-09-25, SF4). A lead reading their own crew's week is the one
//! schedule read the brewery runs on. This pins the two seed files to
//! each other, naming the role on either side that drifts.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn brewery_seeds_dir() -> PathBuf {
    boss_testing::repo_root().join("examples/brewery/seeds")
}

fn read(name: &str) -> String {
    let path = brewery_seeds_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The role of every employee some other employee reports to.
fn manager_roles() -> BTreeSet<String> {
    let employees: Vec<serde_json::Value> =
        serde_json::from_str(&read("employees.json")).expect("parsing employees.json");
    let role_of = |id: &str| {
        employees
            .iter()
            .find(|e| e["id"].as_str() == Some(id))
            .and_then(|e| e["role"].as_str())
            .map(str::to_string)
            .unwrap_or_else(|| panic!("manager_id {id} names no employee in employees.json"))
    };
    employees
        .iter()
        .filter_map(|e| e["manager_id"].as_str())
        .map(role_of)
        .collect()
}

/// The roles holding Read on `schedule` at `scope`.
fn schedule_grants(scope: &str) -> BTreeSet<String> {
    let policy: toml::Value = toml::from_str(&read("policy_rules.toml")).expect("parsing policy");
    policy
        .get("grants")
        .and_then(|g| g.as_array())
        .into_iter()
        .flatten()
        .filter(|g| g.get("resource").and_then(|r| r.as_str()) == Some("schedule"))
        .filter(|g| g.get("action").and_then(|a| a.as_str()) == Some("read"))
        .filter(|g| g.get("scope").and_then(|s| s.as_str()) == Some(scope))
        .flat_map(|g| {
            g.get("roles")
                .and_then(|r| r.as_array())
                .into_iter()
                .flatten()
                .chain(g.get("role"))
                .filter_map(|r| r.as_str())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn every_manager_role_reads_their_teams_schedule() {
    let managers = manager_roles();
    assert!(
        managers.contains("ceo"),
        "no manager roles read from employees.json — the walk is reading the wrong shape"
    );
    let all = schedule_grants("all");
    let team = schedule_grants("team");
    let missing: Vec<&String> = managers
        .iter()
        .filter(|r| !all.contains(*r) && !team.contains(*r))
        .collect();
    assert!(
        missing.is_empty(),
        "roles that manage someone in employees.json with no Read grant on \
         `schedule` in policy_rules.toml: {missing:?}"
    );
}

#[test]
fn a_team_schedule_grant_goes_to_a_manager() {
    let managers = manager_roles();
    let stray: Vec<String> = schedule_grants("team")
        .difference(&managers)
        .cloned()
        .collect();
    assert!(
        stray.is_empty(),
        "roles holding a `team` schedule grant that manage nobody in \
         employees.json — their team is themself, which needs no grant: {stray:?}"
    );
}
