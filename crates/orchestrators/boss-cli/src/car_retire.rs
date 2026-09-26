//! `boss car retire <car> --carried-by <landed car | merge sha>` and
//! `boss car retire <car> --superseded-by <car>` — close a car whose
//! work another car carries, on the evidence, through its terminal
//! (backlog 87f1c86a).
//!
//! WHY. On 2026-09-24 four parked cars were twins: car D's commit rode
//! inside car E, the two device-shop cars inside the refurb car — all
//! landed in train #634 — and an old car G was replaced by its rebuild.
//! They were held by hand so the conductor would stop leaving them
//! behind every train, and then nothing could close them: `boss car`
//! had `open` and `waits-on`, `boss merged` answered UNKNOWN, `boss
//! rerail` refuses a vanished branch. The dock read HELD 4 and the
//! garage stayed amber for as long as nobody closed them by hand.
//!
//! WHAT COUNTS AS CARRIED — MEASURED ON THOSE CARS, NOT ASSUMED. The
//! obvious proof is patch-id: a cherry-pick has a new sha and the same
//! patch-id. It proved ONE of the four twin commits. The other three
//! were replayed with a conflict resolved — car D's `MapPage.svelte`
//! against car E's, the device-shop engine's
//! `tenant-vocabulary.baseline` counts against the refurb car's — so
//! their patches differ in exactly one file each, and a patch-id-only
//! verb would have refused every car it was built for. What a replay
//! keeps is the AUTHORED commit: author, author time to the second, and
//! subject all survive `cherry-pick` and `rebase`. So a commit is
//! carried when the carrier holds its patch-id (identical), or holds
//! the same authored commit (a replay) — and a replay is accepted only
//! when the operator says `--accept-replay`, after the verb has named
//! each file whose patch differs. The machine can prove identity; that
//! a resolution kept the work is a judgement, and it goes on the record
//! as one. A commit matching neither is refused by name.
//!
//! UNLESS THE WHOLE TREE PROVES IT (backlog 2b198cac). A carrier built on
//! top of the car and squashed holds the car's diff inside a commit of
//! its own, so no commit of the car can match — car 8df80582 was refused
//! NOT CARRIED on 2026-09-25 with every line it added on main. For a
//! commit nothing matches, the verb merges the car's head into main
//! (`git merge-tree --write-tree`): when that writes main's own tree, the
//! car's net work is on main and the machine says so. When it does not,
//! the conflicting and differing files are named and only
//! `--accept-net-diff` retires the car, recorded as a judgement.
//!
//! THE CARRIER MUST HAVE LANDED, OBSERVED. A branch resolves to its
//! landed car; a merge sha to the landed cars that record it as their
//! `merge_ref`. Either way the merge is checked to be an ancestor of
//! the forge's main, and the carrier's commits are read from the head
//! it BOARDED — the exact tree the train carried.
//!
//! SUPERSEDED is a different fact and a different terminal: the car's
//! own work never landed, another car replaced it. That is `abandoned`,
//! with the successor named — and the successor must exist, be live or
//! landed, and name the same item.
//!
//! The writes themselves are `boss_jobs::car_retire::retire_writes`,
//! pinned against the real router in
//! `crates/core/boss-jobs/tests/ship_a_change_landed_twin.rs`.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use boss_jobs::car;
use boss_jobs::car_retire::{self, Retirement};
use serde_json::Value;

/// One commit, as the judgement reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Commit {
    pub sha: String,
    /// `git patch-id --stable`; `None` for a commit with no diff.
    pub patch_id: Option<String>,
    /// `Name <email> <author epoch>` — what a replay keeps.
    pub authored: String,
    pub subject: String,
}

impl Commit {
    fn short(&self) -> &str {
        &self.sha[..8.min(self.sha.len())]
    }
}

/// How one of the car's commits is carried, or that it is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Carried {
    /// The carrier holds the same patch.
    Identical { twin: Commit, carrier: String },
    /// The carrier holds the same AUTHORED commit with a different
    /// patch — a replay that resolved a conflict. `differing` is filled
    /// by the caller (it needs git) before [`verdict`] reads it.
    Replayed {
        twin: Commit,
        carrier: String,
        differing: Vec<String>,
    },
    /// No carrier commit holds it, but merging the car's head into main
    /// writes main's OWN tree: the car's net work is on main, in whatever
    /// commit carried it. Proven by the machine (backlog 2b198cac).
    InMainTree { twin: Commit, tree: String },
    /// No carrier commit holds it, and merging the car's head into main
    /// does NOT write main's tree — `files` name where it conflicts or
    /// differs. Accepted only with `--accept-net-diff`.
    NetDiffers { twin: Commit, files: Vec<String> },
    /// Neither, and the whole tree was not read.
    Missing(Commit),
}

/// What merging the car's head into the forge's main writes — `git
/// merge-tree --write-tree main head` — against main's own tree. The
/// whole-tree reading (backlog 2b198cac).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WholeTree {
    pub main_tree: String,
    pub merged_tree: String,
    /// The paths merge-tree could not merge.
    pub conflicted: Vec<String>,
    /// The paths whose merged content is not main's.
    pub differing: Vec<String>,
}

impl WholeTree {
    /// Merging the car into main changes nothing: every file either the
    /// car left alone, or main already holds the car's change to it.
    pub(crate) fn on_main(&self) -> bool {
        self.conflicted.is_empty() && self.merged_tree == self.main_tree
    }

    /// Every path that stops the proof, a conflict marked as one.
    fn files(&self) -> Vec<String> {
        let all: BTreeSet<&String> = self.conflicted.iter().chain(&self.differing).collect();
        all.into_iter()
            .map(|f| {
                if self.conflicted.contains(f) {
                    format!("{f} (conflict)")
                } else {
                    f.clone()
                }
            })
            .collect()
    }
}

/// PURE: how each of `twin` is carried by `carrier`. Patch-id first —
/// the strong match — then the authored identity, then, for a commit
/// neither finds, the whole tree when it was read.
///
/// THE WHOLE-TREE ARM (backlog 2b198cac). A carrier built ON TOP of a car
/// and squashed carries the car's diff inside the carrier's own commit,
/// with its own patch and its own authorship, so no per-commit match can
/// exist — car 8df80582 sat on the dock as CONFLICTS WITH MAIN with its
/// every added line on main. What does prove it is main itself: when the
/// three-way merge of the car's head into main writes main's own tree,
/// the car holds nothing main lacks. A reverse-apply of the car's diff
/// was measured and REJECTED as the proof: main had moved around two of
/// the car's hunks, and `git apply --check -R` failed on work that was
/// there. When the merge is not main's tree, the machine cannot tell a
/// rewrap from an unlanded line, so it names the files and the operator
/// judges — the `--accept-replay` shape.
pub(crate) fn judge(
    twin: &[Commit],
    carrier: &[Commit],
    whole: Option<&WholeTree>,
) -> Vec<Carried> {
    twin.iter()
        .map(|t| {
            if let Some(c) = carrier
                .iter()
                .find(|c| t.patch_id.is_some() && c.patch_id == t.patch_id)
            {
                return Carried::Identical {
                    twin: t.clone(),
                    carrier: c.sha.clone(),
                };
            }
            match carrier
                .iter()
                .find(|c| c.authored == t.authored && c.subject == t.subject)
            {
                Some(c) => Carried::Replayed {
                    twin: t.clone(),
                    carrier: c.sha.clone(),
                    differing: Vec::new(),
                },
                None => match whole {
                    Some(w) if w.on_main() => Carried::InMainTree {
                        twin: t.clone(),
                        tree: w.main_tree.clone(),
                    },
                    Some(w) => Carried::NetDiffers {
                        twin: t.clone(),
                        files: w.files(),
                    },
                    None => Carried::Missing(t.clone()),
                },
            }
        })
        .collect()
}

