//! Is a branch's base current with `origin/main`? ONE definition, shared.
//!
//! Two verbs need the same answer and must not disagree about it
//! (CLAUDE.md §9a): `boss orient`'s FRESHNESS section, which names
//! parked/stranded cars whose base has fallen behind, and `boss gate`,
//! which refuses to spend a gate on a base that is not current. Before
//! this module the first existed and the second did not, and the gap is
//! exactly the window the measured defect lives in: a car that is
//! GATING and RE-GATING is neither parked nor stranded, so nothing
//! looked at its base at all (2026-09-11, backlog 31a28f49).
//!
//! ## What the hazard actually is
//!
//! Not a revert. `train.rs::rerail_onto_consist` rebases every car from
//! its merge-base onto the consist, so the deletions a stale branch's
//! `diff origin/main` shows can never board — that was measured on
//! 2026-08-29, and it is why a deletion count is a SCREEN and not a
//! verdict: one parked car showed 590 deletions against main and was
//! completely safe to merge, and acting on the count alone would have
//! force-pushed a healthy parked car into being unboardable.
//!
//! The hazard is **the receipt**. A gate judges the branch's tree in
//! isolation. If that tree is not the tree that will land, a green
//! vouches for a tree that will never exist — and the car parks with
//! evidence that means nothing. Train #244 went red that way on
//! 2026-09-07 with two innocent cars aboard: a car gated on a base taken
//! before another car landed, whose new test broke against a contract
//! the other car had tightened **in a different file**. Which settles the
//! shape of the test — see [`BaseObservation::untested`].
//!
//! ## Why `origin/main` and not `infra/lint/lib/trunk-ref.sh`
//!
//! That library walks `forge/main` → `origin/main` → `main` because a
//! LINT runs wherever the tree is checked out, and on a dev box `origin`
//! can be the GitHub mirror — a periodic backup that was 24 commits stale
//! the day it was written. `boss gate` is not in that position: it pushes
//! to, resolves against and fetches from `origin` in every other line of
//! its launch path (`resolve_sha`, `observe_landing`), and `boss orient`
//! reads `origin/main` too. Taking a different ref here would mean the
//! branch was gated against one trunk and judged fresh against another.
//! If `origin` ever stops being the forge for this verb, it changes in
//! one place for the whole verb, not for this module alone.

use std::path::Path;

use serde_json::{Value, json};

/// How many of the paths `origin/main` changed since the branch's base
/// get written down. The count is always exact; the list is a sample big
/// enough to act on and small enough to live in packet metadata.
pub(crate) const UNTESTED_SAMPLE: usize = 20;

/// What `git merge-base --is-ancestor origin/main <ref>` answered about
/// a branch's base.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Base {
    /// `origin/main` is an ancestor of the branch: it carries every
    /// landed change (exit 0).
    Current,
    /// Not an ancestor: the branch was cut from an older main (exit 1).
    Behind,
    /// git could not answer — a missing ref, a failed fetch, no git on
    /// the box. NOT a staleness finding, and the default for that
    /// reason. "Cannot read" is its own answer, and a gate that refused
    /// every branch because the forge blipped would stop all delivery,
    /// which is worse than the defect it guards (CLAUDE.md §Diagnosis;
    /// the `exit 3` convention in `infra/lint`).
    #[default]
    Unanswered,
}

/// The exit code of `git merge-base --is-ancestor origin/main <ref>`,
/// read the one way. `None` is "git did not run".
pub(crate) fn base_from_is_ancestor_code(code: Option<i32>) -> Base {
    match code {
        Some(0) => Base::Current,
        Some(1) => Base::Behind,
        _ => Base::Unanswered,
    }
}

/// Everything the launch path learned about the branch's base, in one
/// value so the refusal, the printed note and the packet stamp all read
/// the same observation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct BaseObservation {
    /// Current / Behind / Unanswered.
    pub(crate) standing: Base,
    /// `origin/main`'s sha, or empty when it could not be resolved.
    pub(crate) main_head: String,
    /// The branch's base — `merge-base(origin/main, origin/<branch>)`.
    pub(crate) base: String,
    /// Commits on `origin/main` the branch does not have.
    pub(crate) behind_by: usize,
    /// Paths `origin/main` changed since the branch's base: the files the
    /// gate is about to test at a version that is NOT the version that
    /// will land.
    ///
    /// Deliberately NOT "paths the branch's diff would delete". That
    /// narrower list looks more precise and is wrong twice over: a
    /// three-way merge keeps content the branch never touched, so the
    /// deletions are not a revert (2026-08-29); and #244's broken
    /// contract was in a file the victim car never touched, so a
    /// deletion-based list would not have named it. What makes a receipt
    /// untrustworthy is everything main changed, whether the branch went
    /// near it or not.
    pub(crate) untested: Vec<String>,
    /// Why git could not answer, when `standing` is `Unanswered`.
    pub(crate) unreadable: Option<String>,
}

/// One `git` invocation in `repo`, with the operator's own git config
/// kept out of the way. Returns the output, or the reason it could not
/// be had.
fn git(repo: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    crate::git_auth::command()
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git {} in {}: {e}", args.join(" "), repo.display()))
}

/// The trimmed stdout of a git command that had to succeed.
fn git_line(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = git(repo, args)?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("no stderr")
                .trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Read the branch's base standing out of a git repository.
///
/// Read-only: it fetches the two refs it needs into the remote-tracking
/// namespace and then only `rev-parse` / `merge-base` / `rev-list` /
/// `diff`. Never touches a working tree, an index or a local branch, so
/// it is safe in an operator's dirty checkout — which is where `boss
/// gate` runs, and a verb that stashed or checked out there would be the
/// `never-stash-while-a-gate-runs` defect with a new name.
///
/// Every failure is an [`Base::Unanswered`] carrying the reason, never a
/// staleness finding.
pub(crate) fn observe(repo: &Path, branch: &str) -> BaseObservation {
    let unreadable = |why: String| BaseObservation {
        standing: Base::Unanswered,
        unreadable: Some(why),
        ..Default::default()
    };
    // Precisely the two refs, so a big repository does not pay for a
    // full fetch, and so a branch missing from the forge fails HERE with
    // a reason rather than answering off a stale remote-tracking ref.
    let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
    if let Err(e) = git_line(
        repo,
        &[
            "fetch",
            "--quiet",
            "origin",
            "+refs/heads/main:refs/remotes/origin/main",
            &refspec,
        ],
    ) {
        return unreadable(e);
    }
    let head = format!("origin/{branch}");
    let main_head = match git_line(repo, &["rev-parse", "origin/main"]) {
        Ok(s) => s,
        Err(e) => return unreadable(e),
    };
    let base = match git_line(repo, &["merge-base", "origin/main", &head]) {
        Ok(s) => s,
        Err(e) => return unreadable(e),
    };
    let standing = base_from_is_ancestor_code(
        git(repo, &["merge-base", "--is-ancestor", "origin/main", &head])
            .ok()
            .and_then(|o| o.status.code()),
    );
    if standing != Base::Behind {
        return BaseObservation {
            standing,
            main_head,
            base,
            behind_by: 0,
            untested: Vec::new(),
            unreadable: match standing {
                Base::Unanswered => Some("git merge-base --is-ancestor gave no answer".to_string()),
                _ => None,
            },
        };
    }
    let behind_by = git_line(
        repo,
        &["rev-list", "--count", &format!("{head}..origin/main")],
    )
    .ok()
    .and_then(|s| s.parse().ok())
    .unwrap_or(0);
    let mut untested: Vec<String> = git_line(repo, &["diff", "--name-only", &base, "origin/main"])
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect();
    untested.sort();
    untested.dedup();
    BaseObservation {
        standing,
        main_head,
        base,
        behind_by,
        untested,
        unreadable: None,
    }
}

/// The one escape every door in `infra/dev/` answers to, spelled once
/// on this side of the language boundary (`infra/dev/door-freshness.sh`
/// holds the shell's copy, pinned by its own test).
pub(crate) const FRESHNESS_ENV: &str = "BOSS_DOOR_FRESHNESS";

/// Has the operator silenced the freshness question for this process?
pub(crate) fn freshness_silenced() -> bool {
    std::env::var(FRESHNESS_ENV).is_ok_and(|v| v.trim() == "off")
}

/// The remote-tracking ref every reading here is taken against, read
/// in FULL so a local branch named `origin/main` cannot answer for it.
const MAIN_REF: &str = "refs/remotes/origin/main";

/// Where a CHECKOUT stands against `origin/main` — the question a
/// recorded probe asks without knowing it.
///
/// A probe reads the tree with `git show HEAD:<path>`. On the forge,
/// where the unattended door runs it, HEAD is the converged checkout —
/// production. At the hand door it is whatever this checkout last
/// fast-forwarded to, and on the dev pod the freshness sidecar DEFERS
/// while any gate-run is open, which under load is most of the time. So
/// the staleness is the steady state exactly when proofs are recorded:
/// measured 2026-09-22 (backlog a09bd894) the pod's checkout was NINE
/// trains behind while five shed cars were run through `--from-car`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct TreeObservation {
    /// Current (the checkout carries every landed change) / Behind /
    /// Unanswered.
    pub(crate) standing: Base,
    /// The checkout's HEAD — the tree the probe will read.
    pub(crate) head: String,
    /// `refs/remotes/origin/main`, as the last fetch left it.
    pub(crate) main_head: String,
    /// Commits on `origin/main` this checkout does not have.
    pub(crate) behind_by: usize,
    /// Why git could not answer, when `standing` is `Unanswered`.
    pub(crate) unreadable: Option<String>,
}

