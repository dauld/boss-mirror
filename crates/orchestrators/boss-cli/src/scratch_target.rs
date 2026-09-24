//! A reported green run frees its worktree's cargo target on the dev
//! pod scratch (backlog 4e17c49d, decided 2026-09-24).
//!
//! WHY. Every builder builds through `infra/dev/wt-cargo`, which
//! reflink-seeds a target of its own at `/scratch/target-<worktree
//! basename>` — `target-agent-<id>` for a builder's harness worktree.
//! Nothing removed it when the run ended: it sat until the hourly or
//! build-triggered LRU reclaim (`infra/cluster/dev-scratch-reclaim.sh`,
//! backlog 3f2a08ab) reached it. Measured 2026-09-24 ~05:20Z: /scratch
//! held ~52 GB in 10 `target-agent-*` dirs, ~38 GB of them belonging to
//! runs already reported (the largest 19 GB) — and the dev pod had been
//! evicted at 01:27Z that morning with /scratch at ~430 GiB against its
//! 100Gi request. The operator's hand removal was refused by the
//! harness, which was right: a bounded reclaim is the machine's.
//!
//! WHEN. `boss dispatch --report` is the handback verb, so the run's
//! end is the one moment something KNOWS the target is finished with.
//! It frees the target only once the run is GREEN — `building` closed
//! `gated`, which the landing rule writes on the gate's green (a parked
//! car was green first, so this covers it). A run that is not green
//! keeps its target: a red gate's rescue rebuilds in that worktree, and
//! a warm target is the difference between seconds and a cold build.
//! The report says so and names the second `--report` that frees it.
//!
//! AND AT THE GREEN ITSELF (backlog a3355e14, decided 2026-09-24). The
//! report is not the green: a background builder's run is reported by
//! the agent-stop hook when the Agent tool RETURNS, which for a
//! background agent is at launch (run 781e6e9e's report reads "(the
//! Agent tool returned no text)"), so the report above always found the
//! run not green and kept the target. The decision read "the
//! dispatcher's green handler frees it"; the dispatcher cannot — the
//! scratch is an emptyDir private to the dev pod (boss-dev.yaml: "a
//! sidecar, not a CronJob, because the two things it cleans are
//! pod-private"). The one party ON the pod that sees the green is the
//! gate that went green: `boss gate --wait`, in the builder's own shell,
//! reads its verdict off the gate-run, and the gate-run carries the
//! run's edge and the worktree it was launched from (both stamped at
//! launch). So the waiter frees the target there
//! ([`settle_at_green`]) and records it on the run, by [`GREEN_BY`]; a
//! later report reads that record and leaves it alone. A green nobody
//! waited for is still the hourly reclaim's.
//!
//! WHICH DIR. The gate stamps the worktree it was launched from (`git
//! rev-parse --show-toplevel`) beside the run's edge, and the landing
//! rule carries it onto `building` as `gate_run.worktree` — the one
//! record of where a run was built; the run packet's own `worktree` key
//! used to be the DISPATCHER's cwd, and `boss dispatch` no longer
//! writes it. A green from before that stamp still holds the branch
//! ([`crate::dispatch::run_branch`]), and git holds which linked
//! worktree has that branch checked out (`git worktree list
//! --porcelain`; git refuses one branch in two worktrees). The name is
//! then wt-cargo's own: [`target_dir`] is `wt_target_dir` in
//! `infra/dev/wt-target-dir.sh`, spelled here a third time beside
//! `worktree_target` in the reclaim script, and pinned equal to the
//! helper by `the_target_name_is_the_one_wt_cargo_builds_into`
//! (CLAUDE.md §9a). Only a BUILDER worktree qualifies — basename
//! `agent-*`, the same predicate wt-cargo bounds builders by — so the
//! operator's checkout, whose target is the warm seed, is never mapped.
//!
//! WHAT IT REFUSES. The seed (`WT_SEED`, `/scratch/target`) whatever
//! the mapping says; any path whose resolved parent is not the scratch
//! root (`WT_TARGET_ROOT`, `/scratch` — the two knobs wt-cargo reads,
//! so an override points both at the same place); a symlink; a name
//! not `target-*`. Each refusal is recorded, not swallowed.
//!
//! WHAT IT RECORDS. `scratch_target` on the run packet — the path and
//! the bytes freed, or why it kept or refused — and one stderr line.
//! Bytes are counted as `du` counts them (allocated blocks), which is
//! also how the kubelet measures the pod's scratch against its request.
//! A reflinked extent still shared with the seed is counted but frees
//! nothing on disk; the kubelet counts it all the same, so it is the
//! figure the eviction line reads. Freeing never fails the report: the
//! handback is the deliverable, and a target it could not free is the
//! hourly reclaim's.

