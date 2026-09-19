//! The gate's warm seed keeps ONE executable per test/bin target.
//!
//! Measured 2026-09-17 17:58Z: /gate-seed/target was 154 GB, 153 GB of
//! it debug/deps, and 3,226 of those files were executables — 138
//! builds of `boss`, 195 of `boss_dispatcher` — one per gate that ever
//! relinked them, because cargo names an executable `<stem>-<16 hex>`
//! and never removes a superseded one. A reflink copy of the seed is
//! measured by the kubelet at full size, so the train gate for #424 was
//! evicted at "Usage of EmptyDir volume gate-workspace exceeds the
//! limit 160Gi" two minutes in, before any check ran, and settled LOST.
//! `prune_seed_binaries` in infra/gate-runner/run.sh drops every
//! executable but the newest per stem from the STAGED copy, before the
//! rename; libraries and unhashed files are untouched.

use boss_testing::{repo_root, scratch_dir, write_exec};
use std::fs;
use std::path::Path;
use std::process::Command;

const RUN_SH: &str = "infra/gate-runner/run.sh";

fn run_sh() -> String {
    fs::read_to_string(repo_root().join(RUN_SH)).unwrap()
}

/// Extract the function from run.sh and run it on `deps`; returns stdout.
fn prune(deps: &Path) -> String {
    let script = format!(
        "eval \"$(sed -n '/^prune_seed_binaries()/,/^}}/p' {})\"; KEPT=''; prune_seed_binaries {}",
        repo_root().join(RUN_SH).display(),
        deps.display()
    );
    let out = Command::new("bash").arg("-c").arg(script).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn exe(dir: &Path, name: &str, age_secs: u64) {
    let p = dir.join(name);
    write_exec(&p, "binary");
    fs::write(dir.join(format!("{name}.d")), b"dep").unwrap();
    let t = std::time::SystemTime::now() - std::time::Duration::from_secs(age_secs);
    let ft = filetime_of(t);
    set_mtime(&p, ft);
}

fn filetime_of(t: std::time::SystemTime) -> (i64, i64) {
    let d = t.duration_since(std::time::UNIX_EPOCH).unwrap();
    (d.as_secs() as i64, d.subsec_nanos() as i64)
}

fn set_mtime(p: &Path, (secs, _): (i64, i64)) {
    // touch -d @secs: the shell has the tool, and the function reads mtime through `ls -t`.
    let s = Command::new("touch")
        .arg("-d")
        .arg(format!("@{secs}"))
        .arg(p)
        .status()
        .unwrap();
    assert!(s.success());
}

#[test]
fn only_the_newest_executable_per_stem_survives_and_libraries_are_untouched() {
    let deps = scratch_dir("warm-seed-prune").join("deps");
    fs::create_dir_all(&deps).unwrap();
    exe(&deps, "boss-aaaaaaaaaaaaaaaa", 5000);
    exe(&deps, "boss-bbbbbbbbbbbbbbbb", 10);
    exe(&deps, "boss-cccccccccccccccc", 3000);
    exe(&deps, "boss_dispatcher-2222222222222222", 100);
    fs::write(deps.join("libboss-1111111111111111.rlib"), b"lib").unwrap();
    fs::write(deps.join("libboss-1111111111111111.rmeta"), b"meta").unwrap();
    let tool = deps.join("tool");
    write_exec(&tool, "t");

    let said = prune(&deps);
    assert_eq!(said, "pruned 2 binaries, 0 MiB", "{said}");

    let mut left: Vec<String> = fs::read_dir(&deps)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        vec![
            "boss-bbbbbbbbbbbbbbbb",
            "boss-bbbbbbbbbbbbbbbb.d",
            "boss_dispatcher-2222222222222222",
            "boss_dispatcher-2222222222222222.d",
            "libboss-1111111111111111.rlib",
            "libboss-1111111111111111.rmeta",
            "tool",
        ]
    );
}

#[test]
fn a_missing_deps_dir_prunes_nothing_and_says_so() {
    let deps = scratch_dir("warm-seed-prune-missing").join("absent");
    assert_eq!(prune(&deps), "pruned 0 binaries, 0 MiB");
}

/// The refresh prunes the STAGED copy, after the cp and before the
/// rename — a torn prune can only touch target.partial.
#[test]
fn the_refresh_prunes_the_staged_copy_before_the_rename() {
    let sh = run_sh();
    let cp = sh
        .find("cp -a --reflink=auto /gate-target/target \"$SEED/target.partial\"")
        .unwrap();
    let prune = sh
        .find("prune_seed_binaries \"$SEED/target.partial/debug/deps\"")
        .expect("the refresh must prune the staged copy's debug/deps");
    let mv = sh
        .rfind("mv \"$SEED/target.partial\" \"$SEED/target\"")
        .unwrap();
    assert!(cp < prune && prune < mv, "order must be cp, prune, mv");
}
