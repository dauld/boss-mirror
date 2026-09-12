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

use crate::gate::{AbandonedPlace, abandoned_places, api, queue_order, rows};

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
/// origin/<branch>`), read through the ONE definition of that code
/// (`freshness::base_from_is_ancestor_code`): 0 = main is an ancestor
/// (fresh), 1 = not an ancestor (behind), anything else = a bad or
/// missing ref, which we do NOT flag (a deleted branch is not a stale
/// one). `boss gate` refuses a behind base on that same reading, and the
/// two must not disagree about what "behind" means (§9a). A parked car
/// on a stale base merges clean and reverts the train
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

/// Cars HELD on the dock, each with the reason an operator wrote —
/// `(branch, reason)`, branch-sorted.
///
/// A held car is parked at the dock and cannot board: `metadata.hold` on
/// its review step says "this is gated green and still must not ride
/// yet". The loading-dock station row knows that now (36c3d4ca), which
/// means the queue lens stops LISTING it — so the DOCK section below,
/// which reads that lens, would show a shrinking dock and name nothing
/// about the cars still standing on it. `boss orient` is the verb a
/// session runs before it does anything else; a car that disappears from
/// it is a car the next session rebuilds blind.
///
/// TWO SHARED DEFINITIONS, no third. "Still at the dock" is
/// `boss_jobs::car::is_parked` — the predicate `boss park`, the auto-park
/// handler and the conductor's own dock count already share — plus the
/// `status=open` filter that predicate deliberately leaves to its callers.
/// "Is there a hold" is `boss_jobs::stranded::hold_reason`, where `null`,
/// `false` and `""` are NO marker: a released hold boards again, and a
/// `truthy`/`is_some` reading of it would strand the car here forever.
/// Read off the car packets orient has already fetched — no second read.
fn held_dock_cars(cars: &[Value]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = cars
        .iter()
        .filter(|c| c.get("status").and_then(Value::as_str) == Some("open"))
        .filter(|c| boss_jobs::car::is_parked(c))
        .filter_map(|c| {
            let review = boss_jobs::car::find_step(c, "review", boss_jobs::car::REVIEW)?;
            let reason = boss_jobs::stranded::hold_reason(review.get("metadata")?)?;
            Some((md_str(c, "branch").to_string(), reason))
        })
        .collect();
    out.sort();
    out
}

/// The lines naming every queued gate-run NO PROCESS HOLDS — empty when
/// there are none.
///
/// A TROUBLED PACKET MUST LOOK TROUBLED (CLAUDE.md §Diagnosis; 76d41004).
/// This verb used to list every queued run in one lane as "since
/// <stamp>", so a run whose waiter had been dead for ten minutes —
/// with no Job, and nothing left that would ever launch it — rendered
/// exactly like a run two minutes from its slot. Measured 2026-09-10
/// 22:41–22:51Z; the yard's queued lane still reads the same way.
///
/// Each line carries what the operator's next move needs: the branch,
/// the packet, how long nobody has held it, whether a car is owed, and
/// the verb that recovers it. The recovery is the BARE verb on purpose —
/// a re-gate REUSES the open packet (`reusable_packet`) and an empty
/// `ParkIntent` stamps nothing, so the `park_*` keys already on the
/// packet survive; only a branch that has LANDED has them cleared
/// (610537b2). Pinned by gate.rs's
/// `a_bare_regate_stamps_nothing_so_a_reused_packets_intent_survives`,
/// because this is advice a tired operator will follow verbatim.
fn abandoned_report(places: &[AbandonedPlace]) -> Vec<String> {
    if places.is_empty() {
        return Vec::new();
    }
    let mut out = vec![format!(
        "\n  ABANDONED IN THE QUEUE — {} queued gate-run(s) NO PROCESS HOLDS. A queued \
         run is launched by the `--wait` process holding its place, so a waiter that \
         died leaves an open packet with no Job and nothing to start it (76d41004):",
        places.len()
    )];
    for p in places {
        let idle = match p.idle_secs {
            Some(secs) => format!("idle {}m", secs / 60),
            // Never an invented age: a place whose stamps do not parse
            // cannot be ordered by the queue either, which is itself why
            // it is stranded.
            None => format!("idle unknown — an unreadable queued_at ({})", p.queued_at),
        };
        let owed = if p.park_intent {
            "  (park intent stamped — a car is owed on green)"
        } else {
            ""
        };
        let branch = if p.branch.is_empty() {
            "(no branch recorded)"
        } else {
            &p.branch
        };
        out.push(format!(
            "    {branch}  packet {}  {idle}{owed}",
            &p.packet[..8.min(p.packet.len())],
        ));
        if !p.branch.is_empty() {
            out.push(format!(
                "      recover: boss gate {} --wait  — reuses this packet and keeps its \
                 park intent (a bare re-gate stamps nothing over it)",
                p.branch
            ));
        }
    }
    out
}

