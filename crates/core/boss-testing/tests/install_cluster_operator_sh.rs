//! infra/estate/install-cluster-operator.sh — the ONE definition of what
//! the `cluster-operator` role brings to a host, sourced by the forge's
//! install.sh and by boss-gcp's converge.
//!
//! WHY IT IS A FILE. Until 2026-09-20 this lived inline in
//! infra/forge/install.sh, which was correct while the forge was the
//! estate's only cluster-operator. boss-gcp holding the role too would
//! have meant a second copy of the talosctl version pin — and that pin
//! is exactly the fact that must not drift, because a Talos client more
//! than one minor from the cluster is the failure it exists to prevent.
//! Collapsed rather than pinned in two places, the way node-roles.sh was
//! collapsed for the same reason (CLAUDE.md §9a, design 1bc4b4ed).
//!
//! Pinned here: the version and its checksum are spelled ONCE in the
//! tree; both callers source the file, so the definition cannot quietly
//! become inline again; the credential check REPORTS and never writes,
//! naming what is wrong; and boss-gcp declares the role in the estate
//! registry, which is what makes its converge run any of this.

use boss_testing::repo_root;
use std::process::Command;

/// Source the library and run `script` under bash, with the credential
/// directory pointed somewhere a test may read.
fn sh(script: &str, ops_dir: &str) -> (i32, String) {
    let lib = repo_root().join("infra/estate/install-cluster-operator.sh");
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(". '{}'\n{script}", lib.display()))
        // Never fetch a binary from a test.
        .env("INSTALL_TALOSCTL", "0")
        .env("BOSS_OPS_DIR", ops_dir)
        .output()
        .expect("bash runs");
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap_or(-1), text)
}

#[test]
fn the_talosctl_pin_is_spelled_once_in_the_tree() {
    // The version string, anywhere in a tracked file. The library that
    // owns it is the only legitimate home; a second one is the drift
    // this collapse exists to make impossible.
    let root = repo_root();
    let out = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args([
            "grep",
            "-l",
            "-F",
            "406b56f9e4ff03b1557cc941b1f163aec8a6ebb36e28f0bbbe6d083589529261",
        ])
        .output()
        .expect("git grep runs");
    let files: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        // This test names the checksum in order to look for it.
        .filter(|f| !f.ends_with("install_cluster_operator_sh.rs"))
        .collect();
    assert_eq!(
        files,
        vec!["infra/estate/install-cluster-operator.sh".to_string()],
        "the talosctl checksum must live in exactly one file — found it in {files:?}"
    );
}

#[test]
fn both_callers_source_the_one_definition() {
    let root = repo_root();
    for caller in ["infra/forge/install.sh", "infra/gcp/boss-gcp-converge.sh"] {
        let text = std::fs::read_to_string(root.join(caller))
            .unwrap_or_else(|e| panic!("{caller} is readable: {e}"));
        assert!(
            text.contains("estate/install-cluster-operator.sh"),
            "{caller} must SOURCE the one definition, not carry its own copy"
        );
        assert!(
            text.contains("install_cluster_operator"),
            "{caller} sources the library but never calls it"
        );
    }
}

#[test]
fn absent_credentials_are_named_and_nothing_is_written() {
    // A path that must NOT exist: the per-name root, which nothing here
    // creates.
    let dir = boss_testing::scratch_path("boss-co-absent");
    let (rc, out) = sh("install_cluster_operator", dir.to_str().expect("utf8"));
    assert_eq!(
        rc, 0,
        "a missing credential reports; it does not fail the converge"
    );
    assert!(
        out.contains("talosconfig:absent") && out.contains("kubeconfig:absent"),
        "both missing credentials are named by name, got: {out}"
    );
    assert!(
        !dir.exists(),
        "the role installer must never create the credential directory — credentials are placed by hand, never by a script"
    );
}

#[test]
fn a_credential_with_the_wrong_ownership_is_reported_with_what_it_actually_is() {
    // A test process cannot make a file root:root 0600, which is the
    // point: whatever it CAN make is wrong, and the check must say so
    // rather than pass. A silent pass here would mean a world-readable
    // kubeconfig reported as ready.
    let dir = boss_testing::scratch_dir("boss-co-mode");
    for cred in ["talosconfig", "kubeconfig"] {
        std::fs::write(dir.join(cred), "placeholder").expect("write");
    }
    let (rc, out) = sh("install_cluster_operator", dir.to_str().expect("utf8"));
    std::fs::remove_dir_all(&dir).ok();
    assert_eq!(rc, 0);
    assert!(
        out.contains("not ready"),
        "a credential that is not root:root 600 is NOT ready, got: {out}"
    );
    assert!(
        !out.contains("credentials present"),
        "the check must not report present for a file it could not verify, got: {out}"
    );
}

#[test]
fn boss_gcp_declares_the_cluster_operator_role() {
    // The role is what makes the converge run the installer at all, so
    // the estate registry entry is load-bearing, not documentation.
    let toml = std::fs::read_to_string(repo_root().join("infra/estate/estate.toml"))
        .expect("estate.toml is readable");
    let gcp = toml
        .split("[[node]]")
        .find(|block| block.contains("id = \"boss-gcp\""))
        .expect("estate.toml declares boss-gcp");
    assert!(
        gcp.contains("\"cluster-operator\""),
        "boss-gcp must declare cluster-operator — without it the converge installs nothing and the estate has one cluster-operator again"
    );
}
