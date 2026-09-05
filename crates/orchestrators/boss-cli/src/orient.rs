//! `boss orient` — a session's first verb: the approach, in one read.
//!
//! THE CASE (acedf981, and CLAUDE.md §Engineering Session Startup). A
//! new session starts blind, and the queue does not un-blind it: on
//! 2026-09-01 the durable pod session rebuilt a landed fix from a stale
//! base, closed five already-fixed "open" packets one by one, and
//! duplicated a branch that sat green-gated and unparked — every one of
//! those visible at startup, spread across five separate queries nobody
//! ran. This verb is those queries, assembled: trains in transit, gates
//! running, greens that never became cars (the stranded — reusing the
//! census cross-ref, not a second definition), the dock, and the task
//! queue, with the startup checklist's load-bearing lines at the end.
//!
//! Read-only. Inherits `gate::api`'s no-default `BOSS_JOBS_URL` rule —
//! fitting, since orienting against the wrong instance is the exact
//! defect class the rule exists for (aa783636).

use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeSet;

use crate::gate::{api, rows};

fn md_str<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get("metadata")
        .and_then(|m| m.get(key))
        .and_then(Value::as_str)
        .unwrap_or("")
}

/// The step a packet is currently at: first ready/active title.
fn at_step(v: &Value) -> String {
    v.get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| {
            matches!(
                s.get("status").and_then(Value::as_str),
                Some("ready") | Some("active")
            )
        })
        .and_then(|s| s.get("title").and_then(Value::as_str))
        .unwrap_or("—")
        .to_string()
}

/// Branches whose base has fallen behind `origin/main`. Each pair is
/// (branch, exit code of `git merge-base --is-ancestor origin/main
/// origin/<branch>`): 0 = main is an ancestor (fresh), 1 = not an
/// ancestor (behind), anything else = a bad or missing ref, which we do
/// NOT flag (a deleted branch is not a stale one). A parked car on a
/// stale base merges clean and reverts the train
/// ([[a-clean-merge-is-not-a-correct-merge]]), so the conductor must
/// re-gate it before boarding — L2 of the orientation protocol
/// (acedf981): measure current reality before building or boarding.
/// Open car packets that are residue: their forge branch is GONE, yet
/// they are not in the one state where a vanished branch is normal —
/// `Proven in prod`, the landed-awaiting-proof step whose branch the
/// train already swept. Everything else with a gone branch is a packet
/// that landed via a twin, was abandoned, or stuck while its branch
/// disappeared — the inflated-open-count residue L3 exists to surface
/// (acedf981; [[a-left-behind-car-may-be-a-landed-twin]]). Each pair is
/// (branch, current-step title). A missing/empty branch is not residue:
/// a car that never pushed has nothing gone.
fn residue_cars<'a>(
    open_cars: &'a [(String, String)],
    forge_heads: &std::collections::BTreeSet<String>,
) -> Vec<&'a str> {
    open_cars
        .iter()
        .filter(|(branch, step)| {
            !branch.is_empty() && !forge_heads.contains(branch) && step != "Proven in prod"
        })
        .map(|(branch, _)| branch.as_str())
        .collect()
}

fn bases_behind(checks: &[(String, Option<i32>)]) -> Vec<&str> {
    checks
        .iter()
        .filter(|(_, code)| *code == Some(1))
        .map(|(b, _)| b.as_str())
        .collect()
}