fn bases_behind(checks: &[(String, Option<i32>)]) -> Vec<&str> {
    checks
        .iter()
        .filter(|(_, code)| {
            crate::freshness::base_from_is_ancestor_code(*code) == crate::freshness::Base::Behind
        })
        .map(|(b, _)| b.as_str())
        .collect()
}

/// How many orphans the default listing shows before it says "…and N
/// more". A bound, not a filter: the count above it is always the whole
/// set, and the tail line names the flag that shows the rest.
pub(crate) const ORPHANS_SHOWN: usize = 12;

/// Where a landed, unproven car stands — the yard's inspection shed
/// read in words (`apps/web/src/it/yard/yard-shed.ts`, `shedPlace`).
/// Four landed cars sat at `Proven in prod` on 2026-09-12 with a
/// recorded proof EVENT rather than a probe, which is correct: each
/// can only be proven when a real fault or a real timer firing happens.
/// But `boss orient` did not list them at all, and the yard's counter
/// once rendered them exactly like a car nobody proved (9e3e07aa). A
/// reader must be able to tell "waiting on the next red train" from
/// "nobody ran boss prove"; the difference is the whole question. The
/// three places are disjoint and total, the probe winning over an event
/// (a probe can be run; an event has to happen).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Shed {
    /// A probe is recorded; `last` is the failed attempt's `why`, if any
    /// (a succeeding attempt completes the step and the car leaves).
    ProbePending { last: Option<String> },
    /// Only an event is recorded: prose naming what has to happen.
    WaitingOn(String),
    /// Neither — the forgotten case, and the only troubled one.
    Unproven,
}

pub(crate) fn shed_place(car: &Value) -> Shed {
    let probe = md_str(car, "proof_probe");
    let event = md_str(car, "proof_event");
    if !probe.is_empty() {
        let last = car
            .pointer("/metadata/proof_attempt/why")
            .and_then(Value::as_str)
            .filter(|w| !w.is_empty())
            .map(str::to_string);
        Shed::ProbePending { last }
    } else if !event.is_empty() {
        Shed::WaitingOn(event.to_string())
    } else {
        Shed::Unproven
    }
}

/// One terminal line's worth of a probe verdict or an event's prose.
/// The full text rides the packet and the yard shows it; here the
/// reader wants the first clause, not the paragraph (a 600-character
/// `why` was the first live line).
pub(crate) const SHED_TEXT_CHARS: usize = 160;

fn clipped(text: &str) -> String {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() <= SHED_TEXT_CHARS {
        t
    } else {
        let cut: String = t.chars().take(SHED_TEXT_CHARS - 1).collect();
        format!("{}…", cut.trim_end())
    }
}

/// The SHED listing lines: every open car at `Proven in prod`, one
/// line each, saying which of the three places it stands in. Pure so
/// the shape is testable; the caller prints the heading from the count.
pub(crate) fn shed_lines(cars: &[Value]) -> Vec<String> {
    cars.iter()
        .filter(|c| c.get("status").and_then(Value::as_str) == Some("open"))
        .filter(|c| at_step(c) == "Proven in prod")
        .map(|c| {
            let branch = md_str(c, "branch");
            match shed_place(c) {
                Shed::ProbePending { last: None } => {
                    format!("    {branch}: probe pending (the forge runs it on arrival)")
                }
                Shed::ProbePending { last: Some(why) } => {
                    format!("    {branch}: probe FAILING — {}", clipped(&why))
                }
                Shed::WaitingOn(ev) => format!("    {branch}: waiting on: {}", clipped(&ev)),
                Shed::Unproven => format!(
                    "    {branch}: UNPROVEN — no probe, no event; nothing mechanical can settle it (boss prove --probe)"
                ),
            }
        })
        .collect()
}