/// PURE: accept the judgement, or refuse naming every commit that is
/// not carried — every replay, with the files whose patch differs,
/// unless the operator has accepted replays — and every commit whose
/// net work the whole tree could not prove, with the files, unless the
/// operator has accepted the net diff.
pub(crate) fn verdict(
    judged: &[Carried],
    accept_replay: bool,
    accept_net_diff: bool,
) -> Result<(), String> {
    if judged.is_empty() {
        return Err("the car has no commits to prove carried".into());
    }
    let missing: Vec<String> = judged
        .iter()
        .filter_map(|c| match c {
            Carried::Missing(t) => Some(format!("  {} {}", t.short(), t.subject)),
            _ => None,
        })
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} of the car's {} commit(s) are NOT carried — neither their patch nor the same \
             authored commit is in the carrier:\n{}\nRetiring it would close a car whose work \
             is not on main.",
            missing.len(),
            judged.len(),
            missing.join("\n")
        ));
    }
    let replays: Vec<String> = judged
        .iter()
        .filter_map(|c| match c {
            Carried::Replayed {
                twin,
                carrier,
                differing,
            } => Some(format!(
                "  {} replayed as {} — patch differs in: {}",
                twin.short(),
                &carrier[..8.min(carrier.len())],
                if differing.is_empty() {
                    "(no single file; the whole-commit patch differs)".to_string()
                } else {
                    differing.join(", ")
                }
            )),
            _ => None,
        })
        .collect();
    if !replays.is_empty() && !accept_replay {
        return Err(format!(
            "every commit is carried, but {} as a REPLAY — the same authored commit with a \
             different patch, which is what a conflict resolved on replay looks like:\n{}\n\
             The machine can prove it is the same commit; whether the resolution kept the \
             work is yours to judge. Compare those files, then re-run with --accept-replay \
             and the acceptance is recorded with the evidence.",
            replays.len(),
            replays.join("\n")
        ));
    }
    let unproven: Vec<(String, &[String])> = judged
        .iter()
        .filter_map(|c| match c {
            Carried::NetDiffers { twin, files } => Some((
                format!("  {} {}", twin.short(), twin.subject),
                files.as_slice(),
            )),
            _ => None,
        })
        .collect();
    if let Some((_, files)) = unproven.first()
        && !accept_net_diff
    {
        let commits: Vec<&str> = unproven.iter().map(|(c, _)| c.as_str()).collect();
        return Err(format!(
            "{} of the car's {} commit(s) match no carrier commit, by patch or by authored \
             commit:\n{}\nand merging the car's head into main does NOT write main's own tree — \
             it {} in: {}\nThe machine cannot tell a line main rewrote from a line that never \
             landed. Compare those files against main; if the car's work is there, re-run \
             with --accept-net-diff and the acceptance is recorded with the evidence.",
            unproven.len(),
            judged.len(),
            commits.join("\n"),
            if files.iter().any(|f| f.ends_with("(conflict)")) {
                "conflicts or differs"
            } else {
                "differs"
            },
            if files.is_empty() {
                "(no file named)".to_string()
            } else {
                files.join(", ")
            }
        ));
    }
    Ok(())
}

/// PURE: one commit's match, as the evidence and the terminal print it.
/// `replay_by` and `net_by` are who accepted each judgement, kept apart
/// so one acceptance never reads as the other.
fn match_line(c: &Carried, replay_by: Option<&str>, net_by: Option<&str>) -> String {
    match c {
        Carried::Identical { twin, carrier } => format!(
            "{} = {} (patch-id {})",
            twin.short(),
            &carrier[..8.min(carrier.len())],
            twin.patch_id
                .as_deref()
                .map(|p| &p[..8.min(p.len())])
                .unwrap_or("?")
        ),
        Carried::Replayed {
            twin,
            carrier,
            differing,
        } => format!(
            "{} ~ {} (same authored commit, replayed; patch differs in {}{})",
            twin.short(),
            &carrier[..8.min(carrier.len())],
            if differing.is_empty() {
                "the whole commit".to_string()
            } else {
                differing.join(", ")
            },
            replay_by
                .map(|a| format!("; accepted by {a}"))
                .unwrap_or_default()
        ),
        Carried::InMainTree { twin, tree } => format!(
            "{} in main's tree {} (no carrier commit holds its patch; merging the car's head \
             into main writes main's own tree)",
            twin.short(),
            &tree[..8.min(tree.len())]
        ),
        Carried::NetDiffers { twin, files } => format!(
            "{} matches no carrier commit, and merging the car's head into main differs in {}{}",
            twin.short(),
            files.join(", "),
            net_by
                .map(|a| format!("; net work judged on main and accepted by {a}"))
                .unwrap_or_default()
        ),
        Carried::Missing(t) => format!("{} NOT CARRIED {}", t.short(), t.subject),
    }
}

/// PURE: the evidence the terminal records — the carrier, where it
/// landed, and every commit's match.
pub(crate) fn evidence(
    carrier: &str,
    judged: &[Carried],
    replay_by: Option<&str>,
    net_by: Option<&str>,
) -> String {
    let lines: Vec<String> = judged
        .iter()
        .map(|c| match_line(c, replay_by, net_by))
        .collect();
    format!(
        "{} commit(s) carried by {carrier}: {}",
        judged.len(),
        lines.join("; ")
    )
}

/// The head a car carries: the head it BOARDED (what the train took),
/// else the head its receipt vouches for.
pub(crate) fn carried_head(c: &Value) -> Option<String> {
    crate::train::boarded_head(c)
        .map(str::to_string)
        .or_else(|| receipt_head(c))
}

fn receipt_head(c: &Value) -> Option<String> {
    crate::receipt::select_receipt(c)?
        .get("head")?
        .as_str()
        .filter(|h| !h.is_empty())
        .map(str::to_string)
}

fn md<'a>(c: &'a Value, k: &str) -> Option<&'a str> {
    c.pointer(&format!("/metadata/{k}"))
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn short_id(c: &Value) -> &str {
    let id = c.get("id").and_then(Value::as_str).unwrap_or("?");
    &id[..8.min(id.len())]
}

/// PURE: the landed cars `given` names — the car that landed that
/// branch, or (when `given` resolved to the commit `sha`) every landed
/// car whose `merge_ref` is that commit. A car for the branch that has
/// NOT landed is refused by name: a carrier must be on main.
pub(crate) fn carriers<'a>(
    cars: &'a [Value],
    given: &str,
    sha: Option<&str>,
) -> Result<Vec<&'a Value>, String> {
    if let Some(landed) = car::landed_car_for(cars, given) {
        return Ok(vec![landed]);
    }
    if let Some(unlanded) = cars.iter().find(|c| md(c, "branch") == Some(given)) {
        return Err(format!(
            "car {} for {given} has not landed ({}) — a carrier must already be on main",
            short_id(unlanded),
            unlanded
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("?")
        ));
    }
    let Some(sha) = sha else {
        return Err(format!(
            "`{given}` is neither the branch of a landed car nor a commit this clone can \
             read — name the carrier's branch, or the merge sha of the train that landed it"
        ));
    };
    let by_ref: Vec<&Value> = cars
        .iter()
        .filter(|c| car::is_landed(c))
        .filter(|c| md(c, "merge_ref").is_some_and(|r| r.len() >= 7 && sha.starts_with(r)))
        .collect();
    if by_ref.is_empty() {
        return Err(format!(
            "no landed car records {} as its merge — a merge sha names its carriers through \
             their `merge_ref`",
            &sha[..12.min(sha.len())]
        ));
    }
    Ok(by_ref)
}

