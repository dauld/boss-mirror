//! The platform's StepType registry names no tenant's roles (backlog
//! 18d6a6c9, design e2580840 car 5, 2026-09-17) — part (b), the
//! step-types half.
//!
//! MEASURED before this test existed. `required_roles` in
//! crates/core/boss-jobs/seeds/step_types.toml is read by exactly two
//! things: the dispatcher's NOMINATION fallback (a ready step whose
//! Workflow row declares no `authority_role` is assigned to the first
//! listed role with an active holder, and a list with NO holder NAKs
//! until it dead-letters) and `StepType::is_marker` (empty roles AND
//! zero duration). Nothing consults it at the claim — the claim CAS
//! reads the step's own `authority_role`, and sign-offs read
//! `step-signoff:<role>` policy. 39 of 48 kinds listed roles, 33
//! distinct; 31 were the brewery's org chart, two (`production-lead`,
//! `production-tech`) existed in no registry at all, and only
//! `platform-admin` is the platform's. Of 249 brewery workflow steps
//! four human steps relied on the fallback (place-order,
//! inbound-shipment, receive-delivery, service-parts); the device shop
//! relied on it for none; the platform's 51 workflows for one
//! (`workflow-design`'s publish, whose role is platform-admin).
//!
//! So on a tenant that declares none of those roles, a platform step
//! kind carried an org chart the tenant never wrote — and a step of
//! that kind with no `authority_role` would dead-letter looking for a
//! head-brewer. Now the registry lists only roles the platform itself
//! seeds as system roles (`is_system_role` in
//! infra/postgres/schema/01-registries.sql); a tenant declares who
//! does its work on the Workflow row (`authority_role`), where the
//! brewery's four steps now say so.

use boss_testing::repo_root;
use std::collections::BTreeSet;

const STEP_TYPES: &str = "crates/core/boss-jobs/seeds/step_types.toml";
const REGISTRIES: &str = "infra/postgres/schema/01-registries.sql";

/// The role codes the platform seeds with `is_system_role: true` —
/// read from the schema's own INSERT rows, never a typed list.
fn platform_system_roles() -> BTreeSet<String> {
    let sql = std::fs::read_to_string(repo_root().join(REGISTRIES)).unwrap();
    sql.lines()
        .filter(|l| {
            l.contains("'employee'")
                && l.contains("'role'")
                && l.contains("\"is_system_role\": true")
        })
        .filter_map(|l| {
            let rest = l.split("'employee',").nth(1)?;
            let code = rest.trim_start().strip_prefix('\'')?;
            Some(code.split('\'').next()?.to_string())
        })
        .collect()
}

fn required_roles_by_kind() -> Vec<(String, Vec<String>)> {
    let text = std::fs::read_to_string(repo_root().join(STEP_TYPES)).unwrap();
    let doc: toml::Value = toml::from_str(&text).unwrap();
    doc["step_type"]
        .as_array()
        .unwrap()
        .iter()
        .map(|st| {
            let kind = st["kind"].as_str().unwrap().to_string();
            let roles = st
                .get("required_roles")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|r| r.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            (kind, roles)
        })
        .collect()
}

#[test]
fn every_required_role_is_a_role_the_platform_itself_seeds() {
    let system = platform_system_roles();
    assert!(
        system.contains("platform-admin"),
        "the schema read found the system roles: {system:?}"
    );
    let offenders: Vec<String> = required_roles_by_kind()
        .into_iter()
        .flat_map(|(kind, roles)| {
            roles
                .into_iter()
                .filter(|r| !system.contains(r))
                .map(move |r| format!("{kind}: {r}"))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "{STEP_TYPES} names roles the platform does not seed — a tenant's org chart in \
         the platform's registry. A tenant says who does its work on the Workflow row \
         (authority_role); the registry's list is only a nomination fallback and it must \
         not assume a roster. Offenders:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn the_brewerys_steps_that_relied_on_the_fallback_now_declare_their_role() {
    // The four human steps measured above. Without a declared role the
    // dispatcher leaves them unassigned and the sim's workforce — which
    // executes only ASSIGNED steps — never completes them.
    let text =
        std::fs::read_to_string(repo_root().join("examples/brewery/seeds/workflows.toml")).unwrap();
    let doc: toml::Value = toml::from_str(&text).unwrap();
    let mut missing = Vec::new();
    for wf in doc["workflow"].as_array().unwrap() {
        for step in wf
            .get("step")
            .and_then(|s| s.as_array())
            .into_iter()
            .flatten()
        {
            let title = step["title"].as_str().unwrap_or("");
            if [
                "place-order",
                "inbound-shipment",
                "receive-delivery",
                "service-parts",
            ]
            .contains(&title)
                && step
                    .get("authority_role")
                    .and_then(|v| v.as_str())
                    .is_none()
            {
                missing.push(format!("{}/{title}", wf["kind"].as_str().unwrap_or("?")));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "brewery steps with no authority_role: {missing:?}"
    );
}
