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
    let body = gate::api(
        http,
        reqwest::Method::GET,
        "/api/jobs?kind=ship-a-change&status=open&limit=200",
        None,
    )
    .await?;
    let cars = gate::rows(body);
    let by_branch: Vec<&Value> = cars
        .iter()
        .filter(|c| c.pointer("/metadata/branch").and_then(Value::as_str) == Some(given))
        .collect();
    let car = if let [one] = by_branch.as_slice() {
        (*one).clone()
    } else {
        let id = park::resolve_job_id(&cars, given)?;
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
    let mut patch = boss_jobs::car::regate_patch(
        receipt,
        &format!(
            "rerailed from {old_branch} by boss rerail: new branch cut from \
             origin/main, re-gated, receipt machine-copied to regate_receipt \
             (the frozen gate step stays as the original head's record)"
        ),
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
    println!(
        "boss rerail: {} repointed {old_branch} -> {new_branch} — receipt copied, \
         skip cleared; the next boarding takes it",
        &car_id[..8.min(car_id.len())]
    );
    Ok(())
}

pub async fn run(given: &str, finish_only: bool, dry: bool) -> Result<()> {
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
    )
    .await?;

    finish(&http, &car, &old_branch, &new_branch).await
}
