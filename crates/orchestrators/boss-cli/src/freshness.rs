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
        return Ok(Rebased {
            old_head: old_head.clone(),
            new_head: old_head,
            replayed: 0,
        });
    }
    let tmp = std::env::temp_dir().join(format!(
        "boss-gate-rebase-{}-{}",
        std::process::id(),
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
            let said = String::from_utf8_lossy(&pick.stderr).trim().to_string();
            let _ = crate::git_auth::command()
                .arg("-C")
                .arg(&tmp_str)
                .args(["cherry-pick", "--abort"])
                .output();
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
    let new_head = match ok(
        &crate::git_auth::command()
            .arg("-C")
            .arg(&tmp_str)
            .args(["rev-parse", "HEAD"])
            .output()
            .context("reading the replayed head")?,
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
    if !push.status.success() {
        bail!(
            "boss gate --rebase: the replay succeeded but the push was refused: {}. The branch \
             on the forge is unchanged.",
            String::from_utf8_lossy(&push.stderr).trim()
        );
    }
    // Keep the caller's remote-tracking ref honest; the local branch,
    // if checked out somewhere, is theirs to move.
    let _ = git(&["fetch", "-q", "origin", &format!("refs/heads/{branch}")]);
    Ok(Rebased {
        old_head,
        new_head,
        replayed: commits.len(),
    })
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
    let mut md = json!({
        "base_standing": match obs.standing {
            Base::Current => "current",
            Base::Behind => "behind",
            Base::Unanswered => "unreadable",
        },
    });
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
}