pub async fn run() -> Result<()> {
    let http = reqwest::Client::new();

    println!("boss orient — the approach, before you build");
    println!(
        "  jobs api  {}",
        std::env::var("BOSS_JOBS_URL").unwrap_or_default()
    );

    // Trains in transit.
    let trains = rows(
        api(
            &http,
            reqwest::Method::GET,
            "/api/jobs?kind=pr-train&status=open&limit=10",
            None,
        )
        .await?,
    );
    println!("\n  IN TRANSIT — {} train(s)", trains.len());
    for t in &trains {
        println!(
            "    {}  at: {}",
            t.get("title").and_then(Value::as_str).unwrap_or("?"),
            at_step(t)
        );
    }

    // Gates running now.
    let gating = rows(
        api(
            &http,
            reqwest::Method::GET,
            "/api/jobs?kind=gate-run&status=open&limit=20",
            None,
        )
        .await?,
    );
    println!("\n  GATING — {} run(s)", gating.len());
    for g in &gating {
        println!("    {}", md_str(g, "branch"));
    }

    // Stranded greens: gated, never parked — the census cross-ref, not a
    // second definition (§9a). A gate-run CLOSES on its verdict, so this
    // reads closed ones; a status=open query cannot see them.
    let gate_runs = rows(
        api(
            &http,
            reqwest::Method::GET,
            "/api/jobs?kind=gate-run&limit=60",
            None,
        )
        .await?,
    );
    let cars = rows(
        api(
            &http,
            reqwest::Method::GET,
            "/api/jobs?kind=ship-a-change&limit=800",
            None,
        )
        .await?,
    );
    let car_branches: BTreeSet<String> = cars
        .iter()
        .map(|c| md_str(c, "branch").to_string())
        .filter(|b| !b.is_empty())
        .collect();
    let stranded = crate::census::stranded_gate_runs(&gate_runs, &car_branches);
    if stranded.is_empty() {
        println!("\n  STRANDED — none: every green gate became a car");
    } else {
        println!(
            "\n  STRANDED — {} green gate(s) that never became a car (rescue = rebase \
             onto origin/main + re-gate; never rebuild blind):",
            stranded.len()
        );
        for b in &stranded {
            println!("    {b}");
        }
    }

    // Orphans: forge heads no packet claims (281f9842 — 60 of 80 the
    // day this was measured). The claimed set is every branch any
    // fetched packet names: cars, gate-runs open and closed. Reading
    // the forge needs git rather than the jobs api, and a workstation
    // without the remote must still orient — so a failed read prints
    // WHY and skips, it never fails the verb.
    let mut claimed = car_branches.clone();
    claimed.extend(
        gate_runs
            .iter()
            .chain(gating.iter())
            .map(|g| md_str(g, "branch").to_string())
            .filter(|b| !b.is_empty()),
    );
    match crate::git_auth::command()
        .args(["ls-remote", "--heads", "origin"])
        .output()
    {
        Ok(out) if out.status.success() => {
            let orphans =
                crate::census::orphan_branches(&String::from_utf8_lossy(&out.stdout), &claimed);
            if orphans.is_empty() {
                println!("\n  ORPHANS — none: every forge head is claimed by a packet");
            } else {
                println!(
                    "\n  ORPHANS — {} forge head(s) no packet claims (cannot board; \
                     file a packet or delete the branch):",
                    orphans.len()
                );
                const SHOWN: usize = 12;
                for b in orphans.iter().take(SHOWN) {
                    println!("    {b}");
                }
                if orphans.len() > SHOWN {
                    println!("    …and {} more", orphans.len() - SHOWN);
                }
            }
            // RESIDUE (L3, acedf981): the inverse cross-ref. Orphans are
            // forge heads no packet claims; residue is open car packets
            // whose branch is GONE — landed via a twin, abandoned, or
            // stuck — excluding the normal landed-awaiting-proof state.
            let forge_heads: std::collections::BTreeSet<String> =
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .filter_map(|l| l.split_whitespace().nth(1))
                    .filter_map(|r| r.strip_prefix("refs/heads/"))
                    .map(str::to_string)
                    .collect();
            let open_cars: Vec<(String, String)> = cars
                .iter()
                .filter(|c| c.get("status").and_then(Value::as_str) == Some("open"))
                .map(|c| (md_str(c, "branch").to_string(), at_step(c)))
                .collect();
            let residue = residue_cars(&open_cars, &forge_heads);
            if residue.is_empty() {
                println!(
                    "\n  RESIDUE — none: every open car still has a forge branch (or is landed-awaiting-proof)"
                );
            } else {
                println!(
                    "\n  RESIDUE — {} open car(s) whose forge branch is GONE (likely landed via a twin or abandoned; close the packet, or `boss merged <branch>` — see the left-behind-twin trap):",
                    residue.len()
                );
                for b in &residue {
                    println!("    {b}");
                }
            }
        }
        Ok(out) => println!(
            "\n  ORPHANS — skipped: git ls-remote failed ({})",
            String::from_utf8_lossy(&out.stderr)
                .lines()
                .next()
                .unwrap_or("no stderr")
        ),
        Err(e) => println!("\n  ORPHANS — skipped: could not run git ({e})"),
    }

    // The dock.
    let dock = api(
        &http,
        reqwest::Method::GET,
        "/api/stations/loading-dock/queue",
        None,
    )
    .await?;
    let dock_total = dock
        .as_ref()
        .and_then(|d| d.get("total"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    println!("\n  DOCK — {dock_total} car(s) parked");
    for c in dock
        .as_ref()
        .map(|d| rows(Some(d.clone())))
        .unwrap_or_default()
    {
        println!(
            "    {}",
            c.get("title").and_then(Value::as_str).unwrap_or("?")
        );
    }

    // FRESHNESS (L2, acedf981) — parked and stranded branches whose
    // base has fallen behind origin/main. A car cut from an old main
    // merges clean and reverts the trains, so a green gate on a stale
    // base is a re-gate, not a boarding. Needs the objects (merge-base),
    // so it fetches; like ORPHANS, a failed read prints WHY and skips
    // rather than failing the verb.
    let mut fresh_targets: Vec<String> = dock
        .as_ref()
        .map(|d| rows(Some(d.clone())))
        .unwrap_or_default()
        .iter()
        .map(|c| md_str(c, "branch").to_string())
        .filter(|b| !b.is_empty())
        .collect();
    fresh_targets.extend(stranded.iter().cloned());
    fresh_targets.sort();
    fresh_targets.dedup();
    if !fresh_targets.is_empty() {
        match crate::git_auth::command()
            .args(["fetch", "-q", "origin"])
            .output()
        {
            Ok(out) if out.status.success() => {
                let checks: Vec<(String, Option<i32>)> = fresh_targets
                    .iter()
                    .map(|b| {
                        let code = crate::git_auth::command()
                            .args([
                                "merge-base",
                                "--is-ancestor",
                                "origin/main",
                                &format!("origin/{b}"),
                            ])
                            .output()
                            .ok()
                            .and_then(|o| o.status.code());
                        (b.clone(), code)
                    })
                    .collect();
                let behind = bases_behind(&checks);
                if behind.is_empty() {
                    println!(
                        "\n  FRESHNESS — every parked/stranded branch is based on current origin/main"
                    );
                } else {
                    println!(
                        "\n  FRESHNESS — {} branch(es) based BEHIND origin/main (re-gate before \
                         boarding; a stale base merges clean and reverts the train):",
                        behind.len()
                    );
                    for b in behind {
                        println!("    {b}");
                    }
                }
            }
            Ok(out) => println!(
                "\n  FRESHNESS — skipped: git fetch failed ({})",
                String::from_utf8_lossy(&out.stderr)
                    .lines()
                    .next()
                    .unwrap_or("no stderr")
            ),
            Err(e) => println!("\n  FRESHNESS — skipped: could not run git ({e})"),
        }
    }

    // The task queue, as a number.
    let tasks = api(
        &http,
        reqwest::Method::GET,
        "/api/stations/q.platform-admin.task/queue",
        None,
    )
    .await?;
    let task_total = tasks
        .as_ref()
        .and_then(|d| d.get("total"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    println!("\n  TASK QUEUE — {task_total} open item(s)");

    // The checklist this verb exists to make cheap — the load-bearing
    // lines of CLAUDE.md §Engineering Session Startup.
    println!("\n  BEFORE YOU BUILD:");
    println!(
        "    git fetch origin — branch and diff against origin/main, never a stale local main"
    );
    println!(
        "    a packet's claim may already be fixed on main: verify before building (close stale, not rebuild)"
    );
    println!(
        "    a stranded green above may already BE the fix: rescue it, never rebuild it blind"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heads(bs: &[&str]) -> std::collections::BTreeSet<String> {
        bs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn residue_is_an_open_car_whose_branch_vanished_mid_flight() {
        let open = vec![
            ("fix/gone".to_string(), "Open for review".to_string()),
            ("fix/live".to_string(), "Gate green".to_string()),
        ];
        // fix/gone is not among the forge heads and is not
        // landed-awaiting-proof -> residue; fix/live still has a head.
        assert_eq!(residue_cars(&open, &heads(&["fix/live"])), vec!["fix/gone"]);
    }

    #[test]
    fn a_landed_awaiting_proof_car_with_a_swept_branch_is_not_residue() {
        let open = vec![("fix/landed".to_string(), "Proven in prod".to_string())];
        // Its branch was swept by the train — the normal state, not residue.
        assert!(residue_cars(&open, &heads(&[])).is_empty());
    }

    #[test]
    fn a_car_that_never_pushed_a_branch_is_not_residue() {
        let open = vec![(String::new(), "Publishing".to_string())];
        assert!(residue_cars(&open, &heads(&[])).is_empty());
    }

    #[test]
    fn bases_behind_flags_only_the_not_ancestor_code() {
        let checks = vec![
            ("fresh/a".to_string(), Some(0)),  // main is an ancestor — fresh
            ("stale/b".to_string(), Some(1)),  // not an ancestor — behind
            ("gone/c".to_string(), Some(128)), // bad/missing ref — not flagged
            ("err/d".to_string(), None),       // git could not run — not flagged
        ];
        assert_eq!(bases_behind(&checks), vec!["stale/b"]);
    }

    #[test]
    fn bases_behind_is_empty_when_all_fresh() {
        let checks = vec![("a".to_string(), Some(0)), ("b".to_string(), Some(0))];
        assert!(bases_behind(&checks).is_empty());
    }
}
