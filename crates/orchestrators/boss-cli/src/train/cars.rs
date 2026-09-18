//! A car at the dock — re-rail, publish, skip reasons, the boarding predicate.

use super::*;

/// The skip reason for a car whose branch would not merge onto this
/// window's train: names the conflicted files, truncated to stay
/// chip-sized. At least one file always shows.
/// Replay `branch`'s own commits on top of the consist as it stands,
/// returning a ref that merges cleanly — or `None` when the car has a
/// conflict a rebase cannot resolve either.
///
/// Rebases from the merge-base, so only the car's OWN work is replayed:
/// anything it carries that already reached main (the squash-merge
/// case) is dropped by git as an applied patch rather than re-applied
/// as a conflict.
///
/// Leaves the clone on `train_branch` whatever happens — a caller
/// mid-consist must not be handed a detached HEAD or a half-finished
/// rebase, and the next car in the loop merges into whatever branch it
/// finds itself on.
pub(super) fn rerail_onto_consist(
    clone: &str,
    train_branch: &str,
    branch: &str,
) -> Result<Option<String>> {
    let scratch = "boss-train-rerail";
    let car = format!("fork/{branch}");
    let base_out = sh_unchecked(&["git", "-C", clone, "merge-base", train_branch, &car])?;
    if !base_out.status.success() {
        return Ok(None);
    }
    let base = stdout_str(&base_out).trim().to_string();
    if base.is_empty() {
        return Ok(None);
    }
    sh_unchecked(&["git", "-C", clone, "checkout", "-q", "-B", scratch, &car])?;
    let rebase = sh_unchecked(&[
        "git",
        "-C",
        clone,
        "rebase",
        "--onto",
        train_branch,
        &base,
        scratch,
    ])?;
    if !rebase.status.success() {
        sh_unchecked(&["git", "-C", clone, "rebase", "--abort"])?;
        sh_unchecked(&["git", "-C", clone, "checkout", "-q", train_branch])?;
        return Ok(None);
    }
    sh_unchecked(&["git", "-C", clone, "checkout", "-q", train_branch])?;
    Ok(Some(scratch.to_string()))
}

pub(crate) fn skip_reason_conflict(conflicted: &[String], file_budget: usize) -> String {
    if conflicted.is_empty() {
        return "conflict: unresolved (merge died before conflict markers)".to_string();
    }
    let mut shown = 0usize;
    let mut len = 0usize;
    for f in conflicted {
        let add = f.len() + if shown == 0 { 0 } else { 2 };
        if shown > 0 && len + add > file_budget {
            break;
        }
        shown += 1;
        len += add;
    }
    let files = conflicted[..shown].join(", ");
    match conflicted.len() - shown {
        0 => format!("conflict: {files}"),
        hidden => format!("conflict: {files} +{hidden} more"),
    }
}

/// Put a car's branch on the fork when the conductor can already see
/// it, and say whether it did.
///
/// TWO SOURCES, TRIED IN ORDER, and both are refs the author already
/// published — copying one to the fork is not a judgement call.
///
/// 1. `origin/<branch>`. The natural place to push is the upstream you
///    cloned; the fork is an implementation detail of how this
///    conductor assembles a train. On 2026-08-14 that gap silently
///    held NINE cars for a session — the dock reported 12 parked while
///    the boardable count was 0, because `parked_ready` asks "branch
///    declared, review ready" and boarding asks "branch on the fork".
///
/// 2. `refs/heads/<branch>` — a LOCAL ref in the conductor's own
///    clone. This is what `git push gcp <branch>` produces, because
///    the `gcp` remote IS /var/lib/boss-train/repo. The ref lands here
///    on no remote, and the car was skipped while its branch sat in
///    the conductor's working copy. On 2026-08-16 that cost five
///    hand-run pushes in one evening: a human ran `git push origin`
///    from this very directory, with the credentials the conductor
///    already holds. The old comment called a branch in neither place
///    "never pushed at all", which stopped being true the moment
///    anyone could push to this clone directly.
///
/// Returns `Ok(false)` when neither ref exists — that car really was
/// never pushed, and skipping it is correct. Never called in dry mode.
///
/// WHICH REF WINS WHEN BOTH EXIST: the DESCENDANT, never "whichever
/// was listed first". Until 2026-08-17 this returned on the first ref
/// that pushed successfully, with `origin/<branch>` listed first, so a
/// stale remote-tracking ref beat the car's real head. Two trains in a
/// row assembled code their author had already fixed
/// (`feat/presence-assurance` origin=58083e8 vs local=6cfc15e;
/// `feat/estate-subjects` origin=0381fe6 vs local=d00307b, eight
/// commits behind) and both failed CI on those exact fixed errors —
/// silently, while reporting success. Packet `9150dc6b`.
///
/// Reordering the list is NOT sufficient, and the second test below
/// is the reason: this function is only ever called when the fork
/// does not have the branch, so *any* source pushes cleanly and
/// nothing rejects a stale one. Fast-forward rejection cannot be
/// leaned on as the tiebreak — the choice has to be made here.
///
/// Both remotes are the same forge in production, so "ahead" is the
/// only thing that distinguishes the refs. Unrelated histories cannot
/// be ordered, so the local head wins as the more recent statement of
/// intent in this clone.
pub(crate) fn publish_car_branch(clone: &str, branch: &str) -> Result<bool> {
    let exists = |src: &str| -> Result<bool> {
        Ok(
            sh_unchecked(&["git", "-C", clone, "rev-parse", "--verify", "--quiet", src])?
                .status
                .success(),
        )
    };
    // `<a> is an ancestor of <b>`, i.e. b is at or ahead of a.
    let is_ancestor = |a: &str, b: &str| -> Result<bool> {
        Ok(
            sh_unchecked(&["git", "-C", clone, "merge-base", "--is-ancestor", a, b])?
                .status
                .success(),
        )
    };

    let local = format!("refs/heads/{branch}");
    let upstream = format!("origin/{branch}");
    let (have_local, have_upstream) = (exists(&local)?, exists(&upstream)?);

    // THE FORGE IS THE AUTHORITY ON A CAR'S BRANCH. Read its current head
    // before deciding anything: a clone's remote-tracking ref is a memory
    // of an earlier fetch, and on 2026-09-05 a restacked car's forge
    // branch read as the PRE-restack sha after a boarding (packet
    // c1d56d13). If the forge already carries what this clone would
    // push — or has moved beyond it — there is nothing to publish, and
    // pushing a stale copy is exactly the overwrite this guards against.
    // The push below is a plain push (never --force), so git would refuse
    // a diverged head anyway; refusing here says WHY in the journal
    // instead of a silent rc=1.
    // A `+` refspec: the tracking ref may hold an OLDER head (the very
    // memory this guards against), and a plain fetch refuses to move it
    // backwards-and-sideways — which would leave the clone believing the
    // fork still carries what it last saw.
    let _ = sh_unchecked(&[
        "git",
        "-C",
        clone,
        "fetch",
        "-q",
        "fork",
        &format!("+refs/heads/{branch}:refs/remotes/fork/{branch}"),
    ]);
    let fork_ref = format!("fork/{branch}");
    if exists(&fork_ref)? {
        let candidate = match (have_local, have_upstream) {
            (true, _) => Some(local.clone()),
            (false, true) => Some(upstream.clone()),
            (false, false) => None,
        };
        if let Some(c) = candidate {
            let same = is_ancestor(&c, &fork_ref)? && is_ancestor(&fork_ref, &c)?;
            if same {
                // Already published: the fork carries exactly this head.
                // Nothing to push, and the answer is "yes, it is there" —
                // publishing twice is idempotent, not a refusal.
                return Ok(true);
            }
            let fork_ahead = is_ancestor(&c, &fork_ref)?;
            let diverged = !fork_ahead && !is_ancestor(&fork_ref, &c)?;
            if fork_ahead || diverged {
                let fork_head =
                    sh_unchecked(&["git", "-C", clone, "rev-parse", "--short", &fork_ref])?;
                log(format!(
                    "{branch}: the fork already carries {} ({}) — not publishing {c} over it",
                    stdout_str(&fork_head).trim(),
                    if fork_ahead {
                        "ahead of this clone"
                    } else {
                        "diverged from this clone"
                    }
                ));
                return Ok(false);
            }
        }
    }

    let src = match (have_local, have_upstream) {
        (false, false) => return Ok(false),
        (true, false) => local,
        (false, true) => upstream,
        // Prefer the local head unless upstream is strictly ahead of
        // it. Equal refs take the local branch, which is the same
        // commit by definition.
        (true, true) => {
            if is_ancestor(&local, &upstream)? && !is_ancestor(&upstream, &local)? {
                upstream
            } else {
                local
            }
        }
    };

    let pushed = sh_unchecked(&[
        "git",
        "-C",
        clone,
        "push",
        "fork",
        &format!("{src}:refs/heads/{branch}"),
    ])?;
    if !pushed.status.success() {
        return Ok(false);
    }
    // Name the ref and the sha that actually shipped. The 2026-08-17
    // diagnosis cost a second red train precisely because publishing
    // said nothing about WHAT it published.
    let sha = sh_unchecked(&["git", "-C", clone, "rev-parse", "--short", &src])?;
    log(format!(
        "published {branch} to the fork from {src} @ {}",
        String::from_utf8_lossy(&sha.stdout).trim()
    ));
    Ok(true)
}