/// PURE: the car `given` names among every car, open and closed — by
/// branch the live car (the one `boss gate` and the auto-park handler
/// would call the branch's car), else the landed one, else by id.
pub(crate) fn select_successor(cars: &[Value], given: &str) -> Result<Value> {
    if let Some(c) = car::open_car_for(cars, given).or_else(|| car::landed_car_for(cars, given)) {
        return Ok(c.clone());
    }
    let id = crate::park::resolve_job_id(cars, given)?;
    cars.iter()
        .find(|c| c.get("id").and_then(Value::as_str) == Some(id.as_str()))
        .cloned()
        .ok_or_else(|| anyhow!("no car is `{given}` — neither a live or landed branch nor an id"))
}

/// PURE: the evidence that `successor` replaces `twin`, or the refusal.
/// The successor must be another car, live or landed (not itself spent),
/// naming the same item under either item key.
pub(crate) fn judge_successor(twin: &Value, successor: &Value) -> Result<String, String> {
    if twin.get("id") == successor.get("id") {
        return Err("a car cannot supersede itself".into());
    }
    let live = car::is_open(successor);
    let landed = car::is_landed(successor);
    if !live && !landed {
        return Err(format!(
            "car {} is closed ({}) without landing — a spent car replaces nothing",
            short_id(successor),
            md(successor, "outcome").unwrap_or("no outcome recorded")
        ));
    }
    fn item(c: &Value) -> Option<(&'static str, &str)> {
        md(c, car::BACKLOG_ITEM)
            .map(|i| (car::BACKLOG_ITEM, i))
            .or_else(|| md(c, car::PARTIAL_ITEM).map(|i| (car::PARTIAL_ITEM, i)))
    }
    let (Some((_, mine)), Some((key, theirs))) = (item(twin), item(successor)) else {
        return Err(format!(
            "car {} and car {} do not both name an item (backlog_item or partial_item), so \
             nothing on the record says the one replaces the other",
            short_id(twin),
            short_id(successor)
        ));
    };
    if mine != theirs {
        return Err(format!(
            "car {} answers item {} but car {} answers {} — a successor names the same item",
            short_id(twin),
            &mine[..8.min(mine.len())],
            short_id(successor),
            &theirs[..8.min(theirs.len())]
        ));
    }
    Ok(format!(
        "replaced by car {} ({}, {}), which names the same item {} as its {key}",
        short_id(successor),
        md(successor, "branch").unwrap_or("?"),
        if landed { "landed" } else { "live" },
        &mine[..8.min(mine.len())]
    ))
}

// ----------------------------------------------------------------------
// Git: reads only. Run in the working directory's clone, like `boss
// merged`; every read that fails is a refusal, never a default.
// ----------------------------------------------------------------------

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = crate::git_auth::command()
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn have(repo: &Path, sha: &str) -> bool {
    git(repo, &["cat-file", "-e", &format!("{sha}^{{commit}}")]).is_ok()
}

