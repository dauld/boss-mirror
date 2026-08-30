//! `boss merged <branch>` — did this change actually land on main?
//!
//! WHY THIS IS A VERB. Backlog-item 26b3d203 classified fourteen errors
//! from one session: seven were an ad-hoc shell check producing a
//! confident WRONG answer, and this exact question accounts for three of
//! them. Every occurrence was a fresh pipeline written to answer it,
//! and each failed differently:
//!
//!   - `git merge-base --is-ancestor gcp/<branch> <main>` reported NOT
//!     MERGED for all 89 cars, because an arrived train DELETES its
//!     cars' branches and a missing ref makes merge-base error. A
//!     branch being gone is the SIGNATURE of success here, and the
//!     check read it as failure.
//!   - Testing the car's receipt sha reported NOT MERGED for all 89,
//!     because trains SQUASH-merge, so a car's commit is never an
//!     ancestor of main no matter how thoroughly it landed.
//!   - Content-checking against `gcp/realmain` reported the change
//!     absent, because that ref is a stale local alias and not what
//!     trains merge into.
//!
//! All three answered "no" for reasons unrelated to the question. That
//! is the failure this module exists to make unrepeatable: the rules
//! live in one pure function with a test per documented failure, rather
//! than being re-derived in a shell each time someone asks.
//!
//! THE AUTHORITY IS THE FORGE, read live. A local ref can be stale, and
//! `origin` on a workstation is the GitHub mirror rather than the forge
//! ([[boss-where-to-push-a-car]]). So main is resolved by `ls-remote`
//! against the remote that actually receives trains.

use anyhow::Result;

/// How a change was found on main. Recorded because the two paths have
/// different strength, and a reader deserves to know which one answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// The branch head is reachable from main — a true merge commit.
    Ancestor,
    /// No commit on the branch carries a patch that main lacks. This is
    /// how a SQUASH merge is detected: the squashed commit is not the
    /// branch's commit, but it applies the same patch.
    PatchesPresent,
}

/// The answer. `Unknown` is a first-class outcome, not an error case —
/// the whole point is that "I could not tell" must never be reported as
/// "no".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Merged(How),
    NotMerged,
    Unknown(String),
}

/// What the git plumbing managed to observe. Every field is optional
/// because every one of them has failed to be observable in practice,
/// and the point of this type is that an unobservable input produces
/// `Unknown` rather than a default.
#[derive(Debug, Clone, Default)]
pub struct Observations {
    /// Main's head on the FORGE, via ls-remote. `None` = could not read.
    pub main_sha: Option<String>,
    /// The branch's head. `None` = the ref does not exist anywhere,
    /// which for an arrived car is expected rather than damning.
    pub branch_sha: Option<String>,
    /// Is the branch head reachable from main? `None` = not determinable
    /// (typically because one of the two objects is missing locally).
    pub is_ancestor: Option<bool>,
    /// Commits on the branch whose patch main does not already carry,
    /// by patch-id. `Some(0)` means everything the branch did is in
    /// main. `None` = could not compute.
    pub unmerged_patches: Option<usize>,
}

/// The decision, pure.
///
/// ORDER MATTERS AND IS THE WHOLE DESIGN. Every rule below is a
/// documented wrong answer from 26b3d203, turned into a case:
///
///  1. No main -> Unknown. Without the authority there is no question,
///     and the old pipelines answered "not merged" here.
///  2. Ancestor -> Merged. The unambiguous case.
///  3. Zero unmerged patches -> Merged. The squash case, which
///     ancestry alone always gets wrong.
///  4. No branch anywhere, nothing else conclusive -> Unknown. A
///     deleted branch is what an ARRIVED train leaves behind; reading
///     it as "not merged" inverted the truth for 89 cars.
///  5. Both signals present and negative -> NotMerged. The only path
///     to a definite no.
///  6. Anything else -> Unknown, naming what was missing.
pub fn verdict(o: &Observations) -> Verdict {
    if o.main_sha.is_none() {
        return Verdict::Unknown(
            "could not read main from the forge — without the authority this \
             question has no answer, and reporting `not merged` here is how \
             89 cars were once misreported"
                .into(),
        );
    }
    if o.is_ancestor == Some(true) {
        return Verdict::Merged(How::Ancestor);
    }
    if o.unmerged_patches == Some(0) {
        return Verdict::Merged(How::PatchesPresent);
    }
    if o.branch_sha.is_none() {
        return Verdict::Unknown(
            "the branch does not exist on the forge or locally. For a car this \
             is the SIGNATURE of an arrived train, which deletes the branches \
             it merged — not evidence that it never landed. Check the car's \
             packet, or name the merge commit directly"
                .into(),
        );
    }
    match (o.is_ancestor, o.unmerged_patches) {
        (Some(false), Some(n)) if n > 0 => Verdict::NotMerged,
        _ => Verdict::Unknown(
            "not enough was observable to answer: needs both an ancestry check \
             and a patch comparison against main"
                .into(),
        ),
    }
}