/// The ORPHANS listing lines for a set of forge heads, bounded to
/// `shown` entries unless `all` — and when bounded, the tail line
/// SAYS how to see the rest, because a list truncated with no way to
/// read the remainder is read once and then skipped (ce673e64: 46
/// orphans, 12 shown, 34 invisible from every surface, and `boss
/// orient` took no flags at all). Pure so the shape is testable.
pub(crate) fn orphan_lines(orphans: &[String], shown: usize, all: bool) -> Vec<String> {
    let cap = if all { orphans.len() } else { shown };
    let mut out: Vec<String> = orphans
        .iter()
        .take(cap)
        .map(|b| format!("    {b}"))
        .collect();
    if orphans.len() > cap {
        out.push(format!(
            "    …and {} more — `boss orient --all` lists every one",
            orphans.len() - cap
        ));
    }
    out
}

/// A stranded green that is the second half of a `boss rerail` a killed
/// waiter never finished: `<branch>-rerail` gated green while the car
/// still points at `<branch>`. It lists as stranded (a green no car
/// claims) — correctly — but the lane's generic rescue, "rebase + re-gate",
/// is the wrong verb for it: everything durable is done and one PATCH is
/// owed. `Some(advice)` names the car and the verb that finishes it
/// (464309ee: a `--finish` that existed and nothing pointed at).
pub(crate) fn half_done_rerail(
    branch: &str,
    car_ids: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    let base = branch.strip_suffix("-rerail")?;
    let car = car_ids.get(base)?;
    let id8 = &car[..8.min(car.len())];
    Some(format!(
        "← half-done rerail of {base} (car {id8}): everything but the repoint is done — \
         finish it with `boss rerail {id8} --finish`, never re-gate"
    ))
}