/// `git patch-id --stable` of a commit, or of the part of it touching
/// `paths`. `None` for an empty diff, which has no patch-id.
fn patch_id(repo: &Path, sha: &str, paths: &[&str]) -> Result<Option<String>, String> {
    let mut args = vec![
        "show",
        "--no-color",
        "--no-ext-diff",
        "--format=",
        sha,
        "--",
    ];
    args.extend(paths);
    let out = crate::git_auth::command()
        .arg("-C")
        .arg(repo)
        .args(&args)
        .output()
        .map_err(|e| format!("git show {sha}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git show {sha}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let mut child = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["patch-id", "--stable"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("git patch-id: {e}"))?;
    // patch-id prints one short line per patch, so writing the whole
    // diff before reading cannot fill its output pipe.
    child
        .stdin
        .take()
        .ok_or("git patch-id: no stdin")?
        .write_all(&out.stdout)
        .map_err(|e| format!("git patch-id: {e}"))?;
    let done = child
        .wait_with_output()
        .map_err(|e| format!("git patch-id: {e}"))?;
    Ok(String::from_utf8_lossy(&done.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string))
}

/// Every non-merge commit reachable from `head` and not from `not`,
/// oldest first.
pub(crate) fn read_commits(repo: &Path, head: &str, not: &str) -> Result<Vec<Commit>, String> {
    let list = git(
        repo,
        &["rev-list", "--no-merges", "--reverse", head, "--not", not],
    )?;
    list.lines()
        .filter(|l| !l.is_empty())
        .map(|sha| {
            let who = git(repo, &["log", "-1", "--format=%an <%ae> %at%x00%s", sha])?;
            let (authored, subject) = who.split_once('\0').unwrap_or((who.as_str(), ""));
            Ok(Commit {
                sha: sha.to_string(),
                patch_id: patch_id(repo, sha, &[])?,
                authored: authored.to_string(),
                subject: subject.to_string(),
            })
        })
        .collect()
}

/// The files whose patch differs between two commits — each file's
/// patch-id compared, and a file only one of them touches counted too.
pub(crate) fn differing_files(repo: &Path, a: &str, b: &str) -> Result<Vec<String>, String> {
    let files = |sha: &str| -> Result<BTreeSet<String>, String> {
        Ok(git(
            repo,
            &["diff-tree", "--no-commit-id", "--name-only", "-r", sha],
        )?
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
    };
    let all: BTreeSet<String> = files(a)?.union(&files(b)?).cloned().collect();
    let mut out = Vec::new();
    for f in all {
        if patch_id(repo, a, &[&f])? != patch_id(repo, b, &[&f])? {
            out.push(f);
        }
    }
    Ok(out)
}

/// Merge the car's `head` into `main` in memory — `git merge-tree
/// --write-tree`, no checkout — and read the tree it writes against
/// main's own. Exit 0 is clean; exit 1 WITH a tree is a conflict; exit
/// 1 without one (a ref not there) and anything else is an error, never
/// a verdict — the dock preview's reading of the same command.
pub(crate) fn whole_tree(repo: &Path, main: &str, head: &str) -> Result<WholeTree, String> {
    let out = crate::git_auth::command()
        .arg("-C")
        .arg(repo)
        .args(["merge-tree", "--write-tree", "--name-only", main, head])
        .output()
        .map_err(|e| format!("git merge-tree {main} {head}: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let merged = stdout
        .lines()
        .next()
        .map(str::trim)
        .filter(|l| l.len() >= 40 && l.chars().all(|c| c.is_ascii_hexdigit()));
    let (merged_tree, conflicted) = match (out.status.code(), merged) {
        (Some(0), Some(tree)) => (tree.to_string(), Vec::new()),
        (Some(1), Some(tree)) => (
            tree.to_string(),
            crate::dock_preview::parse_conflicted_files(&stdout),
        ),
        _ => {
            return Err(format!(
                "git merge-tree {main} {head} errored (not a merge verdict): {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    };
    let main_tree = git(repo, &["rev-parse", &format!("{main}^{{tree}}")])?;
    let differing = git(
        repo,
        &["diff-tree", "-r", "--name-only", &main_tree, &merged_tree],
    )?
    .lines()
    .filter(|l| !l.is_empty())
    .map(str::to_string)
    .collect();
    Ok(WholeTree {
        main_tree,
        merged_tree,
        conflicted,
        differing,
    })
}

/// Fill each replay's differing files.
fn with_differences(repo: &Path, judged: Vec<Carried>) -> Result<Vec<Carried>, String> {
    judged
        .into_iter()
        .map(|c| match c {
            Carried::Replayed { twin, carrier, .. } => Ok(Carried::Replayed {
                differing: differing_files(repo, &twin.sha, &carrier)?,
                twin,
                carrier,
            }),
            other => Ok(other),
        })
        .collect()
}

/// The forge's main, with its objects here — fetched once if the clone
/// lags. The authority is the forge, read live (see `boss merged`).
fn forge_main(repo: &Path) -> Result<String> {
    let line = git(repo, &["ls-remote", "origin", "refs/heads/main"]).map_err(|e| anyhow!(e))?;
    let main = line
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("the forge answered no main — without it nothing can be proved"))?
        .to_string();
    if !have(repo, &main) {
        git(repo, &["fetch", "origin", "main"]).map_err(|e| anyhow!(e))?;
    }
    if !have(repo, &main) {
        bail!("main {main} is not readable in this clone even after a fetch");
    }
    Ok(main)
}

/// The head of the car being retired: the forge's branch head when the
/// branch is still there (every commit on it must be carried), else the
/// head its receipt vouches for.
fn twin_head(repo: &Path, twin: &Value, branch: &str) -> Result<String> {
    let forge = git(
        repo,
        &["ls-remote", "origin", &format!("refs/heads/{branch}")],
    )
    .ok()
    .and_then(|l| l.split_whitespace().next().map(str::to_string));
    let head = forge
        .clone()
        .or_else(|| receipt_head(twin))
        .ok_or_else(|| {
            anyhow!("{branch} is not on the forge and the car's receipt names no head")
        })?;
    if !have(repo, &head) && forge.is_some() {
        let _ = git(repo, &["fetch", "origin", branch]);
    }
    if !have(repo, &head) {
        bail!(
            "the car's head {head} is not readable in this clone — without its commits there \
             is nothing to prove carried"
        );
    }
    Ok(head)
}

/// Judge a `--carried-by` retirement: the carrier resolved and proved
/// landed, every commit of the car matched. Returns the evidence.
fn prove_carried(
    repo: &Path,
    cars: &[Value],
    twin: &Value,
    branch: &str,
    given: &str,
    accept_replay: bool,
    accept_net_diff: bool,
    actor: &str,
) -> Result<String> {
    let main = forge_main(repo)?;
    let sha = git(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{given}^{{commit}}"),
        ],
    )
    .ok();
    let found = carriers(cars, given, sha.as_deref()).map_err(|e| anyhow!(e))?;

    let mut carrier_commits = Vec::new();
    let mut landed_as = Vec::new();
    for c in &found {
        let merge_ref = md(c, "merge_ref")
            .ok_or_else(|| anyhow!("carrier car {} records no merge_ref", short_id(c)))?;
        let merge = git(
            repo,
            &["rev-parse", "--verify", &format!("{merge_ref}^{{commit}}")],
        )
        .map_err(|e| anyhow!("carrier car {}'s merge {merge_ref}: {e}", short_id(c)))?;
        if git(repo, &["merge-base", "--is-ancestor", &merge, &main]).is_err() {
            bail!(
                "carrier car {}'s merge {merge_ref} is not on the forge's main {} — a carrier \
                 must have landed",
                short_id(c),
                &main[..12]
            );
        }
        let head = carried_head(c).ok_or_else(|| {
            anyhow!(
                "carrier car {} records neither a boarded head nor a receipt",
                short_id(c)
            )
        })?;
        if !have(repo, &head) {
            bail!(
                "carrier car {}'s head {head} is not readable in this clone — its branch is \
                 gone from the forge, so only a clone that fetched it before can prove this",
                short_id(c)
            );
        }
        // The carrier's own commits: what its head holds that main did
        // not, the moment before its merge.
        carrier_commits
            .extend(read_commits(repo, &head, &format!("{merge}^1")).map_err(|e| anyhow!(e))?);
        landed_as.push(format!(
            "car {} ({}, merged as {merge_ref}, boarded {})",
            short_id(c),
            md(c, "branch").unwrap_or("?"),
            &head[..8.min(head.len())]
        ));
    }
    let carrier = landed_as.join(" + ");

    let head = twin_head(repo, twin, branch)?;
    let mine = read_commits(repo, &head, &main).map_err(|e| anyhow!(e))?;
    if mine.is_empty() {
        println!("  every commit of {head} is already an ancestor of main");
        return Ok(format!(
            "its head {head} is an ancestor of the forge's main {main}; carrier named: {carrier}"
        ));
    }
    // The whole tree is read only when a commit matches nothing per
    // commit: an identical or replayed car proves itself as it always did.
    let whole = if judge(&mine, &carrier_commits, None)
        .iter()
        .any(|c| matches!(c, Carried::Missing(_)))
    {
        let w = whole_tree(repo, &main, &head).map_err(|e| anyhow!(e))?;
        println!(
            "  whole tree: merging {} into main {} writes {} ({})",
            &head[..8.min(head.len())],
            &main[..8.min(main.len())],
            &w.merged_tree[..8.min(w.merged_tree.len())],
            if w.on_main() {
                "main's own tree".to_string()
            } else {
                format!("not main's {}", &w.main_tree[..8.min(w.main_tree.len())])
            }
        );
        Some(w)
    } else {
        None
    };
    let judged = with_differences(repo, judge(&mine, &carrier_commits, whole.as_ref()))
        .map_err(|e| anyhow!(e))?;
    println!("  carrier: {carrier}");
    for c in &judged {
        println!("  {}", match_line(c, None, None));
    }
    verdict(&judged, accept_replay, accept_net_diff)
        .map_err(|e| anyhow!("boss car retire: REFUSED — {e}"))?;
    Ok(evidence(
        &carrier,
        &judged,
        accept_replay.then_some(actor),
        accept_net_diff.then_some(actor),
    ))
}

/// `boss car retire`.
pub(crate) async fn retire(
    given: &str,
    carried_by: Option<&str>,
    superseded_by: Option<&str>,
    accept_replay: bool,
    accept_net_diff: bool,
    dry_run: bool,
) -> Result<()> {
    let http = reqwest::Client::new();
    // The actor FIRST: a write nobody names is refused, and the refusal
    // costs a line rather than a half-retired car (5083d6f5).
    let actor = if dry_run {
        "dry-run".to_string()
    } else {
        crate::identity::sign(&reqwest::Method::PATCH, "/api/jobs")?
    };
    let (twin, branch) = crate::rerail::find_car(&http, given).await?;
    let id = twin
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("car {branch} carries no id"))?
        .to_string();
    let cars = crate::gate::all_cars(&http).await?;
    println!("boss car retire: car {} ({branch})", &id[..8]);

    let retirement = match (carried_by, superseded_by) {
        (Some(by), None) => {
            let proof = prove_carried(
                Path::new("."),
                &cars,
                &twin,
                &branch,
                by,
                accept_replay,
                accept_net_diff,
                &actor,
            )?;
            Retirement::CarriedBy {
                by: by.to_string(),
                evidence: proof,
            }
        }
        (None, Some(by)) => {
            let successor = select_successor(&cars, by)?;
            let proof = judge_successor(&twin, &successor)
                .map_err(|e| anyhow!("boss car retire: REFUSED — {e}"))?;
            Retirement::SupersededBy {
                by: successor
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or(by)
                    .to_string(),
                evidence: proof,
            }
        }
        _ => bail!("name exactly one of --carried-by or --superseded-by"),
    };
    println!("  evidence: {}", evidence_of(&retirement));

    // A car pinned to a version without the terminal is MOVED first —
    // through the conversion door, which previews, refuses an unsafe
    // move by step, and records the re-pin on the packet (7cf202a9).
    let mut current = twin.clone();
    let slug = retirement.outcome_slug();
    if car::find_step(&current, slug, "").is_none() {
        println!(
            "  car {} is pinned to v{} with no `{slug}` terminal — converting it",
            &id[..8],
            current["workflow_version"]
        );
        crate::job::convert(&id, None, dry_run).await?;
        if dry_run {
            println!(
                "boss car retire: DRY — nothing written; after the conversion it closes \
                 through `{slug}`"
            );
            return Ok(());
        }
        current = read(&http, &id).await?;
    }
    let writes = car_retire::retire_writes(&current, &retirement)
        .map_err(|e| anyhow!("boss car retire: REFUSED — {e}"))?;
    if dry_run {
        println!(
            "boss car retire: DRY — would record the evidence on `{slug}`, {}set {}, and \
             complete `{slug}`",
            if writes.held_review.is_some() {
                "release the hold, "
            } else {
                ""
            },
            writes.marker
        );
        return Ok(());
    }
    apply(&http, &id, &current, &writes, slug, "boss car retire").await?;
    notes(&current, &retirement);
    Ok(())
}

fn evidence_of(r: &Retirement) -> &str {
    match r {
        Retirement::CarriedBy { evidence, .. } | Retirement::SupersededBy { evidence, .. } => {
            evidence
        }
    }
}

pub(crate) async fn read(http: &reqwest::Client, id: &str) -> Result<Value> {
    read_via(&Http(http), id).await
}

/// The jobs API as a car writer speaks to it: one call. The operator's
/// verbs answer it with their signed client ([`Http`], or the signed
/// [`crate::steps::Wire`]); the train conductor answers it with its own
/// blip-guarded client, signed as itself — which is how `boss car
/// unland`'s writer runs unchanged under the conductor's merge-lost arm
/// (backlog f9256445) rather than as a second copy of these writes.
#[async_trait::async_trait]
pub(crate) trait Door: Send + Sync {
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Option<Value>>;
}

/// The operator's door: `crate::gate::api`, signed as the actor running
/// the verb.
pub(crate) struct Http<'a>(pub(crate) &'a reqwest::Client);

#[async_trait::async_trait]
impl Door for Http<'_> {
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Option<Value>> {
        crate::gate::api(self.0, method, path, body).await
    }
}

/// The signed wire — the same actor rule as [`Http`], at an explicit
/// base, which is what lets a test drive the writer against a real
/// router on a local socket.
#[async_trait::async_trait]
impl Door for crate::steps::Wire {
    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<Option<Value>> {
        self.call(method, path, body).await
    }
}

pub(crate) async fn read_via(door: &dyn Door, id: &str) -> Result<Value> {
    door.send(reqwest::Method::GET, &format!("/api/jobs/{id}"), None)
        .await?
        .with_context(|| format!("car {id} read back empty"))
}

/// The writes, in `retire_writes`' order, then the read-back that is the
/// only confirmation: a 204 is a claim, the closed packet is the fact.
/// Shared with `boss prove --disproved` (08664157), whose writes are the
/// same three in the same order; `verb` is who prints the confirmation.
pub(crate) async fn apply(
    http: &reqwest::Client,
    id: &str,
    car_json: &Value,
    w: &car_retire::RetireWrites,
    slug: &str,
    verb: &str,
) -> Result<()> {
    apply_via(&Http(http), id, car_json, w, slug, verb).await
}

/// [`apply`] through any [`Door`] — `boss car unland` and the
/// conductor's merge-lost arm close a car through `unlanded` with these
/// same writes (backlog f9256445).
pub(crate) async fn apply_via(
    door: &dyn Door,
    id: &str,
    car_json: &Value,
    w: &car_retire::RetireWrites,
    slug: &str,
    verb: &str,
) -> Result<()> {
    use reqwest::Method;
    door.send(
        Method::PATCH,
        &w.outcome.merge_path(id),
        Some(w.outcome.metadata.clone()),
    )
    .await
    .context("recording the evidence on the terminal")?;
    let held = w.held_review.as_deref();
    if let Some(review) = held {
        door.send(
            Method::PATCH,
            &format!("/api/jobs/{id}/steps/{review}/metadata"),
            Some(crate::steps::release_patch_body()),
        )
        .await
        .context("releasing the hold")?;
    }
    if let Err(e) = door
        .send(
            Method::PATCH,
            &format!("/api/jobs/{id}/metadata"),
            Some(w.marker.clone()),
        )
        .await
    {
        // Put the brake back on: a released duplicate with no terminal
        // would board the next train and red it on an empty diff.
        if let (Some(review), Some(reason)) = (
            held,
            car::find_step(car_json, car::REVIEW_SLUG, car::REVIEW)
                .and_then(|s| s.pointer("/metadata/hold"))
                .and_then(Value::as_str),
        ) {
            let _ = door
                .send(
                    Method::PATCH,
                    &format!("/api/jobs/{id}/steps/{review}/metadata"),
                    Some(crate::steps::hold_patch(reason)),
                )
                .await;
        }
        return Err(e.context("setting the terminal's marker (the hold was put back)"));
    }
    // The marker readied the terminal; the dispatcher completes a ready
    // outcome on its own, so it may already be done. Complete it here
    // only if it is not — and judge by the read-back either way.
    let marked = read_via(door, id).await?;
    let done = |c: &Value| {
        car::find_step(c, slug, "")
            .and_then(|s| s.get("status"))
            .and_then(Value::as_str)
            == Some("completed")
    };
    if !done(&marked)
        && let Err(e) = door
            .send(
                Method::PUT,
                &w.outcome.status_path(id),
                Some(w.outcome.status_body.clone()),
            )
            .await
    {
        println!("  completing `{slug}` answered {e} — reading the car to see who did");
    }
    let after = read_via(door, id).await?;
    let outcome = after.pointer("/metadata/outcome").and_then(Value::as_str);
    if after.get("status").and_then(Value::as_str) != Some("closed") || outcome != Some(slug) {
        bail!(
            "the writes were answered but car {id} reads {} with outcome {} — not closed \
             through `{slug}`. Nothing was rolled back: read the packet.",
            after.get("status").and_then(Value::as_str).unwrap_or("?"),
            outcome.unwrap_or("none")
        );
    }
    println!(
        "{verb}: car {} closed through `{slug}`{} — confirmed by reading it back",
        &id[..8.min(id.len())],
        if held.is_some() {
            ", hold released"
        } else {
            ""
        }
    );
    Ok(())
}

/// What a retirement does NOT do, said rather than left to be found.
fn notes(c: &Value, r: &Retirement) {
    if let Some(item) = md(c, car::BACKLOG_ITEM) {
        match r {
            Retirement::CarriedBy { .. } => println!(
                "  note: this car named item {} as its closing edge. The arrival rule fires on \
                 `merged` only, so the item is NOT closed by this — it closes with its \
                 carrier's, or through its own triage.",
                &item[..8.min(item.len())]
            ),
            Retirement::SupersededBy { .. } => println!(
                "  note: item {} rides on with the successor.",
                &item[..8.min(item.len())]
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn commit(sha: &str, patch: Option<&str>, authored: &str, subject: &str) -> Commit {
        Commit {
            sha: sha.into(),
            patch_id: patch.map(str::to_string),
            authored: authored.into(),
            subject: subject.into(),
        }
    }

    const DAVID: &str = "David Auld <d@x> 1790262633";

    /// THE FOUR CARS' SHAPES, MEASURED 2026-09-24. One commit whose
    /// patch the carrier holds (the device-shop residue), one replayed
    /// with a conflict resolved (car D inside car E), one not carried.
    #[test]
    fn a_commit_is_carried_by_patch_or_by_its_authored_identity() {
        let twin = [
            commit("5cca34cc", Some("p-residue"), DAVID, "Retire the residue"),
            commit(
                "697951cd",
                Some("p-d"),
                DAVID,
                "Give each IT region its own page",
            ),
            commit(
                "deadbeef",
                Some("p-x"),
                DAVID,
                "Something only this car did",
            ),
        ];
        let carrier = [
            commit("91e5d92e", Some("p-residue"), DAVID, "Retire the residue"),
            commit(
                "2213c8f7",
                Some("p-d-resolved"),
                DAVID,
                "Give each IT region its own page",
            ),
        ];
        let judged = judge(&twin, &carrier, None);
        assert!(matches!(&judged[0], Carried::Identical { carrier, .. } if carrier == "91e5d92e"));
        assert!(matches!(&judged[1], Carried::Replayed { carrier, .. } if carrier == "2213c8f7"));
        assert!(matches!(&judged[2], Carried::Missing(c) if c.sha == "deadbeef"));
    }

    /// A different author, or the same author at a different second, is
    /// a different commit — the identity match is exact.
    #[test]
    fn a_similar_commit_by_subject_alone_is_not_carried() {
        let twin = [commit("aaaa0001", Some("p1"), DAVID, "Fix the thing")];
        for other in [
            commit(
                "bbbb0001",
                Some("p2"),
                "David Auld <d@x> 1790262634",
                "Fix the thing",
            ),
            commit(
                "bbbb0002",
                Some("p2"),
                "Someone <s@x> 1790262633",
                "Fix the thing",
            ),
        ] {
            assert!(matches!(
                judge(&twin, &[other], None)[0],
                Carried::Missing(_)
            ));
        }
        // No patch at all never matches by patch-id.
        let empty = [commit("aaaa0002", None, DAVID, "Empty")];
        let other = [commit("bbbb0003", None, "X <x@x> 1", "Other")];
        assert!(matches!(
            judge(&empty, &other, None)[0],
            Carried::Missing(_)
        ));
    }

    /// NO EVIDENCE IS NOT A PASS: a missing commit refuses, by name; a
    /// replay refuses until accepted, naming the files; an empty list
    /// refuses.
    #[test]
    fn the_verdict_refuses_a_missing_commit_and_an_unaccepted_replay() {
        let t = commit("697951cd11", Some("p"), DAVID, "Give each region a page");
        let missing = [Carried::Missing(t.clone())];
        let e = verdict(&missing, true, false).unwrap_err();
        assert!(e.contains("697951cd Give each region a page"), "{e}");
        assert!(e.contains("NOT carried"), "{e}");

        let replay = [Carried::Replayed {
            twin: t.clone(),
            carrier: "2213c8f7aa".into(),
            differing: vec!["apps/web/src/it/yard/MapPage.svelte".into()],
        }];
        let e = verdict(&replay, false, false).unwrap_err();
        assert!(e.contains("MapPage.svelte"), "{e}");
        assert!(e.contains("--accept-replay"), "{e}");
        assert!(verdict(&replay, true, false).is_ok());

        let identical = [Carried::Identical {
            twin: t,
            carrier: "91e5d92e".into(),
        }];
        assert!(verdict(&identical, false, false).is_ok());
        assert!(verdict(&[], true, false).is_err());
    }

    /// The evidence names the carrier and every commit's match — and
    /// who accepted a replay.
    #[test]
    fn the_evidence_names_each_match_and_who_accepted_a_replay() {
        let t = commit("697951cd11", Some("p-abcdef12"), DAVID, "s");
        let e = evidence(
            "car 6a113231 (feat/e, merged as d013ec41e555, boarded ed8a341c)",
            &[
                Carried::Identical {
                    twin: t.clone(),
                    carrier: "91e5d92e".into(),
                },
                Carried::Replayed {
                    twin: t,
                    carrier: "2213c8f7".into(),
                    differing: vec!["MapPage.svelte".into()],
                },
            ],
            Some("claude@algedonic.dev"),
            None,
        );
        assert!(e.starts_with("2 commit(s) carried by car 6a113231"), "{e}");
        assert!(e.contains("697951cd = 91e5d92e (patch-id p-abcdef)"), "{e}");
        assert!(
            e.contains("697951cd ~ 2213c8f7 (same authored commit, replayed; patch differs in MapPage.svelte; accepted by claude@algedonic.dev)"),
            "{e}"
        );
    }

    fn landed(id: &str, branch: &str, merge_ref: &str) -> Value {
        json!({"id": id, "status": "closed",
               "metadata": {"branch": branch, "outcome": "merged", "merged": "true",
                            "merge_ref": merge_ref, "boarded_head": "ed8a341c"}})
    }

    /// A carrier by branch is its landed car; by merge sha, every landed
    /// car that records it. A car that has not landed is refused.
    #[test]
    fn a_carrier_is_a_landed_car_named_by_branch_or_by_its_merge() {
        let cars = [
            landed("6a113231-e", "feat/e", "d013ec41e555"),
            landed("8755e8ce-r", "feat/refurb", "d013ec41e555"),
            landed("a2838fe0-o", "feat/older", "f170eb49548c"),
            json!({"id": "d4439bcf-g", "status": "open", "metadata": {"branch": "feat/g"}}),
        ];
        let by_branch = carriers(&cars, "feat/e", None).unwrap();
        assert_eq!(by_branch.len(), 1);
        assert_eq!(by_branch[0]["id"], "6a113231-e");

        let sha = "d013ec41e5550000000000000000000000000000";
        let by_sha = carriers(&cars, "d013ec41", Some(sha)).unwrap();
        assert_eq!(by_sha.len(), 2, "both cars of the train that landed it");

        let e = carriers(&cars, "feat/g", None).unwrap_err();
        assert!(e.contains("has not landed"), "{e}");
        let e = carriers(&cars, "feat/nowhere", None).unwrap_err();
        assert!(e.contains("neither the branch of a landed car"), "{e}");
        let e = carriers(&cars, "abc", Some("abc1230000")).unwrap_err();
        assert!(e.contains("no landed car records"), "{e}");
    }

    /// The carrier's commits are read from the head it BOARDED.
    #[test]
    fn a_carriers_head_is_the_head_it_boarded() {
        assert_eq!(
            carried_head(&landed("x", "feat/e", "d013")).as_deref(),
            Some("ed8a341c")
        );
    }

    fn open_car(id: &str, branch: &str, item: Option<(&str, &str)>) -> Value {
        let mut c = json!({"id": id, "status": "open", "metadata": {"branch": branch}});
        if let Some((k, v)) = item {
            c["metadata"][k] = json!(v);
        }
        c
    }

    /// CAR G's SHAPE: replaced by its rebuild, the same partial item.
    #[test]
    fn a_successor_is_another_live_or_landed_car_for_the_same_item() {
        let item = ("partial_item", "c3105b2a-001d-4ae4-a172-5b7f8aba2598");
        let g = open_car("6c7c75f7-old", "feat/it-phone-strip-map", Some(item));
        let g2 = open_car("d4439bcf-new", "feat/it-phone-strip-map-2", Some(item));
        let ev = judge_successor(&g, &g2).unwrap();
        assert!(ev.contains("d4439bcf"), "{ev}");
        assert!(ev.contains("c3105b2a"), "{ev}");
        assert!(ev.contains("live"), "{ev}");

        assert!(judge_successor(&g, &g).is_err(), "not itself");
        let other = open_car(
            "e0000000-x",
            "feat/x",
            Some(("backlog_item", "99999999-0000")),
        );
        let e = judge_successor(&g, &other).unwrap_err();
        assert!(e.contains("same item"), "{e}");
        let none = open_car("f0000000-y", "feat/y", None);
        assert!(judge_successor(&g, &none).is_err());
        let mut spent = g2.clone();
        spent["status"] = json!("closed");
        spent["metadata"]["outcome"] = json!("abandoned");
        let e = judge_successor(&g, &spent).unwrap_err();
        assert!(e.contains("spent car"), "{e}");
    }

    /// A REAL REPOSITORY, the measured shape: a twin branch of two
    /// commits; a carrier that cherry-picked the first unchanged and
    /// replayed the second with a conflict resolved in one file; a train
    /// that squash-merged the carrier. The first is carried by patch,
    /// the second by identity with that one file named — and a third
    /// commit nobody carried is missing.
    #[test]
    fn a_real_replay_is_read_from_git_as_the_same_commit_with_one_file_different() {
        let root = boss_testing::scratch::scratch_dir("car-retire-replay");
        let g = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let w = |rel: &str, body: &str| boss_testing::scratch::write_file(&root.join(rel), body);
        g(&["init", "-q", "-b", "main"]);
        w("base.rs", "base\n");
        w("map.svelte", "one\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "base"]);

        g(&["checkout", "-q", "-b", "twin"]);
        w("region.ts", "export const r = 1;\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "twin: region page"]);
        w("map.svelte", "one\ntwin\n");
        w("board.svelte", "board\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "twin: mount it"]);
        let twin_head = g(&["rev-parse", "HEAD"]);
        w("orphan.rs", "only here\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "twin: nobody carried this"]);
        let orphan_head = g(&["rev-parse", "HEAD"]);

        g(&["checkout", "-q", "-b", "carrier", "main"]);
        w("map.svelte", "one\ncarrier\n");
        g(&["commit", "-qam", "carrier: its own change"]);
        g(&["cherry-pick", &format!("{twin_head}~1")]);
        // The replay: the second commit conflicts on map.svelte, and the
        // resolution keeps both lines. --amend keeps the authored commit.
        let pick = std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["cherry-pick", &twin_head])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("git runs");
        assert!(!pick.status.success(), "the replay conflicts, as measured");
        w("map.svelte", "one\ncarrier\ntwin\n");
        g(&["add", "map.svelte"]);
        std::process::Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(["-c", "core.editor=true", "cherry-pick", "--continue"])
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .env("GIT_EDITOR", "true")
            .output()
            .expect("git runs");
        let carrier_head = g(&["rev-parse", "HEAD"]);
        assert_ne!(carrier_head, twin_head);

        g(&["checkout", "-q", "main"]);
        g(&["merge", "-q", "--squash", "carrier"]);
        g(&["commit", "-qm", "train: carrier lands"]);
        let main = g(&["rev-parse", "HEAD"]);

        let repo = root.as_path();
        let mine = read_commits(repo, &orphan_head, &main).unwrap();
        assert_eq!(mine.len(), 3, "{mine:?}");
        let theirs = read_commits(repo, &carrier_head, &format!("{main}^1")).unwrap();
        assert_eq!(theirs.len(), 3, "{theirs:?}");

        let judged = with_differences(repo, judge(&mine, &theirs, None)).unwrap();
        assert!(
            matches!(&judged[0], Carried::Identical { .. }),
            "{judged:?}"
        );
        match &judged[1] {
            Carried::Replayed { differing, .. } => {
                assert_eq!(differing, &vec!["map.svelte".to_string()], "{judged:?}")
            }
            other => panic!("the conflicted pick is a replay: {other:?}"),
        }
        assert!(matches!(&judged[2], Carried::Missing(c) if c.sha == orphan_head));
        let e = verdict(&judged, true, false).unwrap_err();
        assert!(e.contains("twin: nobody carried this"), "{e}");
        // Without the orphan, the replay alone asks for acceptance.
        let e = verdict(&judged[..2], false, false).unwrap_err();
        assert!(e.contains("map.svelte"), "{e}");
        assert!(verdict(&judged[..2], true, false).is_ok());
    }

    fn whole(on_main: bool, conflicted: &[&str], differing: &[&str]) -> WholeTree {
        WholeTree {
            main_tree: "9c5066b4aaaa".into(),
            merged_tree: if on_main {
                "9c5066b4aaaa".into()
            } else {
                "1234abcd0000".into()
            },
            conflicted: conflicted.iter().map(|s| s.to_string()).collect(),
            differing: differing.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// CAR 8df80582's SHAPE, MEASURED 2026-09-25 (backlog 2b198cac): the
    /// carrier squashed the car's diff into its OWN commit, so no carrier
    /// commit holds either car commit's patch or authored identity. When
    /// merging the car's head into main writes main's own tree, the car's
    /// net work is on main — the machine proves it, no flag asked.
    #[test]
    fn a_commit_no_carrier_commit_holds_is_carried_when_merging_the_car_writes_mains_tree() {
        let twin = [
            commit(
                "270b19fb",
                Some("p-freeze"),
                DAVID,
                "A terminal step freezes",
            ),
            commit(
                "52bcb1a9",
                Some("p-actor"),
                DAVID,
                "A completion names its actor",
            ),
        ];
        let carrier = [commit(
            "e341f7cd",
            Some("p-cas"),
            DAVID,
            "A stale step write is refused by row version",
        )];
        // Without the whole tree, nothing matches — the refusal that
        // stranded the car.
        assert!(
            judge(&twin, &carrier, None)
                .iter()
                .all(|c| matches!(c, Carried::Missing(_)))
        );
        let on_main = whole(true, &[], &[]);
        let judged = judge(&twin, &carrier, Some(&on_main));
        assert!(
            judged
                .iter()
                .all(|c| matches!(c, Carried::InMainTree { tree, .. } if tree == "9c5066b4aaaa")),
            "{judged:?}"
        );
        assert!(verdict(&judged, false, false).is_ok());
        let e = evidence("car 6ec22d71", &judged, None, None);
        assert!(e.contains("270b19fb in main's tree 9c5066b4"), "{e}");
        assert!(e.contains("writes main's own tree"), "{e}");

        // The per-commit proof still wins where it exists: a commit the
        // carrier holds by patch stays Identical.
        let mixed = judge(
            &twin,
            &[commit("91e5d92e", Some("p-freeze"), DAVID, "x")],
            Some(&on_main),
        );
        assert!(matches!(&mixed[0], Carried::Identical { .. }), "{mixed:?}");
        assert!(matches!(&mixed[1], Carried::InMainTree { .. }), "{mixed:?}");
    }

    /// NO EVIDENCE IS NOT A PASS: when the merge conflicts, or is clean
    /// but writes a tree that is not main's, the machine cannot prove the
    /// net work landed. The refusal names every file and the flag; the
    /// flag's acceptance goes on the record with who gave it.
    #[test]
    fn a_car_whose_merge_into_main_differs_is_refused_until_the_operator_accepts_the_files() {
        let twin = [commit(
            "52bcb1a9",
            Some("p"),
            DAVID,
            "A completion names its actor",
        )];
        let differs = whole(
            false,
            &["crates/core/boss-dispatcher/src/dispatcher.rs"],
            &["crates/core/boss-dispatcher/src/dispatcher.rs", "orphan.rs"],
        );
        let judged = judge(&twin, &[], Some(&differs));
        match &judged[0] {
            Carried::NetDiffers { files, .. } => assert_eq!(
                files,
                &vec![
                    "crates/core/boss-dispatcher/src/dispatcher.rs (conflict)".to_string(),
                    "orphan.rs".to_string()
                ]
            ),
            other => panic!("a differing merge is NetDiffers: {other:?}"),
        }
        let e = verdict(&judged, true, false).unwrap_err();
        assert!(e.contains("dispatcher.rs (conflict)"), "{e}");
        assert!(e.contains("orphan.rs"), "{e}");
        assert!(e.contains("--accept-net-diff"), "{e}");
        assert!(e.contains("52bcb1a9 A completion names its actor"), "{e}");
        // --accept-replay is a different judgement and does not stand in.
        assert!(verdict(&judged, true, false).is_err());
        assert!(verdict(&judged, false, true).is_ok());

        let ev = evidence("car 6ec22d71", &judged, None, Some("claude@algedonic.dev"));
        assert!(ev.contains("orphan.rs"), "{ev}");
        assert!(ev.contains("accepted by claude@algedonic.dev"), "{ev}");
        // Accepting a replay does not read as accepting the net diff.
        let ev = evidence("car 6ec22d71", &judged, Some("claude@algedonic.dev"), None);
        assert!(!ev.contains("accepted by"), "{ev}");
    }

    fn fixture(name: &str) -> (std::path::PathBuf, impl Fn(&[&str]) -> String) {
        let root = boss_testing::scratch::scratch_dir(name);
        let at = root.clone();
        let g = move |args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(&at)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        (root, g)
    }

    /// A REAL REPOSITORY, the squashed-carrier shape of car 8df80582: a
    /// twin of two commits, the second rewriting part of the first; a
    /// carrier cut from main that holds the twin's NET content inside its
    /// own single commit; a train that squash-merged the carrier; then
    /// main moving on. No commit matches, and the whole tree proves it.
    /// Two controls, each a shape the arm must NOT bless: work nobody
    /// carried (a clean merge whose tree is not main's), and main having
    /// rewritten a line the twin added (a conflict) — each named by file.
    #[test]
    fn a_squashed_carrier_is_proven_by_the_whole_tree_and_unlanded_work_is_named() {
        let (root, g) = fixture("car-retire-squashed");
        let w = |rel: &str, body: &str| boss_testing::scratch::write_file(&root.join(rel), body);
        g(&["init", "-q", "-b", "main"]);
        w("steps.rs", "fn a() {}\n");
        w("dispatcher.rs", "use super::{a};\n\nfn d() {}\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "base"]);

        g(&["checkout", "-q", "-b", "twin"]);
        w("steps.rs", "fn a() {}\nfn freeze() { 1 }\n");
        w("steps_test.rs", "#[test] fn frozen() {}\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "twin: a terminal step freezes"]);
        w("steps.rs", "fn a() {}\nfn freeze() { 2 }\n");
        w("dispatcher.rs", "use super::{a, finished};\n\nfn d() {}\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "twin: a completion names its actor"]);
        let twin_head = g(&["rev-parse", "HEAD"]);

        // The carrier: cut from main, the twin's net content plus its own
        // change, ONE commit — what a squash onto the twin leaves.
        g(&["checkout", "-q", "-b", "carrier", "main"]);
        w("steps.rs", "fn a() {}\nfn freeze() { 2 }\n");
        w("steps_test.rs", "#[test] fn frozen() {}\n");
        w("dispatcher.rs", "use super::{a, finished};\n\nfn d() {}\n");
        w("postgres.rs", "fn cas() {}\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "carrier: a stale step write is refused"]);
        let carrier_head = g(&["rev-parse", "HEAD"]);
        g(&["checkout", "-q", "main"]);
        g(&["merge", "-q", "--squash", "carrier"]);
        g(&["commit", "-qm", "train: carrier lands"]);
        let merge = g(&["rev-parse", "HEAD"]);
        w("other.rs", "later\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "train: main moves on"]);
        let main = g(&["rev-parse", "HEAD"]);

        let repo = root.as_path();
        let mine = read_commits(repo, &twin_head, &main).unwrap();
        assert_eq!(mine.len(), 2, "{mine:?}");
        let theirs = read_commits(repo, &carrier_head, &format!("{merge}^1")).unwrap();
        assert_eq!(theirs.len(), 1, "{theirs:?}");
        assert!(
            judge(&mine, &theirs, None)
                .iter()
                .all(|c| matches!(c, Carried::Missing(_))),
            "per commit, nothing matches — the refusal that stranded the car"
        );

        let t = whole_tree(repo, &main, &twin_head).unwrap();
        assert!(t.on_main(), "{t:?}");
        assert_eq!(
            t.merged_tree,
            g(&["rev-parse", &format!("{main}^{{tree}}")])
        );
        let judged = judge(&mine, &theirs, Some(&t));
        assert!(
            judged
                .iter()
                .all(|c| matches!(c, Carried::InMainTree { .. })),
            "{judged:?}"
        );
        assert!(verdict(&judged, false, false).is_ok());

        // Control 1: a commit nobody carried. The merge is clean, and its
        // tree is not main's — the file is named, the flag is asked for.
        g(&["checkout", "-q", "twin"]);
        w("orphan.rs", "only here\n");
        g(&["add", "."]);
        g(&["commit", "-qm", "twin: nobody carried this"]);
        let orphan_head = g(&["rev-parse", "HEAD"]);
        let t = whole_tree(repo, &main, &orphan_head).unwrap();
        assert!(!t.on_main(), "{t:?}");
        assert!(t.conflicted.is_empty(), "{t:?}");
        assert_eq!(t.differing, vec!["orphan.rs".to_string()], "{t:?}");
        let mine = read_commits(repo, &orphan_head, &main).unwrap();
        let e = verdict(&judge(&mine, &theirs, Some(&t)), false, false).unwrap_err();
        assert!(e.contains("orphan.rs"), "{e}");
        assert!(e.contains("--accept-net-diff"), "{e}");

        // Control 2: main rewrapped the import the twin added (the
        // measured dispatcher.rs shape). The merge conflicts, by name.
        g(&["checkout", "-q", "main"]);
        w(
            "dispatcher.rs",
            "use super::{\n    a, finished, held,\n};\n\nfn d() {}\n",
        );
        g(&["commit", "-qam", "train: main rewraps the import"]);
        let main2 = g(&["rev-parse", "HEAD"]);
        let t = whole_tree(repo, &main2, &twin_head).unwrap();
        assert_eq!(t.conflicted, vec!["dispatcher.rs".to_string()], "{t:?}");
        let judged = judge(
            &read_commits(repo, &twin_head, &main2).unwrap(),
            &theirs,
            Some(&t),
        );
        let e = verdict(&judged, false, false).unwrap_err();
        assert!(e.contains("dispatcher.rs (conflict)"), "{e}");
        assert!(verdict(&judged, false, true).is_ok());

        // A ref that is not there is an error, never a verdict.
        assert!(whole_tree(repo, &main, "0000000000000000000000000000000000000000").is_err());
    }
}