use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// The env knob wt-cargo reads for the per-worktree target root.
pub(crate) const ROOT_ENV: &str = "WT_TARGET_ROOT";
/// The env knob wt-cargo reads for the warm seed.
pub(crate) const SEED_ENV: &str = "WT_SEED";
/// wt-cargo's defaults, the pod's layout.
pub(crate) const DEFAULT_ROOT: &str = "/scratch";
pub(crate) const DEFAULT_SEED: &str = "/scratch/target";
/// A builder's harness worktree — the prefix wt-cargo bounds by.
const BUILDER_PREFIX: &str = "agent-";
/// Every per-worktree target's name starts so; the seed's does not.
const TARGET_PREFIX: &str = "target-";
/// Who freed it, on the record: the waiter that saw the green, or the
/// handback that came after it.
pub(crate) const GREEN_BY: &str = "boss gate --wait, on its green";
pub(crate) const REPORT_BY: &str = "boss dispatch --report";

/// Where this box keeps worktree targets, and the one it must never
/// touch, with the worktree list read at the CLI boundary.
#[derive(Debug, Clone)]
pub(crate) struct Scratch {
    pub root: PathBuf,
    pub seed: PathBuf,
    /// `git worktree list --porcelain`, or `None` when it could not
    /// be read here (not a checkout, no git).
    pub porcelain: Option<String>,
}

impl Scratch {
    /// wt-cargo's two knobs (or its defaults) and this checkout's
    /// worktree list. Blocking: it runs git.
    pub(crate) fn from_env() -> Self {
        let porcelain = std::process::Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        let (root, seed) = roots_from_env();
        Scratch {
            root,
            seed,
            porcelain,
        }
    }
}

/// wt-cargo's two knobs, or its defaults: the scratch root and the
/// seed. No git — the gate's waiter already knows the worktree.
pub(crate) fn roots_from_env() -> (PathBuf, PathBuf) {
    let path = |key: &str, default: &str| {
        std::env::var_os(key)
            .filter(|v| !v.is_empty())
            .map_or_else(|| PathBuf::from(default), PathBuf::from)
    };
    (path(ROOT_ENV, DEFAULT_ROOT), path(SEED_ENV, DEFAULT_SEED))
}

/// The worktree the run's gate stamped at launch, carried onto
/// `building` by the landing rule as `gate_run.worktree`.
pub(crate) fn stamped_worktree(run: &Value) -> Option<PathBuf> {
    crate::envelope::steps(run)
        .into_iter()
        .find(|s| {
            s.get("spec_slug").and_then(Value::as_str) == Some(crate::dispatch::BUILDING_SLUG)
        })?
        .pointer("/metadata/gate_run/worktree")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(PathBuf::from)
}

/// A BUILDER worktree's target (`agent-*`, the predicate wt-cargo
/// bounds builders by), or `None` for any other tree.
fn builder_target(root: &Path, worktree: &Path) -> Option<PathBuf> {
    let is_builder = worktree
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(BUILDER_PREFIX));
    target_dir(root, worktree).filter(|_| is_builder)
}

fn not_a_builder(worktree: &Path, branch: &str) -> String {
    format!(
        "{} has {branch} checked out but is not a builder worktree ({BUILDER_PREFIX}*), \
         so its target is not the run's",
        worktree.display()
    )
}

