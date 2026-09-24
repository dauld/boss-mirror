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
use std::collections::{BTreeMap, BTreeSet};

use crate::gate::{AbandonedPlace, abandoned_places, api, queue_order};
// The one rows helper: a read that is not a list refuses rather than
// printing an empty yard (backlog 7b7e0529).
use crate::train::rows;

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

/// One IN TRANSIT line: the train, and the step it stands at with that
/// step's status beside the title. `at_step` alone printed
/// `at: In transit — cluster converged` for a READY step and was read as
/// done (648a68a9); the phrase is the server's (`yard::standing_at`),
/// so this line and the yard cannot disagree. `at_step` itself stays the
/// bare title — the shed and the residue sweep compare it to one.
fn in_transit_line(t: &Value) -> String {
    let title = t.get("title").and_then(Value::as_str).unwrap_or("?");
    let at = t
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|s| {
            let status = s.get("status").and_then(Value::as_str)?;
            matches!(status, "ready" | "active").then(|| {
                let step = s.get("title").and_then(Value::as_str).unwrap_or("?");
                boss_jobs::yard::standing_at(step, status)
            })
        })
        .unwrap_or_else(|| "—".to_string());
    format!("    {title}  at: {at}")
}

/// One GATING line for a running gate-run. A car's run is its branch. A
/// TRAIN's run (`metadata.train_gate`, filed by the conductor for the
/// train branch — design 128b5496) is the train being tested, not a car
/// being gated, and the lane has to say so: on 2026-09-14 it listed
/// `train/20260914-1641` beside a car branch as two indistinguishable
/// runs, and David asked three times why a PR train was in the gates
/// (b96a878f). The predicate is `boss_jobs::stranded::is_train_gate` —
/// the ONE definition, never the branch name (§9a). The title is read
/// off the trains IN TRANSIT already fetched; a train not among them is
/// named by id alone rather than by a second read or an invented title.
fn gating_line(run: &Value, trains: &[Value]) -> String {
    let branch = md_str(run, "branch");
    let metadata = run.get("metadata").unwrap_or(&Value::Null);
    if !boss_jobs::stranded::is_train_gate(metadata) {
        return branch.to_string();
    }
    let train_id = md_str(run, "train");
    let id8: String = train_id.chars().take(8).collect();
    let title = trains
        .iter()
        .find(|t| t.get("id").and_then(Value::as_str) == Some(train_id))
        .and_then(|t| t.get("title").and_then(Value::as_str))
        .filter(|t| !t.is_empty());
    match title {
        Some(title) => format!("{branch}  (train gate — testing train {id8}, {title})"),
        None => format!("{branch}  (train gate — testing train {id8})"),
    }
}

