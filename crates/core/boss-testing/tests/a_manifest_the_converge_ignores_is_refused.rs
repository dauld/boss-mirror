//! The converge applies `infra/cluster/manifests/*.yaml` and nothing
//! else. This RUNS the guard that makes that statement true of the whole
//! directory — `infra/lint/a-manifest-the-converge-ignores-is-refused.sh`
//! — and the converge-side refusal it shares a definition with
//! (`manifests_the_converge_ignores` in
//! `infra/forge/cluster-deploy-lib.sh`).
//!
//! WHY THIS EXISTS (backlog e37a833d). `manifests_with_image` stages the
//! apply directory with `cp "$src"/*.yaml "$dst"/`, so a manifest
//! committed as `.yml` or `.json` is in the tree, reviewed, merged — and
//! never reaches the cluster. Nothing said so: no lint, no converge
//! warning, no drift alarm. It is the failure CLAUDE.md §Diagnosis
//! forbids outright, a change that looks delivered and is not, and it is
//! indistinguishable from a manifest that has merely not converged YET.
//!
//! WHY THE FIX IS A REFUSAL AND NOT A WIDER GLOB. Seven readers derive
//! "what the tree declares to the cluster" from this directory, each with
//! its own `*.yaml`. Widening the converge to `.yml`/`.json` leaves all
//! seven free to disagree about the set — a reader that widened to one
//! and not the other reports an object as declared that the converge
//! will never create. Refusing instead makes `*.yaml` select the WHOLE
//! directory, so the seven globs are equal by construction rather than
//! by seven edits or seven pins (CLAUDE.md §9a: collapse, do not pin).
//! The builder of the orphan derivation nearly widened its glob and
//! found the derivation was RIGHT to match only `.yaml`, because that is
//! exactly what the converge applies; the defect is on the converge side.

use boss_testing::repo_root;
use std::path::{Path, PathBuf};
use std::process::Command;

const LINT_REL: &str = "infra/lint/a-manifest-the-converge-ignores-is-refused.sh";
const LIB_REL: &str = "infra/forge/cluster-deploy-lib.sh";
const MANIFESTS_REL: &str = "infra/cluster/manifests";

/// A scratch directory THIS uid and THIS process own outright — pid and
/// uid in the name. `/tmp` is sticky, so a fixed fixture path left by
/// another account is a directory this run can neither remove nor write,
/// and the gate runs as 65534 while a developer runs as root.
fn scratch(case: &str) -> PathBuf {
    boss_testing::scratch_dir(&format!("converge-ignores-{case}"))
}

/// A fixture tree laid out like the repo: the real lint and the real lib,
/// beside a manifests directory of `count` appliable manifests plus the
/// README that documents them. The scripts under test are the
/// repository's own, so every verdict below is one they actually reached.
fn fixture(case: &str, count: usize) -> PathBuf {
    let tree = scratch(case).join("tree");
    let manifests = tree.join(MANIFESTS_REL);
    boss_testing::create_dir(&manifests);
    for i in 0..count {
        boss_testing::write_file(
            &manifests.join(format!("boss-svc-{i}.yaml")),
            &format!("kind: Service\nmetadata:\n  name: svc-{i}\n  namespace: boss\n"),
        );
    }
    boss_testing::write_file(
        &manifests.join("README.md"),
        "# manifests\n\nDocumentation, deliberately not applied.\n",
    );
    for rel in [LINT_REL, LIB_REL] {
        let dst = tree.join(rel);
        boss_testing::create_dir(dst.parent().expect("a parent"));
        std::fs::copy(repo_root().join(rel), &dst)
            .unwrap_or_else(|e| panic!("copy {rel} into the fixture: {e}"));
    }
    tree
}