/// The sha `publish_car_branch` would ship for this branch, or `None`
/// when the conductor can see no ref for it at all.
///
/// Exists so BOARDING can ask the same question PUBLISHING answers.
/// Until 2026-08-17 `candidates` only asked "is this branch on the
/// fork", which is not the question: once a branch is on the forge, a
/// later commit to it never gets there, so a car repaired after a red
/// train boards the version that just failed. It happened twice in one
/// day — `feat/dev-shared-target` was `3370b42` locally and `96109f7`
/// on the forge, and train 38d49597 assembled the red one. Packet
/// `7d2f30b9`.
pub(crate) fn car_head(clone: &str, branch: &str) -> Result<Option<String>> {
    for src in [format!("refs/heads/{branch}"), format!("origin/{branch}")] {
        let out = sh_unchecked(&["git", "-C", clone, "rev-parse", "--verify", "--quiet", &src])?;
        if out.status.success() {
            let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !sha.is_empty() {
                // Whichever of the two publish_car_branch would pick
                // resolves to a commit; comparing the LOCAL one first
                // is enough to notice a fork that has fallen behind,
                // and publish_car_branch still makes the final choice.
                return Ok(Some(sha));
            }
        }
    }
    Ok(None)
}

/// The head this car will actually board: the fork ref the consist is
/// assembled from.
///
/// `car_head` answers a DIFFERENT question — "is there a newer commit
/// anywhere that publishing should ship" — and prefers the conductor
/// clone's own `refs/heads` to answer it. That is right for deciding
/// whether to publish and wrong as an answer to "what will ride", and
/// the two come apart whenever a car is rebased and re-pushed. The
/// clone keeps the pre-rebase commit; `publish_car_branch` cannot
/// fast-forward the fork past it, so the fork rightly keeps the gated
/// commit and boards it. `rerail_onto_consist` reads `fork/{branch}`
/// and the boarded head is stamped from it, so this is the only ref a
/// gate receipt can honestly be checked against.
///
/// Measured on a live dock, 2026-08-29: car c6531868 was left behind
/// for "gated, then changed" while its receipt (56b817eb) matched the
/// fork exactly — the mismatch was against a local ref eight hours
/// older that no train would ever have carried.
pub(crate) fn fork_head(clone: &str, branch: &str) -> Result<Option<String>> {
    let out = sh_unchecked(&[
        "git",
        "-C",
        clone,
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("fork/{branch}"),
    ])?;
    if !out.status.success() {
        return Ok(None);
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok((!sha.is_empty()).then_some(sha))
}

/// The skip reason for a car parked at review whose branch was never
/// pushed to the fork.
pub(crate) fn skip_reason_branch_missing(branch: &str) -> String {
    format!("branch {branch} not on fork")
}

/// Which of a repo's action runs belong to this train and are still
/// burning the runner — the decision half of "cancelling a train
/// should cancel its CI", kept pure so it can be tested against real
/// API shapes rather than a fake.
///
/// MEASURED FIELD SHAPES, 2026-08-17, against this Forgejo. The
/// obvious key does not work: **`head_branch` is `null` on every run**
/// this deployment returns, so a filter written against it cancels
/// nothing at all, silently, which is indistinguishable from "there
/// was nothing to cancel". Two fields ARE populated and identify a
/// train's runs:
///   - `prettyref`  — `"#64"` for a pull_request run, i.e. the PR the
///     conductor already holds the url of;
///   - `commit_sha` — the train branch head the run was queued for.
/// Either is sufficient; both are matched so a run queued before the
/// PR existed is still caught.
///
/// CONSERVATIVE ON STATUS, deliberately. Only runs in a known-active
/// state are cancelled. The alternative — "anything not in a terminal
/// set" — cancels runs whose status this code has never heard of, and
/// the cost of a false cancel (killing someone's live run) is much
/// higher than the cost of a miss (the run finishes and wastes the
/// time it was already wasting).
pub(crate) fn cancellable_run_ids(runs: &[Value], pr_index: &str, head_sha: &str) -> Vec<i64> {
    const ACTIVE: [&str; 3] = ["running", "waiting", "blocked"];
    let want_ref = format!("#{pr_index}");
    runs.iter()
        .filter(|r| {
            let status = r.get("status").and_then(Value::as_str).unwrap_or_default();
            ACTIVE.contains(&status)
        })
        .filter(|r| {
            let pretty = r
                .get("prettyref")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let sha = r
                .get("commit_sha")
                .and_then(Value::as_str)
                .unwrap_or_default();
            (!pr_index.is_empty() && pretty == want_ref)
                || (!head_sha.is_empty() && !sha.is_empty() && sha == head_sha)
        })
        .filter_map(|r| r.get("id").and_then(Value::as_i64))
        .collect()
}

/// Is this ship-a-change Job a parked ready car — at review with a
/// branch declared and not already on a train? ONE definition, shared
/// by the boarding collector below and the cadence loop's dock-depth
/// probe (`boss train cadence`, the queue-depth basis): the count that
/// fires a boarding must be the same predicate boarding itself uses.
/// (The fork-branch existence check stays in `candidates` — it needs
/// the clone, and a car whose branch was never pushed still occupies
/// the dock from the author's point of view.)
pub(crate) fn parked_ready(job: &Value) -> bool {
    // "Parked" — named branch, no train stamp, review still waiting — is
    // core's predicate (`car::is_parked`), shared with `boss park` and
    // the auto-park handler so that what they refresh on a re-gate is
    // exactly what this counts at the dock. Boarding's own refinements
    // stay here: a `train/` branch is a consist, not a car, and a held
    // car is parked but must not ride.
    let md = job.get("metadata").cloned().unwrap_or(Value::Null);
    let branch = md.get("branch").and_then(Value::as_str).unwrap_or_default();
    if branch.starts_with("train/") || !boss_jobs::car::is_parked(job) {
        return false;
    }
    let review = find_step(job, "review", "Open for review");
    // A HELD car does not board. `metadata.hold` on the review step is
    // set by whoever parked it, and says "this is gated green and still
    // must not ride yet" - a car whose branch is correct but whose
    // WORLD is not. The case that forced this: a car repointing the
    // gate rig at a node label was parked green, and hours later the
    // only node carrying that label was cordoned for a hardware fault.
    // Landing it would have left the rig unschedulable and stopped
    // gating entirely. A note was written on the review step and the
    // conductor could not read it, because this predicate asked only
    // about status - documentation standing where a mechanism belonged.
    //
    // It lives HERE rather than in the boarding collector because the
    // cadence loop shares this predicate for dock depth: a held car
    // must not count toward the threshold either, or it would fire a
    // train it then declines to join, producing the empty windows that
    // made arrival rate unreadable (feedback f4baea39).
    //
    // THE MARKER READ IS NOT LOCAL. `stranded::hold_reason` is the one
    // definition of "is this marker set" — `null`/`false`/blank are no
    // marker, `true` is a marker with no reason — and the loading-dock
    // station row's `metadata_unmarked: ["hold"]` clause calls the same
    // function. This used to be a local `truthy`, which agreed by
    // coincidence rather than by construction; the station row had no
    // hold term at all until 20260910201453, and the two predicates
    // answering "is this car boardable" differently is the defect
    // backlog 36c3d4ca records (CLAUDE.md §9a).
    review
        .and_then(|s| s.get("metadata"))
        .and_then(boss_jobs::stranded::hold_reason)
        .is_none()
}

// ---------------------------------------------------------------------------
// The dock pin — `parked_ready` against the SEEDED loading-dock row.
// ---------------------------------------------------------------------------

/// "Is this car parked and ready to board?" is answered twice: here, by
/// the conductor, and by the `loading-dock` station row the yard, the
/// `/queue` endpoint and `boss orient` all read. On 2026-09-10 they
/// disagreed — the row had no hold term, so `/api/yard/status` reported
/// `dock_depth: 2, threshold_met: true` over two cars that could not
/// board (backlog 36c3d4ca). This is the equality test CLAUDE.md §9a
/// asks for while the duplication stands.
///
/// **A PIN IS A HOLDING ACTION, NOT A DESTINATION.** The destination is
/// the conductor reading the row instead of carrying its own copy —
/// `GET /api/stations/loading-dock/queue` already serves exactly this
/// predicate. That is not this car: `parked_ready` is a pure
/// `fn(&Value) -> bool` called from the boarding collector AND the
/// cadence loop's depth probe, and making it HTTP-bound puts every
/// landing behind a new network dependency, which is the failure mode
/// "a conductor loop write must not be fatal" was written about. So both
/// predicates stay, and this test refuses to let them drift again.
#[cfg(test)]
mod dock_pin {
    use super::parked_ready;
    use boss_core::job::{Job, JobStatus, Priority, Step, StepStatus, Subject};
    use boss_jobs::{PgStations, StationRegistry};
    use serde_json::Value;

    /// ONE fixture, both shapes. The typed pair feeds the station
    /// predicate; its own serialisation feeds `parked_ready`, which
    /// reads a car as JSON off the API. Deriving the JSON rather than
    /// hand-writing it is the point — a hand-written copy can disagree
    /// with the types and the test would still pass.
    fn car(
        branch: &str,
        review: StepStatus,
        review_metadata: Value,
        train: Option<&str>,
    ) -> (Job, Vec<Step>) {
        let mut job = Job::new(
            "ship-a-change",
            Subject::new("custom", branch),
            format!("car {branch}"),
            "emp-1",
            Priority::Standard,
            chrono::NaiveDate::from_ymd_opt(2026, 9, 10).unwrap(),
        );
        job.status = JobStatus::Open;
        let mut metadata = serde_json::json!({ "branch": branch });
        if let Some(t) = train {
            metadata["train"] = serde_json::json!(t);
        }
        job.metadata = metadata;
        let mut step = Step::new(job.id, "task", "Open for review", 0);
        step.spec_slug = Some("review".to_string());
        step.status = review;
        step.metadata = review_metadata;
        (job, vec![step])
    }

    fn as_api_json(job: &Job, steps: &[Step]) -> Value {
        let mut v = serde_json::to_value(job).expect("a Job serialises");
        v["steps"] = serde_json::to_value(steps).expect("steps serialise");
        v
    }

    async fn seeded_dock(db: &boss_testing::TestDb) -> boss_jobs::StationSpec {
        PgStations::new(db.pool.clone())
            .get_active("loading-dock")
            .await
            .expect("the schema seeds one active loading-dock row")
    }

    /// Every case the dock actually holds, and the answer BOTH readers
    /// must give. Two deliberate differences are left out and named in
    /// the test below; they are the measure of what a full collapse
    /// would buy.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_seeded_dock_row_and_the_conductor_agree_on_every_car() {
        let db = boss_testing::TestDb::new().await;
        let spec = seeded_dock(&db).await;

        let cases: Vec<(&str, bool, (Job, Vec<Step>))> = vec![
            (
                "parked, nothing written on the review step",
                true,
                car("feat/a", StepStatus::Ready, serde_json::json!({}), None),
            ),
            (
                "parked and claimed — active is boardable too",
                true,
                car("feat/b", StepStatus::Active, serde_json::json!({}), None),
            ),
            (
                "held with a reason",
                false,
                car(
                    "feat/c",
                    StepStatus::Ready,
                    serde_json::json!({"hold": "waiting on a kubectl delete"}),
                    None,
                ),
            ),
            (
                "held with no reason recorded",
                false,
                car(
                    "feat/d",
                    StepStatus::Ready,
                    serde_json::json!({"hold": true}),
                    None,
                ),
            ),
            // THE TRAP: a released hold is `false`, not a deleted key.
            (
                "hold released by writing false",
                true,
                car(
                    "feat/e",
                    StepStatus::Ready,
                    serde_json::json!({"hold": false}),
                    None,
                ),
            ),
            (
                "hold released by blanking the field",
                true,
                car(
                    "feat/f",
                    StepStatus::Ready,
                    serde_json::json!({"hold": ""}),
                    None,
                ),
            ),
            (
                "metadata that says nothing about holding",
                true,
                car(
                    "feat/g",
                    StepStatus::Ready,
                    serde_json::json!({"note": "looks fine"}),
                    None,
                ),
            ),
            (
                "already aboard a train",
                false,
                car(
                    "feat/h",
                    StepStatus::Ready,
                    serde_json::json!({}),
                    Some("train-job-id"),
                ),
            ),
            (
                "review completed — past the dock",
                false,
                car("feat/i", StepStatus::Completed, serde_json::json!({}), None),
            ),
            (
                "review not yet ready",
                false,
                car("feat/j", StepStatus::Pending, serde_json::json!({}), None),
            ),
        ];

        for (what, expected, (job, steps)) in &cases {
            let conductor = parked_ready(&as_api_json(job, steps));
            assert_eq!(
                conductor, *expected,
                "`parked_ready` is wrong about a car {what}"
            );
            assert_eq!(
                spec.predicate.matches(job, steps),
                *expected,
                "the seeded loading-dock predicate is wrong about a car {what} \
                 — the row and the conductor have drifted, which is backlog \
                 36c3d4ca happening again"
            );
        }
    }

    /// THE TWO KNOWN DIFFERENCES, written as a test so they cannot be
    /// forgotten, and both unreachable for a real car:
    ///
    /// - a `train/` branch. `parked_ready` refuses one ("a consist is
    ///   not a car"); the predicate language has no prefix clause. It
    ///   cannot matter, because the row's `kind: ship-a-change` clause
    ///   never admits a consist — a train is a `pr-train` packet.
    /// - a blank `branch`. `metadata_present` means "present and
    ///   non-null", not "non-blank", so the row counts a car whose
    ///   branch is `""` and the conductor does not. Nothing writes one:
    ///   `boss park` and the auto-park handler both take the branch from
    ///   the gate receipt.
    ///
    /// Closing either means widening the predicate language again, which
    /// is worth doing only once a real car lands in one of these states.
    /// If one does, this test is where the evidence goes.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_differences_that_remain_are_unreachable() {
        let db = boss_testing::TestDb::new().await;
        let spec = seeded_dock(&db).await;

        let (consist, steps) = car(
            "train/20260910-2000",
            StepStatus::Ready,
            serde_json::json!({}),
            None,
        );
        assert!(!parked_ready(&as_api_json(&consist, &steps)));
        assert!(
            spec.predicate.matches(&consist, &steps),
            "documented difference: the row has no branch-prefix clause"
        );
        let mut pr_train = consist.clone();
        pr_train.kind = "pr-train".to_string();
        assert!(
            !spec.predicate.matches(&pr_train, &steps),
            "the `kind` clause is what makes that difference unreachable: a real \
             consist is a pr-train packet and the dock never admits one"
        );

        let (blank, steps) = car("", StepStatus::Ready, serde_json::json!({}), None);
        assert!(!parked_ready(&as_api_json(&blank, &steps)));
        assert!(
            spec.predicate.matches(&blank, &steps),
            "documented difference: `metadata_present` means non-null, not non-blank"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::train::test_support::*;

    // ---------------------------------------------------------------
    // Re-railing — the conductor's answer to a squash-merged base.
    // ---------------------------------------------------------------

    /// The exact shape that cost four hand re-rails on 2026-08-15: a
    /// car whose work is partly in main already, because the branch it
    /// was cut from was SQUASH-merged and so is not an ancestor of
    /// main. Merging re-applies the landed hunk onto itself; rebasing
    /// recognises it as already applied and drops it.
    #[test]
    fn a_car_whose_base_was_squash_merged_is_re_railed_not_skipped() {
        let dir = std::env::temp_dir().join(format!("boss-rerail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap();
        let git = |args: &[&str]| {
            let mut a = vec!["git", "-C", d];
            a.extend_from_slice(args);
            let out = sh_unchecked(&a).unwrap();
            assert!(out.status.success(), "git {args:?}: {}", stdout_str(&out));
        };
        let write = |name: &str, body: &str| {
            std::fs::write(dir.join(name), body).unwrap();
        };

        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        write("base.txt", "base\n");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "base"]);

        // The parent car adds a line; the child car is cut from it and
        // adds another to the SAME file.
        git(&["checkout", "-q", "-b", "parent"]);
        write("shared.txt", "from parent\n");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "parent work"]);
        git(&["checkout", "-q", "-b", "child"]);
        write("shared.txt", "from parent\nfrom child\n");
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "child work"]);

        // The parent lands as a SQUASH — new sha, same content, and
        // `parent` is now not an ancestor of main.
        git(&["checkout", "-q", "main"]);
        git(&["merge", "-q", "--squash", "parent"]);
        git(&["commit", "-q", "-m", "train: parent (squashed)"]);

        // The conductor's world: fork/<branch> refs and a train branch
        // cut from main.
        git(&["update-ref", "refs/remotes/fork/child", "child"]);
        git(&["checkout", "-q", "-B", "train", "main"]);

        // A plain merge collides on the line the squash already landed.
        let merged = sh_unchecked(&[
            "git",
            "-C",
            d,
            "merge",
            "--no-ff",
            "-m",
            "train: merge child",
            "fork/child",
        ])
        .unwrap();
        assert!(
            !merged.status.success(),
            "the bug only exists because this merge conflicts"
        );
        sh_unchecked(&["git", "-C", d, "merge", "--abort"]).unwrap();

        // Re-railing replays only the child's own commit and lands it.
        let rerailed = rerail_onto_consist(d, "train", "child")
            .unwrap()
            .expect("a squash-merged base is exactly what rebase resolves");
        let retry = sh_unchecked(&[
            "git",
            "-C",
            d,
            "merge",
            "--no-ff",
            "-m",
            "train: merge child (re-railed)",
            &rerailed,
        ])
        .unwrap();
        assert!(retry.status.success(), "re-railed car must merge cleanly");

        let body = std::fs::read_to_string(dir.join("shared.txt")).unwrap();
        assert_eq!(
            body, "from parent\nfrom child\n",
            "the child's work lands on top of the parent's, once"
        );

        // And the clone is left on the train branch, ready for the next
        // car in the loop.
        let head = sh_unchecked(&["git", "-C", d, "rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert_eq!(stdout_str(&head).trim(), "train");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A car that genuinely disagrees with the consist still gets
    /// skipped — re-railing must not paper over a real conflict.
    #[test]
    fn a_real_conflict_still_refuses_to_re_rail() {
        let dir = std::env::temp_dir().join(format!("boss-rerail-real-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap();
        let git = |args: &[&str]| {
            let mut a = vec!["git", "-C", d];
            a.extend_from_slice(args);
            let out = sh_unchecked(&a).unwrap();
            assert!(out.status.success(), "git {args:?}: {}", stdout_str(&out));
        };

        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@t"]);
        git(&["config", "user.name", "t"]);
        std::fs::write(dir.join("f.txt"), "original\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "base"]);

        git(&["checkout", "-q", "-b", "car"]);
        std::fs::write(dir.join("f.txt"), "the car's answer\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "car"]);

        git(&["checkout", "-q", "main"]);
        std::fs::write(dir.join("f.txt"), "a different answer\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "someone else"]);

        git(&["update-ref", "refs/remotes/fork/car", "car"]);
        git(&["checkout", "-q", "-B", "train", "main"]);

        assert!(
            rerail_onto_consist(d, "train", "car").unwrap().is_none(),
            "two answers to the same line is a conflict a human owns"
        );
        let head = sh_unchecked(&["git", "-C", d, "rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert_eq!(
            stdout_str(&head).trim(),
            "train",
            "a failed re-rail must still leave the clone usable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
    /// A car parked at review, branch pushed, not on a train.
    fn ready_car() -> serde_json::Value {
        json!({
            "id": "car-1",
            "metadata": {"branch": "feat/x"},
            "steps": [
                {"spec_slug": "review", "title": "Open for review", "status": "ready"}
            ],
        })
    }

    /// The receipt spot-check (742d1faa, David: "Agreed"): the two
    /// lies it exists to catch, the unverifiable cases, and the one
    /// honest pass.
    fn car_with_receipt(receipt: serde_json::Value) -> serde_json::Value {
        let mut c = ready_car();
        c["steps"].as_array_mut().unwrap().push(json!({
            "spec_slug": "gate",
            "title": "Green, and observed working",
            "status": "completed",
            "metadata": {"receipt": receipt.to_string()},
        }));
        c
    }

    /// A RE-GATE RECEIPT IS THE ONE THAT COUNTS.
    ///
    /// User feedback 64cae7e9: a car whose branch legitimately moved was
    /// unrepairable, because the gate step is immutable and its receipt
    /// correctly stops vouching for the new head. Two cars were abandoned
    /// on 2026-08-28 for exactly this. With `regate_receipt` on the car,
    /// the fresh receipt is read and the car boards — no new packet, and
    /// the car keeps its history.
    #[test]
    fn a_regate_receipt_supersedes_a_stale_gate_receipt() {
        let stale = r#"{"verdict":"green","head":"1111111111111111111111111111111111111111"}"#;
        let fresh = r#"{"verdict":"green","head":"2222222222222222222222222222222222222222"}"#;
        let car = json!({
            "metadata": {"regate_receipt": fresh},
            "steps": [{"spec_slug": "gate", "title": "Green, and observed working",
                       "status": "completed", "metadata": {"receipt": stale}}],
        });
        assert_eq!(
            receipt_skip_reason(&car, Some("2222222222222222222222222222222222222222")),
            None,
            "the re-gate vouches for the head being boarded; the car should board"
        );
    }

    /// Without a re-gate the original still rules, so a stale car is
    /// still refused and the reason still names the mismatch.
    #[test]
    fn no_regate_leaves_the_original_receipt_in_force() {
        let stale = r#"{"verdict":"green","head":"1111111111111111111111111111111111111111"}"#;
        let car = json!({
            "metadata": {},
            "steps": [{"spec_slug": "gate", "title": "Green, and observed working",
                       "status": "completed", "metadata": {"receipt": stale}}],
        });
        let reason = receipt_skip_reason(&car, Some("2222222222222222222222222222222222222222"))
            .expect("a stale receipt with no re-gate must still be refused");
        assert!(reason.contains("gated, then changed"), "{reason}");
    }

    /// A re-gate is not a way to launder a red run.
    #[test]
    fn a_regate_that_is_not_green_is_refused_like_any_other() {
        let stale = r#"{"verdict":"green","head":"1111111111111111111111111111111111111111"}"#;
        let red = r#"{"verdict":"failed","head":"2222222222222222222222222222222222222222"}"#;
        let car = json!({
            "metadata": {"regate_receipt": red},
            "steps": [{"spec_slug": "gate", "title": "Green, and observed working",
                       "status": "completed", "metadata": {"receipt": stale}}],
        });
        let reason = receipt_skip_reason(&car, Some("2222222222222222222222222222222222222222"))
            .expect("a failed re-gate must not board");
        assert!(reason.contains("not green"), "{reason}");
    }

    #[test]
    fn an_honest_receipt_boards() {
        let c = car_with_receipt(json!({
            "verdict": "green", "dirty": false,
            "head": "abcdef1234567890abcdef1234567890abcdef12",
        }));
        assert_eq!(
            receipt_skip_reason(&c, Some("abcdef1234567890abcdef1234567890abcdef12")),
            None
        );
        // Prefix-tolerant like every other head comparison here.
        assert_eq!(receipt_skip_reason(&c, Some("abcdef12345678")), None);
    }

    #[test]
    fn a_receipt_for_a_different_head_is_named_and_left_behind() {
        let c = car_with_receipt(json!({
            "verdict": "green", "dirty": false,
            "head": "abcdef1234567890abcdef1234567890abcdef12",
        }));
        let reason = receipt_skip_reason(&c, Some("1234567890abcdef1234567890abcdef12345678"))
            .expect("gated-then-changed must be caught");
        assert!(reason.contains("gated, then changed"), "{reason}");
    }

    #[test]
    fn a_non_green_or_dirty_receipt_is_left_behind() {
        let red = car_with_receipt(json!({"verdict": "failed", "dirty": false, "head": "abc"}));
        assert!(
            receipt_skip_reason(&red, Some("abc"))
                .expect("red must be caught")
                .contains("not green")
        );
        let dirty = car_with_receipt(json!({"verdict": "green", "dirty": true, "head": "abc"}));
        assert!(
            receipt_skip_reason(&dirty, Some("abc"))
                .expect("dirty must be caught")
                .contains("dirty tree")
        );
    }

    #[test]
    fn a_missing_receipt_is_unverifiable_not_a_pass() {
        let mut c = ready_car();
        c["steps"].as_array_mut().unwrap().push(json!({
            "spec_slug": "gate", "title": "Green, and observed working",
            "status": "completed", "metadata": {},
        }));
        assert!(
            receipt_skip_reason(&c, Some("abc"))
                .expect("no receipt must be caught")
                .contains("unverifiable")
        );
        // An OBJECT-shaped receipt is a receipt too.
        let mut obj = ready_car();
        obj["steps"].as_array_mut().unwrap().push(json!({
            "spec_slug": "gate", "title": "Green, and observed working",
            "status": "completed",
            "metadata": {"receipt": {"verdict": "green", "dirty": false, "head": "abc12345"}},
        }));
        assert_eq!(receipt_skip_reason(&obj, Some("abc12345")), None);
    }

    /// BOTH RECEIPT SHAPES BOARD, because both are on real cars.
    ///
    /// Until 2026-09-09 the gate runner reduced `infra/gate.sh`'s account
    /// of a run to `{verdict, head, mode, fails}` before reporting it, so
    /// every receipt on a landed car has exactly those four keys and this
    /// check's `dirty` clause never had a value to read. The runner now
    /// reports the whole receipt — mode, scope, dirty, host, ci, free_gb,
    /// unverifiable, every check with its duration, and the report-back's
    /// own story. Widening a record two verbs already parse is where this
    /// breaks, so the two shapes are pinned side by side: the extra keys
    /// must be ignored, and the four-field receipts already on the dock
    /// must keep boarding.
    #[test]
    fn a_wide_receipt_and_a_four_field_one_both_board() {
        const HEAD: &str = "abcdef1234567890abcdef1234567890abcdef12";
        let old = car_with_receipt(json!({
            "verdict": "green", "head": HEAD, "mode": "full", "fails": [],
        }));
        assert_eq!(
            receipt_skip_reason(&old, Some(HEAD)),
            None,
            "a receipt recorded before the runner stopped reducing must still board"
        );
        let wide = car_with_receipt(json!({
            "verdict": "green", "mode": "full", "scope": "", "head": HEAD,
            "dirty": false, "host": "gate-runner-abc", "ci": true, "free_gb": 91,
            "unverifiable": [],
            "report": {"attempts": 4, "waited_s": 63, "sor_unreachable": true},
            "checks": [{"name": "fmt", "result": "pass", "seconds": 3},
                       {"name": "test", "result": "pass", "seconds": 812}],
        }));
        assert_eq!(
            receipt_skip_reason(&wide, Some(HEAD)),
            None,
            "the fields the whole receipt adds must be ignored, not refused"
        );
    }

    /// ...and the clause the widening WAKES UP still refuses.
    ///
    /// `dirty` has been in gate.sh's receipt from the start and has never
    /// once reached this function, because the runner dropped it. It
    /// arrives now, so the refusal it was written for becomes live for
    /// the first time on a real car — pinned here deliberately rather
    /// than discovered on a train.
    #[test]
    fn a_wide_receipt_taken_on_a_dirty_tree_is_still_refused() {
        const HEAD: &str = "abcdef1234567890abcdef1234567890abcdef12";
        let dirty = car_with_receipt(json!({
            "verdict": "green", "mode": "full", "head": HEAD, "dirty": true,
            "checks": [{"name": "fmt", "result": "pass", "seconds": 3}],
        }));
        let reason = receipt_skip_reason(&dirty, Some(HEAD))
            .expect("a receipt taken on a dirty tree must not board");
        assert!(reason.contains("dirty tree"), "{reason}");
    }

    #[test]
    fn a_car_at_review_with_a_branch_is_parked_ready() {
        assert!(parked_ready(&ready_car()));
        let mut active = ready_car();
        active["steps"][0]["status"] = json!("active");
        assert!(parked_ready(&active));
    }

    #[test]
    fn a_held_car_does_not_board_and_does_not_count() {
        // The hold is the whole point: this car is at review, has a
        // branch, and is gated green. It still must not ride, because
        // something about the WORLD is wrong - on 2026-08-26, the only
        // node carrying the label a car repointed the gate rig at had
        // been cordoned for a hardware fault.
        let mut held = ready_car();
        held["steps"][0]["metadata"] = json!({"hold": "w-1 is cordoned"});
        assert!(!parked_ready(&held));

        // `active` is boardable too, so it must honour the hold as well.
        let mut held_active = ready_car();
        held_active["steps"][0]["status"] = json!("active");
        held_active["steps"][0]["metadata"] = json!({"hold": "not yet"});
        assert!(!parked_ready(&held_active));

        // A RELEASED hold is not a hold, and it is released by writing a
        // falsy value, not by deleting the key - `hold: false` is what
        // releasing car 04520403 wrote on 2026-09-10, and `""`/`null`
        // arrive the same way. `stranded::marked` reads all three as no
        // marker; a rule built on "the key is missing" would hold these
        // cars forever.
        for released in [json!(""), json!(false), json!(null), json!("   ")] {
            let mut car = ready_car();
            car["steps"][0]["metadata"] = json!({"hold": released});
            assert!(parked_ready(&car), "a released hold must board: {released}");
        }

        // Metadata that says nothing about holding leaves it boardable.
        let mut other = ready_car();
        other["steps"][0]["metadata"] = json!({"note": "looks fine"});
        assert!(parked_ready(&other));
    }

    #[test]
    fn a_car_without_a_branch_is_not_ready() {
        let mut j = ready_car();
        j["metadata"] = json!({});
        assert!(!parked_ready(&j));
        j["metadata"] = json!({"branch": ""});
        assert!(!parked_ready(&j));
    }

    #[test]
    fn a_car_already_on_a_train_is_not_ready() {
        let mut j = ready_car();
        j["metadata"]["train"] = json!("train-job-id");
        assert!(!parked_ready(&j));
        // A train's own branch is never a car either.
        let mut t = ready_car();
        t["metadata"]["branch"] = json!("train/20260812-0600");
        assert!(!parked_ready(&t));
    }

    #[test]
    fn a_released_car_is_parked_ready_again() {
        // THE INVARIANT THAT MAKES CANCELLING A LOADED TRAIN POSSIBLE.
        // Releasing a car clears `train` and nothing else, so the car
        // must be boardable on that write alone. It is — as long as
        // boarding left the review step ready.
        let mut boarded = ready_car();
        boarded["metadata"]["train"] = json!("train-job-id");
        boarded["metadata"]["boarded_head"] = json!("abc1234");
        assert!(!parked_ready(&boarded));

        let mut released = boarded.clone();
        released["metadata"]["train"] = Value::Null;
        released["metadata"]["boarded_head"] = Value::Null;
        assert!(
            parked_ready(&released),
            "a released car must re-enter the dock on the metadata write alone"
        );

        // And the reason boarding must NOT complete the step: a
        // completed review is frozen at the row, so a released car
        // carrying one could never board again and the cancel would be
        // a lie.
        let mut released_but_reviewed = released.clone();
        released_but_reviewed["steps"][0]["status"] = json!("completed");
        assert!(!parked_ready(&released_but_reviewed));
    }

    #[test]
    fn a_car_not_yet_at_review_is_not_ready() {
        let mut j = ready_car();
        j["steps"][0]["status"] = json!("pending");
        assert!(!parked_ready(&j));
        j["steps"][0]["status"] = json!("completed");
        assert!(!parked_ready(&j));
        // No review step at all.
        j["steps"] = json!([]);
        assert!(!parked_ready(&j));
    }

    // -- the skip reason on the car job ------------------------------------
    //
    // Train #8 conflict-skipped three cars; the journal said "left for
    // the next train" but the car Jobs carried nothing, so the yard's
    // dock showed them unexplained. The PacketCard chip renders
    // `metadata.skip_reason` ("LEFT BEHIND — <reason>"), so the string
    // stays short: a truncated file list, or the missing branch.

    #[test]
    fn a_conflict_skip_reason_names_the_files() {
        let files = vec!["src/a.rs".to_string(), "src/b.rs".to_string()];
        assert_eq!(
            skip_reason_conflict(&files, policy().skip_reason_file_budget),
            "conflict: src/a.rs, src/b.rs"
        );
    }

    #[test]
    fn a_long_conflict_list_truncates_with_a_count() {
        let files: Vec<String> = (0..20)
            .map(|i| format!("crates/core/boss-jobs/src/file_{i:02}.rs"))
            .collect();
        let reason = skip_reason_conflict(&files, policy().skip_reason_file_budget);
        assert!(
            reason.starts_with("conflict: crates/core/boss-jobs/src/file_00.rs"),
            "leads with the first file: {reason}"
        );
        assert!(reason.ends_with("+18 more"), "counts what it hid: {reason}");
        assert!(
            reason.len() <= 120,
            "stays chip-sized ({} chars): {reason}",
            reason.len()
        );
    }

    #[test]
    fn one_huge_conflict_file_is_still_named() {
        // Truncation drops files, never the whole answer: at least one
        // file always shows.
        let long = format!("crates/{}.rs", "x".repeat(150));
        let reason = skip_reason_conflict(
            std::slice::from_ref(&long),
            policy().skip_reason_file_budget,
        );
        assert_eq!(reason, format!("conflict: {long}"));
    }

    #[test]
    fn a_merge_that_died_before_markers_says_so() {
        assert_eq!(
            skip_reason_conflict(&[], policy().skip_reason_file_budget),
            "conflict: unresolved (merge died before conflict markers)"
        );
    }

    /// The dock-depth metric and the boardable count must answer the
    /// same question. On 2026-08-14 they did not: `parked_ready` said
    /// 12 while `candidates` boarded 0, because every branch had been
    /// pushed upstream and none to the fork. The conductor now closes
    /// that gap by copying the ref, so this reason is reserved for a
    /// branch that exists in NEITHER place — a car never pushed at all.
    #[test]
    fn a_branch_missing_everywhere_is_still_a_real_skip() {
        assert_eq!(
            skip_reason_branch_missing("feat/never-pushed"),
            "branch feat/never-pushed not on fork",
            "the skip survives for the genuine case: nothing to copy"
        );
    }

    #[test]
    fn a_missing_branch_skip_reason_names_the_branch() {
        assert_eq!(
            skip_reason_branch_missing("feat/x"),
            "branch feat/x not on fork"
        );
    }

    // ---- publish_car_branch -------------------------------------
    //
    // `candidates` skips any parked car whose branch is not on the
    // fork, and until 2026-08-16 it could recover only one case: the
    // branch already on `origin`. A branch sitting as a LOCAL ref in
    // the conductor's own clone counted as "never pushed at all" —
    // which is exactly what `git push gcp <branch>` produces, since
    // the `gcp` remote IS /var/lib/boss-train/repo. That cost five
    // hand-run pushes in one evening, each a human running
    // `git push origin` from that very directory with credentials the
    // conductor already held.
    //
    // Neither recovery path had a test; only the skip MESSAGE did.
    // These drive real git repositories, because the behaviour is
    // entirely "which refs exist and what does git push do with them"
    // — a faked git would only prove this file agrees with itself.

    /// THE BUG THAT REDDENED TWO TRAINS ON 2026-08-17.
    ///
    /// `origin/<branch>` existed and was stale; the fix sat on the
    /// local head. Preferring whichever ref was tried first shipped
    /// the stale commit to the fork, so the train compiled code the
    /// author had already fixed — and reported success doing it.
    #[test]
    fn a_stale_origin_ref_does_not_beat_the_cars_real_head() {
        let (_g, clone) = clone_fixture("stale-origin");
        commit_branch(&clone, "feat/stale");
        git_ok(&clone, &["push", "-q", "origin", "feat/stale"]);
        let stale = rev(&clone, "feat/stale");

        // The fix lands locally and is NOT pushed upstream — exactly
        // what `git push gcp <branch>` leaves behind.
        advance_branch(&clone, "feat/stale", "the fix");
        let real = rev(&clone, "feat/stale");
        assert_ne!(stale, real, "precondition: the branch moved");
        assert_eq!(
            rev(&clone, "origin/feat/stale"),
            stale,
            "precondition: origin is behind"
        );

        assert!(
            publish_car_branch(clone.to_str().expect("utf8"), "feat/stale").expect("publish"),
            "a car with a real head must publish"
        );
        assert_eq!(
            rev(&clone, "fork/feat/stale"),
            real,
            "the fork must carry the car's real head, not the stale origin ref"
        );
    }

    /// The mirror image, and the reason ordering alone is not the fix:
    /// when the fork does not have the branch at all, ANY source
    #[test]
    fn a_fork_that_moved_on_is_never_overwritten() {
        // The car was restacked and force-pushed to the FORK by its author;
        // this clone still holds the pre-restack head locally and as
        // origin/<branch>. Boarding must not publish the stale copy over
        // the fork's newer, diverged head (2026-09-05, packet c1d56d13).
        let (_g, clone) = clone_fixture("fork-moved-on");
        commit_branch(&clone, "feat/moved");
        git_ok(&clone, &["push", "-q", "origin", "feat/moved"]);
        git_ok(&clone, &["push", "-q", "fork", "feat/moved"]);
        let stale = rev(&clone, "feat/moved");
        // A second clone restacks the branch (a new root commit) and
        // force-pushes it to the fork — the author's re-rail.
        let other = clone.parent().expect("root").join("other");
        let fork_url = clone.parent().expect("root").join("fork.git");
        let out = std::process::Command::new("git")
            .args(["clone", "-q", fork_url.to_str().expect("utf8")])
            .arg(&other)
            .output()
            .expect("clone");
        assert!(out.status.success());
        git_ok(&other, &["config", "user.email", "t@example.com"]);
        git_ok(&other, &["config", "user.name", "t"]);
        git_ok(&other, &["checkout", "-q", "--orphan", "feat/moved"]);
        std::fs::write(other.join("README"), "restacked").expect("write");
        git_ok(&other, &["add", "-A"]);
        git_ok(&other, &["commit", "-qm", "restacked"]);
        git_ok(&other, &["push", "-q", "-f", "origin", "feat/moved"]);
        let moved = rev(&other, "feat/moved");
        assert_ne!(moved, stale, "precondition: the fork moved on");

        assert!(
            !publish_car_branch(clone.to_str().expect("utf8"), "feat/moved").expect("publish"),
            "a diverged fork head is not something this clone may overwrite"
        );
        let fork_now = String::from_utf8_lossy(
            &std::process::Command::new("git")
                .args([
                    "-C",
                    fork_url.to_str().expect("utf8"),
                    "rev-parse",
                    "refs/heads/feat/moved",
                ])
                .output()
                .expect("rev-parse")
                .stdout,
        )
        .trim()
        .to_string();
        assert_eq!(
            fork_now, moved,
            "the fork must still carry the author's restacked head"
        );
    }

    /// pushes cleanly, so nothing rejects a stale one. The descendant
    /// has to be chosen deliberately.
    #[test]
    fn an_origin_ref_ahead_of_a_stale_local_head_wins() {
        let (_g, clone) = clone_fixture("stale-local");
        commit_branch(&clone, "feat/ahead");
        // Advance on a scratch clone and push, so `origin/<branch>`
        // moves ahead while this clone's local ref stays put.
        let stale_local = rev(&clone, "feat/ahead");
        git_ok(&clone, &["push", "-q", "origin", "feat/ahead"]);
        advance_branch(&clone, "feat/ahead", "upstream work");
        let ahead = rev(&clone, "feat/ahead");
        git_ok(&clone, &["push", "-q", "origin", "feat/ahead"]);
        git_ok(&clone, &["branch", "-qf", "feat/ahead", &stale_local]);
        assert_eq!(rev(&clone, "feat/ahead"), stale_local, "precondition");

        assert!(publish_car_branch(clone.to_str().expect("utf8"), "feat/ahead").expect("publish"));
        assert_eq!(
            rev(&clone, "fork/feat/ahead"),
            ahead,
            "the newer upstream ref must win over a stale local one"
        );
    }

    // ---- cancellable_run_ids ---------------------------------
    //
    // Shapes copied from a live `GET /actions/runs?limit=N` on the
    // forge on 2026-08-17, INCLUDING `head_branch: null`, which is the
    // whole reason this function exists rather than a one-line filter.

    fn run(id: i64, status: &str, pretty: &str, sha: &str) -> Value {
        json!({
            "id": id,
            "status": status,
            "conclusion": Value::Null,
            "head_branch": Value::Null,
            "prettyref": pretty,
            "commit_sha": sha,
            "event": "pull_request"
        })
    }

    /// The measured trap: keying on `head_branch` finds nothing,
    /// because this forge does not populate it.
    #[test]
    fn a_running_train_job_is_found_without_head_branch() {
        let runs = vec![run(153, "running", "#64", "13c9e4ad")];
        assert_eq!(
            cancellable_run_ids(&runs, "64", "13c9e4ad"),
            vec![153],
            "the run must be identified by prettyref/commit_sha, since head_branch is null"
        );
    }

    #[test]
    fn finished_runs_are_left_alone() {
        let runs = vec![
            run(152, "success", "#64", "13c9e4ad"),
            run(151, "failure", "#64", "13c9e4ad"),
            run(150, "cancelled", "#64", "13c9e4ad"),
        ];
        assert!(
            cancellable_run_ids(&runs, "64", "13c9e4ad").is_empty(),
            "cancelling a finished run is a pointless API call at best"
        );
    }

    /// The expensive mistake this guards against: killing a live run
    /// that belongs to a DIFFERENT train.
    #[test]
    fn another_trains_run_is_never_cancelled() {
        let runs = vec![
            run(153, "running", "#64", "13c9e4ad"),
            run(154, "running", "#65", "deadbeef"),
        ];
        assert_eq!(
            cancellable_run_ids(&runs, "64", "13c9e4ad"),
            vec![153],
            "only this train's runs"
        );
    }

    /// A run queued before the PR existed carries no `#N` but does
    /// carry the sha, so the sha clause has to stand on its own.
    #[test]
    fn a_run_matching_only_on_sha_is_still_ours() {
        let runs = vec![run(155, "running", "", "13c9e4ad")];
        assert_eq!(cancellable_run_ids(&runs, "64", "13c9e4ad"), vec![155]);
    }

    /// Empty selectors must not turn into "match everything" — the
    /// worst possible reading of a cancel.
    #[test]
    fn empty_selectors_cancel_nothing() {
        let runs = vec![run(153, "running", "", "")];
        assert!(cancellable_run_ids(&runs, "", "").is_empty());
    }

    /// An unrecognised status is left running on purpose.
    #[test]
    fn an_unknown_status_is_not_assumed_cancellable() {
        let runs = vec![run(156, "some-future-state", "#64", "13c9e4ad")];
        assert!(
            cancellable_run_ids(&runs, "64", "13c9e4ad").is_empty(),
            "a false cancel costs someone's live run; a miss costs only time"
        );
    }

    /// THE REPAIR PATH, which is where staleness actually costs.
    ///
    /// A car boards, its train reddens, the author fixes the branch and
    /// reboards. `candidates` used to ask only "is this branch on the
    /// fork" — and it is, from the first boarding — so nothing
    /// republished it and the new consist carried the commit that just
    /// failed. Measured twice on 2026-08-17: feat/dev-shared-target was
    /// 3370b42 locally and 96109f7 on the forge, and train 38d49597
    /// assembled the red one.
    #[test]
    fn a_fork_branch_behind_the_car_is_republished() {
        let (_g, clone) = clone_fixture("stale-fork");
        commit_branch(&clone, "feat/repaired");
        let path = clone.to_str().expect("utf8");
        assert!(publish_car_branch(path, "feat/repaired").expect("first publish"));
        let first = rev(&clone, "fork/feat/repaired");

        // The repair.
        advance_branch(&clone, "feat/repaired", "the fix");
        let fixed = rev(&clone, "feat/repaired");
        assert_ne!(first, fixed, "precondition: the branch moved");
        assert_eq!(
            rev(&clone, "fork/feat/repaired"),
            first,
            "precondition: the fork still holds the pre-repair commit"
        );

        assert_eq!(
            car_head(path, "feat/repaired").expect("car_head"),
            Some(fixed.clone()),
            "car_head must report the head the car actually names"
        );
        assert!(publish_car_branch(path, "feat/repaired").expect("republish"));
        assert_eq!(
            rev(&clone, "fork/feat/repaired"),
            fixed,
            "the forge must end up holding the repaired commit"
        );
    }

    /// THE OTHER HALF OF THE REPAIR PATH — a rebase, not a fast-forward.
    ///
    /// The test above covers a car repaired by ADDING a commit, where
    /// the local head is strictly ahead and publishing fast-forwards
    /// the fork onto it. The commoner repair is a REBASE: the author
    /// rebuilds the branch on a newer main and re-pushes it, and the
    /// conductor's clone — which never checked the branch out again —
    /// keeps the pre-rebase commit forever.
    ///
    /// Now the two refs have DIVERGED, so no push can fast-forward and
    /// the fork keeps the commit the gate actually ran on. `car_head`
    /// still reports the local one, and checking a receipt against it
    /// leaves a correctly-gated car behind for "gated, then changed".
    /// Live on 2026-08-29: c6531868, receipt 56b817eb matching the fork
    /// exactly, held out against a local ref eight hours older.
    #[test]
    fn a_rebased_car_boards_the_head_the_fork_holds() {
        let (_g, clone) = clone_fixture("diverged-fork");
        commit_branch(&clone, "fix/rebased");
        let path = clone.to_str().expect("utf8");
        assert!(publish_car_branch(path, "fix/rebased").expect("first publish"));
        let pre_rebase = rev(&clone, "fix/rebased");

        // The author rebases onto a newer base and re-pushes. Both refs
        // now descend from main independently — neither is an ancestor
        // of the other, which is what makes the push unable to help.
        git_ok(&clone, &["checkout", "-q", "-b", "scratch", "main"]);
        std::fs::write(clone.join("x"), "rebased work").expect("write");
        git_ok(&clone, &["add", "-A"]);
        git_ok(&clone, &["commit", "-qm", "rebased work"]);
        git_ok(
            &clone,
            &["push", "-q", "-f", "fork", "scratch:refs/heads/fix/rebased"],
        );
        git_ok(&clone, &["checkout", "-q", "main"]);
        git_ok(&clone, &["fetch", "-q", "fork"]);
        let gated = rev(&clone, "fork/fix/rebased");
        assert_ne!(pre_rebase, gated, "precondition: the refs diverged");
        assert_eq!(
            rev(&clone, "refs/heads/fix/rebased"),
            pre_rebase,
            "precondition: the clone still holds the pre-rebase commit"
        );

        assert_eq!(
            fork_head(path, "fix/rebased").expect("fork_head"),
            Some(gated.clone()),
            "the head that boards is the one the consist is assembled from"
        );

        // The receipt the gate wrote vouches for what is on the fork.
        let car = json!({
            "metadata": {},
            "steps": [{
                "spec_slug": "gate",
                "title": "Green, and observed working",
                "metadata": {"receipt": {"verdict": "green", "head": gated}},
            }],
        });
        assert_eq!(
            receipt_skip_reason(
                &car,
                fork_head(path, "fix/rebased").expect("fork").as_deref()
            ),
            None,
            "a correctly-gated car must board after a rebase"
        );
        let stale =
            receipt_skip_reason(&car, car_head(path, "fix/rebased").expect("car").as_deref())
                .expect("the local ref is the wrong question and must be seen to be");
        assert!(
            stale.contains("gated, then changed"),
            "documents the regression this test pins: {stale}"
        );
    }

    /// A car nobody has pushed anywhere has no head to board.
    #[test]
    fn car_head_is_none_when_the_branch_exists_nowhere() {
        let (_g, clone) = clone_fixture("no-head");
        assert_eq!(
            car_head(clone.to_str().expect("utf8"), "feat/never").expect("car_head"),
            None
        );
    }

    /// THE CASE THAT COST FIVE PUSHES.
    #[test]
    fn a_branch_local_to_the_clone_gets_published() {
        let (_g, clone) = clone_fixture("local-only");
        commit_branch(&clone, "feat/local-only");
        assert!(!on_fork(&clone, "feat/local-only"), "precondition");

        let published =
            publish_car_branch(clone.to_str().expect("utf8"), "feat/local-only").expect("publish");
        assert!(
            published,
            "a local ref the conductor can see should publish"
        );
        assert!(
            on_fork(&clone, "feat/local-only"),
            "fork/<branch> must resolve afterwards — that is what candidates checks"
        );
    }

    /// The older recovery path, also previously untested.
    #[test]
    fn a_branch_on_origin_gets_copied_to_the_fork() {
        let (_g, clone) = clone_fixture("origin-only");
        commit_branch(&clone, "feat/upstream");
        git_ok(&clone, &["push", "-q", "origin", "feat/upstream"]);
        // Drop the local ref so origin/<branch> is the only source.
        git_ok(&clone, &["branch", "-qD", "feat/upstream"]);

        assert!(!on_fork(&clone, "feat/upstream"));
        assert!(
            publish_car_branch(clone.to_str().expect("utf8"), "feat/upstream").expect("publish")
        );
        assert!(on_fork(&clone, "feat/upstream"));
    }

    /// Recovery must not become "board anything".
    #[test]
    fn a_branch_that_exists_nowhere_is_still_a_skip() {
        let (_g, clone) = clone_fixture("nowhere");
        assert!(
            !publish_car_branch(clone.to_str().expect("utf8"), "feat/never-pushed")
                .expect("publish"),
            "nothing to copy: that car was never pushed, and skipping is right"
        );
        assert!(!on_fork(&clone, "feat/never-pushed"));
    }

    /// `candidates` runs on every boarding attempt, so an unchanged
    /// branch is seen again next train. "Already up to date" must not
    /// read as a failure that strands the car.
    #[test]
    fn publishing_an_already_published_branch_is_idempotent() {
        let (_g, clone) = clone_fixture("twice");
        commit_branch(&clone, "feat/twice");
        let path = clone.to_str().expect("utf8");
        assert!(publish_car_branch(path, "feat/twice").expect("first"));
        assert!(
            publish_car_branch(path, "feat/twice").expect("second"),
            "a second publish of an unchanged branch must still report success"
        );
        assert!(on_fork(&clone, "feat/twice"));
    }
}
