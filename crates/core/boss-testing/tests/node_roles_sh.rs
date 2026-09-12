//! infra/estate/node-roles.sh — the ONE definition of how a managed host
//! reads its roles off the system of record, sourced by boss-gcp's and
//! the forge's converge (design 1bc4b4ed; the second host is why it is
//! a file and not a second copy, CLAUDE.md §9a).
//!
//! Pinned here: `has_role` matches a whole role and never a substring
//! (`operator` must not match `cluster-operator`, or the credential
//! check would run on a host that never declared the role); a caller's
//! preset BOSS_NODE_ROLES wins over the registry read; and a registry
//! that does not answer leaves the roles EMPTY and says so, rather than
//! failing the converge — an arm that needs the patient is not an arm.
//! The two converges and install.sh are pinned to source the file, so
//! the definition cannot quietly become inline again.

use boss_testing::repo_root;
use std::process::Command;

fn sh(script: &str, env: &[(&str, &str)]) -> (i32, String) {
    let lib = repo_root().join("infra/estate/node-roles.sh");
    let mut cmd = Command::new("bash");
    cmd.arg("-c")
        .arg(format!(". '{}'\n{script}", lib.display()))
        .env_remove("BOSS_NODE_ROLES");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("bash runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn has_role_matches_a_whole_role_and_never_a_substring() {
    let (rc, _) = sh(
        "has_role cluster-operator",
        &[("BOSS_NODE_ROLES", "off-cluster-observer,cluster-operator")],
    );
    assert_eq!(rc, 0, "a declared role is found");
    let (rc, _) = sh(
        "has_role operator",
        &[("BOSS_NODE_ROLES", "off-cluster-observer,cluster-operator")],
    );
    assert_ne!(rc, 0, "`operator` is not a role this host declares");
    let (rc, _) = sh("has_role cluster-operator", &[]);
    assert_ne!(rc, 0, "no roles declared, nothing matches");
}

#[test]
fn a_preset_roles_list_wins_and_an_unreachable_registry_leaves_it_empty() {
    let (rc, out) = sh(
        "read_node_roles forge; echo \"roles=<$BOSS_NODE_ROLES>\"",
        &[
            ("BOSS_NODE_ROLES", "cluster-operator"),
            ("BOSS_ESTATE_NODES_URL", "http://127.0.0.1:9/never"),
        ],
    );
    assert_eq!(rc, 0);
    assert!(
        out.contains("roles=<cluster-operator>"),
        "a caller's preset list is not overwritten by a read: {out}"
    );

    let (rc, out) = sh(
        "read_node_roles forge; echo \"roles=<$BOSS_NODE_ROLES>\"",
        &[("BOSS_ESTATE_NODES_URL", "http://127.0.0.1:9/never")],
    );
    assert_eq!(rc, 0, "an unreachable registry is not a failed converge");
    assert!(
        out.contains("did not answer"),
        "the read says why the roles are empty: {out}"
    );
    assert!(
        out.contains("roles=<>"),
        "empty, so every row installs as before roles existed: {out}"
    );
}

#[test]
fn every_converge_and_the_installer_source_the_one_definition() {
    for f in [
        "infra/gcp/boss-gcp-converge.sh",
        "infra/forge/forge-converge.sh",
        "infra/forge/install.sh",
    ] {
        let text = std::fs::read_to_string(repo_root().join(f)).expect(f);
        assert!(
            text.contains("estate/node-roles.sh"),
            "{f} must source infra/estate/node-roles.sh, not carry its own role read"
        );
    }
    let unit = std::fs::read_to_string(repo_root().join("infra/forge/forge-converge.service"))
        .expect("forge-converge.service");
    assert!(
        unit.contains("Environment=BOSS_NODE_ID=forge"),
        "the forge's node id is declared on its unit, never guessed from a hostname"
    );
}