/// One QUEUED FOR A SLOT line for a gate-run holding a place in line:
/// the same words as [`gating_line`] — the ONE definition of how a
/// train's gate is named (§9a) — with the run's `queued_at` stamp kept
/// as it was. Until 90ee6fcd (2026-09-14) this lane printed the bare
/// branch, so a train gate waiting for a bay was indistinguishable from
/// a queued car the same afternoon GATING learned to name it (#365).
fn queued_lane_line(run: &Value, trains: &[Value]) -> String {
    format!(
        "{}  since {}",
        gating_line(run, trains),
        md_str(run, boss_jobs::yard::QUEUED_AT)
    )
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

/// How many consecutive boardings must have refused a car before this
/// verb calls it troubled.
///
/// A SKIP IS ROUTINE AND MUST NOT PRINT (backlog 94896e74). Measured
/// 2026-09-22: 52 of 132 trains (39%) carried a skipped branch and
/// departed anyway, and one branch was skipped 23 consecutive times and
/// landed fine. A line on every skip is a line on a normal occurrence,
/// which is how a surface becomes noise and then becomes unread. Five
/// consecutive refusals is hours of a car not landing while trains keep
/// leaving without it — the repetition the packet is about, not the
/// event.
const TROUBLED_SKIPS: u64 = 5;

/// Cars standing on the dock that boarding has refused over and over —
/// branch, how many consecutive windows refused it, and the reason the
/// last one gave.
///
/// The conductor has always recorded both facts ON the car
/// (`skip_reason`, and `skips` since 94896e74) and cleared them the
/// moment it boards, so this is a read of the packets orient has
/// already fetched — no second call, no copy of the conductor's
/// judgement. Deepest first: the car nobody can land is the one the
/// operator is deciding about.
fn troubled_dock_cars(cars: &[Value]) -> Vec<(String, u64, String)> {
    let mut out: Vec<(String, u64, String)> = cars
        .iter()
        .filter(|c| c.get("status").and_then(Value::as_str) == Some("open"))
        .filter(|c| boss_jobs::car::is_parked(c))
        .filter_map(|c| {
            let skips = c
                .get("metadata")
                .and_then(|m| m.get("skips"))
                .and_then(Value::as_u64)
                .unwrap_or(0);
            (skips >= TROUBLED_SKIPS).then(|| {
                let reason = md_str(c, "skip_reason");
                let reason = if reason.is_empty() {
                    "no reason recorded".to_string()
                } else {
                    reason.to_string()
                };
                (md_str(c, "branch").to_string(), skips, reason)
            })
        })
        .collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

/// Pairs of dock cars that cannot BOTH board — `(branch, branch, files)`,
/// each pair once, branch-sorted.
///
/// THE CHECK EXISTED; THE READER DID NOT (backlog 5c567c27). On
/// 2026-09-20 three cars, each told to stay additive and each green
/// alone, could not board together — two touched WorldMap.svelte — and
/// the pipeline stopped for nine and a half hours with a full dock. The
/// packet asked for a dock-time check naming the pair when the second car
/// parks. The conductor has computed exactly that on every reconcile tick
/// since 12a25f3e (`preview_dock`: pairwise `git merge-tree` across the
/// parked set, onto each car's `metadata.merge_preview.conflicts_with`)
/// and nothing read it. A check nobody reads is a check that is not
/// running, so this is the read — of the packets orient has already
/// fetched, no second call and no copy of the conductor's judgement.
///
/// ONE TICK, BOTH AT THE DOCK. The preview is rewritten only for cars
/// still parked-ready, so a car that boarded or is held keeps the
/// preview of the dock it last saw (measured 2026-09-23: five cars
/// stamped 09:50 still named a branch whose 10:10 preview no longer named
/// them). Two previews pair only when both cars are open and parked
/// (`boss_jobs::car::is_parked`, the dock's shared predicate) and both
/// were measured over the SAME `anchored.parked_set` — one measurement,
/// stale-not-wrong the moment either input moves.
fn unboardable_pairs(cars: &[Value]) -> Vec<(String, String, Vec<String>)> {
    let set_of = |c: &Value| {
        c.pointer("/metadata/merge_preview/anchored/parked_set")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let dock: std::collections::BTreeMap<String, (String, &Value)> = cars
        .iter()
        .filter(|c| c.get("status").and_then(Value::as_str) == Some("open"))
        .filter(|c| boss_jobs::car::is_parked(c))
        .filter_map(|c| Some((md_str(c, "branch").to_string(), (set_of(c)?, c))))
        .collect();
    let mut out: Vec<(String, String, Vec<String>)> = dock
        .iter()
        .flat_map(|(branch, (set, c))| {
            c.pointer("/metadata/merge_preview/conflicts_with")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(move |e| {
                    let other = e.get("branch").and_then(Value::as_str)?;
                    let files: Vec<String> = e
                        .get("files")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect();
                    (branch.as_str() < other).then(|| (branch.clone(), other.to_string(), files))
                })
                .filter(|(_, other, _)| dock.get(other).is_some_and(|(s, _)| s == set))
                .collect::<Vec<_>>()
        })
        .collect();
    out.sort();
    out
}

/// Dock cars that no longer merge onto main — `(branch, files, main)`,
/// branch-sorted, `main` the short sha the verdict was measured against.
///
/// THE OTHER HALF OF THE SAME PREVIEW (backlog 20d0d717). `preview_dock`
/// writes `merge_preview.vs_main` beside `conflicts_with` on every tick,
/// and after 5c567c27 gave the pairs a reader this half still had none:
/// on 2026-09-23 car 5fba0bda carried `vs_main.clean=false` while this
/// verb called it only FRESHNESS-stale — behind main, which a re-gate
/// answers, rather than conflicting with it, which only a rerail does.
///
/// THE PAIRS' DISCIPLINE, FOR ONE CAR. A verdict is read only off an
/// open, parked car whose preview was measured over the dock's CURRENT
/// parked set — the set of the newest preview on the dock, because a
/// change of set rewrites every parked-ready car's preview in one tick.
/// A car carrying an older set was not in the last measurement (held,
/// or left and back), so its verdict is stale, not wrong, and unread.
fn conflicts_with_main(cars: &[Value]) -> Vec<(String, Vec<String>, String)> {
    let preview = |c: &Value, key: &str| {
        c.pointer(&format!("/metadata/merge_preview/{key}"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let dock: Vec<&Value> = cars
        .iter()
        .filter(|c| c.get("status").and_then(Value::as_str) == Some("open"))
        .filter(|c| boss_jobs::car::is_parked(c))
        .collect();
    let Some(current) = dock
        .iter()
        .filter_map(|c| {
            Some((
                preview(c, "checked_at")?,
                preview(c, "anchored/parked_set")?,
            ))
        })
        .max()
        .map(|(_, set)| set)
    else {
        return Vec::new();
    };
    let mut out: Vec<(String, Vec<String>, String)> = dock
        .iter()
        .filter(|c| preview(c, "anchored/parked_set").as_ref() == Some(&current))
        .filter(|c| {
            c.pointer("/metadata/merge_preview/vs_main/clean")
                .and_then(Value::as_bool)
                == Some(false)
        })
        .map(|c| {
            let files = c
                .pointer("/metadata/merge_preview/vs_main/files")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect();
            let main: String = preview(c, "anchored/main")
                .unwrap_or_else(|| "?".to_string())
                .chars()
                .take(8)
                .collect();
            (md_str(c, "branch").to_string(), files, main)
        })
        .collect();
    out.sort();
    out
}

/// The two gate-run reads behind the STRANDED and HELD GREENS lanes,
/// composed here so the test that pins their shape reads the strings
/// the server will.
///
/// A held green is read BY THE HOLD (`metadata_has=hold`, the
/// `metadata ? $n` door), never by its place in a recency page: it is
/// the one state that exists to be seen until a person releases it, and
/// it stays exactly as long as the hold does. On 2026-09-15 the dev-pod
/// car (gate-run 3164b0d5, green on `--hold` since 17:56Z the day
/// before) had 80 gate-runs open behind it and this verb read the newest
/// 60 — the held green was on the record and on no surface (backlog
/// 2fa96d34). The stranded read is windowed in DAYS (`closed_within`,
/// the boards' retention field) for the same reason a count is not a
/// filter; measured 2026-09-15, a week is 415 gate-runs and 4.3 MB, read
/// in 0.2 s. Both pages are bounded by the server's ceiling and judged
/// against `total` — see [`cut_note`].
pub(crate) const GATE_RUN_PAGE: i64 = 1000;
pub(crate) fn held_gate_runs_query() -> String {
    format!("/api/jobs?kind=gate-run&metadata_has=hold&limit={GATE_RUN_PAGE}")
}
/// The week the stranded read is windowed in — and the week the FLAKES
/// line counts over, since it reads the same page (one query, one
/// window, one number in both sentences).
pub(crate) const GATE_RUN_WINDOW_DAYS: i64 = 7;
pub(crate) fn stranded_gate_runs_query() -> String {
    format!("/api/jobs?kind=gate-run&closed_within={GATE_RUN_WINDOW_DAYS}&limit={GATE_RUN_PAGE}")
}

/// The FLAKES section: how many times each check went red then green
/// at one head this week, read off gate-runs the green stamped
/// `flake_of` (`boss_jobs::flake`, backlog 36cc4913). One line either
/// way — the count is the point, and a zero is a stated zero.
pub(crate) fn flakes_line(gate_runs: &[Value]) -> String {
    format!(
        "\n  {}",
        boss_jobs::flake::line(&boss_jobs::flake::tally(gate_runs), GATE_RUN_WINDOW_DAYS)
    )
}

/// `Some("<read> of <total>")` when the record held more rows than the
/// page — the note a lane prints so an empty lane never reads as a fact
/// about the whole record. `None` with no `total`: absence is not a
/// claim either way.
pub(crate) fn cut_note(total: Option<i64>, read: usize) -> Option<String> {
    let total = total?;
    (total > read as i64).then(|| format!("{read} of {total}"))
}

/// Green gate-runs no car claims WITH a hold — `(branch, reason)`,
/// branch-sorted and de-duped. The other half of the stranded predicate
/// (one definition, `boss_jobs::stranded::unparked_green`, through the
/// census's JSON adapter): a stranded green was forgotten, a held green
/// is deliberately waiting, and a lane that shows only the first makes
/// the second invisible.
fn held_greens(gate_runs: &[Value], car_branches: &BTreeSet<String>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = crate::census::unparked_greens(gate_runs, car_branches)
        .into_iter()
        .filter_map(|u| Some((u.branch, u.hold?)))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The lines naming every queued gate-run NO PROCESS HOLDS — empty when
/// there are none.
///
/// A TROUBLED PACKET MUST LOOK TROUBLED (CLAUDE.md §Diagnosis; 76d41004).
/// This verb used to list every queued run in one lane as "since
/// `<stamp>`", so a run whose waiter had been dead for ten minutes —
/// with no Job, and nothing left that would ever launch it — rendered
/// exactly like a run two minutes from its slot. Measured 2026-09-10
/// 22:41–22:51Z; the yard's queued lane still reads the same way.
///
/// Each line carries what the operator's next move needs: the branch,
/// the packet, how long nobody has held it, whether a car is owed, and
/// the verb that recovers it. The recovery carries no `--park-*` flag on
/// purpose — a re-gate REUSES the open packet (`reusable_packet`) and an
/// empty `ParkIntent` stamps nothing, so the `park_*` keys already on
/// the packet survive; only a branch that has LANDED has them cleared
/// (610537b2). Pinned by gate.rs's
/// `a_bare_regate_stamps_nothing_so_a_reused_packets_intent_survives`,
/// because this is advice a tired operator will follow verbatim.
///
/// IT CARRIES `--rebase`, AND A LANDED PLACE GETS NO RECOVERY AT ALL
/// (backlog e9cdd83f, 2026-09-24). The bare verb this line printed was
/// refused for its base — the waiters had died before train #601 moved
/// main — and the hand rebase that followed moved the head, so the next
/// `boss gate` matched no packet and filed two NEW gate-runs with no
/// intent. `--rebase` replays inside the verb after the packet is
/// matched. And orient went on advising a re-gate of both after their
/// branches had landed, which with park intent is the twin-car trap:
/// `landed` names, by packet, the places whose work main already holds
/// ([`superseded_by_main`]), and each is reported as superseded, with
/// what closes it and no verb to run.
fn abandoned_report(places: &[AbandonedPlace], landed: &BTreeMap<String, String>) -> Vec<String> {
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
        if let Some(how) = landed.get(&p.packet) {
            // Closed by the conductor's reap of a gate-run past its Job
            // deadline, measured on opened_at — the one path that closed
            // both of 2026-09-24's (as lost, 04:30Z). Named, not run:
            // this verb is read-only.
            out.push(format!(
                "      LANDED — {how}: superseded, nothing to recover. Do not re-gate it \
                 (with park intent that files a twin car); the conductor settles the packet \
                 as lost once it is {}h old.",
                crate::train::GATE_DEADLINE_HOURS
            ));
        } else if !p.branch.is_empty() {
            out.push(format!(
                "      recover: boss gate {} --wait --rebase  — reuses this packet and keeps \
                 its park intent; --rebase replays onto origin/main INSIDE the verb, after \
                 the packet is matched on the head it queued at (a hand rebase first moves \
                 the head and files a new packet with no intent)",
                p.branch
            ));
        }
    }
    out
}

/// PURE: has main already taken the work an abandoned place queued?
/// The `how` of the landing when it has, `None` otherwise.
///
/// `verdict_for` is `boss merged`'s rules applied to one target (the
/// adapter in [`run`] is `merged::verdict(&merged::observe(..))`), and
/// the words are `gate::landing`'s — the one tested merged-check and the
/// one phrasing, called rather than re-derived (26b3d203). Asked of the
/// QUEUED head first, because a train deletes the branches it lands and
/// a deleted branch reads Unknown by name; then of the branch, because a
/// head that moved on and landed supersedes the one queued. Only a
/// Merged answer supersedes — Unknown is "could not tell", never "no",
/// and leaves the recovery in place, where `boss gate`'s own landed
/// guard still stands between it and a twin.
fn superseded_by_main(
    place: &AbandonedPlace,
    verdict_for: impl Fn(&str) -> crate::merged::Verdict,
) -> Option<String> {
    // `origin/<branch>` is what `resolve_sha` records when the forge did
    // not answer — a name, not a head, so it is not asked about.
    let head =
        (!place.sha.is_empty() && !place.sha.starts_with("origin/")).then_some(place.sha.as_str());
    let branch = (!place.branch.is_empty()).then_some(place.branch.as_str());
    head.into_iter().chain(branch).find_map(|target| {
        crate::gate::landing(&verdict_for(target), &[], &place.branch, &place.sha).map(|l| l.how)
    })
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

/// Where a landed, unproven car stands — `boss_jobs::regions::ShedPlace`,
/// the ONE classification the yard's regions read and this verb share
/// (design 0524fc95). Until 2026-09-19 it lived here alone; the read
/// that bubbles the shed's state up to the map needed the same rule,
/// and a second copy would have been the drift the design exists to
/// end (CLAUDE.md §9a).
pub(crate) use boss_jobs::regions::ShedPlace as Shed;

pub(crate) fn shed_place(car: &Value) -> Shed {
    boss_jobs::regions::shed_place(car.get("metadata").unwrap_or(&Value::Null))
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
/// line each, saying which of the three places it stands in AND which
/// backlog-item it is holding open. Pure so the shape is testable; the
/// caller prints the heading from the count.
///
/// WHY THE ITEM IS ON THE LINE (a7837d81). A car in the shed is not
/// only waiting — it is BLOCKING. `ship-a-change` reaches `merged` off
/// `steps.proven.done`, so an unproven car never closes,
/// `jobs.complete_linked_step` never fires, and the item the car
/// carries stays open for exactly as long as the proof does. Measured
/// 2026-09-19: twelve cars stood here, the oldest landed 58 h earlier,
/// holding ten open backlog-items between them, and two of those items
/// had each already cost a dispatched agent run at high effort
/// re-deriving a fix that was on main (d7fef617, f47861a5). The shed
/// read as a queue of chores; what it was was the residue list, and
/// the item id is what makes that legible without a second read.
pub(crate) fn shed_lines(cars: &[Value]) -> Vec<String> {
    cars.iter()
        .filter(|c| c.get("status").and_then(Value::as_str) == Some("open"))
        .filter(|c| at_step(c) == "Proven in prod")
        .map(|c| {
            let branch = md_str(c, "branch");
            let holds = match c.pointer("/metadata/backlog_item").and_then(Value::as_str) {
                Some(i) => format!("{branch} (holds {})", &i[..i.len().min(8)]),
                None => branch.to_string(),
            };
            format!("    {holds}: {}", shed_text(c))
        })
        .collect()
}

/// What a landed car's proof is waiting on, in one clause — the shed's
/// line and MY WORK's carried row say it in the same words, because
/// they are one fact read twice (bc416f60).
fn shed_text(car: &Value) -> String {
    match shed_place(car) {
        Shed::ProbePending { last: None } => {
            "probe pending (the forge runs it on arrival)".to_string()
        }
        Shed::ProbePending { last: Some(why) } => format!("probe FAILING — {}", clipped(&why)),
        // A streak past the bound is named, not folded into the
        // plain line: a probe that can never pass answers exit 75
        // exactly like a patient one (adef5ddf). A car that
        // DECLARED what it waits on is exempt from the bound, and
        // named instead once its own event is seen (b461341d) —
        // one predicate, the shed's.
        Shed::ProbeNotYet { said } => {
            let md = car.get("metadata").unwrap_or(&Value::Null);
            match boss_jobs::car::starved(md) {
                Some(boss_jobs::car::Starved::Undeclared(s)) => format!(
                    "probe NOT YET for {}h straight ({} runs) — past {}h, \
                     read the probe against the tree: it may never pass — {}",
                    s.hours,
                    s.runs,
                    boss_jobs::regions::NOT_YET_STARVED_HOURS,
                    clipped(&said)
                ),
                Some(boss_jobs::car::Starved::SeenWhileNotYet { on, seen_at }) => format!(
                    "probe NOT YET though what it waits on ({}) was seen \
                     in the record at {seen_at} — read the probe against the tree — {}",
                    clipped(&on),
                    clipped(&said)
                ),
                None => match boss_jobs::car::waits_on(md) {
                    // A declaration nothing observes (e9b164a1):
                    // exempt from the bound AND never contradicted,
                    // so the line names the missing check.
                    Some(w) if w.seen.is_none() => format!(
                        "probe says NOT YET, waiting on {} — no seen check, \
                         so nothing can say it arrived (boss car waits-on --seen) — {}",
                        clipped(&w.on),
                        clipped(&said)
                    ),
                    Some(w) => format!(
                        "probe says NOT YET, waiting on {} — {}",
                        clipped(&w.on),
                        clipped(&said)
                    ),
                    None => format!("probe says NOT YET — {}", clipped(&said)),
                },
            }
        }
        Shed::WaitingOn(ev) => format!("waiting on: {}", clipped(&ev)),
        Shed::Unproven => "UNPROVEN — no probe, no event; nothing mechanical can settle it \
             (boss prove --probe)"
            .to_string(),
    }
}

/// The backlog-items an OPEN car already carries, keyed by item id, each
/// with the clause MY WORK prints in place of the item's age: LANDED
/// (at `Proven in prod` — merged, and what its proof waits on) or IN
/// FLIGHT (the step the car stands at). A landed car wins over an
/// in-flight one for the same item. Closed cars carry nothing: a merged
/// car's close is what completes the item's step (the chain
/// complete-feedback-branch-on-car-merged), so its row is gone anyway.
///
/// WHY (bc416f60). Measured 2026-09-22, working the queue oldest-first:
/// six of the twelve oldest steps on the agent were build steps whose
/// cars had merged three to five days earlier and stood in the shed,
/// their probes honestly answering not-yet on a real-world event. The
/// chain was correct — an item closes when its car proves — but MY
/// WORK drew each one as an unstarted build of its filing age, first in
/// line, and telling them apart cost a git-log grep per packet (nine
/// of them from train #567, 2026-09-23). The car already holds the
/// answer; this is that answer read at the queue, with no new write.
pub(crate) fn carried_items(cars: &[Value]) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for c in cars
        .iter()
        .filter(|c| c.get("status").and_then(Value::as_str) == Some("open"))
    {
        let item = md_str(c, "backlog_item");
        if item.is_empty() {
            continue;
        }
        let branch = md_str(c, "branch");
        let step = at_step(c);
        if step == "Proven in prod" {
            out.insert(
                item.to_string(),
                format!("LANDED (car {branch}), awaiting proof: {}", shed_text(c)),
            );
        } else {
            out.entry(item.to_string())
                .or_insert_with(|| format!("IN FLIGHT (car {branch} at {step})"));
        }
    }
    out
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

/// The forge branches the mirror's pull requests stand on, keyed to
/// their PR: `publish/<date>` is the daily GitHub-mirror snapshot,
/// pushed to the forge FIRST so the push mirror does not prune the PR's
/// head (ce5339d6), and it must stay there until GitHub reports the PR
/// merged or closed. The claim is on the open-pr step of a
/// publish-to-github packet, which records `head = <fork>:<branch>`
/// beside `pr_url`; the forge holds the part after the colon. Read
/// from every packet, not only open ones — a publish packet closes on
/// `pr-opened` the instant the head is recorded, so an open-only read
/// would see no claim at all (01915167: the first orient after PR #239
/// opened listed `publish/2026-09-19` as 'a forge head no packet
/// claims' with the archive-sweep hint). Deleting the branch once the
/// PR is merged stays with the archive sweep, which knows the forge.
pub(crate) fn published_heads(
    publish_packets: &[Value],
) -> std::collections::BTreeMap<String, String> {
    publish_packets
        .iter()
        .flat_map(|p| {
            p.get("steps")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .filter(|s| s.get("spec_slug").and_then(Value::as_str) == Some("open-pr"))
        .filter_map(|s| {
            let head = md_str(s, "head");
            let branch = head.split_once(':').map_or(head, |(_, b)| b);
            (!branch.is_empty()).then(|| (branch.to_string(), md_str(s, "pr_url").to_string()))
        })
        .collect()
}

/// The ORPHANS lane's two halves from one `git ls-remote --heads` read:
/// the heads no packet claims, and the published heads the forge still
/// holds, each with its PR. A published head is claimed — it is never
/// an orphan — and it is drawn only while the forge has it, because
/// the line says what is ON the forge, not what a packet once named.
pub(crate) fn orphans_and_published(
    ls_remote: &str,
    claimed: &BTreeSet<String>,
    published: &std::collections::BTreeMap<String, String>,
) -> (Vec<String>, Vec<(String, String)>) {
    let mut all_claimed = claimed.clone();
    all_claimed.extend(published.keys().cloned());
    let orphans = crate::census::orphan_branches(ls_remote, &all_claimed);
    let on_forge = ls_remote
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .filter_map(|r| r.strip_prefix("refs/heads/"))
        .filter_map(|b| published.get(b).map(|pr| (b.to_string(), pr.clone())))
        .collect();
    (orphans, on_forge)
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

// ---- MY WORK ----------------------------------------------------------
//
// THE CASE (65a89769). Measured 2026-09-18 03:10Z: 25 ready steps sat
// on `claude@algedonic.dev` — the four daily sweep inspections, the
// publish-to-github measure, a user-feedback triage the founder did
// himself, cadence-silent and estate-alarm triages, every landed car's
// proven step — nominated by the dispatcher and never read, because the
// agent's standing order reads the backlog station and this verb
// printed everything about the pipeline except the actor's own queue.
// The section below is that queue: the ready + active steps assigned to
// the actor this process signs as, or to any id the agents registry
// ties to it. Two identities, because the dispatcher nominates to the
// ALIAS while a box may be named by the agent's id (25 on the alias, 0
// on `agent-claude`, the same morning).

/// How many characters of a title a MY WORK line shows.
pub(crate) const MY_WORK_TITLE_CHARS: usize = 60;

/// The identities one MY WORK read asks for: the caller first, then
/// every id the agents registry ties to it — the aliases when the
/// caller is an agent's id, the id and sibling aliases when the caller
/// IS an alias. A caller the registry does not know is read alone.
pub(crate) fn my_work_identities(caller: &str, agents: &[Value]) -> Vec<String> {
    let mut out = vec![caller.to_string()];
    for agent in agents {
        let id = agent.get("id").and_then(Value::as_str).unwrap_or_default();
        let aliases: Vec<&str> = agent
            .get("aliases")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        if id != caller && !aliases.contains(&caller) {
            continue;
        }
        for candidate in std::iter::once(id).chain(aliases) {
            if !candidate.is_empty() && !out.iter().any(|o| o == candidate) {
                out.push(candidate.to_string());
            }
        }
    }
    out
}

/// What a step of this shape IS, for an actor deciding what to do
/// with it — one clause per (workflow, slug) the section knows. A
/// slug it does not know gets no hint rather than a wrong one.
fn my_work_hint(workflow: &str, slug: &str) -> Option<String> {
    let text = match (workflow, slug) {
        (_, "triage") => "a decision the agent makes: measure the claim, choose a route",
        ("ship-a-change", "proven") => {
            "a probe to run: the forge runs it via run-car-probe (boss prove --recheck re-runs it)"
        }
        ("maintenance-sweep", "inspect") => {
            "a measurement to record: findings + measured on the step, action_needed on the job"
        }
        ("user-feedback", "design-review") => {
            "file the design that answers it: boss design ... --answers <feedback id>"
        }
        (_, "build") => "a change to build: branch, gate, park (boss brief <packet>)",
        (_, "measure") => "a measurement to record on the checklist",
        _ => return None,
    };
    Some(format!("{slug} = {text}"))
}

/// The MY WORK listing lines: rows deduplicated by step id (one step
/// answers once however many identities it was read under), grouped
/// by workflow, groups and rows both oldest first, each group headed
/// by its count and closed by one hint line. Pure so the shape is
/// testable; the caller prints the section heading from the count.
///
/// A row whose packet a car already carries (`carried`, from
/// `carried_items`) prints what the car says in place of its age, and
/// sorts after its group's unstarted rows: oldest-first is a rule for
/// choosing work to START, and a carried item is not that (bc416f60).
pub(crate) fn my_work_lines(
    rows: &[Value],
    carried: &std::collections::BTreeMap<String, String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    let carried_by = |r: &Value| {
        r.get("job_id")
            .and_then(Value::as_str)
            .and_then(|j| carried.get(j))
    };
    let mut seen = BTreeSet::new();
    let mut keyed: Vec<(Option<chrono::NaiveDate>, &Value)> = rows
        .iter()
        .filter(|r| {
            let step_id = r
                .pointer("/step/id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            seen.insert(step_id)
        })
        .map(|r| {
            let opened = r
                .get("opened_on")
                .and_then(Value::as_str)
                .and_then(|d| d.parse::<chrono::NaiveDate>().ok());
            (opened, r)
        })
        .collect();
    // Unknown ages sort last: an older server's row is still listed,
    // never mistaken for today's.
    keyed.sort_by_key(|(opened, r)| {
        (
            carried_by(r).is_some(),
            opened.is_none(),
            *opened,
            r.get("job_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        )
    });
    // Groups in order of their oldest row.
    let mut groups: Vec<(&str, Vec<(Option<chrono::NaiveDate>, &Value)>)> = Vec::new();
    for (opened, r) in keyed {
        let kind = r.get("workflow").and_then(Value::as_str).unwrap_or("?");
        match groups.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, rows)) => rows.push((opened, r)),
            None => groups.push((kind, vec![(opened, r)])),
        }
    }
    let mut out = Vec::new();
    for (kind, rows) in groups {
        let held = rows.iter().filter(|(_, r)| carried_by(r).is_some()).count();
        out.push(match held {
            0 => format!("    {kind} — {}", rows.len()),
            n => format!("    {kind} — {} ({n} already carried by a car)", rows.len()),
        });
        let mut slugs: Vec<&str> = Vec::new();
        for (opened, r) in rows {
            let job = r.get("job_id").and_then(Value::as_str).unwrap_or("");
            let id8 = &job[..8.min(job.len())];
            let slug = r
                .pointer("/step/spec_slug")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let title: String = r
                .get("job_title")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .chars()
                .take(MY_WORK_TITLE_CHARS)
                .collect();
            // A carried row's hint is the carried one: "build = a change
            // to build" is exactly the misreading this line exists to stop.
            if let Some(state) = carried_by(r) {
                out.push(format!(
                    "      {id8} {kind} {slug} {} — {state}",
                    title.trim_end()
                ));
                continue;
            }
            if !slugs.contains(&slug) {
                slugs.push(slug);
            }
            let age = match opened {
                Some(d) => format!("{}d", crate::census::age_days(d, now)),
                None => "age ?".to_string(),
            };
            out.push(format!(
                "      {id8} {kind} {slug} {} ({age})",
                title.trim_end()
            ));
        }
        let mut hints: Vec<String> = slugs.iter().filter_map(|s| my_work_hint(kind, s)).collect();
        if held > 0 {
            hints.push(
                "carried = its car holds the work; the item closes itself when that car \
                 proves — do not build it again"
                    .to_string(),
            );
        }
        if !hints.is_empty() {
            out.push(format!("      → {}", hints.join("; ")));
        }
    }
    out
}

/// The whole section as printed. `None` for the identities is the
/// unnamed caller: the read is REFUSED rather than made under
/// `operator:unidentified`, which would answer 0 and read as an empty
/// queue — the wrong-target trap (CLAUDE.md §Doors), with the two ways
/// to name yourself on the line.
pub(crate) fn my_work_section(
    identities: Option<&[String]>,
    rows: &[Value],
    cars: &[Value],
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    let Some(ids) = identities else {
        return vec![format!(
            "  MY WORK — REFUSED: nothing names the actor running this command, so its own \
             queue cannot be read (an unidentified read answers 0, not an error). Name \
             yourself with `export {}=<your id>` or by writing that id into {}.",
            crate::identity::ACTOR_ENV,
            crate::identity::actor_file_display()
        )];
    };
    let who = match ids {
        [] => "nobody".to_string(),
        [one] => one.clone(),
        [first, rest @ ..] => format!("{first} (+ {})", rest.join(", ")),
    };
    let carried = carried_items(cars);
    let lines = my_work_lines(rows, &carried, now);
    if lines.is_empty() {
        return vec![format!(
            "  MY WORK — nothing: no ready/active step is assigned to {who}"
        )];
    }
    let count = lines
        .iter()
        .filter(|l| l.starts_with("      ") && !l.starts_with("      →"))
        .count();
    // Counted by step id, as the lines are: one step read under two
    // identities is one step.
    let held = rows
        .iter()
        .filter(|r| {
            r.get("job_id")
                .and_then(Value::as_str)
                .is_some_and(|j| carried.contains_key(j))
        })
        .filter_map(|r| r.pointer("/step/id").and_then(Value::as_str))
        .collect::<BTreeSet<_>>()
        .len();
    let tail = match held {
        0 => String::new(),
        n => format!("; {n} already carried by a car, listed last"),
    };
    let mut out = vec![format!(
        "  MY WORK — {count} ready/active step(s) assigned to {who} — yours to move, oldest first{tail}"
    )];
    out.extend(lines);
    out
}

/// The REGIONS header — the IT system map's KPI cards, one line
/// each, from `GET /api/yard/regions` (design 0524fc95, car 1). The
/// server owns these numbers now: the count, the clear/busy/troubled
/// state and the trend are ONE definition in `boss_jobs::regions`, the
/// same one the map reads, so this verb and the yard cannot disagree
/// about how many trains are in transit or what the time at CI is. The
/// lanes below still list WHAT is there — the header says how much and
/// whether it is trouble.
///
/// Pure over the payload, so the shape is testable; a state other than
/// `clear` is printed upper-case so trouble reads as trouble. The
/// `window_hours` the server answered rides the heading, because a
/// rate without its window is not a number.
pub(crate) fn region_lines(map: &Value) -> Vec<String> {
    let hours = map.get("window_hours").and_then(Value::as_i64).unwrap_or(0);
    let mut out = vec![format!(
        "  REGIONS — the IT system map over the last {hours}h (count · state · trend)"
    )];
    let regions = map
        .get("regions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for r in &regions {
        let name = r.get("name").and_then(Value::as_str).unwrap_or("?");
        let count = match r.get("count") {
            Some(Value::Number(n)) => n.to_string(),
            _ => "?".to_string(),
        };
        let bound = r
            .get("bound")
            .and_then(Value::as_i64)
            .map(|b| format!(" of {b}"))
            .unwrap_or_default();
        let state = r.get("state").and_then(Value::as_str).unwrap_or("?");
        let state = if state == "clear" {
            state.to_string()
        } else {
            state.to_uppercase()
        };
        let why = r.get("why").and_then(Value::as_str).unwrap_or("");
        out.push(format!(
            "    {name:<12} {count:>5}{bound:<6} {state:<9} {}  — {why}",
            trend_text(r.get("trend").unwrap_or(&Value::Null))
        ));
    }
    out
}

/// `dock wait 1.5h (was 2.0h)` / `arrivals 17/day (was 12/day)` /
/// `time at CI — (was —)`: the trend as a phrase, with a null half
/// printed as `—` rather than as a zero.
fn trend_text(t: &Value) -> String {
    let metric = t.get("metric").and_then(Value::as_str).unwrap_or("?");
    let unit = t.get("unit").and_then(Value::as_str).unwrap_or("");
    let one = |key: &str| -> String {
        match t.get(key).and_then(Value::as_f64) {
            None => "—".to_string(),
            Some(v) => match unit {
                "hours" => format!("{v:.1}h"),
                "minutes" => format!("{v:.0}m"),
                "per day" => format!("{v:.1}/day"),
                _ => format!("{v:.1} {unit}"),
            },
        }
    };
    format!("{metric} {} (was {})", one("current"), one("previous"))
}

/// The BORDERS header — one line per border of the IT world map, from
/// `GET /api/yard/borders` (design d2154293, car 2). The regions above
/// say how much is in each place; these say what MOVES between them:
/// the crossing rate this window against the last, how many packets
/// stand at the border now, and the machine that moves them with how
/// long it has been silent.
///
/// Pure over the payload, so the shape is testable. Every unknown
/// prints as `?` or `—` — a border whose flow could not be computed
/// must not read as a quiet one, which on a CLI is exactly what a 0
/// would say.
pub(crate) fn border_lines(map: &Value) -> Vec<String> {
    let hours = map.get("window_hours").and_then(Value::as_i64).unwrap_or(0);
    let mut out = vec![format!(
        "  BORDERS — what moves between the regions over the last {hours}h (rate · waiting · machine)"
    )];
    let borders = map
        .get("borders")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for b in &borders {
        let from = b.get("from").and_then(Value::as_str).unwrap_or("?");
        let to = b.get("to").and_then(Value::as_str).unwrap_or("?");
        let hop = format!("{from} -> {to}");
        let state = b.get("state").and_then(Value::as_str).unwrap_or("?");
        let state = if state == "clear" {
            state.to_string()
        } else {
            state.to_uppercase()
        };
        // A waiting count the server could not take is `?`, never 0.
        let waiting = match b.get("waiting") {
            Some(Value::Number(n)) => format!("{n} waiting"),
            _ => "? waiting".to_string(),
        };
        let why = b.get("why").and_then(Value::as_str).unwrap_or("");
        out.push(format!(
            "    {hop:<26} {:<24} {waiting:<12} {state:<9} {}  — {why}",
            rate_text(b.get("rate").unwrap_or(&Value::Null)),
            machine_text(b.get("machine").unwrap_or(&Value::Null)),
        ));
    }
    out
}

/// `3.0/day (was 5.0/day)`, with a half nobody measured as `—`.
fn rate_text(t: &Value) -> String {
    let one = |key: &str| -> String {
        match t.get(key).and_then(Value::as_f64) {
            None => "—".to_string(),
            Some(v) => format!("{v:.1}/day"),
        }
    };
    format!("{} (was {})", one("current"), one("previous"))
}

/// `train-reconcile 4m ago` — and `SILENT 180m` when the machine has
/// been quiet past its own declared cadence. A machine with no firing
/// record says so rather than printing an age it does not have.
///
/// EXCEPT WHERE THERE IS NO MACHINE. A border of kind `actors` is
/// crossed by a person or an agent, so there is no rule to fire and
/// "(no firing recorded)" is not a finding — it is the only sentence
/// that could ever be true there, and it reads exactly like a dead
/// automation. It sat on `receiving -> marshalling`, the most
/// backed-up border in the yard, which is the pairing most likely to
/// send a reader hunting for a rule that does not exist (beec1130).
/// §Diagnosis says a troubled packet must look troubled; the inverse
/// holds too, or the phrase stops carrying weight on the borders where
/// it IS a finding.
///
/// The `kind` is the one definition this and the rail both read —
/// `MachineKind` in boss-jobs/src/borders.rs — so the two surfaces
/// branch on the same fact rather than on a phrase copied between
/// them.
fn machine_text(m: &Value) -> String {
    let name = m.get("name").and_then(Value::as_str).unwrap_or("?");
    if m.get("kind").and_then(Value::as_str) == Some("actors") {
        return format!("{name} (worked by actors)");
    }
    let silent = m.get("silent").and_then(Value::as_bool);
    let age = m.get("silent_for_minutes").and_then(Value::as_i64);
    match (silent, age) {
        (Some(true), Some(mins)) => format!("{name} SILENT {mins}m"),
        (_, Some(mins)) => format!("{name} {mins}m ago"),
        _ => format!("{name} (no firing recorded)"),
    }
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
    let built = crate::built_from::built_from();
    let main = crate::built_from::origin_main_head();
    let ancestry = main
        .as_deref()
        .map(|m| crate::built_from::ancestry(built, m))
        .unwrap_or(crate::built_from::Ancestry::Unknown);
    println!(
        "{}",
        crate::built_from::freshness_line(built, main.as_deref(), ancestry)
    );

    // THE MEMORY INDEX — the session's other instrument, measured
    // against a declared budget (4ea1b28c). It sits here, beside the
    // binary's own freshness and above every lane, because it is a
    // statement about what this session was HANDED rather than about the
    // yard: an agent reading a truncated index does not know it is
    // reading one, and the harness that cut it says nothing. Loud when
    // over, quiet-but-numbered when under, and never fatal.
    for line in crate::memory_index::report() {
        println!("{line}");
    }

    // REPORTING TO — who reads this session's reports, their company
    // address and their timezone, from the people and locations
    // registries rather than from agent memory (backlog 2de32950). Beside
    // the memory index for the same reason: it is about what this
    // session is handed, and it prints before any lane can fail.
    for line in crate::reporting_to::section(&http).await {
        println!("{line}");
    }

    // THE REGIONS — the map's numbers, from the server's one definition
    // (design 0524fc95). A server without the read (older than this
    // verb) says so and the approach still prints; the lanes below are
    // unchanged and still list what is there.
    println!();
    match api(&http, reqwest::Method::GET, "/api/yard/regions", None).await {
        Ok(Some(map)) => {
            for line in region_lines(&map) {
                println!("{line}");
            }
        }
        Ok(None) => println!("  REGIONS — unavailable: the read answered nothing"),
        Err(e) => println!("  REGIONS — unavailable: {e}"),
    }

    // THE BORDERS — what moves BETWEEN those regions (design d2154293):
    // the crossing rate, the queue standing at each border, and the
    // machine that moves it. An older server without the read says so
    // and the approach still prints.
    println!();
    match api(&http, reqwest::Method::GET, "/api/yard/borders", None).await {
        Ok(Some(map)) => {
            for line in border_lines(&map) {
                println!("{line}");
            }
        }
        Ok(None) => println!("  BORDERS — unavailable: the read answered nothing"),
        Err(e) => println!("  BORDERS — unavailable: {e}"),
    }

    // Trains in transit.
    let trains = rows(
        api(
            &http,
            reqwest::Method::GET,
            "/api/jobs?kind=pr-train&status=open&limit=10",
            None,
        )
        .await?,
    )?;
    println!("\n  IN TRANSIT — {} train(s)", trains.len());
    for t in &trains {
        println!("{}", in_transit_line(t));
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
    )?;
    // A QUEUED run is not gating: it is waiting for a slot and has no
    // Job at all (boss_jobs::yard::QUEUED_AT). Reporting it as GATING
    // would overstate what the node is doing by exactly the number of
    // builders standing in line.
    let (waiting, running): (Vec<&Value>, Vec<&Value>) = gating
        .iter()
        .partition(|g| !md_str(g, boss_jobs::yard::QUEUED_AT).is_empty());
    println!("\n  GATING — {} run(s)", running.len());
    for g in &running {
        println!("    {}", gating_line(g, &trains));
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
            println!("    {}", queued_lane_line(g, &trains));
        }
    }
    // Which of them main already holds — asked only when there is a
    // strand, after the same quiet fetch of main `boss gate`'s landed
    // guard makes, so the content comparison has main's objects. Every
    // probe may fail, and a failure is Unknown: the place keeps its
    // recovery rather than being called landed (backlog e9cdd83f).
    let landed: BTreeMap<String, String> = if abandoned.is_empty() {
        BTreeMap::new()
    } else {
        let _ = crate::git_auth::command()
            .args(["fetch", "--quiet", "origin", "main"])
            .status();
        abandoned
            .iter()
            .filter_map(|p| {
                superseded_by_main(p, |target| {
                    crate::merged::verdict(&crate::merged::observe(".", "origin", target))
                })
                .map(|how| (p.packet.clone(), how))
            })
            .collect()
    };
    for line in abandoned_report(&abandoned, &landed) {
        println!("{line}");
    }

    // Stranded greens: gated, never parked — the census cross-ref, not a
    // second definition (§9a). A gate-run CLOSES on its verdict, so this
    // reads closed ones; a status=open query cannot see them. Windowed
    // in DAYS, not by count, and the page judged against `total` — see
    // [`stranded_gate_runs_query`] for the held green a count hid.
    let stranded_body = api(
        &http,
        reqwest::Method::GET,
        &stranded_gate_runs_query(),
        None,
    )
    .await?;
    let stranded_total = stranded_body
        .as_ref()
        .and_then(|b| b.get("total"))
        .and_then(Value::as_i64);
    let gate_runs = rows(stranded_body)?;
    let stranded_cut = cut_note(stranded_total, gate_runs.len());
    // Held greens: read BY THE HOLD, so a hold is seen for as long as it
    // stands, whatever gated after it.
    let held_body = api(&http, reqwest::Method::GET, &held_gate_runs_query(), None).await?;
    let held_total = held_body
        .as_ref()
        .and_then(|b| b.get("total"))
        .and_then(Value::as_i64);
    let held_runs = rows(held_body)?;
    let held_cut = cut_note(held_total, held_runs.len());
    // Every car, paged on `total` (backlog 10776b6c): the shed, the
    // dock's held and troubled lanes and the stranded cross-ref all read
    // this list, and one bare `limit=800` page would have dropped the
    // oldest landed cars out of all four silently once the yard passed
    // 800.
    let cars = crate::gate::all_cars(&http).await?;
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
    if let Some(cut) = &stranded_cut {
        println!("    (read {cut} gate-runs in the week — the rest were not cross-referenced)");
    }

    // Held greens: gated green, no car, and an operator's hold — the
    // deliberate half of the stranded predicate. A brake on is not an
    // alarm; an invisible brake is (2fa96d34).
    let held_greens = held_greens(&held_runs, &car_branches);
    if held_greens.is_empty() {
        println!("\n  HELD GREENS — none: no green gate is held before parking");
    } else {
        println!(
            "\n  HELD GREENS — {} green gate(s) held off the dock on purpose (release = \
             re-gate with the --park-* intent; never rebuild one blind):",
            held_greens.len()
        );
        for (branch, reason) in &held_greens {
            println!("    {branch}  —  {reason}");
        }
    }
    if let Some(cut) = &held_cut {
        println!("    (read {cut} held gate-runs — the rest were not cross-referenced)");
    }

    // Flakes: reds that went green at the same head, by check — read
    // off the week of gate-runs already fetched above, so the count
    // costs no second read and names the same window (36cc4913).
    println!("{}", flakes_line(&gate_runs));
    if let Some(cut) = &stranded_cut {
        println!("    (read {cut} gate-runs in the week — the rest were not counted)");
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
            .chain(held_runs.iter())
            .chain(gating.iter())
            .map(|g| md_str(g, "branch").to_string())
            .filter(|b| !b.is_empty()),
    );
    // The mirror's publish branches are claimed by publish-to-github
    // packets, whose open-pr step records the head — see
    // [`published_heads`] for why the read is not status=open.
    let published = published_heads(&rows(
        api(
            &http,
            reqwest::Method::GET,
            "/api/jobs?kind=publish-to-github&limit=200",
            None,
        )
        .await?,
    )?);
    match crate::git_auth::command()
        .args(["ls-remote", "--heads", "origin"])
        .output()
    {
        Ok(out) if out.status.success() => {
            let (orphans, on_forge) =
                orphans_and_published(&String::from_utf8_lossy(&out.stdout), &claimed, &published);
            if !on_forge.is_empty() {
                println!(
                    "\n  PUBLISHED — {} forge head(s) backing a mirror pull request (stays until \
                     GitHub reports the PR merged or closed; the archive sweep deletes it):",
                    on_forge.len()
                );
                for (branch, pr_url) in &on_forge {
                    println!("    {branch}  {pr_url}");
                }
            }
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
                // A car that landed before a database switch is recorded
                // in the ARCHIVE, where the arrival sweep never looks, so
                // its branch stays an orphan forever (dfd83788: 50 of them
                // on 2026-09-17). The door that reads the archive's
                // records and applies the sweep's own rule is a forge
                // verb; name it here, or the operator deletes by hand.
                println!(
                    "    landed before a database switch? the archive decides: \
                     boss ops forge sweep-archive-branches --wait -- --dry-run boss <archive-db>"
                );
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
    // Read once, and refused if it is not a list: "0 car(s) parked"
    // from a dark door is the empty-yard answer (7b7e0529).
    let dock = rows(dock)?;
    println!("\n  DOCK — {dock_total} car(s) parked");
    for c in &dock {
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

    // SKIPPED REPEATEDLY — a car the assembled tree keeps refusing.
    // Nothing prints for a healthy dock: one skip is routine and a line
    // on every one of them is noise on a normal occurrence. What has no
    // surface at all is REPETITION, and that is what cost seven hours on
    // 2026-09-22 (backlog 94896e74) — the conductor named the conflict
    // every window and no read an operator runs carried the count.
    let troubled = troubled_dock_cars(&cars);
    if !troubled.is_empty() {
        println!(
            "  SKIPPED REPEATEDLY — {} car(s) refused {TROUBLED_SKIPS}+ consecutive \
             boardings (repair: boss rerail <car>, which stops for you on a real conflict):",
            troubled.len()
        );
        for (branch, skips, reason) in &troubled {
            println!("    {branch}  —  skipped {skips}x  —  {reason}");
        }
    }

    // CANNOT BOTH BOARD — the conductor's merge preview, read (5c567c27).
    // Each car of a pair merges clean onto main alone; together one is
    // left at assembly. Named at the first tick after the second parks,
    // not nine hours later at the boarding that discovers it.
    let pairs = unboardable_pairs(&cars);
    if !pairs.is_empty() {
        println!(
            "  CANNOT BOTH BOARD — {} pair(s) of dock cars that conflict with each other \
             (land one, then boss rerail the other onto the main it landed on; rerailing \
             both at once recreates the conflict):",
            pairs.len()
        );
        for (a, b, files) in &pairs {
            println!("    {a}  x  {b}  —  {}", files.join(", "));
        }
    }

    // CONFLICTS WITH MAIN — the preview's other half, read (20d0d717).
    // Not FRESHNESS: a car behind main is repaired by a re-gate, a car
    // that conflicts with it only by a rerail.
    let off_main = conflicts_with_main(&cars);
    if !off_main.is_empty() {
        println!(
            "  CONFLICTS WITH MAIN — {} dock car(s) that no longer merge onto main \
             (repair: boss rerail <car>, which stops for you on a real conflict):",
            off_main.len()
        );
        for (branch, files, main) in &off_main {
            println!("    {branch}  —  {}  (as of main@{main})", files.join(", "));
        }
    }

    // MY WORK — the actor's own queue, after the dock (65a89769). One
    // read of the agents registry for the aliases, one assignments read
    // per identity; a caller nobody named is refused here and the rest
    // of the approach still prints (identity's read/write split).
    let identities = match crate::identity::caller() {
        Some(c) => {
            let agents = rows(api(&http, reqwest::Method::GET, "/api/agents", None).await?)?;
            Some(my_work_identities(&c.id, &agents))
        }
        None => None,
    };
    let mut my_rows: Vec<Value> = Vec::new();
    for id in identities.iter().flatten() {
        my_rows.extend(rows(
            api(
                &http,
                reqwest::Method::GET,
                &format!("/api/jobs/assignments?assignee_id={id}&limit=1000"),
                None,
            )
            .await?,
        )?);
    }
    println!();
    for line in my_work_section(identities.as_deref(), &my_rows, &cars, now) {
        println!("{line}");
    }

    // FRESHNESS (L2, acedf981) — parked and stranded branches whose
    // base has fallen behind origin/main. A car cut from an old main
    // merges clean and reverts the trains, so a green gate on a stale
    // base is a re-gate, not a boarding. Needs the objects (merge-base),
    // so it fetches; like ORPHANS, a failed read prints WHY and skips
    // rather than failing the verb.
    let mut fresh_targets: Vec<String> = dock
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
    // A held GREEN too: it has no car yet, but the same hold keeps its
    // base falling behind, and releasing it is a re-gate.
    fresh_targets.extend(held_greens.iter().map(|(b, _)| b.clone()));
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

    // BUNDLES (5449111c) — a versioned bundle file the live lineage has
    // moved past. An author bumps the version FROM THE FILE, so a file
    // behind live turns "bump the version" into a collision the seed
    // refuses — and the gate, which reads only files, stays green. Said
    // here, at session start, before anyone bumps; the judgement is the
    // boot seed's own decision table, run dry against the live rows.
    for line in crate::bundle_lineage::bundles_section(&http).await {
        println!("{line}");
    }

    // THE SHED — landed cars not yet proven, and what each waits on.
    let shed = shed_lines(&cars);
    if shed.is_empty() {
        println!("\n  SHED — empty: every landed car is proven");
    } else {
        println!(
            "\n  SHED — {} landed car(s) awaiting proof (probe pending / waiting on an event / \
             UNPROVEN). Each one it names an item for is HOLDING that item open until it \
             proves — that is where the queue's inflated open count comes from (a7837d81):",
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
    /// `IN TRANSIT` printed `at: In transit — cluster converged` while
    /// that step was READY (train 8b365d83, 2026-09-22), and was read as
    /// "the cluster has converged" mid-incident (648a68a9). The line
    /// carries the step's status beside its title, in the server's own
    /// words (`boss_jobs::yard::standing_at`), so the terminal and the
    /// yard say the same thing.
    #[test]
    fn a_train_in_transit_names_the_status_of_the_step_it_stands_at() {
        use serde_json::json;
        let train = json!({
            "title": "PR train 2026-09-23 07:01",
            "steps": [
                {"title": "Yard inspection — CI verdict", "status": "completed"},
                {"title": "DEPARTED — merged into main", "status": "ready"},
                {"title": "Train arrived", "status": "pending"},
            ],
        });
        assert_eq!(
            super::in_transit_line(&train),
            "    PR train 2026-09-23 07:01  at: DEPARTED — merged into main (ready, not yet done)"
        );
        let nowhere = json!({"title": "PR train x", "steps": []});
        assert_eq!(super::in_transit_line(&nowhere), "    PR train x  at: —");
    }

    /// A TRAIN's gate-run (128b5496) in the GATING lane is the train being
    /// tested, not a car being gated. On 2026-09-14 `boss orient` listed
    /// `train/20260914-1641` and a car branch as two indistinguishable
    /// runs, the afternoon David asked three times why a PR train was in
    /// the gates (b96a878f). The yard learned to say so in three lanes
    /// that day; the terminal says the same thing in the same words — the
    /// predicate is `boss_jobs::stranded::is_train_gate`, never the
    /// branch name, and the title comes from the trains IN TRANSIT
    /// already fetched, so no extra read. A car's line does not change.
    #[test]
    fn a_train_gate_in_the_gating_lane_is_named_as_the_trains_test() {
        use serde_json::json;
        let trains = vec![json!({
            "id": "9a3af298-0000-4000-8000-000000000000",
            "title": "PR train 2026-09-14 16:41",
        })];
        let car =
            json!({"metadata": {"branch": "fix/a-dark-registry-does-not-widen-what-a-host-runs"}});
        let train_gate = json!({"metadata": {
            "branch": "train/20260914-1641",
            "train_gate": true,
            "train": "9a3af298-0000-4000-8000-000000000000",
        }});
        assert_eq!(
            super::gating_line(&car, &trains),
            "fix/a-dark-registry-does-not-widen-what-a-host-runs",
            "a car's line is its branch, as before"
        );
        assert_eq!(
            super::gating_line(&train_gate, &trains),
            "train/20260914-1641  (train gate — testing train 9a3af298, PR train 2026-09-14 16:41)"
        );
        // The QUEUED FOR A SLOT lane says the same thing in the same
        // words, with its place-in-line stamp kept: until 90ee6fcd a
        // train gate waiting for a bay printed as a bare branch there,
        // indistinguishable from a queued car, while GATING named it.
        let queued_train_gate = json!({"metadata": {
            "branch": "train/20260914-1641",
            "train_gate": true,
            "train": "9a3af298-0000-4000-8000-000000000000",
            boss_jobs::yard::QUEUED_AT: "2026-09-14T20:01:00Z",
        }});
        assert_eq!(
            super::queued_lane_line(&queued_train_gate, &trains),
            "train/20260914-1641  (train gate — testing train 9a3af298, PR train 2026-09-14 16:41)  since 2026-09-14T20:01:00Z"
        );
        let queued_car = json!({"metadata": {
            "branch": "fix/a-dark-registry-does-not-widen-what-a-host-runs",
            boss_jobs::yard::QUEUED_AT: "2026-09-14T20:02:00Z",
        }});
        assert_eq!(
            super::queued_lane_line(&queued_car, &trains),
            "fix/a-dark-registry-does-not-widen-what-a-host-runs  since 2026-09-14T20:02:00Z",
            "a queued car's line is its branch and its stamp, as before"
        );
    }

    /// The IN TRANSIT fetch is capped at ten trains; a train gate whose
    /// train is not among them still says WHICH train, by id, and does
    /// not invent a title. And a `train/…` branch WITHOUT the flag is a
    /// car (the predicate is the metadata, not the name).
    #[test]
    fn a_train_gate_whose_train_was_not_fetched_names_the_id_alone() {
        use serde_json::json;
        let train_gate = json!({"metadata": {
            "branch": "train/20260914-1641",
            "train_gate": true,
            "train": "9a3af298-0000-4000-8000-000000000000",
        }});
        assert_eq!(
            super::gating_line(&train_gate, &[]),
            "train/20260914-1641  (train gate — testing train 9a3af298)"
        );
        let named_like_a_train = json!({"metadata": {"branch": "train/20260914-1641"}});
        assert_eq!(
            super::gating_line(&named_like_a_train, &[]),
            "train/20260914-1641",
            "the branch name is not the predicate"
        );
    }

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

    /// A publish packet's open-pr step records the mirror PR's head as
    /// `<fork owner>:<branch>` beside the pr_url; the forge holds the
    /// branch under its bare name. A skipped open-pr (declined,
    /// superseded, held) names no head and claims nothing.
    #[test]
    fn a_publish_packet_claims_the_branch_after_the_colon() {
        use serde_json::json;
        let opened = json!({"status": "closed", "steps": [{
            "spec_slug": "open-pr", "status": "completed",
            "metadata": {"head": "dauld:publish/2026-09-19",
                         "pr_url": "https://github.com/algedonic-dev/boss/pull/239"}}]});
        let skipped = json!({"status": "closed", "steps": [{
            "spec_slug": "open-pr", "status": "skipped", "metadata": {}}]});
        let heads = super::published_heads(&[opened, skipped]);
        assert_eq!(
            heads.into_iter().collect::<Vec<_>>(),
            vec![(
                "publish/2026-09-19".to_string(),
                "https://github.com/algedonic-dev/boss/pull/239".to_string()
            )]
        );
    }

    /// One car branch, one publish branch, one true orphan: the car is
    /// claimed, the publish head is drawn under PUBLISHED with its PR,
    /// and only the third is an orphan (01915167: the daily mirror
    /// snapshot read as 'a forge head no packet claims').
    #[test]
    fn a_published_head_is_not_an_orphan() {
        let ls = "aaa\trefs/heads/main\n\
                  bbb\trefs/heads/feat/claimed\n\
                  ccc\trefs/heads/publish/2026-09-19\n\
                  ddd\trefs/heads/docs/lost-work\n";
        let claimed: BTreeSet<String> = ["feat/claimed".to_string()].into_iter().collect();
        let published: std::collections::BTreeMap<String, String> = [(
            "publish/2026-09-19".to_string(),
            "https://github.com/algedonic-dev/boss/pull/239".to_string(),
        )]
        .into_iter()
        .collect();
        let (orphans, on_forge) = super::orphans_and_published(ls, &claimed, &published);
        assert_eq!(orphans, vec!["docs/lost-work".to_string()]);
        assert_eq!(
            on_forge,
            vec![(
                "publish/2026-09-19".to_string(),
                "https://github.com/algedonic-dev/boss/pull/239".to_string()
            )]
        );
        // a publish head the forge no longer holds (swept after the merge)
        // is not drawn: the line says what is ON the forge
        let (_, gone) =
            super::orphans_and_published("aaa\trefs/heads/main\n", &claimed, &published);
        assert!(gone.is_empty());
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

    /// Exit 75 is "not yet": early, not wrong — a different word from
    /// FAILING, and the probe's own reason rides the line.
    #[test]
    fn a_probe_that_said_not_yet_is_not_failing() {
        let early = landed(
            "fix/early",
            json!({ "proof_probe": "bash x.sh", "proof_attempt": { "exit": 75, "not_yet": true, "why": "NOT YET: no disk-report request yet — the sweeps fire daily" } }),
        );
        assert!(matches!(shed_place(&early), Shed::ProbeNotYet { .. }));
        let line = &shed_lines(&[early])[0];
        assert!(
            line.contains("probe says NOT YET — NOT YET: no disk-report"),
            "{line}"
        );
        assert!(!line.contains("FAILING"), "{line}");
        // An attempt written before the flag existed, exit 75 alone, reads the same.
        let bare = landed(
            "fix/bare",
            json!({ "proof_probe": "bash x.sh", "proof_attempt": { "exit": 75, "why": "later" } }),
        );
        assert!(matches!(shed_place(&bare), Shed::ProbeNotYet { .. }));
    }

    /// A NOT-YET THAT HAS LASTED DAYS IS NAMED AS SUCH (backlog adef5ddf):
    /// the line carries the streak and says the probe is ours to read,
    /// because exit 75 from a probe that can never pass reads exactly like
    /// a patient one. A short streak keeps the plain line.
    #[test]
    fn a_not_yet_streak_past_the_bound_is_named_on_the_line() {
        let streak = |since: &str, runs: u64| {
            landed(
                "fix/starved",
                json!({ "proof_probe": "bash x.sh", "proof_attempt": {
                    "at": "2026-09-23T07:00:00Z", "exit": 75, "not_yet": true,
                    "probe": "bash x.sh", "why": "NOT YET: grep found 0",
                    "not_yet_since": since, "not_yet_runs": runs,
                } }),
            )
        };
        let line = &shed_lines(&[streak("2026-09-19T05:50:00Z", 86)])[0];
        assert!(
            line.contains("NOT YET for 97h straight (86 runs)") && line.contains("read the probe"),
            "{line}"
        );
        let line = &shed_lines(&[streak("2026-09-23T01:00:00Z", 7)])[0];
        assert!(
            line.contains("probe says NOT YET — NOT YET: grep"),
            "{line}"
        );
        assert!(!line.contains("straight"), "{line}");
    }

    /// A DECLARED WAIT (backlog b461341d): the same 97h streak reads as
    /// the world's, naming what it waits on — and as ours once the
    /// declared event was seen while the probe still said not yet.
    #[test]
    fn a_declared_wait_names_what_it_waits_on_and_is_ours_once_seen() {
        let declared = |seen_at: Value| {
            landed(
                "fix/declared",
                json!({ "proof_probe": "bash x.sh",
                    "waits_on": {"on": "a cut-a-release packet David opens", "seen": "true"},
                    "proof_attempt": {
                    "at": "2026-09-23T07:00:00Z", "exit": 75, "not_yet": true,
                    "probe": "bash x.sh", "why": "NOT YET: no tag",
                    "not_yet_since": "2026-09-19T05:50:00Z", "not_yet_runs": 86,
                    "waits_on_seen_at": seen_at,
                } }),
            )
        };
        let line = &shed_lines(&[declared(Value::Null)])[0];
        assert!(
            line.contains("probe says NOT YET, waiting on a cut-a-release packet David opens"),
            "{line}"
        );
        assert!(!line.contains("straight"), "{line}");
        let line = &shed_lines(&[declared(json!("2026-09-23T06:00:00Z"))])[0];
        assert!(
            line.contains("was seen in the record at 2026-09-23T06:00:00Z"),
            "{line}"
        );
        assert!(!line.contains("no seen check"), "{line}");
    }

    /// A DECLARED WAIT WITH NO OBSERVER SAYS SO (backlog e9b164a1): a
    /// `seen` of null exempts the car from the streak bound and hands
    /// the judgement to nothing, so the line names the missing check
    /// and the verb that writes one, rather than reading as patience.
    #[test]
    fn a_declared_wait_with_no_seen_check_names_the_missing_observer() {
        let line = &shed_lines(&[landed(
            "fix/unobserved",
            json!({ "proof_probe": "bash x.sh",
                "waits_on": {"on": "a new Stripe sponsorship charge", "seen": null},
                "proof_attempt": {
                "at": "2026-09-23T07:00:00Z", "exit": 75, "not_yet": true,
                "probe": "bash x.sh", "why": "NOT YET: no charge",
                "not_yet_since": "2026-09-19T05:50:00Z", "not_yet_runs": 86,
            } }),
        )])[0];
        assert!(
            line.contains("waiting on a new Stripe sponsorship charge")
                && line.contains("no seen check")
                && line.contains("boss car waits-on"),
            "{line}"
        );
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

    /// A shed line names the backlog-item the car is holding open, so
    /// the residue is readable from the one verb a session runs first.
    ///
    /// Measured 2026-09-19 (a7837d81): twelve cars stood in the shed,
    /// the oldest landed 58 h earlier, holding ten open backlog-items —
    /// and two of those items had each already cost a dispatched agent
    /// run at high effort re-deriving a landed fix. A car with no item
    /// holds nothing open and says nothing extra.
    #[test]
    fn a_shed_line_names_the_item_the_unproven_car_is_holding_open() {
        let mut holding = landed("fix/holding", json!({ "proof_probe": "bash x.sh" }));
        holding["metadata"]["backlog_item"] = json!("f47861a5-2a86-4b8e-bb01-6491377b9499");
        let lines = shed_lines(&[holding, landed("fix/no-item", json!({}))]);
        assert_eq!(
            lines[0],
            "    fix/holding (holds f47861a5): probe pending (the forge runs it on arrival)"
        );
        assert!(
            lines[1].starts_with("    fix/no-item: UNPROVEN"),
            "{}",
            lines[1]
        );
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

    /// The query string as `(key, value)` pairs — what the server reads,
    /// not what the string looks like.
    fn params(query: &str) -> Vec<(&str, &str)> {
        query
            .split_once('?')
            .map(|(_, q)| q)
            .unwrap_or_default()
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .collect()
    }

    /// A held green is read by its HOLD, a stranded one by a window in
    /// DAYS — neither by a recency COUNT. David, 2026-09-15: the dev-pod
    /// car (gate-run 3164b0d5, green on `--hold` since 17:56Z 09-14)
    /// vanished from every surface after 80 newer gate-runs, because the
    /// approach read the newest N and cross-referenced those. A limit is
    /// not a filter (backlog 2fa96d34): the held read narrows on
    /// `metadata_has=hold` (the `metadata ? $n` door, 4d9aa761) and the
    /// stranded read keeps every run closed inside the retention window
    /// (`closed_within`, the same field the boards use), so a count only
    /// bounds the page and is checked against `total`.
    #[test]
    fn the_held_read_narrows_on_the_hold_and_the_stranded_read_on_days() {
        let held_q = held_gate_runs_query();
        let held = params(&held_q);
        assert!(held.contains(&("kind", "gate-run")), "{held_q}");
        assert!(
            held.contains(&("metadata_has", "hold")),
            "the held lane reads BY THE HOLD: {held_q}"
        );

        let stranded_q = stranded_gate_runs_query();
        let stranded = params(&stranded_q);
        assert!(stranded.contains(&("kind", "gate-run")), "{stranded_q}");
        assert!(
            stranded
                .iter()
                .any(|(k, v)| *k == "closed_within" && v.parse::<u32>().is_ok_and(|d| d >= 7)),
            "the stranded lane is windowed in DAYS, a week or more: {stranded_q}"
        );
        // The page bound is the server's ceiling, not a recency window:
        // the read is judged against `total`, and a count below the
        // ceiling would silently be one.
        for q in [&held_q, &stranded_q] {
            let limit = params(q)
                .iter()
                .find(|(k, _)| *k == "limit")
                .and_then(|(_, v)| v.parse::<i64>().ok());
            assert_eq!(limit, Some(GATE_RUN_PAGE), "{q}");
        }
    }

    /// Backlog 36cc4913: the flakiest check is a number, not a memory.
    /// The FLAKES line is read over the SAME week of gate-runs the
    /// stranded lane reads — one query, one window — counting the
    /// checks on runs a green-after-red stamped `flake_of`, and it is
    /// one line either way.
    #[test]
    fn the_flakes_line_counts_checks_over_the_weeks_gate_runs_and_says_none() {
        let runs = vec![
            gate_run(
                "fix/a",
                json!({ "flake_of": "p1", "flaky_checks": ["test"] }),
                "green",
            ),
            gate_run(
                "fix/b",
                json!({ "flake_of": "p2", "flaky_checks": ["test", "fmt"] }),
                "green",
            ),
            gate_run(
                "fix/c",
                json!({ "regate_of": "p3", "prior_failed": ["clippy"] }),
                "failed",
            ),
            gate_run("fix/d", json!({}), "green"),
        ];
        let line = flakes_line(&runs);
        assert_eq!(
            line,
            "\n  FLAKES — 2 check(s) red then green at the same head in the last 7 days: test: 2, fmt: 1"
        );
        assert_eq!(
            flakes_line(&[]),
            "\n  FLAKES — none: no gate went red then green at the same head in the last 7 days"
        );
        // The window the line names is the window the read narrows on.
        let q = stranded_gate_runs_query();
        assert!(params(&q).contains(&("closed_within", "7")), "{q}");
        assert!(line.contains("last 7 days"), "{line}");
    }

    /// The truncation note is a reading of `total` against the page, and
    /// silent when the page held everything.
    #[test]
    fn a_cut_read_says_how_much_it_read() {
        assert_eq!(cut_note(Some(1200), 1000), Some("1000 of 1200".to_string()));
        assert_eq!(cut_note(Some(415), 415), None);
        assert_eq!(cut_note(None, 415), None, "no total is no claim");
    }

    fn gate_run(branch: &str, md: Value, verdict: &str) -> Value {
        let mut m = md;
        m["branch"] = json!(branch);
        json!({
            "id": branch,
            "kind": "gate-run",
            "status": "closed",
            "metadata": m,
            "steps": [{ "spec_slug": "gate", "metadata": { "verdict": verdict } }],
        })
    }

    /// The held-green lane: a green no car claims WITH a hold, named with
    /// the operator's reason. A stranded green (no hold) is the other
    /// lane's; a train's own gate-run carries a hold too (128b5496) and
    /// is neither — the shared predicate (`stranded::unparked_green`)
    /// decides, not this lane.
    #[test]
    fn a_held_green_is_named_with_its_reason_and_a_train_gate_is_not() {
        let runs = vec![
            gate_run(
                "feat/held",
                json!({ "hold": "lands at the next restart" }),
                "green",
            ),
            gate_run("feat/stranded", json!({}), "green"),
            gate_run(
                "train/20260915-1427",
                json!({ "hold": "train gate", "train_gate": true }),
                "green",
            ),
            gate_run("feat/red-held", json!({ "hold": "x" }), "failed"),
            gate_run("feat/parked", json!({ "hold": "x" }), "green"),
        ];
        assert_eq!(
            held_greens(&runs, &heads(&["feat/parked"])),
            vec![(
                "feat/held".to_string(),
                "lands at the next restart".to_string()
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
        let lines = abandoned_report(
            &[AbandonedPlace {
                packet: "fafe8ba4-0000-0000-0000-000000000000".to_string(),
                branch: "fix/two-operator-verbs-stop-lying".to_string(),
                sha: "0448698fcfef9aa8728f9b3381c1de2a89911447".to_string(),
                queued_at: "2026-09-10T22:40:00Z".to_string(),
                idle_secs: Some(11 * 60),
                park_intent: true,
            }],
            &BTreeMap::new(),
        );
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

    /// THE RECOVERY REBASES IN THE SAME VERB (backlog e9cdd83f). A waiter
    /// that died has usually been dead long enough for main to move, and
    /// the bare re-gate this line used to print was then refused for its
    /// base. Rebasing by hand moved the head, and the next `boss gate`
    /// matched no open packet (`reusable_packet` keys on the head), so it
    /// filed a NEW gate-run with no `park_*` keys beside the old one —
    /// measured 2026-09-24 on 91594262 and 03af83b4, repaired by hand.
    /// `--rebase` replays inside the verb AFTER the packet is matched on
    /// the head it queued at, so the packet and its intent are the ones
    /// that run.
    #[test]
    fn a_recovery_rebases_inside_the_verb_so_the_packet_and_its_intent_are_kept() {
        let lines = abandoned_report(
            &[AbandonedPlace {
                packet: "91594262-0000-0000-0000-000000000000".to_string(),
                branch: "fix/x".to_string(),
                sha: "0448698fcfef9aa8728f9b3381c1de2a89911447".to_string(),
                queued_at: "2026-09-24T01:26:24Z".to_string(),
                idle_secs: Some(160 * 60),
                park_intent: true,
            }],
            &BTreeMap::new(),
        );
        let all = lines.join("\n");
        assert!(
            all.contains("recover: boss gate fix/x --wait --rebase"),
            "the recovery replays onto main in the verb, never by hand: {all}"
        );
    }

    /// A LANDED STRAND IS SUPERSEDED, NOT RECOVERABLE (backlog e9cdd83f).
    /// On 2026-09-24 orient went on advising a re-gate of 91594262 and
    /// 03af83b4 after both branches had landed; following that line with
    /// park intent is how a twin car is filed (610537b2). The place is
    /// still reported — it is an open packet — but as superseded, with
    /// what closes it, and no verb to run.
    #[test]
    fn an_abandoned_place_whose_work_landed_is_superseded_with_no_recovery() {
        let place = AbandonedPlace {
            packet: "03af83b4-0000-0000-0000-000000000000".to_string(),
            branch: "fix/a-car-names-every-item-it-answers".to_string(),
            sha: "27ebe999aaaa".to_string(),
            queued_at: "2026-09-24T01:24:28Z".to_string(),
            idle_secs: Some(160 * 60),
            park_intent: true,
        };
        let landed = BTreeMap::from([(
            place.packet.clone(),
            "main already holds its version of every file it changed".to_string(),
        )]);
        let all = abandoned_report(&[place], &landed).join("\n");
        assert!(all.contains("03af83b4"), "still reported: {all}");
        assert!(all.contains("LANDED"), "{all}");
        assert!(
            all.contains("main already holds its version of every file it changed"),
            "names how it was judged landed: {all}"
        );
        assert!(
            !all.contains("recover:"),
            "no re-gate for landed work: {all}"
        );
        assert!(
            all.contains(&format!("{}h", crate::train::GATE_DEADLINE_HOURS)),
            "names what closes it: {all}"
        );
    }

    /// LANDED IS JUDGED BY `boss merged`'s RULES, on the head the place
    /// queued at and then on the branch — never re-derived here. The
    /// queued head is asked first because a branch deleted by the train
    /// that landed it reads Unknown by ref; the branch second because a
    /// head that moved on and landed supersedes the one queued. Only a
    /// Merged answer supersedes: NotMerged and Unknown leave the recovery
    /// in place, where the gate's own landed guard still stands.
    #[test]
    fn superseded_asks_the_queued_head_then_the_branch_and_only_merged_counts() {
        use crate::merged::{How, Verdict};
        let place = AbandonedPlace {
            packet: "p".to_string(),
            branch: "fix/x".to_string(),
            sha: "0448698f".to_string(),
            queued_at: String::new(),
            idle_secs: None,
            park_intent: true,
        };
        let asked = std::cell::RefCell::new(Vec::new());
        let by_head = superseded_by_main(&place, |t| {
            asked.borrow_mut().push(t.to_string());
            if t == "0448698f" {
                Verdict::Merged(How::ContentPresent)
            } else {
                Verdict::NotMerged
            }
        });
        assert!(by_head.is_some());
        assert_eq!(asked.borrow().as_slice(), ["0448698f"]);

        let by_branch = superseded_by_main(&place, |t| {
            if t == "fix/x" {
                Verdict::Merged(How::Ancestor)
            } else {
                Verdict::Unknown("branch absent".into())
            }
        });
        assert!(by_branch.is_some());

        assert_eq!(
            superseded_by_main(&place, |_| Verdict::Unknown("no main".into())),
            None
        );
        assert_eq!(superseded_by_main(&place, |_| Verdict::NotMerged), None);

        // A symbolic head (`origin/<branch>`, what `resolve_sha` records
        // when the forge could not answer) is not a head to ask about.
        let symbolic = AbandonedPlace {
            sha: "origin/fix/x".to_string(),
            ..place.clone()
        };
        let asked = std::cell::RefCell::new(Vec::new());
        let _ = superseded_by_main(&symbolic, |t| {
            asked.borrow_mut().push(t.to_string());
            Verdict::NotMerged
        });
        assert_eq!(asked.borrow().as_slice(), ["fix/x"]);
    }

    /// NO STRAND, NO SECTION — and no alarm language for a healthy
    /// queue. An empty report prints nothing: the queue lane above
    /// already says how many places are held.
    #[test]
    fn a_healthy_queue_reports_no_abandoned_section() {
        assert!(abandoned_report(&[], &BTreeMap::new()).is_empty());
    }

    /// A RECOVERY LINE THAT NAMES NO BRANCH IS NOT ADVICE. A packet with
    /// no `branch` cannot be re-gated by name, so the strand is still
    /// reported — an operator has to see it — but without a verb that
    /// would run `boss gate  --wait` and fail on an empty argument.
    #[test]
    fn a_place_with_no_branch_is_named_without_a_verb_to_run() {
        let lines = abandoned_report(
            &[AbandonedPlace {
                packet: "c0ffee00-0000-0000-0000-000000000000".to_string(),
                branch: String::new(),
                sha: String::new(),
                queued_at: "2026-09-10T22:40:00Z".to_string(),
                idle_secs: Some(600),
                park_intent: false,
            }],
            &BTreeMap::new(),
        );
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
        let lines = abandoned_report(
            &[AbandonedPlace {
                packet: "deadbeef-0000-0000-0000-000000000000".to_string(),
                branch: "fix/x".to_string(),
                sha: String::new(),
                queued_at: "yesterday".to_string(),
                idle_secs: None,
                park_intent: false,
            }],
            &BTreeMap::new(),
        );
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

    // ---- SKIPPED REPEATEDLY (94896e74) ---------------------------------

    fn skipped_car(branch: &str, status: &str, review: &str, skips: Value, reason: &str) -> Value {
        let mut c = car(branch, status, review, json!({}));
        c["metadata"] = json!({ "branch": branch, "skips": skips, "skip_reason": reason });
        c
    }

    /// ONE SKIP IS ROUTINE. 39% of trains carry a skipped branch and
    /// depart anyway, so a car refused once — or four times — prints
    /// nothing. A line on a normal occurrence is how a surface becomes
    /// noise and then becomes unread, which is the defect this lane
    /// exists to fix, not to repeat.
    #[test]
    fn a_dock_whose_cars_are_skipped_now_and_then_says_nothing() {
        let cars = vec![
            skipped_car("fix/a", "open", "ready", json!(1), "conflict: a.rs"),
            skipped_car("fix/b", "open", "ready", json!(4), "conflict: b.rs"),
            car("fix/clean", "open", "ready", json!({})),
        ];
        assert!(troubled_dock_cars(&cars).is_empty());
    }

    /// REPETITION IS THE SIGNAL. At the threshold the car is named with
    /// its count and the reason the last window gave — the two facts the
    /// conductor already recorded and no read carried.
    #[test]
    fn a_car_refused_over_and_over_is_named_with_its_count_and_reason() {
        let cars = vec![
            skipped_car("fix/a", "open", "ready", json!(5), "conflict: steps.rs"),
            skipped_car("fix/deep", "open", "ready", json!(23), "conflict: a.rs"),
        ];
        assert_eq!(
            troubled_dock_cars(&cars),
            vec![
                ("fix/deep".to_string(), 23, "conflict: a.rs".to_string()),
                ("fix/a".to_string(), 5, "conflict: steps.rs".to_string()),
            ],
            "deepest first: the car nobody can land is the one being decided about"
        );
    }

    /// Only a car still AT the dock can be refused there. A boarded car
    /// has its `skips` cleared in the same write that stamps the train,
    /// but a stale stamp on a car that left must not paint trouble
    /// either — the dock predicate answers that, not the stamp.
    #[test]
    fn a_car_that_left_the_dock_is_not_troubled_on_it() {
        let cars = vec![
            skipped_car(
                "fix/boarded",
                "open",
                "completed",
                json!(9),
                "conflict: a.rs",
            ),
            skipped_car("fix/closed", "closed", "ready", json!(9), "conflict: a.rs"),
        ];
        assert!(troubled_dock_cars(&cars).is_empty());
    }

    /// A MALFORMED COUNT IS NOT A STALL, and a missing reason is not a
    /// blank line. Neither may invent trouble, and neither may hide a
    /// car that has one.
    #[test]
    fn a_count_that_is_not_a_count_paints_nothing_and_a_missing_reason_says_so() {
        for stamp in [json!("many"), json!(-9), json!(null), json!(9.5)] {
            let cars = vec![skipped_car("fix/a", "open", "ready", stamp.clone(), "x")];
            assert!(
                troubled_dock_cars(&cars).is_empty(),
                "{stamp} is not a count of anything"
            );
        }
        let cars = vec![skipped_car("fix/a", "open", "ready", json!(7), "")];
        assert_eq!(
            troubled_dock_cars(&cars),
            vec![("fix/a".to_string(), 7, "no reason recorded".to_string())],
            "seven refusals are still seven refusals with no reason on the car"
        );
    }

    // ---- CANNOT BOTH BOARD (5c567c27) ---------------------------------

    /// A parked car carrying the conductor's merge preview as
    /// `preview_dock` writes it: `(branch, files)` it conflicts with,
    /// measured over the parked set `set`.
    fn previewed(branch: &str, review: &str, set: &str, co: &[(&str, &[&str])]) -> Value {
        let mut c = car(branch, "open", review, json!({}));
        let co: Vec<Value> = co
            .iter()
            .map(|(b, f)| json!({ "branch": b, "files": f }))
            .collect();
        c["metadata"]["merge_preview"] = json!({
            "vs_main": { "clean": true },
            "conflicts_with": co,
            "anchored": { "main": "m", "parked_set": set },
            "checked_at": "2026-09-23T10:10:55Z",
        });
        c
    }

    /// THE PAIR IS NAMED ONCE, with its files. On 2026-09-20 three cars
    /// each green alone could not board together (WorldMap.svelte in two
    /// of them) and the pipeline stopped nine and a half hours; the
    /// conductor's merge preview had been measuring exactly that pair on
    /// every tick since 12a25f3e and nothing read it. A conflict is
    /// symmetric, so both cars carry it — one line, not two.
    #[test]
    fn two_dock_cars_that_conflict_with_each_other_are_one_named_pair() {
        let cars = vec![
            previewed("fix/b", "ready", "s1", &[("fix/a", &["WorldMap.svelte"])]),
            previewed("fix/a", "ready", "s1", &[("fix/b", &["WorldMap.svelte"])]),
            previewed("fix/clean", "ready", "s1", &[]),
        ];
        assert_eq!(
            unboardable_pairs(&cars),
            vec![(
                "fix/a".to_string(),
                "fix/b".to_string(),
                vec!["WorldMap.svelte".to_string()],
            )]
        );
    }

    /// A PREVIEW IS STALE, NOT WRONG, WHEN ITS SET MOVED. The conductor
    /// rewrites only the cars still parked-ready, so a car that boarded,
    /// or is held, keeps the preview of the dock it last saw. Only two
    /// previews measured over the SAME parked set (one tick) may pair,
    /// and only two cars still at the dock — a pair with a car that has
    /// left is a conflict nobody will meet. Measured live 2026-09-23:
    /// five cars stamped at 09:50 still named a branch whose own 10:10
    /// preview no longer named them.
    #[test]
    fn a_pair_from_another_tick_or_with_a_car_that_left_is_not_named() {
        let cars = vec![
            previewed("fix/a", "ready", "s1", &[("fix/b", &["x.rs"])]),
            previewed("fix/b", "ready", "s2", &[("fix/a", &["x.rs"])]),
            previewed("fix/c", "ready", "s3", &[("fix/gone", &["y.rs"])]),
            previewed("fix/gone", "completed", "s3", &[("fix/c", &["y.rs"])]),
        ];
        assert!(unboardable_pairs(&cars).is_empty());
    }

    /// No preview is no verdict — a car the conductor never measured
    /// pairs with nothing, rather than reading as either side of one.
    #[test]
    fn a_car_without_a_preview_pairs_with_nothing() {
        let cars = vec![
            previewed("fix/a", "ready", "s1", &[("fix/b", &["x.rs"])]),
            car("fix/b", "open", "ready", json!({})),
        ];
        assert!(unboardable_pairs(&cars).is_empty());
    }

    // ---- CONFLICTS WITH MAIN (20d0d717) -------------------------------

    /// A parked car whose preview, measured over `set` at `at`, says it no
    /// longer merges onto main on `files`.
    fn off_main(branch: &str, review: &str, set: &str, at: &str, files: &[&str]) -> Value {
        let mut c = previewed(branch, review, set, &[]);
        c["metadata"]["merge_preview"]["vs_main"] = json!({ "clean": false, "files": files });
        c["metadata"]["merge_preview"]["anchored"]["main"] = json!("1d917084abcdef00");
        c["metadata"]["merge_preview"]["checked_at"] = json!(at);
        c
    }

    /// THE VERDICT THE CONDUCTOR ALREADY MEASURED IS NAMED, with its
    /// files and the main it was measured on. Car 5fba0bda carried
    /// `vs_main.clean=false` on 2026-09-23 while orient called it only
    /// FRESHNESS-stale — a re-gate cannot fix a conflict with main.
    #[test]
    fn a_dock_car_that_conflicts_with_main_is_named_with_its_files() {
        let cars = vec![
            off_main(
                "fix/my-work",
                "ready",
                "s1",
                "2026-09-23T10:10:55Z",
                &["orient.rs"],
            ),
            previewed("fix/clean", "ready", "s1", &[]),
        ];
        assert_eq!(
            conflicts_with_main(&cars),
            vec![(
                "fix/my-work".to_string(),
                vec!["orient.rs".to_string()],
                "1d917084".to_string(),
            )]
        );
    }

    /// Only the dock's CURRENT measurement is read: a car whose preview
    /// carries an older parked set (held, or measured before the dock
    /// moved) and a car no longer at the dock are both silent — the
    /// same rule the pairs above obey (5c567c27).
    #[test]
    fn a_stale_or_departed_main_conflict_is_not_named() {
        // `previewed` stamps 10:10:55, so fix/now carries the newest set.
        let cars = vec![
            previewed("fix/now", "ready", "s2", &[]),
            off_main(
                "fix/old-set",
                "ready",
                "s1",
                "2026-09-23T09:50:00Z",
                &["a.rs"],
            ),
            off_main(
                "fix/boarded",
                "completed",
                "s2",
                "2026-09-23T10:10:55Z",
                &["b.rs"],
            ),
        ];
        assert!(conflicts_with_main(&cars).is_empty());
    }

    // ---- MY WORK (65a89769) --------------------------------------------

    fn agents() -> Vec<Value> {
        vec![json!({
            "id": "agent-claude",
            "aliases": ["claude@algedonic.dev"],
            "display_name": "Claude (engineering)",
        })]
    }

    /// One assignment row as `/api/jobs/assignments` answers it (the
    /// measured shape, 2026-09-18): the packet's identity beside the
    /// step, and the admission date this car adds to the row.
    fn asg(job: &str, workflow: &str, slug: &str, title: &str, opened: &str, step: &str) -> Value {
        json!({
            "job_id": job,
            "job_title": title,
            "workflow": workflow,
            "opened_on": opened,
            "priority": "standard",
            "step": { "id": step, "spec_slug": slug, "kind": "task", "status": "ready" },
        })
    }

    /// The pod signs as the ALIAS (`BOSS_ACTOR=claude@algedonic.dev`)
    /// while the dispatcher nominates to the alias too — but a box
    /// named by the agent's id would read 0 (measured: 25 on the alias,
    /// 0 on `agent-claude`, 2026-09-18). So the read asks for the
    /// caller AND every id the registry ties to it, from either end.
    #[test]
    fn my_work_reads_for_the_caller_and_every_registry_alias() {
        assert_eq!(
            my_work_identities("agent-claude", &agents()),
            vec![
                "agent-claude".to_string(),
                "claude@algedonic.dev".to_string()
            ]
        );
        assert_eq!(
            my_work_identities("claude@algedonic.dev", &agents()),
            vec![
                "claude@algedonic.dev".to_string(),
                "agent-claude".to_string()
            ]
        );
        // Nobody in the registry: the caller alone, never nothing.
        assert_eq!(
            my_work_identities("emp-david", &agents()),
            vec!["emp-david".to_string()]
        );
    }

    #[test]
    fn my_work_groups_by_kind_oldest_first_with_one_hint_per_group() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-18T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let rows = vec![
            asg(
                "2604d814-0000-4000-8000-000000000000",
                "backlog-item",
                "build",
                "DECISION: 37 orphan forge branches are provably landed (forge PR ancestry); 13 have no merged PR",
                "2026-09-16",
                "s-build",
            ),
            asg(
                "9e627310-0000-4000-8000-000000000000",
                "backlog-item",
                "triage",
                "CADENCE SILENT: maintenance-conservation-invariants",
                "2026-09-11",
                "s-triage",
            ),
            asg(
                "f827bd78-0000-4000-8000-000000000000",
                "maintenance-sweep",
                "inspect",
                "Disk headroom sweep",
                "2026-09-17",
                "s-inspect",
            ),
            asg(
                "3c1f843f-0000-4000-8000-000000000000",
                "ship-a-change",
                "proven",
                "The Stripe sensor adapter",
                "2026-09-18",
                "s-proven",
            ),
            // The same step answered under a second identity: one line.
            asg(
                "f827bd78-0000-4000-8000-000000000000",
                "maintenance-sweep",
                "inspect",
                "Disk headroom sweep",
                "2026-09-17",
                "s-inspect",
            ),
        ];
        let lines = my_work_lines(&rows, &carried_items(&[]), now);
        let all = lines.join("\n");
        assert_eq!(
            lines.len(),
            3 + 4 + 3,
            "3 headers + 4 rows + 3 hints:\n{all}"
        );
        // Groups oldest-first, rows oldest-first inside each.
        let header_at = |kind: &str| {
            lines
                .iter()
                .position(|l| l.starts_with(&format!("    {kind} — ")))
                .unwrap()
        };
        assert!(header_at("backlog-item") < header_at("maintenance-sweep"));
        assert!(header_at("maintenance-sweep") < header_at("ship-a-change"));
        assert_eq!(lines[0], "    backlog-item — 2");
        assert_eq!(
            lines[1],
            "      9e627310 backlog-item triage CADENCE SILENT: maintenance-conservation-invariants (7d)"
        );
        // The title is cut at 60 characters; the age is whole days.
        assert_eq!(
            lines[2],
            "      2604d814 backlog-item build DECISION: 37 orphan forge branches are provably landed (forg (2d)"
        );
        // One hint line per group, naming what the step IS.
        assert!(lines[3].contains("triage = a decision"), "{}", lines[3]);
        assert!(lines[3].contains("build = "), "{}", lines[3]);
        assert!(all.contains("inspect = a measurement to record"), "{all}");
        assert!(all.contains("run-car-probe"), "{all}");
        // A step with no opened_on (an older server) reads as unknown,
        // never as 0d.
        let mut bare = asg(
            "aaaaaaaa-0000-4000-8000-000000000000",
            "user-feedback",
            "design-review",
            "Feedback on /it/estate",
            "2026-09-18",
            "s-dr",
        );
        bare.as_object_mut().unwrap().remove("opened_on");
        let l = my_work_lines(&[bare], &carried_items(&[]), now).join("\n");
        assert!(l.contains("(age ?)"), "{l}");
        assert!(l.contains("--answers"), "{l}");
    }

    /// The whole section, as printed: the count and who it read for;
    /// "nothing" when empty; a loud refusal when nobody is named —
    /// a MY WORK read under `operator:unidentified` would answer 0
    /// and read as an empty queue (CLAUDE.md §Doors: a wrong target
    /// answers instead of erroring).
    #[test]
    fn my_work_section_counts_says_nothing_and_refuses_the_unnamed() {
        let now = chrono::Utc::now();
        let ids = vec![
            "claude@algedonic.dev".to_string(),
            "agent-claude".to_string(),
        ];
        let one = vec![asg(
            "f827bd78-0000-4000-8000-000000000000",
            "maintenance-sweep",
            "inspect",
            "Disk headroom sweep",
            "2026-09-17",
            "s",
        )];
        let full = my_work_section(Some(&ids), &one, &[], now).join("\n");
        assert!(
            full.starts_with(
                "  MY WORK — 1 ready/active step(s) assigned to claude@algedonic.dev (+ agent-claude)"
            ),
            "{full}"
        );
        let empty = my_work_section(Some(&ids), &[], &[], now).join("\n");
        assert!(empty.contains("MY WORK — nothing"), "{empty}");
        assert!(empty.contains("claude@algedonic.dev"), "{empty}");
        let refused = my_work_section(None, &[], &[], now).join("\n");
        assert!(refused.contains("MY WORK — REFUSED"), "{refused}");
        assert!(refused.contains("BOSS_ACTOR"), "{refused}");
        assert!(refused.contains(".config/boss/actor"), "{refused}");
    }

    /// An item a car already carries is not unstarted work, and MY WORK
    /// must not draw it as its filing age (bc416f60). Measured
    /// 2026-09-22: of the twelve oldest steps on the agent, six were
    /// build steps whose cars had merged days earlier and stood in the
    /// shed, their probes honestly answering "not yet" — and they read
    /// as 3-to-5-day-old unstarted builds, first in an oldest-first
    /// queue. The car already says so; the queue now reads it: landed
    /// (with what the proof waits on) or in flight (with where), listed
    /// after the group's unstarted rows, counted in the heading.
    #[test]
    fn an_item_a_car_already_carries_reads_as_carried_not_as_its_age() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-22T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let landed_item = "ea67ad87-0000-4000-8000-000000000000";
        let flight_item = "75198b15-0000-4000-8000-000000000000";
        let fresh_item = "a12736b1-0000-4000-8000-000000000000";
        let rows = vec![
            asg(
                landed_item,
                "backlog-item",
                "build",
                "Old and landed",
                "2026-09-17",
                "s1",
            ),
            asg(
                flight_item,
                "backlog-item",
                "build",
                "Old and parked",
                "2026-09-18",
                "s2",
            ),
            asg(
                fresh_item,
                "backlog-item",
                "build",
                "Newer, untouched",
                "2026-09-20",
                "s3",
            ),
        ];
        let mut shed = landed(
            "fix/landed",
            json!({ "proof_probe": "bash x.sh", "proof_attempt": { "exit": 75, "not_yet": true, "why": "not yet: no real prune since convergence" } }),
        );
        shed["metadata"]["backlog_item"] = json!(landed_item);
        let mut parked = car("fix/parked", "open", "ready", json!({}));
        parked["metadata"]["backlog_item"] = json!(flight_item);
        // A closed car carries nothing: its item's step is the chain's.
        let mut gone = landed("fix/gone", json!({}));
        gone["status"] = json!("closed");
        gone["metadata"]["backlog_item"] = json!(fresh_item);
        let cars = vec![shed, parked, gone];

        let carried = carried_items(&cars);
        assert_eq!(carried.len(), 2, "{carried:?}");
        let lines = my_work_lines(&rows, &carried, now);
        let all = lines.join("\n");
        assert_eq!(
            lines[0],
            "    backlog-item — 3 (2 already carried by a car)"
        );
        // The untouched item leads, with its age; the carried follow.
        assert_eq!(
            lines[1],
            "      a12736b1 backlog-item build Newer, untouched (2d)"
        );
        assert_eq!(
            lines[2],
            "      ea67ad87 backlog-item build Old and landed — LANDED (car fix/landed), \
             awaiting proof: probe says NOT YET — not yet: no real prune since convergence"
        );
        assert_eq!(
            lines[3],
            "      75198b15 backlog-item build Old and parked — IN FLIGHT (car fix/parked at Open for review)"
        );
        // A carried row carries no age: its filing date is not its state.
        assert!(!lines[2].contains("(5d)"), "{}", lines[2]);
        assert!(all.contains("carried = "), "{all}");

        // The section says how many of its count are carried.
        let ids = vec!["claude@algedonic.dev".to_string()];
        let section = my_work_section(Some(&ids), &rows, &cars, now).join("\n");
        assert!(
            section.starts_with(
                "  MY WORK — 3 ready/active step(s) assigned to claude@algedonic.dev — \
                 yours to move, oldest first; 2 already carried by a car, listed last"
            ),
            "{section}"
        );
        // A group with nothing carried reads exactly as before.
        let plain = my_work_lines(&rows[2..], &carried_items(&[]), now);
        assert_eq!(plain[0], "    backlog-item — 1");
        assert!(!plain.join("\n").contains("carried"), "{plain:?}");
    }

    /// `boss orient` reads the map from the server rather than deriving
    /// the numbers itself (design 0524fc95): the fixture is built with
    /// the SERVER's own types, so a renamed field on `boss_jobs::regions`
    /// breaks this before it can print `?` at an operator.
    #[test]
    fn the_regions_header_prints_the_servers_cards() {
        use boss_jobs::regions::{Region, RegionState, Regions, Trend};
        let trend = |metric: &str, unit: &str, cur: Option<f64>, prev: Option<f64>| Trend {
            metric: metric.into(),
            unit: unit.into(),
            current: cur,
            previous: prev,
            samples: usize::from(cur.is_some()),
            previous_samples: usize::from(prev.is_some()),
        };
        let region =
            |name: &str, count: Option<usize>, bound: Option<usize>, state, why: &str, trend| {
                Region {
                    name: name.into(),
                    count,
                    bound,
                    state,
                    why: why.into(),
                    trend,
                    // orient prints a region as a LINE — its count, its
                    // bound and its why. The machinery (car 5) is a
                    // drawing, so this reader takes none of it.
                    machines: Vec::new(),
                }
            };
        let map = Regions {
            window_hours: 24,
            regions: vec![
                region(
                    "dock",
                    Some(2),
                    Some(4),
                    RegionState::Clear,
                    "2 cars parked",
                    trend("dock wait", "hours", Some(1.5), Some(2.0)),
                ),
                region(
                    "gates",
                    Some(3),
                    Some(3),
                    RegionState::Busy,
                    "3 of 3 bays in use — at the bound",
                    trend("gate duration", "minutes", Some(14.0), None),
                ),
                region(
                    "track",
                    Some(1),
                    Some(1),
                    RegionState::Troubled,
                    "blocked: train #470",
                    trend("time at CI", "minutes", None, None),
                ),
                region(
                    "shed",
                    Some(0),
                    None,
                    RegionState::Clear,
                    "every landed car is proven",
                    trend("proven", "per day", Some(17.0), Some(12.0)),
                ),
                region(
                    "arrivals",
                    Some(17),
                    None,
                    RegionState::Clear,
                    "17 arrivals in 24h",
                    trend("arrivals", "per day", Some(17.0), Some(12.0)),
                ),
                region(
                    "garage",
                    Some(0),
                    None,
                    RegionState::Clear,
                    "nothing held, stranded or red",
                    trend("reds", "per day", Some(0.0), Some(1.0)),
                ),
                region(
                    "receiving",
                    None,
                    None,
                    RegionState::Troubled,
                    "the workflow registry that names the inbound kinds could not be read",
                    trend("inbound", "per day", None, None),
                ),
                region(
                    "marshalling",
                    Some(4),
                    None,
                    RegionState::Busy,
                    "4 packets standing at 2 stations",
                    trend("served", "per day", Some(6.0), Some(6.0)),
                ),
            ],
        };
        let lines = region_lines(&serde_json::to_value(&map).unwrap());
        assert_eq!(
            lines.len(),
            9,
            "a heading and a card per region:\n{}",
            lines.join("\n")
        );
        assert!(lines[0].contains("last 24h"), "{}", lines[0]);
        assert!(
            lines[1].starts_with("    dock ")
                && lines[1].contains(" 2 of 4 ")
                && lines[1].contains("clear")
                && lines[1].contains("dock wait 1.5h (was 2.0h)"),
            "{}",
            lines[1]
        );
        assert!(
            lines[2].contains("BUSY") && lines[2].contains("gate duration 14m (was —)"),
            "{}",
            lines[2]
        );
        assert!(
            lines[3].contains("TROUBLED")
                && lines[3].contains("blocked: train #470")
                && lines[3].contains("time at CI — (was —)"),
            "{}",
            lines[3]
        );
        assert!(
            lines[5].contains("arrivals 17.0/day (was 12.0/day)"),
            "{}",
            lines[5]
        );
        assert!(
            lines[7].contains("    receiving        ?") && lines[7].contains("TROUBLED"),
            "{}",
            lines[7]
        );
    }

    /// AN ACTOR-WORKED BORDER MUST NOT READ AS A DEAD RULE.
    ///
    /// `receiving -> marshalling` has `machine_kind: Actors` — a person
    /// or an agent does the crossing, there is no rule to fire — and it
    /// printed "(no firing recorded)", the phrase this surface uses for
    /// a machine that should have fired and did not. On the most
    /// backed-up border in the yard (217 standing, the oldest four days
    /// past the triage band) that is the pairing most likely to send a
    /// reader hunting for a broken automation that does not exist
    /// (beec1130).
    ///
    /// §Diagnosis says a troubled packet must look troubled. The
    /// inverse is load-bearing too: a healthy mechanism that looks
    /// broken spends someone's attention on a non-problem, and teaches
    /// them to discount the phrase on the borders where it is a real
    /// finding.
    #[test]
    fn an_actor_worked_border_says_who_works_it_rather_than_that_nothing_fired() {
        let map = serde_json::json!({
            "window_hours": 24,
            "borders": [
                {
                    "from": "receiving", "to": "marshalling",
                    "rate": { "current": 58.0, "previous": 87.0 },
                    "waiting": 217, "state": "BUSY", "holds": [],
                    "machine": {
                        "name": "the receiving desk", "kind": "actors",
                        "last_fired": null, "silent_for_minutes": null,
                        "expected_every_minutes": null, "silent": null,
                        "why": "no machine moves this hop — an actor does; \
                                the last crossing is the stamp"
                    }
                },
                {
                    "from": "gates", "to": "track",
                    "rate": { "current": 3.0, "previous": 5.0 },
                    "waiting": 0, "state": "clear", "holds": [],
                    "machine": {
                        "name": "train-board-on-dock-depth", "kind": "cadence",
                        "last_fired": null, "silent_for_minutes": null,
                        "expected_every_minutes": 30, "silent": null,
                        "why": "the cadence firing record could not be read"
                    }
                }
            ]
        });
        let lines = border_lines(&map);
        let actors = &lines[1];
        assert!(
            actors.contains("worked by actors"),
            "the actor-worked rail must name who works it:\n{actors}"
        );
        assert!(
            !actors.contains("no firing recorded"),
            "an actor-worked rail must not report a firing it could never have:\n{actors}"
        );
        // THE CONTROL, on the same map: a machine that really does fire
        // and has no record must STILL say so. Without it, a change
        // that dropped the phrase everywhere would pass the two
        // assertions above.
        let cadence = &lines[2];
        assert!(
            cadence.contains("(no firing recorded)"),
            "a cadence rule with no firing record is a finding and must keep saying so:\n{cadence}"
        );
    }

    /// The BORDERS section (design d2154293, car 2): a line per border
    /// with what crosses it, what stands at it and which machine moves
    /// it. Every unknown reads as unknown — the CLI's version of the
    /// rule that a rail whose flow could not be computed must not draw
    /// as a quiet one.
    #[test]
    fn border_lines_print_the_rate_the_queue_and_the_machine() {
        let map = serde_json::json!({
            "window_hours": 24,
            "borders": [
                {
                    "from": "gates", "to": "track",
                    "crossing": "a car boarded a train",
                    "rate": { "metric": "crossings", "unit": "per day",
                              "current": 3.0, "previous": 5.0,
                              "samples": 3, "previous_samples": 5 },
                    "last_crossed": "2026-09-19T11:00:00+00:00",
                    "waiting": 2,
                    "holds": [{ "what": "fix/a", "why": "parked, waiting for the boarding depth" }],
                    "machine": { "name": "train-board-on-dock-depth", "kind": "cadence",
                                 "last_fired": "2026-09-19T09:00:00+00:00",
                                 "silent_for_minutes": 180, "expected_every_minutes": 30,
                                 "silent": true, "why": "its own firing in cadence_firings" },
                    "state": "troubled",
                    "why": "2 packets waiting and train-board-on-dock-depth silent for 180m — it declares every 30m"
                },
                {
                    "from": "receiving", "to": "marshalling",
                    "crossing": "an inbound packet triaged",
                    "rate": { "metric": "crossings", "unit": "per day",
                              "current": null, "previous": null,
                              "samples": 0, "previous_samples": 0 },
                    "last_crossed": null,
                    "waiting": null,
                    "holds": [],
                    // A CADENCE RULE whose firing record could not be
                    // read — which is what this rail is for. It used to
                    // be `kind: actors`, and that made the
                    // "(no firing recorded)" assertion below test the
                    // wrong thing: an actor-worked hop has no rule to
                    // fire, so the phrase was never a finding there
                    // (beec1130). The unknown-record case needs a
                    // machine that really does fire.
                    "machine": { "name": "train-reconcile", "kind": "cadence",
                                 "last_fired": null, "silent_for_minutes": null,
                                 "expected_every_minutes": 10, "silent": null,
                                 "why": "the cadence firing record could not be read" },
                    "state": "troubled",
                    "why": "the workflow registry that names the inbound kinds could not be read"
                }
            ]
        });
        let lines = border_lines(&map);
        assert_eq!(
            lines.len(),
            3,
            "a heading and two rails:\n{}",
            lines.join("\n")
        );
        assert!(lines[0].contains("last 24h"), "{}", lines[0]);
        assert!(
            lines[1].contains("gates -> track")
                && lines[1].contains("3.0/day (was 5.0/day)")
                && lines[1].contains("2 waiting")
                && lines[1].contains("TROUBLED")
                && lines[1].contains("train-board-on-dock-depth SILENT 180m"),
            "{}",
            lines[1]
        );
        // The unknown rail: no rate, no count, no firing — and not one
        // zero anywhere on the line.
        assert!(
            lines[2].contains("— (was —)")
                && lines[2].contains("? waiting")
                && lines[2].contains("(no firing recorded)")
                && lines[2].contains("could not be read"),
            "{}",
            lines[2]
        );
        assert!(
            !lines[2].contains(" 0 "),
            "an unknown rail must not print a zero: {}",
            lines[2]
        );
    }
}