fn sh(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(args[0])
        .args(&args[1..])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Gather what git can see. Each probe is independently allowed to fail.
pub fn observe(clone: &str, remote: &str, branch: &str) -> Observations {
    let main_sha = sh(&["git", "-C", clone, "ls-remote", remote, "refs/heads/main"])
        .and_then(|s| s.split_whitespace().next().map(str::to_string));

    // The branch may live on the forge, locally, or nowhere.
    let branch_sha = sh(&[
        "git",
        "-C",
        clone,
        "ls-remote",
        remote,
        &format!("refs/heads/{branch}"),
    ])
    .and_then(|s| s.split_whitespace().next().map(str::to_string))
    .or_else(|| {
        sh(&[
            "git",
            "-C",
            clone,
            "rev-parse",
            "--verify",
            "--quiet",
            branch,
        ])
    });

    let (mut is_ancestor, mut unmerged_patches) = (None, None);
    if let (Some(m), Some(b)) = (&main_sha, &branch_sha) {
        // Both objects have to be present locally to compare them.
        let have = |s: &str| {
            std::process::Command::new("git")
                .args(["-C", clone, "cat-file", "-e", &format!("{s}^{{commit}}")])
                .status()
                .map(|st| st.success())
                .unwrap_or(false)
        };
        if have(m) && have(b) {
            is_ancestor = std::process::Command::new("git")
                .args(["-C", clone, "merge-base", "--is-ancestor", b, m])
                .status()
                .ok()
                .map(|st| st.success());
            // `git cherry <upstream> <head>` lists the head's commits by
            // patch-id, prefixing `+` for those upstream lacks. Counting
            // the `+` lines is how a squash merge reads as landed.
            unmerged_patches = sh(&["git", "-C", clone, "cherry", m, b])
                .map(|s| s.lines().filter(|l| l.starts_with('+')).count())
                .or(Some(0));
        }
    }

    Observations {
        main_sha,
        branch_sha,
        is_ancestor,
        unmerged_patches,
    }
}

pub fn run(branch: &str, clone: Option<String>, remote: Option<String>) -> Result<()> {
    let clone = clone.unwrap_or_else(|| ".".to_string());
    let remote = remote.unwrap_or_else(|| "origin".to_string());
    let o = observe(&clone, &remote, branch);
    let v = verdict(&o);

    println!("boss merged: {branch}");
    println!(
        "  main({remote}) {}",
        o.main_sha.as_deref().unwrap_or("UNREADABLE")
    );
    println!(
        "  branch        {}",
        o.branch_sha.as_deref().unwrap_or("absent")
    );
    match &v {
        Verdict::Merged(How::Ancestor) => {
            println!("  MERGED — the branch head is reachable from main");
        }
        Verdict::Merged(How::PatchesPresent) => {
            println!(
                "  MERGED — main already carries every patch on this branch \
                 (squash-merged; ancestry alone would say no)"
            );
        }
        Verdict::NotMerged => {
            println!("  NOT MERGED — main carries neither the commit nor its patches");
            // THE ONE ANSWER THIS VERB MUST NOT GIVE QUIETLY.
            //
            // A definite "no" is correct only if `remote` is the trunk
            // this repo actually merges into. Point it at a clone whose
            // own `main` lags — a conductor working copy, a workstation
            // whose `origin` is the GitHub mirror — and every unmerged
            // AND merged branch reads the same way. That is failure
            // three of the three in 26b3d203, and this verb reproduced
            // it on its first real invocation: `--remote gcp` answered
            // NOT MERGED for a car that had squash-merged hours
            // earlier, because that clone's main was two trains behind.
            //
            // The verb cannot tell which remote is authoritative, so it
            // names the one it asked and says what would make the
            // answer wrong. A checkable no beats a confident one.
            println!(
                "  ...but only if `{remote}` is the trunk this repo merges into. A remote \n\
                 \x20    whose main lags — a conductor's working clone, or a workstation whose \n\
                 \x20    `origin` is the GitHub mirror — answers NOT MERGED for landed work too."
            );
            if let Some(m) = &o.main_sha
                && let Some(when) = sh(&["git", "-C", &clone, "log", "-1", "--format=%ci", m])
            {
                println!("  ...main({remote}) is dated {when}");
            }
        }
        Verdict::Unknown(why) => println!("  UNKNOWN — {why}"),
    }
    // An Unknown is not a failure of the verb, but it must not be
    // mistaken for a "no" by a script reading the exit code.
    match v {
        Verdict::Merged(_) => Ok(()),
        Verdict::NotMerged => std::process::exit(1),
        Verdict::Unknown(_) => std::process::exit(2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs() -> Observations {
        Observations {
            main_sha: Some("aaaa".into()),
            branch_sha: Some("bbbb".into()),
            is_ancestor: Some(false),
            unmerged_patches: Some(3),
        }
    }

    /// THE 89-CAR FAILURE, FIRST FORM. An arrived train deletes the
    /// branches it merged, so the ref is gone. The old check read a
    /// missing ref as "not merged" and inverted the truth for every car.
    #[test]
    fn a_deleted_branch_is_unknown_not_a_no() {
        let o = Observations {
            branch_sha: None,
            is_ancestor: None,
            unmerged_patches: None,
            ..obs()
        };
        match verdict(&o) {
            Verdict::Unknown(why) => assert!(
                why.contains("arrived train"),
                "the reason must say why a missing branch is expected: {why}"
            ),
            other => panic!("a deleted branch must not be a definite answer: {other:?}"),
        }
    }

    /// THE 89-CAR FAILURE, SECOND FORM. Trains squash-merge, so a car's
    /// commit is never an ancestor of main however completely it landed.
    /// Ancestry alone answers no; the patch comparison answers yes.
    #[test]
    fn a_squash_merge_reads_as_merged() {
        let o = Observations {
            is_ancestor: Some(false),
            unmerged_patches: Some(0),
            ..obs()
        };
        assert_eq!(verdict(&o), Verdict::Merged(How::PatchesPresent));
    }

    /// An unreadable authority is the case the old pipelines got most
    /// wrong: they reported "not merged" when they had simply failed to
    /// look. Fail closed — same rule as the gate's concurrency guard.
    #[test]
    fn an_unreadable_main_is_never_a_no() {
        let o = Observations {
            main_sha: None,
            ..obs()
        };
        match verdict(&o) {
            Verdict::Unknown(why) => assert!(why.contains("authority")),
            other => panic!("without main there is no answer: {other:?}"),
        }
    }

    #[test]
    fn a_true_merge_is_merged_by_ancestry() {
        let o = Observations {
            is_ancestor: Some(true),
            unmerged_patches: Some(2),
            ..obs()
        };
        assert_eq!(verdict(&o), Verdict::Merged(How::Ancestor));
    }

    /// The only path to a definite no: both signals present, both
    /// negative. Anything less is Unknown.
    #[test]
    fn a_definite_no_needs_both_signals() {
        assert_eq!(verdict(&obs()), Verdict::NotMerged);

        let half = Observations {
            unmerged_patches: None,
            ..obs()
        };
        assert!(
            matches!(verdict(&half), Verdict::Unknown(_)),
            "one signal is not enough to say no"
        );
    }
}
