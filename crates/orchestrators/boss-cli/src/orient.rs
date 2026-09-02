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
    match std::process::Command::new("git")
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
