//! `boss rerail <car>` — a conflict-skipped car back aboard, with the
//! traps encoded.
//!
//! WHY THIS IS A VERB (e7c86455). When boarding skips a car on a merge
//! conflict, the by-hand recovery is ten steps: fetch, worktree,
//! rebase, resolve, gate the new head, transcribe the receipt, repoint
//! the packet, re-board. Done for 7 cars on 2026-08-15, one on 08-23,
//! one on 08-30 — each time re-walking three traps that have each cost
//! real time:
//!
//!   - THE BRANCH MUST BE NEW. A force-push to the old branch is
//!     classifier-blocked (and rightly: it yanks a ref others hold),
//!     so the rebased tree goes to `<branch>-rerail` — the 08-15
//!     precedent, now the verb's contract.
//!   - THE GATE STEP IS FROZEN. A car's gate receipt vouches for ONE
//!     head; superseding it means writing `metadata.regate_receipt`
//!     on the JOB, never editing the completed step (the boarding
//!     logic and `boss receipt` both prefer regate_receipt).
//!   - THE RECEIPT IS MACHINE-COPIED. Never retyped, never
//!     hand-authored (never-write-a-sha-you-did-not-read): it is read
//!     back from the gate-run packet by `park::receipt_for`, byte for
//!     byte.
//!
//! The verb stops for a human at exactly one place — a real conflict
//! hunk — and hands back the worktree with the remaining sequence
//! printed, finishable with `--finish` once the branch is pushed.
//!
//! `--finish` RUNS AS OFTEN AS THE HEAD MOVES (02165b1d). It used to
//! derive `<car branch>-rerail` unconditionally, which meant it worked
//! exactly once per car: run again on an already-re-railed branch it
//! hunted for `…-rerail-rerail` and refused, leaving no designed way to
//! put a fresh receipt on a car whose head had moved — and the
//! improvised way (re-gate with `--park-*`) filed a twin car. It now
//! asks the forge: a rerail in flight is finished onto its branch, and
//! with no such branch `--finish` REFRESHES the car where it stands.
//! Either way the receipt comes from `park::receipt_for`, so it refuses
//! unless a green vouches for the branch's head right now.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::gate;
use crate::park;

