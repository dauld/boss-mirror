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

/// Repoint the car at the re-gated branch: `branch` moves, the fresh
/// receipt rides `regate_receipt` VERBATIM, and the skip_reason is
/// deleted (a PATCH key set to null is removed — the metadata door's
/// documented contract) so the next boarding no longer sees a skip.
async fn repoint(
    http: &reqwest::Client,
    car_id: &str,
    new_branch: &str,
    receipt: &boss_jobs::car::Receipt,
    old_branch: &str,
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
        &format!(
            "rerailed from {old_branch} by boss rerail: new branch cut from \
             origin/main, re-gated, receipt machine-copied to regate_receipt \
             (the frozen gate step stays as the original head's record)"
        ),
        dc.as_deref(),
    );
    patch["branch"] = json!(new_branch);
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
pub(crate) fn rerail_stamps(
    gate_runs: &[Value],
    old_branch: &str,
    new_branch: &str,
) -> Vec<(String, Value)> {
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
    repoint(http, car_id, new_branch, &receipt, old_branch).await?;
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
    println!(
        "boss rerail: {} repointed {old_branch} -> {new_branch} — receipt copied, \
         skip cleared; the next boarding takes it",
        &car_id[..8.min(car_id.len())]
    );
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
        return finish(&http, &car, &old_branch, &new_branch).await;
    }

    // The rebase, in a disposable worktree cut from the CURRENT trunk.
    git(".", &["fetch", "origin"])?;
    if git(
        ".",
        &["ls-remote", "origin", &format!("refs/heads/{new_branch}")],
    )?
    .lines()
    .any(|l| !l.is_empty())
    {
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
}
