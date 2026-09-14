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
        "read_node_roles forge; echo \"roles=<$BOSS_NODE_ROLES> source=$BOSS_NODE_ROLES_SOURCE\"",
        &[
            ("BOSS_NODE_ROLES", "cluster-operator"),
            ("BOSS_ESTATE_NODES_URL", "http://127.0.0.1:9/never"),
        ],
    );
    assert_eq!(rc, 0);
    assert!(
        out.contains("roles=<cluster-operator> source=preset"),
        "a caller's preset list is not overwritten by a read: {out}"
    );

    // A dark registry with NO cache to fall back on: the read installs
    // [always] only — a sentinel no roles.toml section matches — never
    // every row. Widening what a host runs on a failed read was the
    // hazard 6cd124c4 filed the day boss-gcp stopped declaring the
    // legacy stack.
    let cache = boss_testing::scratch_dir("node-roles-nocache").join("roles.cache");
    let _ = std::fs::remove_file(&cache);
    let (rc, out) = sh(
        "read_node_roles forge; echo \"roles=<$BOSS_NODE_ROLES> source=$BOSS_NODE_ROLES_SOURCE\"",
        &[
            ("BOSS_ESTATE_NODES_URL", "http://127.0.0.1:9/never"),
            ("BOSS_NODE_ROLES_CACHE", cache.to_str().unwrap()),
        ],
    );
    assert_eq!(rc, 0, "an unreachable registry is not a failed converge");
    assert!(
        out.contains("did not answer") && out.contains("no cached declaration"),
        "the read says why and what it did: {out}"
    );
    assert!(
        out.contains("roles=<registry-unread> source=none"),
        "a sentinel no role section matches, so only [always] installs — and the source says no read happened, \
         so a verb with a stricter policy (the retire verb refuses unless the registry answered) is not fooled \
         by a non-empty list: {out}"
    );
}

/// A successful read is REMEMBERED beside the checkout, and a dark
/// registry then installs the last declaration it has evidence for —
/// stamped as cached — rather than every row or nothing.
#[test]
fn a_dark_registry_installs_the_last_declaration_it_read() {
    let dir = boss_testing::scratch_dir("node-roles-cache");
    let cache = dir.join("roles.cache");
    let _ = std::fs::remove_file(&cache);
    // A stub registry: one node with two roles.
    let nodes = dir.join("nodes.json");
    std::fs::write(
        &nodes,
        r#"{"data":[{"id":"boss-gcp","roles":["ml-batch-host","off-cluster-observer"]}]}"#,
    )
    .unwrap();
    let url = format!("file://{}", nodes.display());
    let (rc, out) = sh(
        "read_node_roles boss-gcp; echo \"roles=<$BOSS_NODE_ROLES> source=$BOSS_NODE_ROLES_SOURCE\"",
        &[
            ("BOSS_ESTATE_NODES_URL", &url),
            ("BOSS_NODE_ROLES_CACHE", cache.to_str().unwrap()),
        ],
    );
    assert_eq!(rc, 0);
    assert!(
        out.contains("roles=<ml-batch-host,off-cluster-observer> source=registry"),
        "{out}"
    );
    assert_eq!(
        std::fs::read_to_string(&cache).unwrap().trim(),
        "ml-batch-host,off-cluster-observer",
        "the read is remembered"
    );
    // Now the registry is dark: the cache answers, and the log says so.
    let (rc, out) = sh(
        "read_node_roles boss-gcp; echo \"roles=<$BOSS_NODE_ROLES> source=$BOSS_NODE_ROLES_SOURCE\"",
        &[
            ("BOSS_ESTATE_NODES_URL", "http://127.0.0.1:9/never"),
            ("BOSS_NODE_ROLES_CACHE", cache.to_str().unwrap()),
        ],
    );
    assert_eq!(rc, 0);
    assert!(
        out.contains("roles=<ml-batch-host,off-cluster-observer> source=cache"),
        "a caller with its own policy can see the roles did NOT come from a live read: {out}"
    );
    assert!(
        out.contains("did not answer") && out.contains("cached declaration"),
        "the fallback is named as a fallback: {out}"
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