/// The path the run's record says was already freed, when it says so.
fn already_freed(run: &Value) -> Option<String> {
    let rec = run.pointer("/metadata/scratch_target")?;
    rec.get("removed")
        .and_then(Value::as_array)
        .filter(|r| !r.is_empty())?;
    Some(
        rec.get("path")
            .and_then(Value::as_str)
            .unwrap_or("(path unrecorded)")
            .to_string(),
    )
}

/// wt-cargo's name for a worktree's target: `<root>/target-<basename>`.
pub(crate) fn target_dir(root: &Path, worktree: &Path) -> Option<PathBuf> {
    let name = worktree.file_name()?.to_str()?;
    Some(root.join(format!("{TARGET_PREFIX}{name}")))
}

/// The linked worktree that has `branch` checked out, read off `git
/// worktree list --porcelain`. The first block is always the main
/// worktree and is never returned.
pub(crate) fn worktree_on(porcelain: &str, branch: &str) -> Option<PathBuf> {
    let want = format!("branch refs/heads/{branch}");
    porcelain
        .split("\n\n")
        .filter(|block| !block.trim().is_empty())
        .skip(1)
        .find(|block| block.lines().any(|l| l == want))
        .and_then(|block| block.lines().find_map(|l| l.strip_prefix("worktree ")))
        .map(PathBuf::from)
}

/// Did the run's gate go green? `building` completed with `result =
/// gated` — what the green's landing rule writes.
pub(crate) fn is_green(run: &Value) -> bool {
    crate::envelope::steps(run).into_iter().any(|s| {
        s.get("spec_slug").and_then(Value::as_str) == Some(crate::dispatch::BUILDING_SLUG)
            && s.get("status").and_then(Value::as_str) == Some("completed")
            && s.pointer("/metadata/result").and_then(Value::as_str) == Some("gated")
    })
}

/// What the report will do with the run's target.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Plan {
    Free {
        branch: String,
        worktree: PathBuf,
        target: PathBuf,
    },
    Keep(String),
}

/// The decision, from the run and the worktree list alone.
pub(crate) fn plan(run: &Value, porcelain: Option<&str>, root: &Path) -> Plan {
    if !is_green(run) {
        return Plan::Keep(
            "the run is not green (`building` has not closed `gated`), so its target stays \
             for the rescue — a red gate rebuilds in that worktree; run --report again once \
             the gate is green and it is freed"
                .into(),
        );
    }
    let branch = crate::dispatch::run_branch(run);
    // The gate's own stamp first (a3355e14); the worktree list only for
    // a green from before the stamp existed.
    let worktree = match (stamped_worktree(run), branch.as_deref()) {
        (Some(stamped), _) => stamped,
        (None, None) => {
            return Plan::Keep(
                "the green names no branch and no worktree on `building`, so no worktree can \
                 be mapped to it"
                    .into(),
            );
        }
        (None, Some(branch)) => {
            let Some(porcelain) = porcelain else {
                return Plan::Keep(
                    "`git worktree list` could not be read here, so no worktree can be mapped \
                     to the run's branch"
                        .into(),
                );
            };
            let Some(worktree) = worktree_on(porcelain, branch) else {
                return Plan::Keep(format!(
                    "no linked worktree here has {branch} checked out — gone already, or \
                     another box; the hourly reclaim takes a gone worktree's target"
                ));
            };
            worktree
        }
    };
    let branch = branch.unwrap_or_else(|| "(no branch on the green)".into());
    match builder_target(root, &worktree) {
        Some(target) => Plan::Free {
            branch,
            worktree,
            target,
        },
        None => Plan::Keep(not_a_builder(&worktree, &branch)),
    }
}

/// What freeing did.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Freed {
    /// Removed, with the bytes `du` counted in them.
    Removed { bytes: u64, removed: Vec<PathBuf> },
    /// Nothing was there to remove.
    Absent,
    /// A guard said no, or the removal failed.
    Refused(String),
}

