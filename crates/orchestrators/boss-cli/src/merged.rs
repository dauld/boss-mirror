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
    /// Main already holds the branch's version of every file the branch
    /// changed. This is the only signal that survives a train, and it is
    /// the one a human actually uses to answer the question.
    ContentPresent,
}

/// How much of what the branch changed is already in main.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContentMatch {
    /// Files the branch changed against its merge-base.
    pub total: usize,
    /// Of those, how many main already matches byte for byte.
    pub matching: usize,
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
    /// How many of the branch's own files main already matches.
    ///
    /// PATCH-ID CANNOT SEE A TRAIN. A train squashes N cars into ONE
    /// commit, so that commit's patch is the UNION of N cars' changes
    /// and its patch-id equals no individual car's. `git cherry`
    /// therefore reports every car of a multi-car train as unmerged —
    /// which it did, for two cars of train #147 whose files are
    /// byte-identical in main (0d1310f3). Content survives that,
    /// because it asks the question a human asks: is my change there?
    pub content: Option<ContentMatch>,
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
///  4. Every changed file already matching main -> Merged. The TRAIN
///     case, which rules 2 and 3 both get wrong: a train squashes N
///     cars into one commit whose patch-id matches no single car
///     (0d1310f3).
///  5. No branch anywhere, nothing else conclusive -> Unknown. A
///     deleted branch is what an ARRIVED train leaves behind; reading
///     it as "not merged" inverted the truth for 89 cars.
///  6. SOME files matching -> Unknown, with the count. A sibling car
///     editing one of this branch's files is indistinguishable from a
///     partial landing, so neither is asserted.
///  7. All three signals present and negative -> NotMerged. The only
///     path to a definite no.
///  8. Anything else -> Unknown, naming what was missing.
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
    // CONTENT BEFORE THE NEGATIVE SIGNALS. A car batched into a train
    // with siblings fails both checks above by construction, so this is
    // the rule that answers correctly for the ordinary case.
    if let Some(c) = o.content
        && c.total > 0
        && c.matching == c.total
    {
        return Verdict::Merged(How::ContentPresent);
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
    // PARTIALLY PRESENT IS NOT A NO. Some of the branch's files match
    // main and some do not, which has two innocent explanations as well
    // as a guilty one: the car landed and a LATER car edited one of its
    // files, or the car landed and was since amended. Calling that
    // `not merged` is the same overconfidence this verb was built to
    // retire, so it is reported as what it is.
    if let Some(c) = o.content
        && c.matching > 0
        && c.matching < c.total
    {
        return Verdict::Unknown(format!(
            "main already matches {} of the {} files this branch changed. That is \
             not a no: a later car editing one of those files looks exactly like \
             this. Compare the remaining ones by hand, or name the merge commit",
            c.matching, c.total
        ));
    }
    match (o.is_ancestor, o.unmerged_patches, o.content) {
        // A definite no now needs the content check to agree: nothing
        // the branch changed is in main.
        (Some(false), Some(n), Some(c)) if n > 0 && c.total > 0 && c.matching == 0 => {
            Verdict::NotMerged
        }
        // Content unavailable (a branch that changed nothing, or an
        // unreadable merge-base) falls back to the original pair.
        (Some(false), Some(n), None) if n > 0 => Verdict::NotMerged,
        _ => Verdict::Unknown(
            "not enough was observable to answer: needs an ancestry check, a patch \
             comparison, and a content comparison against main"
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

    let (mut is_ancestor, mut unmerged_patches, mut content) = (None, None, None);
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
            content = content_match(clone, m, b);
        }
    }

    Observations {
        main_sha,
        branch_sha,
        is_ancestor,
        unmerged_patches,
        content,
    }
}