/// Read a checkout's standing, LOCALLY — no fetch, ever.
///
/// The same reading `infra/dev/door-freshness.sh` does, for the same
/// reasons stated there: a door called constantly must not take the
/// network, and a dark forge must not stop a probe. Worktrees share
/// remote-tracking refs, so a builder's own `git fetch origin` keeps
/// this answer current at no cost here.
///
/// Polarity is [`observe`]'s: `origin/main` an ancestor of HEAD is
/// CURRENT. That is deliberately stricter than the door helper's
/// ancestor-of-main test, which stays quiet on a branch — a branch cut
/// from an old main reads an old tree too, and that is the thing being
/// judged. Every failure is [`Base::Unanswered`], never a staleness
/// finding.
pub(crate) fn observe_tree(repo: &Path) -> TreeObservation {
    let unreadable = |why: String| TreeObservation {
        standing: Base::Unanswered,
        unreadable: Some(why),
        ..Default::default()
    };
    let head = match git_line(repo, &["rev-parse", "HEAD"]) {
        Ok(s) => s,
        Err(e) => return unreadable(e),
    };
    // The head git DID answer for survives a failure to read the
    // remote-tracking ref, which is absent on plenty of real checkouts
    // — and is the fact the proof stamp wants most, since it names the
    // tree the probe read. Reducing a record before storing it throws
    // away the only copy (CLAUDE.md §Diagnosis).
    let main_head = match git_line(repo, &["rev-parse", MAIN_REF]) {
        Ok(s) => s,
        Err(e) => {
            return TreeObservation {
                standing: Base::Unanswered,
                head,
                unreadable: Some(e),
                ..Default::default()
            };
        }
    };
    let standing = base_from_is_ancestor_code(
        git(repo, &["merge-base", "--is-ancestor", &main_head, &head])
            .ok()
            .and_then(|o| o.status.code()),
    );
    if standing != Base::Behind {
        return TreeObservation {
            standing,
            head,
            main_head,
            behind_by: 0,
            unreadable: match standing {
                Base::Unanswered => Some("git merge-base --is-ancestor gave no answer".to_string()),
                _ => None,
            },
        };
    }
    let behind_by = git_line(
        repo,
        &["rev-list", "--count", &format!("{head}..{main_head}")],
    )
    .ok()
    .and_then(|s| s.parse().ok())
    .unwrap_or(0);
    TreeObservation {
        standing,
        head,
        main_head,
        behind_by,
        unreadable: None,
    }
}

/// What `boss gate` does about the branch's base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BaseGuard {
    /// Gate it, and print this line: the base it will be judged against,
    /// or why that could not be read, or the operator's stated reason for
    /// gating an old base anyway.
    Note(String),
    /// The message, and no gate.
    Refuse(String),
}

/// PURE: never spend a gate on a base that is not current — unless told
/// to, with a reason that is recorded.
///
/// Refusing rather than warning, because a warning at launch is read by
/// nobody: the gate takes a quarter of an hour, the builder is usually an
/// agent in a loop, and the artifact the dock acts on is the green
/// receipt, by which time the launch output is gone. A refusal here costs
/// NOTHING — it fires before a packet is filed or a slot is taken, the
/// same admission law `landed_guard` and `gated_car_guard` follow — and
/// the remedy it prints is the rebase the protocol already requires
/// before gating a wave car.
///
/// The escape is mandatory, not a courtesy: a door with no escape stops
/// being a door and gets routed around (CLAUDE.md §Doors), and there is a
/// legitimate case — gating an old tree deliberately, to reproduce
/// something that only happens there. It takes a reason for the same
/// reason `boss prove --probe-anyway` does: the reason is the part that
/// survives into the record.
/// What a `--rebase` did: the forge head it found, the head it left,
/// and how many of the car's commits it replayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Rebased {
    pub old_head: String,
    pub new_head: String,
    pub replayed: usize,
    /// Commits main already held, in branch order — replayed EMPTY and
    /// skipped, each one named to the operator (1cfab20e).
    pub dropped: Vec<Dropped>,
    /// Did the forge, re-read after the push, actually hold `new_head`?
    /// `false` means the push was accepted and the branch could not be
    /// re-read to confirm it — the caller says so rather than claiming a
    /// move it did not observe (18909a43).
    pub confirmed: bool,
}

/// One commit `--rebase` dropped because its patch is already on main:
/// the predecessor car landed by squash, so the same change sits on main
/// under another sha and the replay produces nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dropped {
    pub sha: String,
    pub title: String,
}

impl Dropped {
    /// The one line the CLI prints per dropped commit.
    pub(crate) fn line(&self) -> String {
        format!(
            "rebase: dropped {} \"{}\" — its patch is already on main",
            &self.sha[..7.min(self.sha.len())],
            self.title
        )
    }
}

/// Replay a stale car onto origin/main and move its branch on the forge
/// — the fix the stale-base refusal always asked for, as a door.
///
/// THE CALLER'S CHECKOUT NEVER MOVES. The work happens in a temporary
/// worktree this function adds and removes (a `git rebase` in the
/// builder's tree is the destructive act a builder's harness rightly
/// refuses, and a worktree left behind is the 3d8bb6e6 shape); the
/// branch is moved on the forge with `--force-with-lease` on the head
/// this function read, so a push that raced it is refused, not
/// overwritten. A replay that CONFLICTS is refused naming the files and
/// nothing is pushed: this verb never guesses a merge.
///
/// Measured 2026-09-12 (protocol retro 8043c1f5): seven cars in one
/// session were built on a main that had moved by gate time — trains
/// land every ~45 min — and each rebase was the same three hand steps.
pub(crate) fn rebase_onto_main(repo: &Path, branch: &str) -> anyhow::Result<Rebased> {
    let mut raced: Vec<String> = Vec::new();
    for _ in 0..REPLAY_ATTEMPTS {
        match replay_once(repo, branch)? {
            Replay::Settled(done) => return Ok(done),
            Replay::Raced(why) => raced.push(why),
        }
    }
    anyhow::bail!(
        "boss gate --rebase: the rebase was NOT applied — {branch} on the forge does not end at \
         the head that was replayed onto origin/main, after {REPLAY_ATTEMPTS} attempts. What each \
         attempt saw:\n  {}\nThe branch is where it was; gate again once it is quiet.",
        raced.join("\n  ")
    );
}

/// How many times a racing branch is replayed before the verb refuses.
///
/// Three, because the race it survives is another push landing in the
/// seconds between this verb reading the head and moving it: replaying
/// against the branch as it now stands is the fix (18909a43, fix 2).
/// A branch pushed to three times in a row while a gate launches is not
/// a race — it is a builder still working — and refusing is then right.
const REPLAY_ATTEMPTS: usize = 3;

/// What one replay attempt left behind.
enum Replay {
    /// The forge, re-read, holds the replayed head — or could not be
    /// re-read at all, in which case [`Rebased::confirmed`] is false and
    /// the caller says so rather than vouching for the move.
    Settled(Rebased),
    /// Something moved the branch under this attempt: the lease refused
    /// the push, or the push was accepted and the branch does not hold
    /// what was pushed. The string is what this attempt observed.
    Raced(String),
}