/// Shell out to git in a directory, capturing stderr for the error.
fn git(dir: &str, args: &[&str]) -> Result<String> {
    let out = crate::git_auth::command()
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .with_context(|| format!("running git {args:?}"))?;
    if !out.status.success() {
        bail!(
            "git {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The car packet for `given` (branch or 8+ chars of id), plus its
/// branch. Reads open ship-a-change packets the same way park does.
async fn find_car(http: &reqwest::Client, given: &str) -> Result<(Value, String)> {
    // Read EVERY open car, not just page one: a rerail target opened
    // days ago sorts to the tail of `opened_on DESC`, and a bare
    // `limit=` read left it off page one and reported it "not found"
    // (a-limit-is-not-a-filter). `gate::all_open_cars` pages on `total`.
    let cars = gate::all_open_cars(http).await?;
    select_car(&cars, given)
}

/// The car for `given` (branch, or 8+ chars of id), plus its branch —
/// pure, over the fully-gathered open cars, so branch-or-id resolution
/// (and that it reaches a car past page one) is testable without a live
/// API. Resolves the same way `park` does.
fn select_car(cars: &[Value], given: &str) -> Result<(Value, String)> {
    let by_branch: Vec<&Value> = cars
        .iter()
        .filter(|c| c.pointer("/metadata/branch").and_then(Value::as_str) == Some(given))
        .collect();
    let car = if let [one] = by_branch.as_slice() {
        (*one).clone()
    } else {
        let id = park::resolve_job_id(cars, given)?;
        cars.iter()
            .find(|c| c.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .cloned()
            .ok_or_else(|| anyhow!("resolved {id} but it vanished from the list"))?
    };
    let branch = car
        .pointer("/metadata/branch")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("car carries no metadata.branch — nothing to rerail"))?
        .to_string();
    Ok((car, branch))
}

/// The head the forge carries for this branch, or `None` when it
/// carries no such branch. Asked of the REMOTE, not of a local ref, so
/// it is true without a fetch — `--finish` never fetches.
fn forge_head(branch: &str) -> Result<Option<String>> {
    Ok(git(
        ".",
        &["ls-remote", "origin", &format!("refs/heads/{branch}")],
    )?
    .lines()
    .find_map(|l| l.split_whitespace().next())
    .map(str::to_string))
}

/// Does the forge already carry this branch? The one probe both the
/// rebase path (refusing to cut over an in-flight rerail) and the
/// finish path (deciding whether there IS a rerail to finish) ask —
/// the same read as `forge_head`, so the two cannot disagree.
fn on_forge(branch: &str) -> Result<bool> {
    Ok(forge_head(branch)?.is_some())
}

/// PURE: which branch `--finish` finishes.
///
/// WHY IT IS NOT ALWAYS THE DERIVED ONE (backlog 02165b1d,
/// `third_instance_2026_09_08_2200Z`). `--finish` used to always look
/// for `<car branch>-rerail`, so it could be run exactly once per car:
/// on an already-re-railed car it went hunting for
/// `feat/…-rerail-rerail`, found nothing, and refused. Car 538775dd hit
/// that on 2026-09-08 — its branch head had moved for a renumbered
/// migration, its receipt was stale, boarding correctly refused it, and
/// there was NO designed way to put the fresh receipt on it. The
/// operator re-gated instead, the auto-park handler filed a twin, and
/// the good car was retired by hand.
///
/// So the question is asked of the forge, not of the string: if the
/// derived branch EXISTS, a rerail is in flight and that is what gets
/// finished (the post-conflict path, unchanged). If it does not, there
/// is nothing to rerail to and `--finish` means what the operator
/// needs it to mean — put this car's CURRENT branch's current green
/// receipt on it and clear the skip. That second reading is also the
/// answer for a plain rebase, so one verb covers both and there is no
/// second one to pick wrong.
///
/// It cannot invent a green: the caller transcribes through
/// `park::receipt_for`, which refuses unless a green gate-run vouches
/// for the branch's head RIGHT NOW.
fn finish_target<'a>(car_branch: &'a str, derived: &'a str, derived_on_forge: bool) -> &'a str {
    if derived_on_forge {
        derived
    } else {
        car_branch
    }
}

/// PURE: the note a repoint writes, which differs by what happened.
/// A rerail moved the car to a new branch; a refresh left it where it
/// was and only superseded the receipt. Saying "rerailed from X" on a
/// car that did not move would be a false record.
fn repoint_note(old_branch: &str, new_branch: &str) -> String {
    if old_branch == new_branch {
        format!(
            "receipt refreshed in place by boss rerail --finish: {new_branch}'s head moved \
             (rebase, re-rail or a renumbered migration), so the current green gate-run's \
             receipt was machine-copied to regate_receipt and the stale skip cleared (the \
             frozen gate step stays as the original head's record)"
        )
    } else {
        format!(
            "rerailed from {old_branch} by boss rerail: new branch cut from \
             origin/main, re-gated, receipt machine-copied to regate_receipt \
             (the frozen gate step stays as the original head's record)"
        )
    }
}

/// PURE: the `rerail_origins` a repoint records — every branch this car
/// has been re-railed OFF, oldest first, each with the head it carried
/// when the car left it. `None` when there is nothing to record, which
/// OMITS the key: a null would DELETE the origins already on the car
/// (the metadata door's contract, and the `delivery_channel` lesson in
/// `boss_jobs::car::regate_patch`).
///
/// WHY THE CAR CARRIES THIS AT ALL (packet 473fda1b, generator 1). The
/// rerail leaves the original branch on the forge, and that branch is
/// never a car — so the arrival sweep, which iterates a train's boarded
/// cars, had nothing to act on and could never delete it. Permanent by
/// construction: 13 originals accumulated and came off by hand on
/// 2026-09-10. The sweep reads THIS record (`train::recorded_branches`)
/// rather than guessing a `-rerail` suffix off a name, because a name
/// is not a record and the head is what the sweep's guard needs anyway:
/// a commit pushed to the original after the rerail keeps it, exactly
/// as a commit pushed after boarding keeps a car's own branch.
///
/// A refresh in place (`old == new`) moved nothing, so it records
/// nothing. A branch already recorded keeps the head it was first
/// recorded with — that is the head the car actually left it at.
fn origins_after(
    car: &Value,
    old_branch: &str,
    old_head: Option<&str>,
    new_branch: &str,
) -> Option<Vec<Value>> {
    if old_branch.is_empty() || old_branch == new_branch {
        return None;
    }
    let mut out: Vec<Value> = car
        .pointer("/metadata/rerail_origins")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if out
        .iter()
        .any(|o| o.get("branch").and_then(Value::as_str) == Some(old_branch))
    {
        return Some(out);
    }
    let mut entry = json!({ "branch": old_branch });
    if let Some(h) = old_head.filter(|h| !h.is_empty()) {
        entry["head"] = json!(h);
    }
    out.push(entry);
    Some(out)
}

/// Repoint the car at the re-gated branch: `branch` moves, the fresh
/// receipt rides `regate_receipt` VERBATIM, and the skip_reason is
/// deleted (a PATCH key set to null is removed — the metadata door's
/// documented contract) so the next boarding no longer sees a skip.
///
/// `old_branch == new_branch` is the refresh-in-place case: the branch
/// write is a no-op and only the receipt and the skip move.
async fn repoint(
    http: &reqwest::Client,
    car_id: &str,
    new_branch: &str,
    receipt: &boss_jobs::car::Receipt,
    old_branch: &str,
    origins: Option<Vec<Value>>,
) -> Result<()> {
    // The regate write itself — receipt verbatim, skip cleared — is
    // core's builder, shared with `boss park` and the auto-park handler
    // (a re-gate of a still-parked car refreshes it the same way); the
    // repoint is what rerail adds, because here the branch moved too.
    // The branch moved, so re-classify the channel from the new branch's
    // diff (None if it won't resolve — safe, the key is then left as-is).
    let dc = crate::channels::delivery_channel_for(new_branch);
    let mut patch = boss_jobs::car::regate_patch(
        receipt,
        &repoint_note(old_branch, new_branch),
        dc.as_deref(),
    );
    patch["branch"] = json!(new_branch);
    // The branch the car is leaving, so the arrival sweep can take it
    // too (473fda1b). Omitted, never nulled, when nothing moved.
    if let Some(origins) = origins {
        patch["rerail_origins"] = json!(origins);
    }
    gate::api(
        http,
        reqwest::Method::PATCH,
        &format!("/api/jobs/{car_id}/metadata"),
        Some(patch),
    )
    .await?;
    Ok(())
}

/// The stamps that record a rerail on the gate-run packets, so the
/// yard's stranded read (and `boss orient`'s) stop counting the
/// original branch's green as a forgotten one (69daaba2: it read as
/// stranded forever, even after the branch was deleted on the forge).
/// Every gate-run naming `old_branch` gets `rerailed_to: <new>`; every
/// gate-run naming `new_branch` gets `rerailed_from: <old>`. Pure —
/// `(packet id, PATCH body)` pairs the caller merges via the metadata
/// door. A packet with no id cannot be stamped and is skipped.
///
/// A REFRESH IN PLACE STAMPS NOTHING. When the branch did not move
/// (`--finish` on an already-re-railed car), there is no old branch
/// whose green went stranded and no new one to point at — stamping
/// `rerailed_to: <itself>` would be a fabricated record, and the yard
/// would then read the branch's own live green as superseded.
pub(crate) fn rerail_stamps(
    gate_runs: &[Value],
    old_branch: &str,
    new_branch: &str,
) -> Vec<(String, Value)> {
    if old_branch == new_branch {
        return Vec::new();
    }
    gate_runs
        .iter()
        .filter_map(|g| {
            let id = g.get("id").and_then(Value::as_str)?;
            let branch = g.pointer("/metadata/branch").and_then(Value::as_str)?;
            let patch = if branch == old_branch {
                json!({ "rerailed_to": new_branch })
            } else if branch == new_branch {
                json!({ "rerailed_from": old_branch })
            } else {
                return None;
            };
            Some((id.to_string(), patch))
        })
        .collect()
}

/// The finishing half, standalone: the new branch exists and has a
/// GREEN gate; transcribe its receipt and repoint the car. Split out
/// so a conflict-interrupted rerail (human resolves, pushes, gates)
/// completes through the same code as the happy path — one definition
/// of the transcription, which is where the hand-typed-sha trap lived.
async fn finish(
    http: &reqwest::Client,
    car: &Value,
    old_branch: &str,
    new_branch: &str,
) -> Result<()> {
    let car_id = car
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("car has no id"))?;
    let head = gate::resolve_sha(new_branch);
    let gate_runs = gate::rows(
        gate::api(
            http,
            reqwest::Method::GET,
            "/api/jobs?kind=gate-run&limit=100",
            None,
        )
        .await?,
    );
    // Machine-copied, green-preferring, head-matched — every property
    // the by-hand transcription kept getting wrong, in one call.
    let receipt = park::receipt_for(&gate_runs, new_branch, &head)?;
    // The head the branch we are LEAVING carries right now — read from
    // the forge, before anything else touches it, because that is the
    // content the rerail carries away and the only evidence the sweep
    // will accept for deleting it later. Best effort on purpose: an
    // unreadable head must not fail a rerail that is otherwise done,
    // and an origin recorded without one is refused by name at the
    // sweep rather than deleted on a guess.
    let old_head = match forge_head(old_branch) {
        Ok(h) => h,
        Err(e) => {
            eprintln!(
                "boss rerail: could not read {old_branch}'s head on the forge, \
                 recording the origin without one (the sweep will refuse it by \
                 name rather than guess): {e:#}"
            );
            None
        }
    };
    let origins = origins_after(car, old_branch, old_head.as_deref(), new_branch);
    repoint(http, car_id, new_branch, &receipt, old_branch, origins).await?;
    // Record the rerail on the gate-runs themselves, so the original
    // branch's green stops reading as stranded. Best effort, after the
    // repoint: the car is already correct, and a missed stamp costs one
    // false amber row, not a car.
    for (packet, patch) in rerail_stamps(&gate_runs, old_branch, new_branch) {
        if let Err(e) = gate::api(
            http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{packet}/metadata"),
            Some(patch),
        )
        .await
        {
            eprintln!(
                "boss rerail: could not stamp gate-run {}: {e:#}",
                &packet[..8.min(packet.len())]
            );
        }
    }
    let id = &car_id[..8.min(car_id.len())];
    if old_branch == new_branch {
        println!(
            "boss rerail: {id} refreshed {new_branch} in place — current green receipt \
             copied, skip cleared; the next boarding takes it"
        );
    } else {
        println!(
            "boss rerail: {id} repointed {old_branch} -> {new_branch} — receipt copied, \
             skip cleared; the next boarding takes it"
        );
    }
    Ok(())
}

pub async fn run(
    given: &str,
    finish_only: bool,
    dry: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let http = reqwest::Client::new();
    let (car, old_branch) = find_car(&http, given).await?;
    let new_branch = format!("{old_branch}-rerail");

    if finish_only {
        // A rerail in flight is finished onto its new branch; with no
        // such branch on the forge there is nothing to rerail TO, and
        // `--finish` refreshes the car where it stands. See
        // `finish_target` for the car this was measured on.
        let target = finish_target(&old_branch, &new_branch, on_forge(&new_branch)?).to_string();
        if target == old_branch {
            println!(
                "boss rerail: no {new_branch} on the forge — refreshing {old_branch}'s car \
                 from its current green receipt instead"
            );
        }
        return finish(&http, &car, &old_branch, &target).await;
    }

    // The rebase, in a disposable worktree cut from the CURRENT trunk.
    git(".", &["fetch", "origin"])?;
    if on_forge(&new_branch)? {
        bail!(
            "{new_branch} already exists on the forge — a previous rerail is in \
             flight. Finish it (`boss rerail {given} --finish`) or delete the \
             branch before cutting a fresh one."
        );
    }
    let base = git(
        ".",
        &["merge-base", "origin/main", &format!("origin/{old_branch}")],
    )?;
    let range = format!("{base}..origin/{old_branch}");
    let wt = format!(".git/rerail-wt/{new_branch}");

    if dry {
        let commits = git(".", &["rev-list", "--count", &range])?;
        println!(
            "boss rerail: DRY — would cut {new_branch} from origin/main, \
             cherry-pick {commits} commit(s) ({range}), push, gate, and repoint \
             the car"
        );
        return Ok(());
    }

    git(".", &["worktree", "add", "--detach", &wt, "origin/main"])?;
    let picked = crate::git_auth::command()
        .arg("-C")
        .arg(&wt)
        .args(["cherry-pick", &range])
        .output()
        .context("running cherry-pick")?;
    if !picked.status.success() {
        // THE one human stop: a real conflict hunk. Everything after
        // resolution is the same finishing half the happy path uses.
        println!(
            "boss rerail: CONFLICT rebasing {old_branch} onto current main.\n  \
             The worktree is left at {wt} — resolve it, then:\n    \
             git -C {wt} cherry-pick --continue\n    \
             git -C {wt} push origin HEAD:refs/heads/{new_branch}\n    \
             boss gate {new_branch} --wait\n    \
             boss rerail {given} --finish\n  \
             ({})",
            String::from_utf8_lossy(&picked.stderr).trim()
        );
        bail!("conflict needs a human — the worktree and next steps are above");
    }
    git(
        &wt,
        &["push", "origin", &format!("HEAD:refs/heads/{new_branch}")],
    )?;
    git(".", &["worktree", "remove", "--force", &wt])?;
    println!("boss rerail: {new_branch} cut from origin/main and pushed — gating");

    // Gate the new head through the existing verb — one definition of
    // launching a gate, waits to a verdict, and a refusal cleans up its
    // own packet (the ed7f1355 fix rides the same binary).
    gate::run(
        &new_branch,
        None,
        None,
        "boss-dev",
        true,
        false,
        gate::ParkIntent::default(),
        None,
        None,
        now,
    )
    .await?;

    finish(&http, &car, &old_branch, &new_branch).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rerail stamps: the old branch's gate-runs (every one — a
    /// branch gated twice has two) get `rerailed_to`, the new branch's
    /// get `rerailed_from`, other branches are untouched, and a packet
    /// without an id is skipped rather than PATCHed at an empty id.
    #[test]
    fn rerail_stamps_the_old_and_new_gate_runs_and_nothing_else() {
        let gate_runs = vec![
            json!({ "id": "old-1", "metadata": { "branch": "fix/x" } }),
            json!({ "id": "old-2", "metadata": { "branch": "fix/x" } }),
            json!({ "id": "new-1", "metadata": { "branch": "fix/x-rerail" } }),
            json!({ "id": "other", "metadata": { "branch": "feat/other" } }),
            json!({ "metadata": { "branch": "fix/x" } }),
        ];
        assert_eq!(
            rerail_stamps(&gate_runs, "fix/x", "fix/x-rerail"),
            vec![
                (
                    "old-1".to_string(),
                    json!({ "rerailed_to": "fix/x-rerail" })
                ),
                (
                    "old-2".to_string(),
                    json!({ "rerailed_to": "fix/x-rerail" })
                ),
                ("new-1".to_string(), json!({ "rerailed_from": "fix/x" })),
            ]
        );
    }

    /// A LIMIT IS NOT A FILTER (memory: a-limit-is-not-a-filter). A
    /// conflict-skipped car sits at the tail of `opened_on DESC` once
    /// open cars fill more than a page; the old bare `limit=200` read
    /// left it off page one and `boss rerail` reported it "not found".
    /// The read now pages on `total` (via `train::list_all_pages`, whose
    /// page-two behaviour is pinned there), so the branch lookup must
    /// reach a car gathered from page two.
    #[tokio::test]
    async fn a_car_on_page_two_is_rerailable_not_not_found() {
        let mut all: Vec<Value> = (0..crate::train::PAGE_LIMIT + 40)
            .map(|i| json!({ "id": format!("decoy-{i}"), "metadata": { "branch": format!("decoy-{i}") } }))
            .collect();
        all.push(json!({ "id": "car-tail", "metadata": { "branch": "fix/tail-car" } }));
        let all_ref = &all;
        let gathered = crate::train::list_all_pages(|offset| async move {
            let page: Vec<Value> = all_ref
                .iter()
                .skip(offset)
                .take(crate::train::PAGE_LIMIT)
                .cloned()
                .collect();
            anyhow::Ok(Some(json!({ "data": page, "total": all_ref.len() })))
        })
        .await
        .unwrap();
        let (car, branch) = select_car(&gathered, "fix/tail-car")
            .expect("the car on page two must be found, not reported not-found");
        assert_eq!(branch, "fix/tail-car");
        assert_eq!(car.get("id").and_then(Value::as_str), Some("car-tail"));
    }

    /// `--finish` RUNS TWICE (02165b1d, third instance). Car 538775dd
    /// was already on `…-rerail` when its head moved again; the old
    /// suffix-deriving finish looked for `…-rerail-rerail`, refused,
    /// and left the operator with no designed way to refresh the
    /// receipt — so they re-gated and got a twin car instead.
    ///
    /// The forge answers the question now: a rerail in flight is
    /// finished onto its branch; with no such branch, the car is
    /// refreshed where it stands.
    #[test]
    fn finish_targets_the_rerail_branch_only_when_one_exists() {
        // The post-conflict path: the human pushed feat/x-rerail.
        assert_eq!(
            finish_target("feat/x", "feat/x-rerail", true),
            "feat/x-rerail"
        );
        // Car 538775dd: already re-railed, head moved again. There is no
        // feat/…-rerail-rerail and there never will be — refresh in
        // place rather than refuse.
        assert_eq!(
            finish_target(
                "feat/arrival-runs-the-probe-rerail",
                "feat/arrival-runs-the-probe-rerail-rerail",
                false
            ),
            "feat/arrival-runs-the-probe-rerail"
        );
        // The same reading covers a car that was never re-railed at all
        // and simply had a rebase — one verb, no second one to pick
        // wrong.
        assert_eq!(finish_target("fix/y", "fix/y-rerail", false), "fix/y");
    }

    /// THE ORIGIN THE REPOINT RECORDS (packet 473fda1b, generator 1).
    /// A rerail leaves the branch it moved off on the forge, and that
    /// branch is never a car — so the arrival sweep, which iterates a
    /// train's boarded cars, could never see it: 13 originals
    /// accumulated and came off by hand on 2026-09-10. The car's record
    /// now names the branch AND the head it carried when the car left
    /// it, which is what lets the sweep delete only what the rerail
    /// actually carried away (the same head guard a car's own branch
    /// gets).
    #[test]
    fn a_rerail_records_the_branch_it_moved_off_with_its_head() {
        let car = json!({ "id": "car-1", "metadata": { "branch": "feat/x" } });
        assert_eq!(
            origins_after(&car, "feat/x", Some("abc123"), "feat/x-rerail"),
            Some(vec![json!({ "branch": "feat/x", "head": "abc123" })])
        );
    }

    /// An unreadable head is recorded as absent, never guessed: the
    /// sweep then refuses the branch by name (`SweepGuard::NoRecord`),
    /// which is a line an operator can act on. A fabricated head would
    /// be a deletion with no evidence behind it.
    #[test]
    fn an_unreadable_old_head_is_recorded_as_absent() {
        let car = json!({ "id": "car-1", "metadata": { "branch": "feat/x" } });
        assert_eq!(
            origins_after(&car, "feat/x", None, "feat/x-rerail"),
            Some(vec![json!({ "branch": "feat/x" })])
        );
    }

    /// A refresh in place moved no branch, so there is no original to
    /// record — and `None` OMITS the key rather than nulling it, which
    /// the metadata door would read as "delete the origins this car
    /// already recorded" (the `delivery_channel` lesson in
    /// `boss_jobs::car::regate_patch`).
    #[test]
    fn a_refresh_in_place_records_no_origin() {
        let car = json!({
            "id": "car-1",
            "metadata": {
                "branch": "feat/x-rerail",
                "rerail_origins": [{ "branch": "feat/x", "head": "abc123" }]
            }
        });
        assert_eq!(
            origins_after(&car, "feat/x-rerail", Some("def456"), "feat/x-rerail"),
            None
        );
    }

    /// A CAR RE-RAILED TWICE keeps both originals: the chain is
    /// appended to, oldest first, so the second rerail does not drop
    /// the first original back into the leak. A branch already recorded
    /// is not recorded twice, and its first-recorded head — the one it
    /// carried when the car actually left it — stands.
    #[test]
    fn a_second_rerail_appends_the_branch_it_moved_off() {
        let car = json!({
            "id": "car-1",
            "metadata": {
                "branch": "feat/x-rerail",
                "rerail_origins": [{ "branch": "feat/x", "head": "abc123" }]
            }
        });
        assert_eq!(
            origins_after(
                &car,
                "feat/x-rerail",
                Some("def456"),
                "feat/x-rerail-rerail"
            ),
            Some(vec![
                json!({ "branch": "feat/x", "head": "abc123" }),
                json!({ "branch": "feat/x-rerail", "head": "def456" }),
            ])
        );
        // And again, with the first branch somehow re-offered: recorded
        // once, with the head it carried the first time.
        let twice = json!({
            "id": "car-1",
            "metadata": {
                "branch": "feat/x",
                "rerail_origins": [{ "branch": "feat/x", "head": "abc123" }]
            }
        });
        assert_eq!(
            origins_after(&twice, "feat/x", Some("zzz999"), "feat/x-rerail-2"),
            Some(vec![json!({ "branch": "feat/x", "head": "abc123" })])
        );
    }

    /// A refresh in place did not move the branch, so it stamps no
    /// gate-run: `rerailed_to: <itself>` would be a fabricated record
    /// and would make the yard read the branch's own live green as
    /// superseded.
    #[test]
    fn a_refresh_in_place_stamps_no_gate_run() {
        let gate_runs = vec![
            json!({ "id": "g1", "metadata": { "branch": "feat/x-rerail" } }),
            json!({ "id": "g2", "metadata": { "branch": "feat/x-rerail" } }),
        ];
        assert!(
            rerail_stamps(&gate_runs, "feat/x-rerail", "feat/x-rerail").is_empty(),
            "the branch did not move — nothing was superseded"
        );
    }

    /// And the note the car carries says which of the two happened. A
    /// refresh that claimed "rerailed from X" would put a move in the
    /// record that never occurred.
    #[test]
    fn the_note_records_a_refresh_as_a_refresh() {
        let moved = repoint_note("feat/x", "feat/x-rerail");
        assert!(moved.contains("rerailed from feat/x"), "{moved}");

        let in_place = repoint_note("feat/x-rerail", "feat/x-rerail");
        assert!(in_place.contains("refreshed in place"), "{in_place}");
        assert!(
            !in_place.contains("rerailed from"),
            "a car that did not move was not rerailed: {in_place}"
        );
        // Both say where the receipt came from — the copy contract is
        // the same either way.
        for n in [&moved, &in_place] {
            assert!(n.contains("regate_receipt"), "{n}");
        }
    }
}
