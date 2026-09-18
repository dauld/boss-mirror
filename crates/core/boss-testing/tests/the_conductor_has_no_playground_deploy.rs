//! The conductor carries no bare-metal deploy hop (backlog ed64f852,
//! design 42277636 H7 residue, 2026-09-18).
//!
//! Train #443 deleted the bare-metal deploy path — deploy-services.sh,
//! deploy-web.sh, build-release's callers on a host tree — and left
//! the hop that CALLED them: `Conductor::deploy` in train.rs still
//! pulled `/opt/boss`, ran migrate.sh, build-release.sh,
//! `sudo deploy-services.sh prod` and `sudo deploy-web.sh`, reachable
//! only when `BOSS_TRAIN_DEPLOY_TREE` named a tree, which the one live
//! conductor (infra/cluster/manifests/boss-conductor.yaml) has set
//! empty since the 2026-09-04 cutover. A hop nothing can reach, calling
//! scripts that no longer exist, is the residue this pins gone: the
//! `deployed` step is completed with the one evidence the cluster
//! conductor ever stamped (the cluster converges on forge main via the
//! deploy-runner), and neither the knob nor the scripts are named by
//! the conductor or its manifest again.
//!
//! Read from the tree, never a typed roster: the two places that held
//! the knob are named; the scripts are the ones #443 deleted. The
//! conductor is a directory module since consolidation H1 (5cb0bddb,
//! 2026-09-18), so it is read as every `.rs` file under it — a single
//! file would narrow the check to whichever region the knob landed in.

use boss_testing::repo_root;

const CONDUCTOR: &str = "crates/orchestrators/boss-cli/src/train/";
const MANIFEST: &str = "infra/cluster/manifests/boss-conductor.yaml";

/// Every file `path` names: itself, or, for a directory, each `.rs`
/// under it (recursively), as repo-relative paths in a stable order.
fn files_of(path: &str) -> Vec<String> {
    let abs = repo_root().join(path);
    if !abs.is_dir() {
        return vec![path.to_string()];
    }
    let mut out = Vec::new();
    let mut stack = vec![abs];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let p = entry.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let rel = p.strip_prefix(repo_root()).unwrap();
                out.push(rel.to_string_lossy().into_owned());
            }
        }
    }
    out.sort();
    assert!(
        !out.is_empty(),
        "{path}: no .rs file under it — a pin that reads nothing passes for nothing"
    );
    out
}

/// Lines of `path` (comments included: a comment that documents a
/// knob invites the next reader to set it) containing `needle`.
fn lines_naming(path: &str, needle: &str) -> Vec<String> {
    files_of(path)
        .into_iter()
        .flat_map(|file| {
            std::fs::read_to_string(repo_root().join(&file))
                .unwrap_or_else(|e| panic!("{file}: {e}"))
                .lines()
                .enumerate()
                .filter(|(_, l)| l.contains(needle))
                .map(|(i, l)| format!("{file}:{}: {}", i + 1, l.trim()))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn the_deploy_tree_knob_is_gone_from_the_conductor_and_its_manifest() {
    let hits: Vec<String> = [CONDUCTOR, MANIFEST]
        .iter()
        .flat_map(|p| lines_naming(p, "BOSS_TRAIN_DEPLOY_TREE"))
        .collect();
    assert!(
        hits.is_empty(),
        "the playground-deploy knob outlived the hop it switched:\n{}",
        hits.join("\n")
    );
}

#[test]
fn the_conductor_calls_no_deleted_deploy_script() {
    let deleted = ["deploy-services.sh", "deploy-web.sh", "build-release.sh"];
    let hits: Vec<String> = deleted
        .iter()
        .flat_map(|s| lines_naming(CONDUCTOR, s))
        .collect();
    assert!(
        hits.is_empty(),
        "the conductor names a script train #443 deleted:\n{}",
        hits.join("\n")
    );
}