pub async fn run(all: bool) -> Result<()> {
    let http = reqwest::Client::new();

    println!("boss orient — the approach, before you build");
    println!(
        "  jobs api  {}",
        std::env::var("BOSS_JOBS_URL").unwrap_or_default()
    );
    // The tool reading the approach must say what IT is: a binary that
    // lags main runs older verbs silently (895c9a3b).
    println!(
        "{}",
        crate::built_from::freshness_line(
            crate::built_from::built_from(),
            crate::built_from::origin_main_head().as_deref()
        )
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
    // A QUEUED run is not gating: it is waiting for a slot and has no
    // Job at all (boss_jobs::yard::QUEUED_AT). Reporting it as GATING
    // would overstate what the node is doing by exactly the number of
    // builders standing in line.
    let (waiting, running): (Vec<&Value>, Vec<&Value>) = gating
        .iter()
        .partition(|g| !md_str(g, boss_jobs::yard::QUEUED_AT).is_empty());
    println!("\n  GATING — {} run(s)", running.len());
    for g in &running {
        println!("    {}", md_str(g, "branch"));
    }
    // A PLACE NOBODY HOLDS IS NOT A QUEUE. The two readings are the
    // system of record's own: `queue_order` is every place a live
    // process is still heartbeating for, and `abandoned_places` is the
    // rest of the marked runs — one definition, split (§9a). Wall time,
    // because the heartbeat that keeps a place is wall-stamped by the
    // waiter; `wall_now()` is the sanctioned source (the no-wallclock
    // lint's rule 1) and keeps this read-only verb's signature out of
    // the contended boundary in main.rs.
    let now = boss_clock_client::wall_now();
    let in_line = queue_order(&gating, now, crate::gate::QUEUE_PLACE_TTL_SECS);
    let abandoned = abandoned_places(&gating, now, crate::gate::QUEUE_PLACE_TTL_SECS);
    if !waiting.is_empty() {
        println!(
            "\n  QUEUED FOR A SLOT — {} place(s) held by a live process",
            in_line.len()
        );
        for g in waiting.iter().filter(|g| {
            let id = g.get("id").and_then(Value::as_str).unwrap_or_default();
            in_line.iter().any(|held| held == id)
        }) {
            println!(
                "    {}  since {}",
                md_str(g, "branch"),
                md_str(g, boss_jobs::yard::QUEUED_AT)
            );
        }
    }
    for line in abandoned_report(&abandoned) {
        println!("{line}");
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
        let car_ids: std::collections::BTreeMap<String, String> = cars
            .iter()
            .filter_map(|c| {
                let b = md_str(c, "branch");
                let id = c.get("id")?.as_str()?;
                (!b.is_empty()).then(|| (b.to_string(), id.to_string()))
            })
            .collect();
        for b in &stranded {
            match half_done_rerail(b, &car_ids) {
                Some(line) => println!("    {b}  {line}"),
                None => println!("    {b}"),
            }
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
                for line in orphan_lines(&orphans, ORPHANS_SHOWN, all) {
                    println!("{line}");
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

    // HELD ON THE DOCK — cars standing there that cannot board. The
    // queue lens above no longer lists them (36c3d4ca), so without this
    // they appear nowhere in the one read a session runs first, and their
    // branches read as orphans or residue instead of as work waiting on a
    // named action. A brake deliberately on is not an alarm — but an
    // invisible brake is.
    let held = held_dock_cars(&cars);
    if held.is_empty() {
        println!("  HELD — none: every car on the dock can board");
    } else {
        println!(
            "  HELD — {} car(s) ON the dock that cannot board (release = clear \
             `hold` on the review step; never rebuild one blind):",
            held.len()
        );
        for (branch, reason) in &held {
            println!("    {branch}  —  {reason}");
        }
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
    // A held car is still a car: the longer a hold lasts the further its
    // base falls behind, and a stale base merges clean and reverts the
    // train ([[parked-cars-go-stale]]). It has to be re-gated before it is
    // released, so it belongs in the freshness check beside the parked.
    fresh_targets.extend(held.iter().map(|(b, _)| b.clone()));
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

    // THE SHED — landed cars not yet proven, and what each waits on.
    let shed = shed_lines(&cars);
    if shed.is_empty() {
        println!("\n  SHED — empty: every landed car is proven");
    } else {
        println!(
            "\n  SHED — {} landed car(s) awaiting proof (probe pending / waiting on an event / UNPROVEN):",
            shed.len()
        );
        for line in &shed {
            println!("{line}");
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
    #[test]
    fn a_stranded_rerail_head_names_the_car_and_the_finishing_verb() {
        let cars: std::collections::BTreeMap<String, String> = [(
            "feat/x".to_string(),
            "abcdef12-0000-4000-8000-000000000000".to_string(),
        )]
        .into_iter()
        .collect();
        let line = super::half_done_rerail("feat/x-rerail", &cars).expect("a half-done rerail");
        assert!(
            line.contains("feat/x") && line.contains("boss rerail abcdef12 --finish"),
            "{line}"
        );
        assert!(line.contains("never re-gate"), "{line}");
    }

    #[test]
    fn a_plain_stranded_green_gets_no_rerail_advice() {
        let cars: std::collections::BTreeMap<String, String> = [(
            "feat/x".to_string(),
            "abcdef12-0000-4000-8000-000000000000".to_string(),
        )]
        .into_iter()
        .collect();
        assert_eq!(super::half_done_rerail("feat/y", &cars), None);
        // a -rerail head whose base has no car is just stranded
        assert_eq!(super::half_done_rerail("feat/z-rerail", &cars), None);
    }

    #[test]
    fn a_bounded_orphan_list_names_the_flag_that_shows_the_rest() {
        let orphans: Vec<String> = (0..46).map(|i| format!("feat/b{i}")).collect();
        let lines = super::orphan_lines(&orphans, 12, false);
        assert_eq!(lines.len(), 13, "12 entries plus one tail line");
        assert_eq!(lines[0], "    feat/b0");
        assert_eq!(lines[11], "    feat/b11");
        assert!(
            lines[12].contains("34 more") && lines[12].contains("boss orient --all"),
            "the tail must say how many are hidden AND how to see them: {}",
            lines[12]
        );
    }

    #[test]
    fn all_lists_every_orphan_with_no_tail() {
        let orphans: Vec<String> = (0..46).map(|i| format!("feat/b{i}")).collect();
        let lines = super::orphan_lines(&orphans, 12, true);
        assert_eq!(lines.len(), 46);
        assert!(lines.iter().all(|l| l.starts_with("    feat/b")));
    }

    #[test]
    fn a_list_within_the_bound_has_no_tail_either_way() {
        let orphans: Vec<String> = (0..5).map(|i| format!("feat/b{i}")).collect();
        assert_eq!(super::orphan_lines(&orphans, 12, false).len(), 5);
        assert_eq!(super::orphan_lines(&orphans, 12, true).len(), 5);
    }

    use super::*;
    use serde_json::json;

    /// A car packet as the jobs API serves it: a branch, a status, and a
    /// review step carrying whatever the operator wrote on it.
    fn car(branch: &str, status: &str, review: &str, review_md: serde_json::Value) -> Value {
        serde_json::json!({
            "id": branch,
            "kind": "ship-a-change",
            "status": status,
            "title": branch,
            "metadata": { "branch": branch },
            "steps": [{
                "spec_slug": "review",
                "title": "Open for review",
                "status": review,
                "metadata": review_md,
            }],
        })
    }

    fn heads(bs: &[&str]) -> std::collections::BTreeSet<String> {
        bs.iter().map(|s| s.to_string()).collect()
    }

    fn landed(branch: &str, md: Value) -> Value {
        let mut m = md;
        m["branch"] = json!(branch);
        json!({
            "status": "open",
            "metadata": m,
            "steps": [
                { "title": "Boarded", "status": "completed" },
                { "title": "Proven in prod", "status": "ready" }
            ]
        })
    }

    #[test]
    fn a_probe_wins_over_an_event_and_a_failed_attempt_is_named() {
        assert_eq!(
            shed_place(&landed(
                "fix/a",
                json!({ "proof_probe": "bash x.sh", "proof_event": "the next red train" })
            )),
            Shed::ProbePending { last: None }
        );
        assert_eq!(
            shed_place(&landed(
                "fix/b",
                json!({ "proof_probe": "bash x.sh", "proof_attempt": { "why": "exit 1: grep found nothing" } })
            )),
            Shed::ProbePending {
                last: Some("exit 1: grep found nothing".into())
            }
        );
        assert_eq!(
            shed_place(&landed(
                "fix/c",
                json!({ "proof_event": "the next red train" })
            )),
            Shed::WaitingOn("the next red train".into())
        );
        assert_eq!(shed_place(&landed("fix/d", json!({}))), Shed::Unproven);
    }

    #[test]
    fn the_shed_lists_only_open_cars_at_proven_and_says_what_each_waits_on() {
        let mut closed = landed("fix/closed", json!({}));
        closed["status"] = json!("closed");
        let mut gating = landed("fix/gating", json!({}));
        gating["steps"] = json!([{ "title": "Gated", "status": "ready" }]);
        let cars = vec![
            landed("fix/event", json!({ "proof_event": "the next red train" })),
            landed("fix/probe", json!({ "proof_probe": "bash x.sh" })),
            landed(
                "fix/failing",
                json!({ "proof_probe": "bash x.sh", "proof_attempt": { "why": "exit 1" } }),
            ),
            landed("fix/forgot", json!({})),
            closed,
            gating,
        ];
        let lines = shed_lines(&cars);
        assert_eq!(lines.len(), 4, "{lines:?}");
        assert_eq!(lines[0], "    fix/event: waiting on: the next red train");
        assert_eq!(
            lines[1],
            "    fix/probe: probe pending (the forge runs it on arrival)"
        );
        assert_eq!(lines[2], "    fix/failing: probe FAILING — exit 1");
        assert!(
            lines[3].starts_with("    fix/forgot: UNPROVEN — no probe, no event"),
            "{}",
            lines[3]
        );
    }

    #[test]
    fn a_paragraph_long_verdict_is_clipped_to_one_line_and_says_so() {
        let long = "x ".repeat(400);
        let lines = shed_lines(&[landed(
            "fix/long",
            json!({ "proof_probe": "bash x.sh", "proof_attempt": { "why": long } }),
        )]);
        let line = &lines[0];
        assert!(line.ends_with('…'), "{line}");
        assert!(
            line.chars().count() < SHED_TEXT_CHARS + 40,
            "{}",
            line.chars().count()
        );
        assert_eq!(clipped("short  and\n spaced"), "short and spaced");
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

    /// A held car is ON the dock and cannot board, so the queue lens
    /// stops listing it (36c3d4ca taught the loading-dock row that a
    /// held car does not board). `boss orient` is the first verb a
    /// session runs, so a car that vanishes from the dock vanishes from
    /// the one read that was meant to un-blind the session. It is named
    /// from the car packets orient has already fetched — no second read,
    /// and the predicate is the shared one (`car::is_parked`) plus the
    /// shared marker reading (`stranded::hold_reason`).
    #[test]
    fn a_held_car_is_named_with_its_reason() {
        let cars = vec![
            car("fix/free", "open", "ready", json!({})),
            car(
                "fix/held",
                "open",
                "ready",
                json!({ "hold": "waiting on an operator action" }),
            ),
        ];
        assert_eq!(
            held_dock_cars(&cars),
            vec![(
                "fix/held".to_string(),
                "waiting on an operator action".to_string()
            )]
        );
    }

    /// THE TRAP. `hold: false` is a RELEASED hold, and a `truthy`/`is_some`
    /// reading of the marker would hold the car in this list forever. The
    /// marker semantics live once, in `stranded::hold_reason`; a
    /// hold/no-hold test alone passes with that bug.
    #[test]
    fn a_released_hold_is_not_a_held_car() {
        for released in [json!({ "hold": false }), json!({ "hold": "" })] {
            let cars = vec![car("fix/released", "open", "ready", released.clone())];
            assert!(
                held_dock_cars(&cars).is_empty(),
                "{released} is a released hold, not a held car"
            );
        }
    }

    /// Only a car still AT the dock can be held there. A boarded car
    /// (review completed, or a train stamp) and a closed one are gone
    /// from the dock, and a hold left on their record is history.
    /// A TROUBLED PACKET MUST LOOK TROUBLED (CLAUDE.md §Diagnosis;
    /// 76d41004). A queued gate-run whose waiter died has no launcher at
    /// all, and until this split it rendered in this verb's QUEUED FOR A
    /// SLOT lane identically to a run waiting its turn — "since
    /// <stamp>", indefinitely. The report must name the packet, how long
    /// nobody has held it, and the recovery that keeps the park intent.
    #[test]
    fn an_abandoned_place_is_reported_as_troubled_with_its_recovery() {
        let lines = abandoned_report(&[AbandonedPlace {
            packet: "fafe8ba4-0000-0000-0000-000000000000".to_string(),
            branch: "fix/two-operator-verbs-stop-lying".to_string(),
            queued_at: "2026-09-10T22:40:00Z".to_string(),
            idle_secs: Some(11 * 60),
            park_intent: true,
        }]);
        let all = lines.join("\n");
        assert!(all.contains("ABANDONED"), "{all}");
        assert!(all.contains("fix/two-operator-verbs-stop-lying"), "{all}");
        assert!(all.contains("fafe8ba4"), "{all}");
        assert!(all.contains("11m"), "how long nobody has held it: {all}");
        assert!(
            all.contains("boss gate fix/two-operator-verbs-stop-lying --wait"),
            "the recovery is a verb the operator can run: {all}"
        );
        assert!(
            all.contains("park intent"),
            "a strand that also owes a car must say so: {all}"
        );
    }

    /// NO STRAND, NO SECTION — and no alarm language for a healthy
    /// queue. An empty report prints nothing: the queue lane above
    /// already says how many places are held.
    #[test]
    fn a_healthy_queue_reports_no_abandoned_section() {
        assert!(abandoned_report(&[]).is_empty());
    }

    /// A RECOVERY LINE THAT NAMES NO BRANCH IS NOT ADVICE. A packet with
    /// no `branch` cannot be re-gated by name, so the strand is still
    /// reported — an operator has to see it — but without a verb that
    /// would run `boss gate  --wait` and fail on an empty argument.
    #[test]
    fn a_place_with_no_branch_is_named_without_a_verb_to_run() {
        let lines = abandoned_report(&[AbandonedPlace {
            packet: "c0ffee00-0000-0000-0000-000000000000".to_string(),
            branch: String::new(),
            queued_at: "2026-09-10T22:40:00Z".to_string(),
            idle_secs: Some(600),
            park_intent: false,
        }]);
        let all = lines.join("\n");
        assert!(
            all.contains("c0ffee00"),
            "the strand is still reported: {all}"
        );
        assert!(!all.contains("recover:"), "no verb without a branch: {all}");
    }

    /// AN UNREADABLE STAMP IS STILL A STRAND. A place whose `queued_at`
    /// does not parse can never be launched (the queue cannot order it),
    /// so it is reported — with its idle time named as unknown rather
    /// than guessed.
    #[test]
    fn an_unreadable_place_is_reported_without_inventing_an_age() {
        let lines = abandoned_report(&[AbandonedPlace {
            packet: "deadbeef-0000-0000-0000-000000000000".to_string(),
            branch: "fix/x".to_string(),
            queued_at: "yesterday".to_string(),
            idle_secs: None,
            park_intent: false,
        }]);
        let all = lines.join("\n");
        assert!(
            all.contains("unreadable") || all.contains("unknown"),
            "{all}"
        );
        assert!(!all.contains("0m"), "never an invented age: {all}");
    }

    #[test]
    fn a_car_that_left_the_dock_is_not_held_on_it() {
        let held = json!({ "hold": "x" });
        let boarded = car("fix/boarded", "open", "completed", held.clone());
        let closed = car("fix/closed", "closed", "ready", held.clone());
        assert!(held_dock_cars(&[boarded, closed]).is_empty());
    }
}
