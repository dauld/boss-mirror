//! The platform's Workflow bundle names no tenant as owner (backlog
//! 18d6a6c9, design e2580840 car 5, 2026-09-17) — part (c).
//!
//! Measured across infra/platform/workflows/*.toml: 46 rows say
//! `owning_team = "platform"` and three say `"it"` (the IT department
//! the platform seeds), 41 say `metadata.owner_role = "platform-admin"`
//! — and publish-request.toml alone said `owning_team =
//! "brewery-bootstrap"` and `owner_role = "shift-lead"`, a brewery role,
//! because the file was rendered from the live row that a brewery-era
//! operator authored. The bundle loader ignores the file's owning_team
//! (it stamps `platform`), but `owner_role` is read: it is the role the
//! owner-resolution step tries FIRST when a packet of that kind is
//! opened (boss-jobs owner_resolution.rs), so on a tenant with no
//! shift-lead every publish-request fell through to the step's role.
//! And the platform bundle's roles are what car 3's eviction test
//! (718ac982) treats as the platform's own — a tenant role here leaks
//! into what a fresh instance is told to keep.
//!
//! Both values are read from the tree's own definitions, never a typed
//! list: an owning_team is `platform` or a department the schema seeds;
//! an owner_role is a role the schema seeds as a system role.

use boss_testing::repo_root;
use std::collections::BTreeSet;

const WORKFLOWS: &str = "infra/platform/workflows";
const REGISTRIES: &str = "infra/postgres/schema/01-registries.sql";

/// Codes of `(employee, <code>, <member_attribute>)` rows in the
/// schema's seed INSERTs, optionally only those whose metadata carries
/// `"is_system_role": true`.
fn seeded_employee_codes(member_attribute: &str, system_only: bool) -> BTreeSet<String> {
    let sql = std::fs::read_to_string(repo_root().join(REGISTRIES)).unwrap();
    sql.lines()
        .filter(|l| l.contains("'employee'") && l.contains(&format!("'{member_attribute}'")))
        .filter(|l| !system_only || l.contains("\"is_system_role\": true"))
        .filter_map(|l| {
            let rest = l.split("'employee',").nth(1)?;
            let code = rest.trim_start().strip_prefix('\'')?;
            Some(code.split('\'').next()?.to_string())
        })
        .collect()
}

fn platform_workflows() -> Vec<(String, toml::Value)> {
    let dir = repo_root().join(WORKFLOWS);
    let mut out: Vec<(String, toml::Value)> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .flat_map(|p| {
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            let doc: toml::Value = toml::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            doc["workflow"]
                .as_array()
                .unwrap()
                .iter()
                .cloned()
                .map(move |w| (name.clone(), w))
                .collect::<Vec<_>>()
        })
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

#[test]
fn every_owning_team_is_the_platform_or_a_department_the_platform_seeds() {
    let mut allowed = seeded_employee_codes("department", false);
    allowed.insert("platform".into());
    assert!(
        allowed.contains("it"),
        "the schema read found the departments: {allowed:?}"
    );
    let offenders: Vec<String> = platform_workflows()
        .into_iter()
        .filter_map(|(file, w)| {
            let team = w.get("owning_team")?.as_str()?.to_string();
            (!allowed.contains(&team)).then(|| format!("{file}: owning_team = {team}"))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "{WORKFLOWS} rows owned by something the platform does not seed:\n  {}",
        offenders.join("\n  ")
    );
}

#[test]
fn every_owner_role_is_a_role_the_platform_itself_seeds() {
    let system = seeded_employee_codes("role", true);
    assert!(system.contains("platform-admin"), "{system:?}");
    let offenders: Vec<String> = platform_workflows()
        .into_iter()
        .filter_map(|(file, w)| {
            let role = w.get("metadata")?.get("owner_role")?.as_str()?.to_string();
            (!system.contains(&role)).then(|| format!("{file}: owner_role = {role}"))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "{WORKFLOWS} rows whose owner_role is a tenant's role, not the platform's:\n  {}",
        offenders.join("\n  ")
    );
}