/// How many of the files this branch touched does main already match?
///
/// Deliberately scoped to the branch's OWN files, taken against its
/// merge-base — everything else in main is another car's business. That
/// scoping is what makes the answer survive a train: it does not matter
/// how many siblings shared the squash commit, only whether this
/// branch's work is present.
///
/// Renames and deletions come out right for free, because `git diff`
/// between the two trees reports a path as differing precisely when the
/// two sides disagree about its content — including when one side does
/// not have it.
fn content_match(clone: &str, main: &str, branch: &str) -> Option<ContentMatch> {
    let base = sh(&["git", "-C", clone, "merge-base", main, branch])?;
    let changed = sh(&["git", "-C", clone, "diff", "--name-only", &base, branch])?;
    let files: Vec<&str> = changed.lines().filter(|l| !l.is_empty()).collect();
    if files.is_empty() {
        return None;
    }
    let mut args = vec![
        "git",
        "-C",
        clone,
        "diff",
        "--name-only",
        main,
        branch,
        "--",
    ];
    args.extend(files.iter().copied());
    // No differing files at all is a clean landing; `sh` returns None on
    // empty output, which is exactly that case.
    let differing = match sh(&args) {
        Some(s) => s.lines().filter(|l| !l.is_empty()).count(),
        None => 0,
    };
    Some(ContentMatch {
        total: files.len(),
        matching: files.len().saturating_sub(differing),
    })
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
        Verdict::Merged(How::ContentPresent) => {
            let n = o.content.map_or(0, |c| c.total);
            println!(
                "  MERGED — main already holds this branch's version of all {n} file(s) \
                 it changed.\n\
                 \x20   Patch comparison says no here and is wrong: a train squashes several \n\
                 \x20   cars into ONE commit, so no single car's patch-id matches it."
            );
        }
        Verdict::NotMerged => {
            println!(
                "  NOT MERGED — main carries neither the commit, nor its patches, nor \
                 the content of any file it changed"
            );
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
            content: Some(ContentMatch {
                total: 4,
                matching: 0,
            }),
        }
    }

    /// THE TRAIN CASE, and the reason this verb needed a third signal.
    /// A train squashes N cars into ONE commit, so that commit's patch
    /// is the union of N cars' changes and its patch-id matches no
    /// individual car. Both older signals therefore say "no" for a car
    /// that plainly landed: on 2026-08-30 two cars of train #147 were
    /// reported NOT MERGED while their files were byte-identical in
    /// main (0d1310f3).
    #[test]
    fn a_car_batched_into_a_train_reads_as_merged_by_content() {
        let batched = Observations {
            is_ancestor: Some(false),
            unmerged_patches: Some(1),
            content: Some(ContentMatch {
                total: 12,
                matching: 12,
            }),
            ..obs()
        };
        assert_eq!(verdict(&batched), Verdict::Merged(How::ContentPresent));
    }

    /// PARTIAL IS NOT A NO. A sibling car on the same train editing one
    /// of this branch's files looks exactly like a partial landing —
    /// which is not hypothetical: feat/steps-pair-by-slug matched 13 of
    /// 14 files, and the fourteenth was `infra/gate.sh`, which the
    /// dead-styles car on the SAME train also touched.
    #[test]
    fn a_partial_content_match_is_unknown_and_says_how_much() {
        let partial = Observations {
            content: Some(ContentMatch {
                total: 14,
                matching: 13,
            }),
            ..obs()
        };
        match verdict(&partial) {
            Verdict::Unknown(why) => {
                assert!(why.contains("13 of the 14"), "{why}");
                assert!(why.contains("not a no"), "{why}");
            }
            v => panic!("partial must not be decisive: {v:?}"),
        }
    }

    /// A definite no now needs all three to agree.
    #[test]
    fn a_definite_no_requires_the_content_check_too() {
        assert_eq!(verdict(&obs()), Verdict::NotMerged);
    }

    /// ...and ancestry still short-circuits everything, so a true merge
    /// is never demoted by a file a later car rewrote.
    #[test]
    fn ancestry_still_wins_over_a_stale_content_reading() {
        let merged = Observations {
            is_ancestor: Some(true),
            content: Some(ContentMatch {
                total: 4,
                matching: 1,
            }),
            ..obs()
        };
        assert_eq!(verdict(&merged), Verdict::Merged(How::Ancestor));
    }

    /// A branch that changed nothing, or an unreadable merge-base,
    /// leaves content unobservable — the original pair still answers.
    #[test]
    fn an_unobservable_content_check_falls_back_to_the_old_pair() {
        let no_content = Observations {
            content: None,
            ..obs()
        };
        assert_eq!(verdict(&no_content), Verdict::NotMerged);
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