fn replay_once(repo: &Path, branch: &str) -> anyhow::Result<Replay> {
    use anyhow::{Context, bail};
    let git = |args: &[&str]| -> anyhow::Result<std::process::Output> {
        let mut cmd = crate::git_auth::command();
        cmd.arg("-C").arg(repo).args(args);
        cmd.output()
            .with_context(|| format!("git {args:?} in {}", repo.display()))
    };
    let ok = |o: &std::process::Output, what: &str| -> anyhow::Result<String> {
        if !o.status.success() {
            bail!("{what}: {}", String::from_utf8_lossy(&o.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    ok(
        &git(&[
            "fetch",
            "-q",
            "origin",
            "main",
            &format!("refs/heads/{branch}"),
        ])?,
        "fetching main and the branch",
    )?;
    let old_head = ok(
        &git(&["rev-parse", &format!("origin/{branch}")])?,
        "reading the branch's forge head",
    )?;
    let main_head = ok(&git(&["rev-parse", "origin/main"])?, "reading origin/main")?;
    let commits = ok(
        &git(&[
            "rev-list",
            "--reverse",
            &format!("origin/main..origin/{branch}"),
        ])?,
        "listing the car's commits",
    )?;
    let commits: Vec<&str> = commits.lines().filter(|l| !l.is_empty()).collect();
    let behind = ok(
        &git(&[
            "rev-list",
            "--count",
            &format!("origin/{branch}..origin/main"),
        ])?,
        "counting how far behind",
    )?;
    if behind.trim() == "0" {
        return Ok(Replay::Settled(Rebased {
            old_head: old_head.clone(),
            new_head: old_head,
            replayed: 0,
            dropped: vec![],
            // Nothing was pushed, so the head read above IS the forge's.
            confirmed: true,
        }));
    }
    // ONE ATTEMPT, ONE DIRECTORY. pid + head alone named the same path
    // for every replay of the same head, and `cleanup` below removes it
    // with --force: a retry (18909a43) or a second caller replaying that
    // head pulled the directory out from under the first, which reads as
    // "failed before any conflict could be read". The counter makes the
    // path unique within this process; the pid, between processes.
    static ATTEMPT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let tmp = std::env::temp_dir().join(format!(
        "boss-gate-rebase-{}-{}-{}",
        std::process::id(),
        ATTEMPT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        &old_head[..8.min(old_head.len())]
    ));
    let _ = std::fs::remove_dir_all(&tmp);
    ok(
        &git(&[
            "worktree",
            "add",
            "-q",
            "--detach",
            tmp.to_str().context("temp path is utf8")?,
            &main_head,
        ])?,
        "adding the temporary worktree",
    )?;
    let cleanup = |git: &dyn Fn(&[&str]) -> anyhow::Result<std::process::Output>| {
        let _ = git(&["worktree", "remove", "--force", tmp.to_str().unwrap_or("")]);
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = git(&["worktree", "prune"]);
    };
    let tmp_str = tmp.to_str().context("temp path is utf8")?.to_string();
    let in_tmp = |args: &[&str]| -> anyhow::Result<std::process::Output> {
        crate::git_auth::command()
            .arg("-C")
            .arg(&tmp_str)
            .args(args)
            .output()
            .with_context(|| format!("git {args:?} in {tmp_str}"))
    };
    let mut dropped: Vec<Dropped> = Vec::new();
    for c in &commits {
        // cherry-pick keeps the car's AUTHOR; the COMMITTER is this
        // verb, acting for whoever runs it — set explicitly so the replay
        // does not depend on a git identity in the environment (a gate
        // runner's or a test's has none).
        let pick = crate::git_auth::command()
            .arg("-C")
            .arg(&tmp_str)
            .args(["cherry-pick", c])
            .env("GIT_COMMITTER_NAME", "boss gate --rebase")
            .env("GIT_COMMITTER_EMAIL", "boss@algedonic.dev")
            .output()
            .context("cherry-pick")?;
        if !pick.status.success() {
            let files = crate::git_auth::command()
                .arg("-C")
                .arg(&tmp_str)
                .args(["diff", "--name-only", "--diff-filter=U"])
                .output()
                .map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .trim()
                        .replace('\n', ", ")
                })
                .unwrap_or_default();
            // A pick with NO conflict that left the index equal to HEAD
            // produced nothing: main already holds this patch under
            // another sha (the predecessor car landed by squash). git
            // stops there — "The previous cherry-pick is now empty" —
            // and until 2026-09-17 (1cfab20e) so did this verb, and the
            // builder cherry-picked by hand. Drop it, say so, go on.
            // (`cherry-pick --empty=drop` is git 2.45; the pod has 2.39.)
            let empty = files.is_empty()
                && in_tmp(&["diff", "--cached", "--quiet", "HEAD"])
                    .map(|o| o.status.success())
                    .unwrap_or(false);
            let skipped = empty
                && in_tmp(&["cherry-pick", "--skip"])
                    .map(|o| o.status.success())
                    .unwrap_or(false);
            if skipped {
                let title = in_tmp(&["log", "-1", "--format=%s", c])
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                    .unwrap_or_default();
                dropped.push(Dropped {
                    sha: c.to_string(),
                    title,
                });
                continue;
            }
            let said = String::from_utf8_lossy(&pick.stderr).trim().to_string();
            let _ = in_tmp(&["cherry-pick", "--abort"]);
            cleanup(&git);
            if files.is_empty() {
                bail!(
                    "boss gate --rebase: replaying {} onto origin/main@{} failed before any \
                     conflict could be read — git said: {said}. Nothing was pushed; {branch} still \
                     points at {}.",
                    &c[..8.min(c.len())],
                    &main_head[..8],
                    &old_head[..8]
                );
            }
            bail!(
                "boss gate --rebase: REFUSED — replaying {} onto origin/main@{} hit a conflict in: \
                 {files}. Nothing was pushed; {branch} still points at {}. Resolve it in your own \
                 worktree (git rebase origin/main) and gate again.",
                &c[..8.min(c.len())],
                &main_head[..8],
                &old_head[..8]
            );
        }
    }
    // Every commit dropped: the branch is entirely on main. Nothing to
    // gate — pushing main's head to the branch would only manufacture a
    // car with no diff — so refuse, and point at the verb that proves it.
    if !commits.is_empty() && dropped.len() == commits.len() {
        cleanup(&git);
        let named = dropped
            .iter()
            .map(|d| format!("{} \"{}\"", &d.sha[..8.min(d.sha.len())], d.title))
            .collect::<Vec<_>>()
            .join(", ");
        bail!(
            "boss gate --rebase: REFUSED — {branch} is already landed: every commit it carries \
             ({named}) replays empty onto origin/main@{}, so there is nothing to gate. Nothing \
             was pushed; {branch} still points at {}. Confirm with `boss merged {branch}`.",
            &main_head[..8],
            &old_head[..8]
        );
    }
    let new_head = match ok(
        &in_tmp(&["rev-parse", "HEAD"])?,
        "reading the replayed head",
    ) {
        Ok(h) => h,
        Err(e) => {
            cleanup(&git);
            return Err(e);
        }
    };
    // Author preserved by cherry-pick; committer is whoever runs this, as
    // with any push. The lease is the head this function read, so a push
    // that raced us is refused rather than overwritten.
    let push = crate::git_auth::command()
        .arg("-C")
        .arg(&tmp_str)
        .args([
            "push",
            "-q",
            &format!("--force-with-lease=refs/heads/{branch}:{old_head}"),
            "origin",
            &format!("HEAD:refs/heads/{branch}"),
        ])
        .output()
        .context("pushing the replayed branch")?;
    cleanup(&git);
    let short = |s: &str| s[..8.min(s.len())].to_string();
    if !push.status.success() {
        // A lease refused IS the race — the branch moved between the head
        // this attempt read and the push. Say what was seen and let the
        // caller replay against the branch as it now stands.
        return Ok(Replay::Raced(format!(
            "the push of {} was refused under the lease on {}: {}",
            short(&new_head),
            short(&old_head),
            String::from_utf8_lossy(&push.stderr).trim()
        )));
    }
    // THE PUSH IS READ BACK, NOT BELIEVED (18909a43, 2026-09-20). A push
    // git reports as successful is a forge ANSWER and not a forge EFFECT:
    // this verb printed "04ca1ae7 -> 64ecfe32 (pushed with a lease)" and
    // the branch still read 04ca1ae7 — the replayed commit survived as an
    // object no ref pointed at, and the car gated, and parked, one commit
    // behind main. The exit code does not decide what this attempt did;
    // the re-read does. It doubles as keeping the caller's
    // remote-tracking ref honest (the local branch, if checked out
    // somewhere, is theirs to move).
    let _ = git(&[
        "fetch",
        "-q",
        "origin",
        &format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"),
    ]);
    let observed = git(&["rev-parse", &format!("origin/{branch}")])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    if let Some(head) = &observed
        && head != &new_head
    {
        return Ok(Replay::Raced(format!(
            "{} was pushed with a lease on {} and accepted, but {branch} on the forge reads {}",
            short(&new_head),
            short(&old_head),
            short(head)
        )));
    }
    Ok(Replay::Settled(Rebased {
        old_head,
        new_head,
        replayed: commits.len() - dropped.len(),
        dropped,
        // An unreadable branch is not a finding either way: the caller
        // prints the move without vouching for it.
        confirmed: observed.is_some(),
    }))
}

pub(crate) fn stale_base_guard(
    branch: &str,
    obs: &BaseObservation,
    anyway: Option<&str>,
) -> BaseGuard {
    let short = |s: &str| s.chars().take(7).collect::<String>();
    match obs.standing {
        Base::Current => BaseGuard::Note(format!(
            "boss gate: base is CURRENT — {branch} carries origin/main@{}",
            short(&obs.main_head)
        )),
        Base::Unanswered => BaseGuard::Note(format!(
            "boss gate: base standing UNKNOWN for {branch} ({}) — gating anyway. \
             A base that cannot be READ is not a base that is stale; the receipt \
             will say `base_standing: unreadable` rather than claim it was current.",
            obs.unreadable.as_deref().unwrap_or("no reason recorded")
        )),
        Base::Behind => {
            let reason = anyway.map(str::trim).filter(|r| !r.is_empty());
            let listed = obs
                .untested
                .iter()
                .take(UNTESTED_SAMPLE)
                .map(|p| format!("\n    {p}"))
                .collect::<String>();
            let more = obs.untested.len().saturating_sub(UNTESTED_SAMPLE);
            let more = if more > 0 {
                format!("\n    +{more} more")
            } else {
                String::new()
            };
            match reason {
                Some(r) => BaseGuard::Note(format!(
                    "boss gate: {branch}'s base is BEHIND origin/main by {} commit(s) \
                     (base {}, origin/main {}) — gating anyway because: {r}. Recorded on \
                     the gate-run as `stale_base_anyway`, so this green says what it \
                     could not vouch for.",
                    obs.behind_by,
                    short(&obs.base),
                    short(&obs.main_head),
                )),
                None => BaseGuard::Refuse(format!(
                    "boss gate: REFUSED — {branch}'s base is BEHIND origin/main by {} \
                     commit(s).\n  \
                     base {}, origin/main {}\n  \
                     A gate judges this branch's tree ALONE, so a green here vouches for \
                     a tree that will never exist: the {} path(s) below are at the \
                     version your base had, not the version that will land. That is how \
                     train #244 went red with two innocent cars aboard (2026-09-07) — a \
                     car gated before another landed, whose own test broke against a \
                     contract the other car tightened in a DIFFERENT file.{listed}{more}\n  \
                     Rebase and gate that instead:\n    \
                     git fetch origin && git rebase origin/main && \
                     git push --force-with-lease\n  \
                     To gate THIS base anyway (reproducing something that only happens \
                     on the old tree, say), pass --stale-base-anyway \"<reason>\" — the \
                     reason lands on the gate-run.",
                    obs.behind_by,
                    short(&obs.base),
                    short(&obs.main_head),
                    obs.untested.len(),
                )),
            }
        }
    }
}

/// PURE: what `boss prove --from-car` does about the tree its probe is
/// about to read. `records` is whether this run would WRITE a proof;
/// `silenced` is `BOSS_DOOR_FRESHNESS=off`, the one escape the doors
/// already answer to.
///
/// THE SPLIT IS THE DOOR-FRESHNESS SPLIT (CLAUDE.md §Doors): a read
/// warns, a WRITE is refused, because a read's warning rides beside its
/// answer while a write lands an immutable fact. A recorded proof is a
/// write, and a worse one than most — it asserts a claim about
/// PRODUCTION, from a tree nobody can re-read later, and the dangerous
/// direction is the passing one: a probe asserting a change has
/// converged passes against a checkout that predates a later revert.
/// Measured 2026-09-22 (a09bd894): five cars proved off a six-hour-old
/// tree, every answer right by luck rather than method, and nothing in
/// the output said which tree was read.
///
/// So the refusal is narrow by construction: it fires only where a
/// proof would be recorded, which leaves `--dry` as the rehearsal the
/// flag exists for — the same text, the same environment, the same
/// verdict, and nothing written. That is why this refuses rather than
/// warns despite the packet's own worry about blocking rehearsals on
/// the busy nights: the busy night still has a door.
///
/// It states the tree in EVERY case, including the clean one. The
/// measured defect was two halves and this is the other: a proof taken
/// against production and a proof taken against an old tree looked
/// identical in the output.
pub(crate) fn stale_tree_guard(obs: &TreeObservation, records: bool, silenced: bool) -> BaseGuard {
    let short = |s: &str| s.chars().take(7).collect::<String>();
    match obs.standing {
        Base::Current => BaseGuard::Note(format!(
            "boss prove: the probe reads THIS checkout at {}, which carries every change \
             on origin/main {}",
            short(&obs.head),
            short(&obs.main_head),
        )),
        Base::Unanswered => BaseGuard::Note(format!(
            "boss prove: which tree this probe reads is UNKNOWN ({}) — running anyway. \
             A tree that cannot be READ is not a tree that is stale (CLAUDE.md \
             §Diagnosis); the probe's own output is the evidence either way.",
            obs.unreadable.as_deref().unwrap_or("no reason recorded"),
        )),
        Base::Behind => {
            let facts = format!(
                "this checkout is BEHIND origin/main by {} commit(s) — HEAD {}, origin/main {}",
                obs.behind_by,
                short(&obs.head),
                short(&obs.main_head),
            );
            if !records || silenced {
                return BaseGuard::Note(format!(
                    "boss prove: {facts}. A recorded probe reads the tree with `git show \
                     HEAD:`, so this one is judging a tree the forge has already left \
                     behind. Nothing is recorded by this run{}.",
                    if silenced {
                        format!(
                            " ({FRESHNESS_ENV}=off silenced the refusal, so a proof taken \
                             here vouches for a tree that is not production)"
                        )
                    } else {
                        String::new()
                    },
                ));
            }
            BaseGuard::Refuse(format!(
                "boss prove: REFUSED — {facts}.\n  \
                 A recorded probe reads the tree with `git show HEAD:<path>`. On the \
                 forge, where this car's probe will be re-run unattended, HEAD is the \
                 CONVERGED checkout; here it is whatever this one last fast-forwarded \
                 to, and the dev pod's sidecar defers while any gate-run is open — so \
                 under load, which is when proofs get recorded, stale is the steady \
                 state.\n  \
                 A proof recorded off it is immutable and says nothing about which tree \
                 answered. The failure mode is the passing one: a probe asserting a \
                 change HAS converged passes against a checkout that predates a later \
                 revert.\n  \
                 Fix it — this is one local call, no fetch:\n    \
                 git fetch origin && git merge --ff-only origin/main\n  \
                 To REHEARSE against this tree without recording anything, pass --dry: \
                 same probe, same environment, same verdict, no write. To record anyway \
                 — a claim about the old tree, deliberately — run with \
                 {FRESHNESS_ENV}=off, the escape every other door answers to."
            ))
        }
    }
}

/// How a standing is spelled in a record. ONE definition: a gate-run's
/// `base_standing` and a proof's `tree_standing` answer the same
/// question about different refs, and a reader that learned one word
/// must not meet a second spelling of it.
pub(crate) fn standing_word(b: Base) -> &'static str {
    match b {
        Base::Current => "current",
        Base::Behind => "behind",
        Base::Unanswered => "unreadable",
    }
}

/// THE TREE A PROBE READ, as proof keys (backlog 6f581de6).
///
/// A recorded proof carried `host` and `cwd` — the machine and the
/// directory — and said nothing about WHAT it read, while a directory's
/// contents change under it: a probe reads the tree with `git show
/// HEAD:<path>`, and on the pod HEAD is whatever the checkout last
/// fast-forwarded to. Provenance is the first of the five properties
/// (CLAUDE.md §Founding ideas) and this verb exists to turn a feeling
/// about the past into an artifact, so a proof that cannot say which
/// revision answered it is incomplete in the one dimension it is for.
///
/// `a09bd894` landed the PREVENTION — a stale tree cannot record a
/// proof through `--from-car`. Prevention plus silence is still weaker
/// than a record: it says nothing about proofs already on the board,
/// nothing about the doors the guard does not cover, and a reader with
/// the proof in front of them still cannot tell. The observation is
/// already computed at the moment the probe runs, so this is carrying a
/// value into a record that is already written.
///
/// BOTH KEYS ARE ALWAYS PRESENT, `tree_head` null when git could not
/// answer. Unlike [`base_metadata`], which feeds the metadata PATCH
/// where a null DELETES the key, this is serialised into one opaque
/// `proof` string — so null is safe here and it is the honest shape: a
/// proof whose tree could not be read must not look like one recorded
/// before this key existed.
pub(crate) fn tree_metadata(obs: &TreeObservation) -> Value {
    json!({
        "tree_head": (!obs.head.is_empty()).then(|| obs.head.clone()),
        "tree_standing": standing_word(obs.standing),
    })
}

/// The tree line for a run that RECORDS NOTHING — `boss prove
/// --recheck`, which re-runs a recorded probe and writes nothing.
///
/// Its HOLDS / NO LONGER HOLDS is acted on by a human, off whatever
/// tree happens to be present, and until this it was the one door in
/// the family that said nothing at all about which tree that was. A
/// NOTE rather than a refusal is what its shape earns: the refusal in
/// [`stale_tree_guard`] is paid for by the immutability of what a write
/// lands, and there is no write here. That is why this passes
/// `records = false`, which by construction cannot refuse — pinned by
/// `a_run_that_records_nothing_is_told_the_tree_and_never_refused`.
pub(crate) fn unrecorded_tree_note(obs: &TreeObservation, silenced: bool) -> String {
    match stale_tree_guard(obs, false, silenced) {
        BaseGuard::Note(n) | BaseGuard::Refuse(n) => n,
    }
}

/// The base facts, as gate-run metadata.
///
/// Stamped whether the base was current or not, and whether the packet
/// was just filed or reused, because the question a later reader asks is
/// "which main was this green taken against?" — and "the metadata is
/// absent" answers that only for packets filed before this existed.
///
/// Keys are OMITTED rather than set to null when they do not apply: the
/// jobs API's metadata PATCH DELETES a key set to null, and deleting a
/// key nothing ever wrote is a write that reads as a change.
pub(crate) fn base_metadata(obs: &BaseObservation, anyway: Option<&str>) -> Value {
    let mut md = json!({ "base_standing": standing_word(obs.standing) });
    if !obs.main_head.is_empty() {
        md["base_main_head"] = json!(obs.main_head);
    }
    if !obs.base.is_empty() {
        md["base"] = json!(obs.base);
    }
    if let Some(why) = obs.unreadable.as_deref() {
        md["base_unreadable"] = json!(why);
    }
    if obs.standing == Base::Behind {
        md["base_behind_by"] = json!(obs.behind_by);
        md["base_untested_count"] = json!(obs.untested.len());
        md["base_untested_paths"] = json!(
            obs.untested
                .iter()
                .take(UNTESTED_SAMPLE)
                .cloned()
                .collect::<Vec<String>>()
        );
    }
    if let Some(r) = anyway.map(str::trim).filter(|r| !r.is_empty()) {
        md["stale_base_anyway"] = json!(r);
    }
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------
    // The shared exit-code reading
    // -----------------------------------------------------------------

    #[test]
    fn the_is_ancestor_code_reads_three_ways_and_no_more() {
        assert_eq!(base_from_is_ancestor_code(Some(0)), Base::Current);
        assert_eq!(base_from_is_ancestor_code(Some(1)), Base::Behind);
        // A bad or missing ref is NOT a staleness finding.
        assert_eq!(base_from_is_ancestor_code(Some(128)), Base::Unanswered);
        assert_eq!(base_from_is_ancestor_code(None), Base::Unanswered);
    }

    // -----------------------------------------------------------------
    // The observation, against real git
    // -----------------------------------------------------------------

    /// A real forge + a real clone, in a scratch root this process owns
    /// (pid AND uid — `/tmp` is shared and sticky, and the gate runs as
    /// uid 65534 while a developer runs the suite as someone else).
    ///
    /// The shape is the one measured on 2026-09-11: base `B`; main moves
    /// ahead twice; branches cut from `B` that do and do not overlap what
    /// main touched.
    struct Forge {
        clone: std::path::PathBuf,
    }

    impl Forge {
        fn git(dir: &Path, args: &[&str]) -> std::process::Output {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(dir)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .output()
                .unwrap_or_else(|e| panic!("git {args:?} in {}: {e}", dir.display()));
            assert!(
                out.status.success(),
                "git {args:?} in {}: {}",
                dir.display(),
                String::from_utf8_lossy(&out.stderr)
            );
            out
        }

        /// `origin` is a real repository serving as the forge; the clone
        /// is the checkout `boss gate` runs in.
        fn build(name: &str) -> Self {
            let root = boss_testing::scratch::scratch_dir(name);
            let origin = root.join("forge");
            let clone = root.join("clone");
            boss_testing::scratch::create_dir(&origin);
            let w = |rel: &str, body: &str| {
                boss_testing::scratch::write_file(&origin.join(rel), body);
            };

            Self::git(&origin, &["init", "-q", "-b", "main"]);
            // `B` — the base every stale branch below is cut from.
            w("untouched.rs", "base\n");
            w("landed.rs", "fn one() {}\n");
            w("mine.rs", "fn mine() {}\n");
            Self::git(&origin, &["add", "."]);
            Self::git(&origin, &["commit", "-qm", "base B"]);
            Self::git(&origin, &["branch", "car/overlaps"]);
            Self::git(&origin, &["branch", "car/disjoint"]);

            // main moves ahead twice — the cars that landed while the
            // branch was being built.
            w("landed.rs", "fn one() {}\nfn two() {}\n");
            Self::git(&origin, &["commit", "-qam", "train: landed.rs grows"]);
            w("also-landed.rs", "fn three() {}\n");
            Self::git(&origin, &["add", "."]);
            Self::git(&origin, &["commit", "-qm", "train: a new file lands"]);

            // The measured shape: a branch from B that touches the same
            // file main grew, so its diff against main shows main's new
            // line as a deletion.
            Self::git(&origin, &["checkout", "-q", "car/overlaps"]);
            w("landed.rs", "fn one() {}\nfn mine_instead() {}\n");
            Self::git(&origin, &["commit", "-qam", "car edits landed.rs"]);

            // A branch from B touching only files main never went near.
            Self::git(&origin, &["checkout", "-q", "car/disjoint"]);
            w("mine.rs", "fn mine() {}\nfn also_mine() {}\n");
            Self::git(&origin, &["commit", "-qam", "car edits mine.rs only"]);

            // An ordinary freshly-cut branch: main's own head.
            Self::git(&origin, &["checkout", "-q", "main"]);
            Self::git(&origin, &["branch", "car/exactly-on-main", "main"]);
            // And one AHEAD of main in the normal way.
            Self::git(&origin, &["checkout", "-q", "-b", "car/ahead", "main"]);
            w("mine.rs", "fn ahead() {}\n");
            Self::git(&origin, &["commit", "-qam", "car ahead of main"]);
            Self::git(&origin, &["checkout", "-q", "main"]);

            Self::git(
                &root,
                &["clone", "-q", origin.to_str().expect("utf8 path"), "clone"],
            );
            Self { clone }
        }
    }

    /// `--rebase`: a stale car's commits are replayed onto origin/main in a
    /// TEMPORARY worktree and the branch is moved on the forge with a
    /// lease on its old head. Nothing in the caller's checkout moves.
    /// Measured 2026-09-12: seven cars rebased by hand in one session,
    /// each a worktree-add + cherry-pick + force-with-lease because the
    /// refusal was right and the fix was always the same.
    #[test]
    fn a_stale_car_is_replayed_onto_main_and_moved_on_the_forge() {
        let f = Forge::build("boss-cli-freshness-rebase-clean");
        let before = observe(&f.clone, "car/disjoint");
        assert_eq!(before.standing, Base::Behind, "{before:?}");
        let old_head = String::from_utf8_lossy(
            &Forge::git(&f.clone, &["rev-parse", "origin/car/disjoint"]).stdout,
        )
        .trim()
        .to_string();

        let done =
            rebase_onto_main(&f.clone, "car/disjoint").expect("a disjoint car replays clean");
        assert_eq!(done.old_head, old_head);
        assert_eq!(done.replayed, 1, "one commit replayed");
        assert_ne!(done.new_head, old_head);

        // The forge's branch moved, the replayed commit sits on main, and
        // the observation is now CURRENT.
        Forge::git(&f.clone, &["fetch", "-q", "origin"]);
        let forge_head = String::from_utf8_lossy(
            &Forge::git(&f.clone, &["rev-parse", "origin/car/disjoint"]).stdout,
        )
        .trim()
        .to_string();
        assert_eq!(
            forge_head, done.new_head,
            "the forge carries the replayed head"
        );
        let after = observe(&f.clone, "car/disjoint");
        assert_eq!(after.standing, Base::Current, "{after:?}");
        let body = String::from_utf8_lossy(
            &Forge::git(
                &f.clone,
                &["show", "--stat", "--format=%s", "origin/car/disjoint"],
            )
            .stdout,
        )
        .to_string();
        assert!(
            body.contains("car edits mine.rs only") && body.contains("mine.rs"),
            "{body}"
        );
        // The caller's checkout did not move: no worktree of ours was
        // left behind under it.
        let wts = String::from_utf8_lossy(&Forge::git(&f.clone, &["worktree", "list"]).stdout)
            .to_string();
        assert_eq!(
            wts.lines().count(),
            1,
            "the temporary worktree is gone: {wts}"
        );
    }

    impl Forge {
        /// The forge repository this clone was made from.
        fn origin(&self) -> std::path::PathBuf {
            let url = String::from_utf8_lossy(
                &Forge::git(&self.clone, &["config", "remote.origin.url"]).stdout,
            )
            .trim()
            .to_string();
            std::path::PathBuf::from(url)
        }

        /// Make the forge RETREAT: a `post-receive` hook that puts every
        /// branch it just accepted back where it was. `once` retreats the
        /// first push only, which is the race the verb must survive by
        /// replaying; always-on is the one it must refuse plainly.
        ///
        /// This is the measured shape of 18909a43 (2026-09-20): the push
        /// reported success, the replayed commit existed as an object,
        /// and the branch still read the pre-rebase sha.
        fn retreats(&self, once: bool) {
            let hooks = self.origin().join(".git").join("hooks");
            boss_testing::scratch::create_dir(&hooks);
            let marker = hooks.join("retreated");
            let guard = if once {
                format!(
                    "if [ -f {m} ]; then exit 0; fi\n: > {m}\n",
                    m = marker.display()
                )
            } else {
                String::new()
            };
            boss_testing::scratch::write_exec(
                &hooks.join("post-receive"),
                &format!(
                    "#!/bin/sh\n{guard}while read -r old new ref; do\n  git update-ref \"$ref\" \
                     \"$old\"\ndone\n"
                ),
            );
        }
    }

    /// THE CLAIM IS READ BACK. A push the forge accepts and then undoes
    /// leaves the branch where it started; the verb must say the rebase
    /// was NOT applied and name the head the branch actually has —
    /// never report the replayed sha in the past tense (18909a43).
    #[test]
    fn a_rebase_the_forge_did_not_keep_is_refused_naming_the_head_the_branch_has() {
        let f = Forge::build("boss-cli-freshness-rebase-retreats");
        f.retreats(false);
        let old_head = String::from_utf8_lossy(
            &Forge::git(&f.clone, &["rev-parse", "origin/car/disjoint"]).stdout,
        )
        .trim()
        .to_string();
        let err = rebase_onto_main(&f.clone, "car/disjoint")
            .expect_err("a branch that did not move is not a rebase that happened")
            .to_string();
        assert!(
            err.contains("NOT applied") && err.contains(&old_head[..8]),
            "{err}"
        );
        Forge::git(&f.clone, &["fetch", "-q", "origin"]);
        let forge_head = String::from_utf8_lossy(
            &Forge::git(&f.clone, &["rev-parse", "origin/car/disjoint"]).stdout,
        )
        .trim()
        .to_string();
        assert_eq!(forge_head, old_head, "the forge's branch is where it was");
        let wts = String::from_utf8_lossy(&Forge::git(&f.clone, &["worktree", "list"]).stdout)
            .to_string();
        assert_eq!(
            wts.lines().count(),
            1,
            "no temporary worktree left behind: {wts}"
        );
    }

    /// A ONE-OFF RACE IS REPLAYED, NOT ABANDONED. Resolve, clone and push
    /// must agree on one sha; when they do not, the verb takes the branch
    /// as it now stands and replays again (18909a43, fix 2).
    #[test]
    fn a_branch_that_moves_under_the_push_is_replayed_again() {
        let f = Forge::build("boss-cli-freshness-rebase-race");
        f.retreats(true);
        let done = rebase_onto_main(&f.clone, "car/disjoint")
            .expect("the second attempt settles the branch");
        assert_eq!(done.replayed, 1, "{done:?}");
        Forge::git(&f.clone, &["fetch", "-q", "origin"]);
        let forge_head = String::from_utf8_lossy(
            &Forge::git(&f.clone, &["rev-parse", "origin/car/disjoint"]).stdout,
        )
        .trim()
        .to_string();
        assert_eq!(
            forge_head, done.new_head,
            "the head the verb reports is the head the branch has"
        );
        assert!(done.confirmed, "read back from the forge: {done:?}");
        assert_eq!(observe(&f.clone, "car/disjoint").standing, Base::Current);
    }

    /// A car whose replay conflicts is REFUSED with the files named, the
    /// forge untouched and the temporary worktree removed — the builder
    /// resolves it; this verb never guesses a merge.
    #[test]
    fn a_conflicting_replay_is_refused_naming_the_files_and_moves_nothing() {
        let f = Forge::build("boss-cli-freshness-rebase-conflict");
        let old_head = String::from_utf8_lossy(
            &Forge::git(&f.clone, &["rev-parse", "origin/car/overlaps"]).stdout,
        )
        .trim()
        .to_string();
        let err = rebase_onto_main(&f.clone, "car/overlaps")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("landed.rs") && err.contains("conflict"),
            "{err}"
        );
        Forge::git(&f.clone, &["fetch", "-q", "origin"]);
        let forge_head = String::from_utf8_lossy(
            &Forge::git(&f.clone, &["rev-parse", "origin/car/overlaps"]).stdout,
        )
        .trim()
        .to_string();
        assert_eq!(forge_head, old_head, "the forge's branch did not move");
        let wts = String::from_utf8_lossy(&Forge::git(&f.clone, &["worktree", "list"]).stdout)
            .to_string();
        assert_eq!(
            wts.lines().count(),
            1,
            "no temporary worktree left behind: {wts}"
        );
    }

    /// A car already on main has nothing to replay; saying so is not an
    /// error, and nothing is pushed.
    #[test]
    fn a_current_car_is_left_alone() {
        let f = Forge::build("boss-cli-freshness-rebase-current");
        let done = rebase_onto_main(&f.clone, "car/ahead").expect("nothing to do is ok");
        assert_eq!(done.replayed, 0);
        assert_eq!(done.old_head, done.new_head);
    }

    impl Forge {
        /// The shape measured 2026-09-17 (1cfab20e): `car/first` lands by
        /// SQUASH, so its patch is on main under another sha; `car/second`
        /// was cut from car/first's head and carries that commit plus one
        /// of its own; `car/landed` IS car/first, entirely on main now.
        fn build_after_squash(name: &str) -> Self {
            let root = boss_testing::scratch::scratch_dir(name);
            let origin = root.join("forge");
            let clone = root.join("clone");
            boss_testing::scratch::create_dir(&origin);
            let w = |rel: &str, body: &str| {
                boss_testing::scratch::write_file(&origin.join(rel), body);
            };
            Self::git(&origin, &["init", "-q", "-b", "main"]);
            w("base.rs", "base\n");
            Self::git(&origin, &["add", "."]);
            Self::git(&origin, &["commit", "-qm", "base B"]);

            Self::git(&origin, &["checkout", "-q", "-b", "car/first", "main"]);
            w("first.rs", "fn first() {}\n");
            Self::git(&origin, &["add", "."]);
            Self::git(&origin, &["commit", "-qm", "first car: first.rs"]);
            Self::git(&origin, &["branch", "car/landed", "car/first"]);

            Self::git(
                &origin,
                &["checkout", "-q", "-b", "car/second", "car/first"],
            );
            w("second.rs", "fn second() {}\n");
            Self::git(&origin, &["add", "."]);
            Self::git(&origin, &["commit", "-qm", "second car: second.rs"]);

            // The train squash-merges the first car: same patch, new sha.
            Self::git(&origin, &["checkout", "-q", "main"]);
            Self::git(&origin, &["merge", "-q", "--squash", "car/first"]);
            Self::git(&origin, &["commit", "-qm", "train: first car lands"]);

            Self::git(
                &root,
                &["clone", "-q", origin.to_str().expect("utf8 path"), "clone"],
            );
            Self { clone }
        }
    }

    /// A commit main already holds (the predecessor car landed by squash)
    /// replays EMPTY; it is dropped and NAMED, and the car's own commit
    /// lands. Measured 2026-09-17 (1cfab20e): git stopped on "previous
    /// cherry-pick is now empty" and the builder cherry-picked by hand —
    /// one lost launch.
    #[test]
    fn a_commit_main_already_holds_is_dropped_and_named() {
        let f = Forge::build_after_squash("boss-cli-freshness-rebase-squashed");
        let first_sha = String::from_utf8_lossy(
            &Forge::git(&f.clone, &["rev-parse", "origin/car/first"]).stdout,
        )
        .trim()
        .to_string();
        let done = rebase_onto_main(&f.clone, "car/second")
            .expect("the already-landed commit is dropped, the new one replays");
        assert_eq!(done.replayed, 1, "{done:?}");
        assert_eq!(done.dropped.len(), 1, "{done:?}");
        assert_eq!(done.dropped[0].sha, first_sha, "{done:?}");
        assert_eq!(done.dropped[0].title, "first car: first.rs", "{done:?}");
        let line = done.dropped[0].line();
        assert!(
            line.starts_with(&format!(
                "rebase: dropped {} \"first car: first.rs\" — its patch is already on main",
                &first_sha[..7]
            )),
            "{line}"
        );

        Forge::git(&f.clone, &["fetch", "-q", "origin"]);
        let forge_head = String::from_utf8_lossy(
            &Forge::git(&f.clone, &["rev-parse", "origin/car/second"]).stdout,
        )
        .trim()
        .to_string();
        assert_eq!(
            forge_head, done.new_head,
            "the forge carries the replayed head"
        );
        // Exactly one commit above main, and it is the car's own.
        let above = String::from_utf8_lossy(
            &Forge::git(
                &f.clone,
                &["log", "--format=%s", "origin/main..origin/car/second"],
            )
            .stdout,
        )
        .trim()
        .to_string();
        assert_eq!(above, "second car: second.rs", "{above}");
        assert_eq!(observe(&f.clone, "car/second").standing, Base::Current);
        let wts = String::from_utf8_lossy(&Forge::git(&f.clone, &["worktree", "list"]).stdout)
            .to_string();
        assert_eq!(
            wts.lines().count(),
            1,
            "the temporary worktree is gone: {wts}"
        );
    }

    /// A branch whose EVERY commit main already holds has nothing to gate:
    /// the rebase refuses, names the branch as landed, points at
    /// `boss merged`, and moves nothing on the forge.
    #[test]
    fn a_branch_entirely_on_main_is_refused_as_landed() {
        let f = Forge::build_after_squash("boss-cli-freshness-rebase-all-landed");
        let old_head = String::from_utf8_lossy(
            &Forge::git(&f.clone, &["rev-parse", "origin/car/landed"]).stdout,
        )
        .trim()
        .to_string();
        let err = rebase_onto_main(&f.clone, "car/landed")
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("car/landed")
                && err.contains("landed")
                && err.contains("boss merged car/landed")
                && err.contains("first car: first.rs"),
            "{err}"
        );
        Forge::git(&f.clone, &["fetch", "-q", "origin"]);
        let forge_head = String::from_utf8_lossy(
            &Forge::git(&f.clone, &["rev-parse", "origin/car/landed"]).stdout,
        )
        .trim()
        .to_string();
        assert_eq!(forge_head, old_head, "the forge's branch did not move");
        let wts = String::from_utf8_lossy(&Forge::git(&f.clone, &["worktree", "list"]).stdout)
            .to_string();
        assert_eq!(
            wts.lines().count(),
            1,
            "no temporary worktree left behind: {wts}"
        );
    }

    /// THE ANCHOR. A branch whose base is behind main, and whose diff
    /// against main shows main's landed content as a deletion, is seen as
    /// behind and NAMES the files main changed since that base.
    #[test]
    fn a_base_behind_main_is_named_with_the_files_it_cannot_vouch_for() {
        let f = Forge::build("boss-cli-freshness-overlaps");
        let obs = observe(&f.clone, "car/overlaps");
        assert_eq!(obs.standing, Base::Behind, "{obs:?}");
        assert_eq!(obs.behind_by, 2, "{obs:?}");
        assert_eq!(
            obs.untested,
            vec!["also-landed.rs".to_string(), "landed.rs".to_string()],
            "{obs:?}"
        );
        assert!(obs.unreadable.is_none(), "{obs:?}");
        assert_eq!(obs.main_head.len(), 40, "{obs:?}");
        // And the branch really is the dangerous-looking shape: its diff
        // against main deletes the line main added.
        let diff = Forge::git(
            &f.clone,
            &[
                "diff",
                "origin/main",
                "origin/car/overlaps",
                "--",
                "landed.rs",
            ],
        );
        let diff = String::from_utf8_lossy(&diff.stdout);
        assert!(diff.contains("-fn two() {}"), "{diff}");
    }

    /// A branch exactly on main is current, names nothing, and is not
    /// behind by anything.
    #[test]
    fn a_branch_exactly_on_main_is_current() {
        let f = Forge::build("boss-cli-freshness-on-main");
        let obs = observe(&f.clone, "car/exactly-on-main");
        assert_eq!(obs.standing, Base::Current, "{obs:?}");
        assert_eq!(obs.behind_by, 0, "{obs:?}");
        assert!(obs.untested.is_empty(), "{obs:?}");
        assert!(obs.unreadable.is_none(), "{obs:?}");
    }

    /// A branch ahead of main in the ordinary way — cut from current
    /// main, carrying its own commit — is current.
    #[test]
    fn a_branch_ahead_of_main_is_current() {
        let f = Forge::build("boss-cli-freshness-ahead");
        let obs = observe(&f.clone, "car/ahead");
        assert_eq!(obs.standing, Base::Current, "{obs:?}");
        assert_eq!(obs.behind_by, 0, "{obs:?}");
        assert!(obs.untested.is_empty(), "{obs:?}");
    }

    /// A branch behind main that touches only DISJOINT files is still
    /// behind, and still names what main changed.
    ///
    /// The brief for this change listed the disjoint case as a
    /// must-NOT-fire, on the theory that the hazard is a revert and a
    /// disjoint branch reverts nothing. The revert half is right (the
    /// conductor rebases, so nothing reverts) and the conclusion is not:
    /// train #244 (2026-09-07) was exactly this shape — the victim car's
    /// own test broke against a contract a different car had tightened in
    /// a file the victim never touched. A gate on this base cannot vouch
    /// for the tree that will land, so it is the same finding.
    #[test]
    fn a_disjoint_branch_behind_main_is_still_behind() {
        let f = Forge::build("boss-cli-freshness-disjoint");
        let obs = observe(&f.clone, "car/disjoint");
        assert_eq!(obs.standing, Base::Behind, "{obs:?}");
        assert_eq!(
            obs.untested,
            vec!["also-landed.rs".to_string(), "landed.rs".to_string()],
            "{obs:?}"
        );
        // Disjoint really does mean disjoint: the branch's own work is
        // not in the list of what main changed.
        assert!(!obs.untested.contains(&"mine.rs".to_string()), "{obs:?}");
    }

    /// A branch that is not on the forge at all cannot be read, and
    /// "cannot read" is not "stale".
    #[test]
    fn an_unknown_branch_is_unanswered_not_behind() {
        let f = Forge::build("boss-cli-freshness-unknown");
        let obs = observe(&f.clone, "car/never-pushed");
        assert_eq!(obs.standing, Base::Unanswered, "{obs:?}");
        assert!(obs.unreadable.is_some(), "{obs:?}");
    }

    /// A directory that is not a git repository at all: the same.
    #[test]
    fn a_non_repository_is_unanswered_not_behind() {
        let dir = boss_testing::scratch::scratch_dir("boss-cli-freshness-not-a-repo");
        let obs = observe(&dir, "car/whatever");
        assert_eq!(obs.standing, Base::Unanswered, "{obs:?}");
        assert!(obs.unreadable.is_some(), "{obs:?}");
    }

    // -----------------------------------------------------------------
    // The guard
    // -----------------------------------------------------------------

    fn behind(untested: &[&str]) -> BaseObservation {
        BaseObservation {
            standing: Base::Behind,
            main_head: "e749aa47e749aa47e749aa47e749aa47e749aa47".to_string(),
            base: "6cd8dc9b6cd8dc9b6cd8dc9b6cd8dc9b6cd8dc9b".to_string(),
            behind_by: 3,
            untested: untested.iter().map(|s| s.to_string()).collect(),
            unreadable: None,
        }
    }

    #[test]
    fn a_behind_base_is_refused_and_the_refusal_names_the_files() {
        let obs = behind(&["crates/core/boss-gateway/src/main.rs", "b.rs"]);
        match stale_base_guard("fix/a-thing", &obs, None) {
            BaseGuard::Refuse(why) => {
                assert!(why.contains("REFUSED"), "{why}");
                assert!(why.contains("fix/a-thing"), "{why}");
                assert!(why.contains("BEHIND"), "{why}");
                assert!(why.contains("3 commit(s)"), "{why}");
                assert!(
                    why.contains("crates/core/boss-gateway/src/main.rs"),
                    "{why}"
                );
                // The remedy, verbatim, and the recorded escape.
                assert!(why.contains("git rebase origin/main"), "{why}");
                assert!(why.contains("--stale-base-anyway"), "{why}");
            }
            g => panic!("expected a refusal, got {g:?}"),
        }
    }

    #[test]
    fn a_long_untested_list_is_sampled_in_the_refusal_and_counted_whole() {
        let many: Vec<String> = (0..UNTESTED_SAMPLE + 5)
            .map(|i| format!("crates/f{i}.rs"))
            .collect();
        let obs = BaseObservation {
            untested: many,
            ..behind(&[])
        };
        match stale_base_guard("fix/a-thing", &obs, None) {
            BaseGuard::Refuse(why) => {
                assert!(why.contains("+5 more"), "{why}");
                assert!(
                    why.contains(&format!("{} path(s)", UNTESTED_SAMPLE + 5)),
                    "{why}"
                );
            }
            g => panic!("expected a refusal, got {g:?}"),
        }
    }

    #[test]
    fn a_current_base_proceeds_and_says_which_main_it_judged() {
        let obs = BaseObservation {
            standing: Base::Current,
            main_head: "e749aa47e749aa47e749aa47e749aa47e749aa47".to_string(),
            ..Default::default()
        };
        match stale_base_guard("fix/a-thing", &obs, None) {
            BaseGuard::Note(n) => {
                assert!(n.contains("e749aa4"), "{n}");
                assert!(!n.contains("REFUSED"), "{n}");
            }
            g => panic!("expected a note, got {g:?}"),
        }
    }

    /// The fetch blipped, the forge was down, git is not installed: the
    /// gate PROCEEDS and says so. A guard that failed closed here would
    /// refuse every branch on the network's worst day, and `boss gate` is
    /// the single door every car passes through.
    #[test]
    fn an_unreadable_base_proceeds_with_the_reason() {
        let obs = BaseObservation {
            standing: Base::Unanswered,
            unreadable: Some("could not fetch origin: connection refused".to_string()),
            ..Default::default()
        };
        match stale_base_guard("fix/a-thing", &obs, None) {
            BaseGuard::Note(n) => {
                assert!(n.contains("connection refused"), "{n}");
                assert!(!n.contains("REFUSED"), "{n}");
            }
            g => panic!("expected a note, got {g:?}"),
        }
    }

    #[test]
    fn a_stated_reason_gates_the_old_base_and_the_note_carries_it() {
        let obs = behind(&["a.rs"]);
        match stale_base_guard("fix/a-thing", &obs, Some("reproducing the tracing flake")) {
            BaseGuard::Note(n) => {
                assert!(n.contains("reproducing the tracing flake"), "{n}");
                assert!(n.contains("BEHIND"), "{n}");
            }
            g => panic!("expected a forced note, got {g:?}"),
        }
    }

    /// An escape with an EMPTY reason is not an escape — the whole point
    /// of the flag is that the reason lands in the record.
    #[test]
    fn an_empty_reason_does_not_open_the_escape() {
        let obs = behind(&["a.rs"]);
        assert!(matches!(
            stale_base_guard("fix/a-thing", &obs, Some("   ")),
            BaseGuard::Refuse(_)
        ));
    }

    // -----------------------------------------------------------------
    // The stamp
    // -----------------------------------------------------------------

    #[test]
    fn the_stamp_says_which_main_and_whether_it_was_current() {
        let md = base_metadata(&behind(&["a.rs", "b.rs"]), Some("a reason"));
        assert_eq!(md["base_standing"], json!("behind"));
        assert_eq!(
            md["base_main_head"],
            json!("e749aa47e749aa47e749aa47e749aa47e749aa47")
        );
        assert_eq!(
            md["base"],
            json!("6cd8dc9b6cd8dc9b6cd8dc9b6cd8dc9b6cd8dc9b")
        );
        assert_eq!(md["base_behind_by"], json!(3));
        assert_eq!(md["base_untested_count"], json!(2));
        assert_eq!(md["base_untested_paths"], json!(["a.rs", "b.rs"]));
        assert_eq!(md["stale_base_anyway"], json!("a reason"));
    }

    #[test]
    fn a_current_stamp_carries_no_stale_keys() {
        let obs = BaseObservation {
            standing: Base::Current,
            main_head: "abc1234abc1234abc1234abc1234abc1234abc12".to_string(),
            base: "abc1234abc1234abc1234abc1234abc1234abc12".to_string(),
            ..Default::default()
        };
        let md = base_metadata(&obs, None);
        assert_eq!(md["base_standing"], json!("current"));
        // Absent, not null-and-present: a key set to null DELETES on the
        // jobs API's metadata PATCH, and these have never been set.
        assert!(md.get("base_untested_paths").is_none(), "{md}");
        assert!(md.get("stale_base_anyway").is_none(), "{md}");
        assert!(md.get("base_behind_by").is_none(), "{md}");
    }

    /// The sample is capped so a wide window cannot write an unbounded
    /// list into packet metadata; the COUNT stays exact.
    #[test]
    fn the_untested_sample_is_capped_but_the_count_is_not() {
        let many: Vec<String> = (0..UNTESTED_SAMPLE + 7)
            .map(|i| format!("crates/f{i}.rs"))
            .collect();
        let obs = BaseObservation {
            standing: Base::Behind,
            untested: many.clone(),
            behind_by: 1,
            ..Default::default()
        };
        let md = base_metadata(&obs, None);
        assert_eq!(md["base_untested_count"], json!(many.len()));
        assert_eq!(
            md["base_untested_paths"].as_array().map(Vec::len),
            Some(UNTESTED_SAMPLE)
        );
    }

    /// An unreadable base writes down WHY, so a later reader of a green
    /// receipt can tell "checked and current" from "never checked".
    #[test]
    fn an_unreadable_stamp_records_why_rather_than_claiming_current() {
        let obs = BaseObservation {
            standing: Base::Unanswered,
            unreadable: Some("connection refused".to_string()),
            ..Default::default()
        };
        let md = base_metadata(&obs, None);
        assert_eq!(md["base_standing"], json!("unreadable"));
        assert_eq!(md["base_unreadable"], json!("connection refused"));
    }

    // -----------------------------------------------------------------
    // The TREE a recorded probe reads (backlog a09bd894)
    // -----------------------------------------------------------------

    /// `boss prove --from-car` runs the car's probe HERE, and a recorded
    /// probe reads the tree with `git show HEAD:<path>`. So the standing
    /// that matters is the CHECKOUT's, not a branch's, and it is read
    /// with no fetch — the same local-only reading `door-freshness.sh`
    /// does.
    #[test]
    fn the_tree_a_probe_would_read_is_judged_locally_with_no_fetch() {
        let f = Forge::build("boss-cli-freshness-tree");
        // The fresh clone stands exactly on origin/main.
        let current = observe_tree(&f.clone);
        assert_eq!(current.standing, Base::Current, "{current:?}");
        assert_eq!(current.head, current.main_head, "{current:?}");
        assert_eq!(current.behind_by, 0, "{current:?}");

        // Move the checkout back two trains, the way the pod's is when
        // the freshness sidecar has deferred: `git show HEAD:` now reads
        // a tree that is NOT the one the forge would read.
        let base = String::from_utf8_lossy(
            &Forge::git(&f.clone, &["rev-list", "--max-parents=0", "HEAD"]).stdout,
        )
        .trim()
        .to_string();
        Forge::git(&f.clone, &["checkout", "-q", &base]);
        let behind = observe_tree(&f.clone);
        assert_eq!(behind.standing, Base::Behind, "{behind:?}");
        assert_eq!(behind.head, base, "{behind:?}");
        assert_eq!(behind.main_head, current.main_head, "{behind:?}");
        assert_eq!(behind.behind_by, 2, "two trains behind: {behind:?}");
        assert_eq!(behind.unreadable, None, "{behind:?}");

        // No fetch: the forge moving is invisible here until someone
        // fetches, which is the point — a door called constantly must
        // not take the network, and a dark forge must not stop a probe.
        let origin = f.origin();
        boss_testing::scratch::write_file(&origin.join("untouched.rs"), "moved\n");
        Forge::git(&origin, &["commit", "-qam", "train: main moves again"]);
        let after = observe_tree(&f.clone);
        assert_eq!(after.main_head, current.main_head, "no fetch: {after:?}");
    }

    /// A checkout git cannot answer about is NOT a stale one — the
    /// `Unanswered` law this module already keeps for a branch's base.
    #[test]
    fn a_tree_git_cannot_answer_about_is_unanswered_not_behind() {
        let dir = boss_testing::scratch::scratch_dir("boss-cli-freshness-tree-nogit");
        boss_testing::scratch::create_dir(&dir);
        let obs = observe_tree(&dir);
        assert_eq!(obs.standing, Base::Unanswered, "{obs:?}");
        assert!(obs.unreadable.is_some(), "{obs:?}");
    }

    /// THE DOOR-FRESHNESS SPLIT, applied to the proof (backlog a09bd894):
    /// a recorded proof is a WRITE — an immutable fact in the audit log
    /// about a tree nobody can re-read later — so a stale checkout
    /// REFUSES it; a rehearsal that records nothing gets the same facts
    /// as a warning beside its answer.
    #[test]
    fn a_stale_tree_refuses_a_recorded_proof_and_warns_a_rehearsal() {
        let behind = TreeObservation {
            standing: Base::Behind,
            head: "1c63ca24ffff".into(),
            main_head: "de960a2affff".into(),
            behind_by: 9,
            unreadable: None,
        };
        let BaseGuard::Refuse(why) = stale_tree_guard(&behind, true, false) else {
            panic!("a recorded proof off a stale tree must be refused");
        };
        assert!(why.contains("1c63ca2") && why.contains("de960a2"), "{why}");
        assert!(why.contains('9'), "it names how far behind: {why}");
        // The remedy and both escapes, because a door with no escape
        // gets routed around (CLAUDE.md §Doors).
        assert!(why.contains("merge --ff-only origin/main"), "{why}");
        assert!(why.contains("--dry"), "{why}");
        assert!(why.contains("BOSS_DOOR_FRESHNESS=off"), "{why}");

        for (records, silenced) in [(false, false), (true, true)] {
            let BaseGuard::Note(note) = stale_tree_guard(&behind, records, silenced) else {
                panic!("records={records} silenced={silenced} must not refuse");
            };
            assert!(
                note.contains("BEHIND"),
                "the facts still ride along: {note}"
            );
            assert!(note.contains("1c63ca2"), "{note}");
        }
    }

    /// AND IT SAYS WHICH TREE IT READ EVEN WHEN NOTHING IS WRONG. The
    /// measured defect was not only the staleness: "nothing in the
    /// output says which tree was read", so a proof against production
    /// and a proof against a six-hour-old tree looked identical.
    #[test]
    fn a_current_or_unreadable_tree_is_still_stated() {
        let current = TreeObservation {
            standing: Base::Current,
            head: "abcdef01".into(),
            main_head: "abcdef01".into(),
            ..Default::default()
        };
        let BaseGuard::Note(note) = stale_tree_guard(&current, true, false) else {
            panic!("a current tree is never refused");
        };
        assert!(
            note.contains("abcdef0"),
            "it names the tree it read: {note}"
        );

        let unknown = TreeObservation {
            standing: Base::Unanswered,
            unreadable: Some("not a git repository".into()),
            ..Default::default()
        };
        let BaseGuard::Note(note) = stale_tree_guard(&unknown, true, false) else {
            panic!("a tree that cannot be READ is not a tree that is stale");
        };
        assert!(note.contains("not a git repository"), "{note}");
    }

    /// AND IT KEEPS THE HEAD IT DID READ. `refs/remotes/origin/main`
    /// can be absent exactly where a probe is most worth stamping — the
    /// forge's converged checkout is the tree every recorded probe
    /// reads, and a clone's remote-tracking ref is not a thing this
    /// code may assume. Discarding a head git ANSWERED because a second
    /// ref did not is reducing a record before storing it, which throws
    /// away the only copy (CLAUDE.md §Diagnosis). The standing is still
    /// `Unanswered`: what could not be read is not a staleness finding.
    #[test]
    fn a_tree_with_no_origin_main_still_names_the_head_it_read() {
        let f = Forge::build("boss-cli-freshness-tree-no-main");
        let head = observe_tree(&f.clone).head;
        assert!(!head.is_empty(), "the fixture clone has a HEAD");
        Forge::git(&f.clone, &["update-ref", "-d", MAIN_REF]);
        let obs = observe_tree(&f.clone);
        assert_eq!(obs.standing, Base::Unanswered, "{obs:?}");
        assert_eq!(obs.head, head, "the head git answered survives: {obs:?}");
        assert!(obs.unreadable.is_some(), "{obs:?}");
    }

    /// THE PROOF STAMP (backlog 6f581de6). A recorded proof carried
    /// `host` and `cwd` — where it ran — and nothing about WHAT it
    /// read, while a directory's contents change under it. These are
    /// the two keys that close that, read off the observation the guard
    /// above already makes at the moment the probe runs.
    #[test]
    fn the_proof_keys_name_the_tree_and_its_standing() {
        let behind = TreeObservation {
            standing: Base::Behind,
            head: "1c63ca24ffff".into(),
            main_head: "de960a2affff".into(),
            behind_by: 9,
            unreadable: None,
        };
        let md = tree_metadata(&behind);
        assert_eq!(md["tree_head"], json!("1c63ca24ffff"));
        assert_eq!(md["tree_standing"], json!("behind"));

        let current = TreeObservation {
            standing: Base::Current,
            head: "abcdef01".into(),
            main_head: "abcdef01".into(),
            ..Default::default()
        };
        assert_eq!(tree_metadata(&current)["tree_standing"], json!("current"));

        // A head that could not be read is NULL, not the empty string:
        // absent is a different fact from empty, and `tree_standing`
        // says which of the two this is.
        let unknown = TreeObservation {
            standing: Base::Unanswered,
            unreadable: Some("not a git repository".into()),
            ..Default::default()
        };
        let md = tree_metadata(&unknown);
        assert_eq!(md["tree_standing"], json!("unreadable"));
        assert_eq!(
            md.get("tree_head"),
            Some(&Value::Null),
            "the key is present and null, so a proof whose tree could not be \
             read reads differently from one recorded before this existed"
        );
    }

    /// A RUN THAT RECORDS NOTHING IS TOLD, NEVER REFUSED (backlog
    /// 6f581de6). `boss prove --recheck` re-runs a recorded probe and
    /// writes nothing, and a human acts on its HOLDS / NO LONGER HOLDS
    /// off whatever tree happens to be present. Silence there is the
    /// same defect one step removed — but a refusal is disproportionate
    /// to a run that leaves no artifact, so the note is the answer, at
    /// every standing and with the escape set or not.
    #[test]
    fn a_run_that_records_nothing_is_told_the_tree_and_never_refused() {
        let behind = TreeObservation {
            standing: Base::Behind,
            head: "1c63ca24ffff".into(),
            main_head: "de960a2affff".into(),
            behind_by: 9,
            unreadable: None,
        };
        let current = TreeObservation {
            standing: Base::Current,
            head: "abcdef01".into(),
            main_head: "abcdef01".into(),
            ..Default::default()
        };
        let unknown = TreeObservation {
            standing: Base::Unanswered,
            unreadable: Some("not a git repository".into()),
            ..Default::default()
        };
        for obs in [&behind, &current, &unknown] {
            for silenced in [false, true] {
                assert!(
                    matches!(stale_tree_guard(obs, false, silenced), BaseGuard::Note(_)),
                    "records=false must never refuse: {obs:?}"
                );
                assert_eq!(
                    unrecorded_tree_note(obs, silenced),
                    match stale_tree_guard(obs, false, silenced) {
                        BaseGuard::Note(n) | BaseGuard::Refuse(n) => n,
                    },
                    "the unrecorded note is the guard's own text: {obs:?}"
                );
            }
        }
        assert!(unrecorded_tree_note(&behind, false).contains("BEHIND"));
        assert!(unrecorded_tree_note(&behind, false).contains("1c63ca2"));
        assert!(
            unrecorded_tree_note(&current, false).contains("abcdef0"),
            "it names the tree even when nothing is wrong"
        );
    }
}