/// Remove `target` (and a `.seeding` sibling wt-cargo left from a
/// killed copy) after every guard holds.
pub(crate) fn free(target: &Path, root: &Path, seed: &Path) -> Freed {
    let mut seeding = target.as_os_str().to_owned();
    seeding.push(".seeding");
    let mut bytes = 0u64;
    let mut removed = Vec::new();
    for dir in [target.to_path_buf(), PathBuf::from(seeding)] {
        match std::fs::symlink_metadata(&dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Freed::Refused(format!("{} could not be read: {e}", dir.display())),
            Ok(m) if !m.is_dir() => {
                return Freed::Refused(format!(
                    "{} is not a directory (a symlink or a file), so it is not a target",
                    dir.display()
                ));
            }
            Ok(_) => {}
        }
        if let Err(why) = guard(&dir, root, seed) {
            return Freed::Refused(why);
        }
        bytes = bytes.saturating_add(du_bytes(&dir));
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            return Freed::Refused(format!(
                "removing {} failed part-way: {e} — the hourly reclaim takes what is left",
                dir.display()
            ));
        }
        removed.push(dir);
    }
    if removed.is_empty() {
        Freed::Absent
    } else {
        Freed::Removed { bytes, removed }
    }
}

/// The guards every removal passes, on the RESOLVED path: its parent
/// is the scratch root, it is not the seed, and its name is a target's.
fn guard(dir: &Path, root: &Path, seed: &Path) -> Result<(), String> {
    let real = dir
        .canonicalize()
        .map_err(|e| format!("{} does not resolve: {e}", dir.display()))?;
    let real_root = root
        .canonicalize()
        .map_err(|e| format!("the scratch root {} does not resolve: {e}", root.display()))?;
    let real_seed = seed.canonicalize().unwrap_or_else(|_| seed.to_path_buf());
    if real == real_seed {
        return Err(format!(
            "{} is the warm seed {} — never freed by a run",
            dir.display(),
            seed.display()
        ));
    }
    if real.parent() != Some(real_root.as_path()) {
        return Err(format!(
            "{} is not directly under the scratch root {} — refused",
            real.display(),
            real_root.display()
        ));
    }
    let named = real
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(TARGET_PREFIX));
    if !named {
        return Err(format!(
            "{} is not named {TARGET_PREFIX}* — not a worktree target",
            real.display()
        ));
    }
    Ok(())
}

/// Bytes allocated under `dir`, as `du` counts them: every entry's
/// blocks, symlinks not followed. Unreadable entries count as zero —
/// the figure is a record of what was freed, not a guard.
fn du_bytes(dir: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return 0;
    };
    let own = meta.blocks().saturating_mul(512);
    if !meta.is_dir() {
        return own;
    }
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| du_bytes(&e.path()))
        .fold(own, u64::saturating_add)
}

/// Bytes in the unit an operator reads a disk in.
fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
}

