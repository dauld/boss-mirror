//! The credential broker is a door, so it is in the Doors list
//! (backlog 3c779ca8, 2026-09-20).
//!
//! Designing a scoped cluster credential, a session specified that
//! David would mint the token and place it at /etc/boss-ops/kubeconfig
//! by hand — the exact ceremony `boss-credential-broker` was built to
//! retire after a hand-placement walkthrough exposed the forge write
//! token in a transcript on 2026-09-02. It reasoned from
//! infra/forge/install.sh's "placed once by David", true of root
//! material and false of anything admin can mint, and nothing in
//! CLAUDE.md §Doors — the one list every session reads — corrected
//! it. David did.
//!
//! Pinned by reading both sides: the entry must exist, carry the line
//! that matters (derivability, not sensitivity), and name a verb, a
//! target and a manifest that are real in the tree — a door that
//! stops being true is a defect worth a car.

use boss_testing::repo_root;

fn read(path: &str) -> String {
    std::fs::read_to_string(repo_root().join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Collapse every run of whitespace to one space, so a phrase the
/// prose wraps across a line still matches — reflowing a paragraph is
/// not a change to what it says.
fn flat(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The Doors entry that names the pull verb — found by the verb, so
/// the test does not depend on the entry's heading wording. Returned
/// flattened (see `flat`).
fn the_credential_door() -> String {
    let doc = read("CLAUDE.md");
    let doors = doc
        .split("## Doors")
        .nth(1)
        .and_then(|rest| rest.split("**And the rule behind all of them").next())
        .expect("CLAUDE.md no longer carries a §Doors section");
    doors
        .split("\n\n- **")
        .map(flat)
        .find(|entry| entry.contains("`boss credential pull forge`"))
        .unwrap_or_else(|| {
            panic!(
                "CLAUDE.md §Doors has no entry naming `boss credential pull forge`. A \
                 credential is exactly the thing whose hand-path is expensive; without \
                 the entry a session designed a hand-placement (backlog 3c779ca8)"
            )
        })
}

#[test]
fn the_doors_list_names_the_broker_and_its_pull_verb() {
    let door = the_credential_door();
    for needle in [
        "infra/cluster/manifests/boss-credential-broker.yaml",
        "rotate-a-credential",
        "never a value",
    ] {
        assert!(
            door.contains(needle),
            "the credential door must name {needle:?}. The entry as written:\n{door}"
        );
    }
}

#[test]
fn the_door_draws_the_boundary_at_derivability() {
    let door = the_credential_door();
    // Root material genuinely is a human act; a credential admin can
    // mint never is. Both halves, or the over-generalisation that
    // opened 3c779ca8 survives in the other direction.
    for needle in ["derivability, not sensitivity", "talosconfig", "MINT"] {
        assert!(
            door.contains(needle),
            "the credential door must state the boundary ({needle:?} missing). The \
             entry as written:\n{door}"
        );
    }
}

#[test]
fn the_door_cites_what_its_sources_say() {
    let door = the_credential_door();
    // The incident is quoted from the broker's own manifest; if the
    // manifest stops saying it, the door is quoting nothing.
    let incident = "the ceremony itself was the vulnerability";
    assert!(
        door.contains(incident),
        "the door must cite the 2026-09-02 incident"
    );
    assert!(
        flat(&read("infra/cluster/manifests/boss-credential-broker.yaml")).contains(incident),
        "the broker manifest no longer carries the sentence the door quotes"
    );
    assert!(
        repo_root()
            .join("infra/platform/workflows/rotate-a-credential.toml")
            .is_file(),
        "the door names the rotate-a-credential protocol, whose in-tree source is gone"
    );
    // The target the door names is the one the verb knows.
    let verb = read("crates/orchestrators/boss-cli/src/credential.rs");
    assert!(
        verb.contains("pub async fn pull(") && verb.contains("if target != \"forge\""),
        "`boss credential pull` no longer takes `forge` as its target; the door names it"
    );
}