/// Run the lint out of a tree, returning its exit code and everything it
/// said. Both streams, because a refusal whose reason went to the stream
/// nobody captured is the reduction §Diagnosis warns about.
fn run_lint(tree: &Path) -> (i32, String) {
    let out = Command::new("bash")
        .arg(tree.join(LINT_REL))
        .output()
        .expect("the lint runs");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

#[track_caller]
fn names_all(out: &str, phrases: &[&str], what: &str) {
    for p in phrases {
        assert!(
            out.contains(p),
            "{what} did not name `{p}` — a verdict someone must go re-derive is not a \
             verdict (CLAUDE.md §Diagnosis). It said:\n{out}"
        );
    }
}

/// The baseline: the directory the converge can apply in full passes, and
/// the pass states how many manifests it checked. Without this every
/// refusal below could be "it always fails".
#[test]
fn a_directory_the_converge_applies_in_full_passes() {
    let tree = fixture("clean", 12);
    let (rc, out) = run_lint(&tree);
    assert_eq!(rc, 0, "a clean manifests directory was refused:\n{out}");
    names_all(&out, &["12"], "the clean run");
}

/// THE DEFECT, in the direction a human would hit it: a manifest spelled
/// `.yml`. Before this car nothing in the tree noticed — the file sat
/// there, merged, declaring an object the cluster would never be told
/// about.
#[test]
fn a_yml_manifest_is_refused_by_name() {
    let tree = fixture("yml", 12);
    boss_testing::write_file(
        &tree.join(MANIFESTS_REL).join("boss-late-arrival.yml"),
        "kind: Service\nmetadata:\n  name: late\n  namespace: boss\n",
    );
    let (rc, out) = run_lint(&tree);
    assert_eq!(
        rc, 1,
        "a `.yml` manifest the converge will never apply passed the lint:\n{out}"
    );
    names_all(
        &out,
        &["boss-late-arrival.yml", ".yaml"],
        "the refusal of a .yml manifest",
    );
}

/// `.json` is the other extension `kubectl apply -f <dir>` would take and
/// the converge's `cp` will not, so it fails the same way and must be
/// named the same way.
#[test]
fn a_json_manifest_is_refused_by_name() {
    let tree = fixture("json", 12);
    boss_testing::write_file(
        &tree.join(MANIFESTS_REL).join("boss-extra.json"),
        "{\"kind\":\"Service\",\"metadata\":{\"name\":\"extra\",\"namespace\":\"boss\"}}\n",
    );
    let (rc, out) = run_lint(&tree);
    assert_eq!(rc, 1, "a `.json` manifest passed the lint:\n{out}");
    names_all(
        &out,
        &["boss-extra.json"],
        "the refusal of a .json manifest",
    );
}

/// Same defect, different spelling: `cp "$src"/*.yaml` does not recurse
/// and `kubectl apply -f <dir>` does not either, so a manifest filed in a
/// subdirectory converges exactly as often as a `.yml` one — never.
#[test]
fn a_subdirectory_of_manifests_is_refused_by_name() {
    let tree = fixture("subdir", 12);
    let nested = tree.join(MANIFESTS_REL).join("staging");
    boss_testing::create_dir(&nested);
    boss_testing::write_file(
        &nested.join("boss-nested.yaml"),
        "kind: Service\nmetadata:\n  name: nested\n  namespace: boss\n",
    );
    let (rc, out) = run_lint(&tree);
    assert_eq!(
        rc, 1,
        "a subdirectory holding a manifest passed the lint:\n{out}"
    );
    names_all(
        &out,
        &["staging"],
        "the refusal of a manifests subdirectory",
    );
}

/// A dotfile is the third spelling, and the one that survives a naive
/// fix: `.hidden.yaml` ENDS in `.yaml` and is still not matched by the
/// converge's glob, because bash does not expand `*` onto a leading dot.
#[test]
fn a_hidden_manifest_is_refused_by_name() {
    let tree = fixture("dotfile", 12);
    boss_testing::write_file(
        &tree.join(MANIFESTS_REL).join(".boss-hidden.yaml"),
        "kind: Service\nmetadata:\n  name: hidden\n  namespace: boss\n",
    );
    let (rc, out) = run_lint(&tree);
    assert_eq!(
        rc, 1,
        "a dot-named manifest, which `*.yaml` never matches, passed the lint:\n{out}"
    );
    names_all(
        &out,
        &[".boss-hidden.yaml"],
        "the refusal of a hidden manifest",
    );
}

/// NON-VACUITY. A check over a directory is two claims — the directory is
/// the right directory, and every entry in it is appliable — and only the
/// second is ever written. An empty directory satisfies the second one
/// vacuously, loudly green, having checked nothing. The shape is
/// `assert_roster_floor!`'s, in bash: a floor below today's real count,
/// never an exact total (CLAUDE.md §9a).
#[test]
fn an_empty_or_thin_manifests_directory_cannot_read_as_clean() {
    let tree = fixture("empty", 0);
    std::fs::remove_file(tree.join(MANIFESTS_REL).join("README.md")).expect("remove the README");
    let (rc, out) = run_lint(&tree);
    assert_ne!(rc, 0, "an EMPTY manifests directory read as clean:\n{out}");
    names_all(&out, &["0"], "the empty-directory refusal");

    // One short of the floor still refuses; the floor is what makes a
    // collapsed scrape loud rather than quietly smaller.
    let tree = fixture("thin", 3);
    let (rc, out) = run_lint(&tree);
    assert_ne!(
        rc, 0,
        "a manifests directory holding 3 manifests read as clean — the scrape broke and \
         the check had almost no subject:\n{out}"
    );
}

/// A missing directory is "unknown", not "clean" — the distinction
/// check-manifests-applied.sh makes between a credential it does not have
/// and infrastructure that is not there.
#[test]
fn a_missing_manifests_directory_is_refused() {
    let tree = fixture("missing", 12);
    std::fs::remove_dir_all(tree.join(MANIFESTS_REL)).expect("remove the manifests directory");
    let (rc, out) = run_lint(&tree);
    assert_ne!(rc, 0, "a missing manifests directory read as clean:\n{out}");
    names_all(&out, [MANIFESTS_REL].as_slice(), "the missing-dir refusal");
}

/// THE REAL TREE. The lint's whole value is what it says about
/// `infra/cluster/manifests/` as it actually stands, and there are zero
/// offenders today — so this is the measurement that the guard is cheap
/// to land, and the one that reddens the day it stops being true.
#[test]
fn the_repository_manifests_directory_is_appliable_in_full() {
    let root = repo_root();
    let (rc, out) = run_lint(&root);
    assert_eq!(
        rc, 0,
        "the repository's own manifests directory holds a file the converge will not \
         apply:\n{out}"
    );
    let manifests: Vec<_> = std::fs::read_dir(root.join(MANIFESTS_REL))
        .expect("read the manifests directory")
        .map(|e| e.expect("an entry").file_name())
        .filter(|n| n.to_string_lossy().ends_with(".yaml"))
        .collect();
    boss_testing::assert_roster_floor!(
        manifests,
        10,
        "the cluster manifests the converge applies (20 on 2026-09-11)"
    );
}

// ---------------------------------------------------------------------------
// The converge side. The lint keeps it from reaching main; this keeps the
// converge from claiming it applied a directory it only partly read.
// ---------------------------------------------------------------------------

/// Run one call against the real `cluster-deploy-lib.sh`, sourced the way
/// `cluster-deploy-runner.sh` sources it.
fn run_lib(script: &str) -> (i32, String) {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!(
            ". {}\n{script}\n",
            repo_root().join(LIB_REL).display()
        ))
        .output()
        .expect("the lib runs");
    (
        out.status.code().unwrap_or(-1),
        format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// `manifests_with_image` must not stage a PARTIAL apply directory. A
/// converge that applies 20 of 21 manifests and exits 0 reports a
/// convergence it did not perform, and `kubectl apply` has no `--prune`
/// to make the omission visible afterwards.
#[test]
fn the_converge_refuses_to_stage_a_directory_it_would_only_partly_apply() {
    let root = scratch("stage");
    let src = root.join("src");
    let dst = root.join("dst");
    boss_testing::create_dir(&src);
    boss_testing::write_file(
        &src.join("boss.yaml"),
        "spec:\n  image: 10.20.0.15:3000/david/boss:b2814ef\n",
    );
    boss_testing::write_file(&src.join("boss-extra.yml"), "kind: Service\n");

    let (rc, out) = run_lib(&format!(
        "manifests_with_image {} {} 10.20.0.15:3000/david/boss lastgood",
        src.display(),
        dst.display()
    ));
    assert_ne!(
        rc, 0,
        "the converge staged an apply directory that silently drops boss-extra.yml:\n{out}"
    );
    names_all(
        &out,
        &["boss-extra.yml"],
        "the converge's refusal to stage a partial apply",
    );
    assert!(
        !dst.join("boss.yaml").exists(),
        "the refusal left a half-staged apply directory behind; it must refuse BEFORE \
         anything is copied, so no caller can apply it by mistake"
    );
}

/// And the same call with nothing but appliable manifests still works —
/// the guard is a refusal, not a new failure mode on the happy path.
/// (`the-converge-rolls-back-to-a-named-build.sh` §4 owns the tag
/// rewriting itself; this is only that the guard does not get in its way.)
#[test]
fn the_converge_still_stages_a_directory_it_can_apply_in_full() {
    let root = scratch("stage-ok");
    let src = root.join("src");
    let dst = root.join("dst");
    boss_testing::create_dir(&src);
    boss_testing::write_file(
        &src.join("boss.yaml"),
        "spec:\n  image: 10.20.0.15:3000/david/boss:b2814ef\n",
    );
    boss_testing::write_file(&src.join("README.md"), "# docs, not a declaration\n");

    let (rc, out) = run_lib(&format!(
        "manifests_with_image {} {} 10.20.0.15:3000/david/boss lastgood",
        src.display(),
        dst.display()
    ));
    assert_eq!(rc, 0, "a fully appliable directory was refused:\n{out}");
    let staged = std::fs::read_to_string(dst.join("boss.yaml")).expect("boss.yaml was staged");
    assert!(
        staged.contains("boss:lastgood"),
        "the converged tag did not reach the staged copy: {staged}"
    );
    assert!(
        !dst.join("README.md").exists(),
        "documentation was staged into the apply directory"
    );
}

/// The one definition both readers share. A second `case` list in the
/// lint would be the §9a duplicate this car exists to avoid, so the lint
/// sources the lib — which means this function is a contract, not an
/// implementation detail.
#[test]
fn the_ignored_set_is_one_definition_both_readers_call() {
    let root = scratch("definition");
    let dir = root.join("manifests");
    boss_testing::create_dir(&dir);
    for name in ["a.yaml", "README.md"] {
        boss_testing::write_file(&dir.join(name), "kind: Service\n");
    }
    let (rc, out) = run_lib(&format!("manifests_the_converge_ignores {}", dir.display()));
    assert_eq!(rc, 0, "the derivation failed on a clean directory:\n{out}");
    assert_eq!(
        out.trim(),
        "",
        "the derivation named something in a directory the converge applies in full:\n{out}"
    );

    for name in ["b.yml", "c.json", "d.txt"] {
        boss_testing::write_file(&dir.join(name), "kind: Service\n");
    }
    let (rc, out) = run_lib(&format!("manifests_the_converge_ignores {}", dir.display()));
    assert_eq!(rc, 0, "the derivation failed on a dirty directory:\n{out}");
    let named: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
    assert_eq!(
        named.len(),
        3,
        "the derivation must name every entry the converge drops, one per line:\n{out}"
    );
    names_all(&out, &["b.yml", "c.json", "d.txt"], "the derivation");

    // The lint must READ that definition rather than carry a second copy
    // of it — the pair that cannot drift because there is only one.
    let lint = std::fs::read_to_string(repo_root().join(LINT_REL)).expect("read the lint");
    assert!(
        lint.contains("manifests_the_converge_ignores"),
        "the lint does not call the converge's own derivation, so the legal extension set \
         lives twice and is free to drift (CLAUDE.md §9a)"
    );
}
