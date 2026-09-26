//! `infra/estate/ops-credentials.sh` — the ONE check of a host's declared
//! root material, read by the converge (install-cluster-operator.sh) and
//! carried on the host observation (observe-host.sh) so the estate
//! compare can raise its absence (backlog 714bc71f).
//!
//! WHY IT IS A FILE. The forge declared cluster-operator and held neither
//! /etc/boss-ops/kubeconfig nor talosconfig from 2026-09-25 22:04Z on.
//! The converge recorded `ops_credentials not ready` on every packet and
//! nothing read it; car 78a88f65 then made the same absence fatal
//! through a second reader, and every forge converge closed failed for
//! twelve hours with no alarm. The fix makes the converge record, not
//! fail, and sends the reading to the estate compare — so two readers
//! need the check, and a second copy of it is the pair CLAUDE.md §9a
//! says to collapse.
//!
//! HOW THIS IS MEASURED. The function runs under `sh` (dash on the
//! hosts — observe-host.sh is `#!/bin/sh`), and observe-host.sh runs for
//! real against an unreachable jobs API, so the observation it would
//! have posted lands in its spool, where this reads it back.

use boss_testing::{repo_root, scratch_dir, scratch_path};
use std::process::Command;

const LIB: &str = "infra/estate/ops-credentials.sh";

fn state(ops_dir: &str) -> (i32, String) {
    let lib = repo_root().join(LIB);
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!(". '{}'\nops_credentials_state", lib.display()))
        .env("BOSS_OPS_DIR", ops_dir)
        .output()
        .expect("sh runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

#[test]
fn an_absent_directory_is_both_credentials_absent() {
    let dir = scratch_path("ops-creds-absent");
    let (rc, out) = state(dir.to_str().expect("utf8"));
    assert_eq!(rc, 0, "{out}");
    assert_eq!(out, "not ready: talosconfig:absent kubeconfig:absent");
    assert!(!dir.exists(), "the check never creates the directory");
}

#[test]
fn a_file_a_test_can_make_is_named_with_what_it_actually_is() {
    // A test process cannot make a root:root 0600 file — whatever it
    // makes is wrong, and the check must say what it found, not pass.
    let dir = scratch_dir("ops-creds-mode");
    std::fs::write(dir.join("kubeconfig"), "placeholder").expect("write");
    let (rc, out) = state(dir.to_str().expect("utf8"));
    assert_eq!(rc, 0, "{out}");
    assert!(
        out.starts_with("not ready: talosconfig:absent kubeconfig:"),
        "{out}"
    );
    assert!(!out.contains("kubeconfig:absent"), "it is there: {out}");
}

#[test]
fn a_directory_the_reader_cannot_search_is_unmeasured_not_absent() {
    // The forge's observer runs as david; a 0700 root directory hides
    // whether anything is in it, and "absent" would be a guess. Root
    // searches any directory, so this branch is only reachable — and
    // only asserted — off root; the gate runs as uid 65534.
    let uid = Command::new("id").arg("-u").output().expect("id runs");
    if String::from_utf8_lossy(&uid.stdout).trim() == "0" {
        return;
    }
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch_dir("ops-creds-sealed");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o000)).expect("chmod");
    let (rc, out) = state(dir.to_str().expect("utf8"));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    assert_eq!(rc, 0, "{out}");
    assert!(out.starts_with("unmeasured: "), "{out}");
}

#[test]
fn both_readers_source_the_one_check() {
    for reader in [
        "infra/estate/install-cluster-operator.sh",
        "infra/estate/observe-host.sh",
    ] {
        let text = std::fs::read_to_string(repo_root().join(reader))
            .unwrap_or_else(|e| panic!("{reader} is readable: {e}"));
        assert!(
            text.contains("ops-credentials.sh") && text.contains("ops_credentials_state"),
            "{reader} must source and call the one check, not carry its own copy"
        );
    }
}

#[test]
fn the_host_observation_carries_the_credential_reading() {
    let root = scratch_dir("ops-creds-observe");
    let spool = root.join("spool");
    let ops = root.join("boss-ops");
    let out = Command::new("sh")
        .arg(repo_root().join("infra/estate/observe-host.sh"))
        .env("HOST_ID", "forge")
        // Nothing listens on port 1: the POST fails and the observation
        // is retained, which is where this reads it.
        .env("JOBS_API", "http://127.0.0.1:1")
        .env("ADDRESS", "127.0.0.1")
        .env("SPOOL_DIR", &spool)
        .env("BOSS_OPS_DIR", &ops)
        .output()
        .expect("observe-host runs");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let spooled: Vec<_> = std::fs::read_dir(&spool)
        .unwrap_or_else(|e| panic!("the observation was spooled ({e}): {text}"))
        .filter_map(Result::ok)
        .collect();
    assert_eq!(spooled.len(), 1, "{text}");
    let body = std::fs::read_to_string(spooled[0].path()).expect("read");
    let v: serde_json::Value = serde_json::from_str(&body).expect("json");
    let creds = &v["nodes"][0]["ops_credentials"];
    assert_eq!(creds["dir"], ops.display().to_string(), "{body}");
    assert_eq!(
        creds["state"], "not ready: talosconfig:absent kubeconfig:absent",
        "{body}"
    );
}