fn stamp(now: chrono::DateTime<chrono::Utc>) -> String {
    now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The report's settlement: plan, free, and what to say and record.
/// The line is printed after `boss dispatch: run <short>`; the record
/// is `scratch_target` on the run packet — `None` when the gate's
/// waiter already freed it at the green, whose record stands.
pub(crate) fn settle(
    run: &Value,
    scratch: &Scratch,
    now: chrono::DateTime<chrono::Utc>,
) -> (String, Option<Value>) {
    let at = stamp(now);
    if let Some(path) = already_freed(run) {
        return (
            format!("already freed its scratch target {path} — the record on the run stands"),
            None,
        );
    }
    match plan(run, scratch.porcelain.as_deref(), &scratch.root) {
        Plan::Keep(why) => (
            format!("kept its scratch target: {why}"),
            Some(json!({ "kept": why, "at": at, "by": REPORT_BY })),
        ),
        Plan::Free {
            branch,
            worktree,
            target,
        } => {
            let (line, rec) = free_and_record(
                &branch,
                &worktree,
                &target,
                &scratch.root,
                &scratch.seed,
                &at,
                REPORT_BY,
            );
            (line, Some(rec))
        }
    }
}

/// The gate's settlement (backlog a3355e14): its own verdict is green,
/// so no run step needs reading — only the worktree it stamped at
/// launch, held to a builder's.
pub(crate) fn settle_at_green(
    worktree: &Path,
    branch: &str,
    root: &Path,
    seed: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> (String, Value) {
    let at = stamp(now);
    match builder_target(root, worktree) {
        Some(target) => free_and_record(branch, worktree, &target, root, seed, &at, GREEN_BY),
        None => {
            let why = not_a_builder(worktree, branch);
            (
                format!("kept its scratch target: {why}"),
                json!({ "kept": why, "at": at, "by": GREEN_BY }),
            )
        }
    }
}

/// Free one mapped target and say what happened, both ways.
fn free_and_record(
    branch: &str,
    worktree: &Path,
    target: &Path,
    root: &Path,
    seed: &Path,
    at: &str,
    by: &str,
) -> (String, Value) {
    let path = target.display().to_string();
    let base = json!({
        "path": path,
        "worktree": worktree.display().to_string(),
        "branch": branch,
        "at": at,
        "by": by,
    });
    let with = |extra: Value| {
        let mut rec = base.clone();
        if let (Some(r), Some(e)) = (rec.as_object_mut(), extra.as_object()) {
            r.extend(e.clone());
        }
        rec
    };
    match free(target, root, seed) {
        Freed::Removed { bytes, removed } => (
            format!(
                "freed its scratch target {path} — {bytes} bytes ({}) as du counts them, its \
                 gate green on {branch}",
                gib(bytes)
            ),
            with(json!({
                "freed_bytes": bytes,
                "removed": removed.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
            })),
        ),
        Freed::Absent => (
            format!("had no scratch target at {path} to free — nothing there"),
            with(json!({ "freed_bytes": 0, "absent": true })),
        ),
        Freed::Refused(why) => (
            format!("did NOT free its scratch target {path}: {why}"),
            with(json!({ "refused": why })),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn green_run(branch: &str) -> Value {
        json!({
            "id": "781e6e9e-e260-4dc8-8c4b-b0f21c629585",
            "steps": [
                { "spec_slug": "building", "status": "completed",
                  "metadata": { "result": "gated",
                                "gate_run": { "branch": branch } } },
            ],
        })
    }

    fn porcelain(branch: &str, worktree: &str) -> String {
        format!(
            "worktree /work/boss\nHEAD 1111111111111111111111111111111111111111\nbranch refs/heads/{branch}\n\n\
             worktree /work/boss/.claude/worktrees/agent-other\nHEAD 2222222222222222222222222222222222222222\nbranch refs/heads/fix/other\n\n\
             worktree {worktree}\nHEAD 3333333333333333333333333333333333333333\nbranch refs/heads/{branch}-not\n\n\
             worktree /work/boss/.claude/worktrees/agent-detached\nHEAD 4444444444444444444444444444444444444444\ndetached\n\n\
             worktree {worktree}-mine\nHEAD 5555555555555555555555555555555555555555\nbranch refs/heads/{branch}\n\n"
        )
    }

    /// The name is wt-cargo's, read from the helper wt-cargo sources —
    /// a third spelling of it held equal to the first (CLAUDE.md §9a).
    #[test]
    fn the_target_name_is_the_one_wt_cargo_builds_into() {
        let helper = boss_testing::repo_root().join("infra/dev/wt-target-dir.sh");
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(r#". "$1" && wt_target_dir /scratch /work/boss/.claude/worktrees/agent-a18c"#)
            .arg("pin")
            .arg(&helper)
            .output()
            .expect("bash runs");
        assert!(out.status.success(), "{out:?}");
        let shell = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let ours = target_dir(
            Path::new("/scratch"),
            Path::new("/work/boss/.claude/worktrees/agent-a18c"),
        )
        .expect("a basename");
        assert_eq!(ours.display().to_string(), shell);
        assert_eq!(shell, "/scratch/target-agent-a18c");
    }

    /// The branch maps to the LINKED worktree that has it checked out —
    /// never the main checkout, even on the same branch, and never a
    /// worktree whose branch merely starts with it.
    #[test]
    fn the_branch_maps_to_the_linked_worktree_that_has_it_out() {
        let text = porcelain("fix/x", "/work/boss/.claude/worktrees/agent-a1");
        assert_eq!(
            worktree_on(&text, "fix/x"),
            Some(PathBuf::from("/work/boss/.claude/worktrees/agent-a1-mine"))
        );
        assert_eq!(worktree_on(&text, "fix/nowhere"), None);
        // The main checkout on the branch alone maps to nothing.
        let only_main = "worktree /work/boss\nHEAD 11\nbranch refs/heads/fix/x\n\n";
        assert_eq!(worktree_on(only_main, "fix/x"), None);
    }

    #[test]
    fn only_a_gated_building_is_green() {
        assert!(is_green(&green_run("fix/x")));
        for (status, result) in [
            ("completed", "refused"),
            ("completed", "died"),
            ("completed", "delivered"),
            ("ready", "gated"),
        ] {
            let run = json!({ "steps": [{ "spec_slug": "building", "status": status,
                                          "metadata": { "result": result } }] });
            assert!(!is_green(&run), "{status}/{result}");
        }
        assert!(!is_green(&json!({ "steps": [] })));
    }

    /// Green, a branch, a builder worktree with it out: free that
    /// worktree's target. Anything short of that keeps it and says why.
    #[test]
    fn the_plan_frees_only_a_green_runs_builder_worktree_target() {
        let wt = "/work/boss/.claude/worktrees/agent-a1";
        let text = porcelain("fix/x", wt);
        let root = Path::new("/scratch");
        assert_eq!(
            plan(&green_run("fix/x"), Some(&text), root),
            Plan::Free {
                branch: "fix/x".into(),
                worktree: PathBuf::from(format!("{wt}-mine")),
                target: PathBuf::from("/scratch/target-agent-a1-mine"),
            }
        );

        let mut red = green_run("fix/x");
        red["steps"][0]["status"] = json!("ready");
        let Plan::Keep(why) = plan(&red, Some(&text), root) else {
            panic!("a run that is not green keeps its target");
        };
        assert!(why.contains("rescue"), "{why}");
        assert!(why.contains("--report"), "names the second report: {why}");

        let Plan::Keep(why) = plan(&green_run("fix/nowhere"), Some(&text), root) else {
            panic!("no worktree, nothing to map");
        };
        assert!(why.contains("fix/nowhere"), "{why}");

        let Plan::Keep(why) = plan(&green_run("fix/x"), None, root) else {
            panic!("no worktree list, nothing to map");
        };
        assert!(why.contains("worktree list"), "{why}");

        // A worktree that is not a builder's is never mapped: the
        // operator's own trees are not the run's.
        let human = "worktree /work/boss\nHEAD 1\nbranch refs/heads/main\n\n\
                     worktree /work/boss-wt/fix-x\nHEAD 2\nbranch refs/heads/fix/x\n\n";
        let Plan::Keep(why) = plan(&green_run("fix/x"), Some(human), root) else {
            panic!("a non-builder worktree keeps its target");
        };
        assert!(why.contains("agent-"), "{why}");

        let mut no_branch = green_run("fix/x");
        no_branch["steps"][0]["metadata"] = json!({ "result": "gated" });
        assert!(matches!(plan(&no_branch, Some(&text), root), Plan::Keep(_)));
    }

    fn filled(dir: &Path) {
        std::fs::create_dir_all(dir.join("debug/deps")).unwrap();
        std::fs::write(dir.join("CACHEDIR.TAG"), b"Signature").unwrap();
        std::fs::write(dir.join("debug/deps/libx.rlib"), vec![7u8; 64 * 1024]).unwrap();
    }

    /// Removed, with the bytes du counts — the sibling `.seeding` a
    /// killed copy left goes with it, and nothing else in the root.
    #[test]
    fn free_removes_the_target_and_counts_its_bytes() {
        let root = boss_testing::scratch_dir("scratch-target-free");
        let seed = root.join("target");
        filled(&seed);
        let target = root.join("target-agent-a1");
        filled(&target);
        filled(&root.join("target-agent-a1.seeding"));
        filled(&root.join("target-agent-b2"));

        let Freed::Removed { bytes, removed } = free(&target, &root, &seed) else {
            panic!("freed");
        };
        assert!(bytes >= 64 * 1024, "du counts the rlib: {bytes}");
        assert_eq!(removed.len(), 2, "{removed:?}");
        assert!(!target.exists());
        assert!(!root.join("target-agent-a1.seeding").exists());
        assert!(
            seed.join("debug/deps/libx.rlib").exists(),
            "the seed stands"
        );
        assert!(
            root.join("target-agent-b2/CACHEDIR.TAG").exists(),
            "another run's stands"
        );

        assert_eq!(free(&target, &root, &seed), Freed::Absent);
    }

    /// The seed, a path outside the root, a symlink, a foreign name:
    /// each refused, and each left exactly where it was.
    #[test]
    fn free_refuses_the_seed_and_anything_outside_the_root() {
        let base = boss_testing::scratch_dir("scratch-target-refuse");
        let root = base.join("scratch");
        let seed = root.join("target");
        filled(&seed);

        // The seed, even when a mapping names it.
        let Freed::Refused(why) = free(&seed, &root, &seed) else {
            panic!("the seed is refused");
        };
        assert!(why.contains("seed"), "{why}");
        assert!(seed.join("CACHEDIR.TAG").exists());

        // A seed named by a path that resolves to the same dir.
        let alias = root.join("target-agent-seed");
        std::os::unix::fs::symlink(&seed, &alias).unwrap();
        assert!(matches!(free(&alias, &root, &seed), Freed::Refused(_)));
        assert!(seed.join("CACHEDIR.TAG").exists());

        // Outside the root.
        let outside = base.join("elsewhere/target-agent-a1");
        filled(&outside);
        let Freed::Refused(why) = free(&outside, &root, &seed) else {
            panic!("outside the root is refused");
        };
        assert!(why.contains("scratch root"), "{why}");
        assert!(outside.join("CACHEDIR.TAG").exists());

        // Escaping the root through `..`.
        let dotted = root.join("target-agent-x/../../elsewhere/target-agent-a1");
        std::fs::create_dir_all(root.join("target-agent-x")).unwrap();
        assert!(matches!(free(&dotted, &root, &seed), Freed::Refused(_)));
        assert!(outside.join("CACHEDIR.TAG").exists());

        // Not a target's name.
        let other = root.join("cache");
        filled(&other);
        assert!(matches!(free(&other, &root, &seed), Freed::Refused(_)));
        assert!(other.join("CACHEDIR.TAG").exists());
    }

    /// The gate's stamp names the worktree (backlog a3355e14): once the
    /// green carried it onto `building`, no worktree list is needed to
    /// map the branch — and the stamp is still held to a BUILDER's
    /// worktree, because an operator's hand gate stamps its own tree.
    #[test]
    fn the_gates_stamp_names_the_worktree_without_git() {
        let root = Path::new("/scratch");
        let mut run = green_run("fix/x");
        run["steps"][0]["metadata"]["gate_run"]["worktree"] =
            json!("/work/boss/.claude/worktrees/agent-a9");
        assert_eq!(
            plan(&run, None, root),
            Plan::Free {
                branch: "fix/x".into(),
                worktree: PathBuf::from("/work/boss/.claude/worktrees/agent-a9"),
                target: PathBuf::from("/scratch/target-agent-a9"),
            },
            "the stamp wins, no worktree list needed"
        );
        // The stamp wins over a worktree list that maps elsewhere.
        let text = porcelain("fix/x", "/work/boss/.claude/worktrees/agent-a1");
        assert!(matches!(
            plan(&run, Some(&text), root),
            Plan::Free { ref target, .. } if target == Path::new("/scratch/target-agent-a9")
        ));

        run["steps"][0]["metadata"]["gate_run"]["worktree"] = json!("/work/boss");
        let Plan::Keep(why) = plan(&run, Some(&text), root) else {
            panic!("the operator's checkout is never a run's");
        };
        assert!(why.contains("agent-"), "{why}");
    }

    /// A target the gate already freed on its green is the record; a
    /// later `--report` must not overwrite the bytes it freed with an
    /// "absent" of its own.
    #[test]
    fn a_target_freed_at_the_green_is_not_recorded_twice() {
        let root = boss_testing::scratch_dir("scratch-target-twice");
        let scratch = Scratch {
            root: root.clone(),
            seed: root.join("target"),
            porcelain: None,
        };
        let mut run = green_run("fix/x");
        run["steps"][0]["metadata"]["gate_run"]["worktree"] =
            json!("/work/boss/.claude/worktrees/agent-a9");
        run["metadata"] = json!({ "scratch_target": {
            "path": "/scratch/target-agent-a9", "freed_bytes": 42,
            "removed": ["/scratch/target-agent-a9"], "by": GREEN_BY } });
        let (line, rec) = settle(&run, &scratch, "2026-09-24T06:00:00Z".parse().unwrap());
        assert!(rec.is_none(), "nothing re-recorded: {rec:?}");
        assert!(line.contains("already freed"), "{line}");
        assert!(line.contains("/scratch/target-agent-a9"), "{line}");
    }

    /// THE GATE'S HALF: `boss gate --wait` saw its own green, and the
    /// gate-run names the worktree it launched from. A builder's target
    /// goes, with the bytes and who freed it on the record; the
    /// operator's tree is kept and says why.
    #[test]
    fn at_the_green_a_builders_target_is_freed_and_an_operators_kept() {
        let root = boss_testing::scratch_dir("scratch-target-green");
        let seed = root.join("target");
        filled(&seed);
        let target = root.join("target-agent-a9");
        filled(&target);
        let now = "2026-09-24T06:00:00Z".parse().unwrap();

        let (line, rec) = settle_at_green(
            Path::new("/work/boss/.claude/worktrees/agent-a9"),
            "fix/x",
            &root,
            &seed,
            now,
        );
        assert!(!target.exists(), "{line}");
        assert!(seed.exists());
        assert!(
            rec["freed_bytes"].as_u64().unwrap_or(0) >= 64 * 1024,
            "{rec}"
        );
        assert_eq!(rec["by"], GREEN_BY);
        assert_eq!(rec["branch"], "fix/x");
        assert!(line.contains("freed"), "{line}");

        let (line, rec) = settle_at_green(Path::new("/work/boss"), "fix/x", &root, &seed, now);
        assert!(rec["kept"].is_string(), "{rec}");
        assert!(line.contains("kept"), "{line}");
        assert!(seed.join("CACHEDIR.TAG").exists());
    }

    /// End to end over a temp root: the line names the bytes, the
    /// record carries them, and a red run's target is untouched.
    #[test]
    fn settle_frees_a_green_runs_target_and_keeps_a_red_ones() {
        let root = boss_testing::scratch_dir("scratch-target-settle");
        let seed = root.join("target");
        filled(&seed);
        let target = root.join("target-agent-a1-mine");
        filled(&target);
        let scratch = Scratch {
            root: root.clone(),
            seed: seed.clone(),
            porcelain: Some(porcelain("fix/x", "/work/boss/.claude/worktrees/agent-a1")),
        };
        let now = "2026-09-24T06:00:00Z".parse().unwrap();

        let mut red = green_run("fix/x");
        red["steps"][0]["metadata"]["result"] = json!("refused");
        let (line, rec) = settle(&red, &scratch, now);
        let rec = rec.expect("a keep is recorded");
        assert!(target.exists(), "a run that is not green keeps its target");
        assert!(rec["kept"].is_string(), "{rec}");
        assert!(line.contains("kept"), "{line}");

        let (line, rec) = settle(&green_run("fix/x"), &scratch, now);
        let rec = rec.expect("a free is recorded");
        assert_eq!(rec["by"], REPORT_BY);
        assert!(!target.exists());
        assert!(seed.exists());
        let bytes = rec["freed_bytes"].as_u64().expect("bytes on the record");
        assert!(bytes >= 64 * 1024, "{rec}");
        assert_eq!(rec["path"], target.display().to_string());
        assert_eq!(rec["branch"], "fix/x");
        assert_eq!(rec["at"], "2026-09-24T06:00:00Z");
        assert!(line.contains(&bytes.to_string()), "{line}");
        assert!(line.contains(&target.display().to_string()), "{line}");
    }
}
