//! A parked car whose files main moved under is re-gated on current main
//! before it may board (backlog 969a1092).
//!
//! WHY THE DOCK AND NOT THE TRAIN. A gate judges a branch's tree in
//! isolation, so its green vouches for the car on the main it was cut
//! from. When main moves before the car boards, the first thing that ever
//! tests the car against what landed in between is the TRAIN gate — and
//! it runs only after a consist is assembled, PR'd and CI'd, so a clash
//! it finds costs a whole train. Measured 2026-09-24: IT map car G
//! (`feat/it-phone-strip-map`) was gated on bb6f4f18; car F
//! (`feat/it-map-hud-frame-one-row-per-third`) landed as a9028721 and
//! changed `apps/web/src/it/yard/regions.ts`, the Regions type G's test
//! builds; train 17:17 (f7bd1e9d) went red on the web typecheck. G was
//! rebuilt on a9028721 as `-2`; car E landed as d013ec41 and changed the
//! same type again; train 18:31 (02801b05) went red the same way. Both
//! were cancelled by hand, and each held the track while it was red.
//!
//! THE RULE, computed from git alone — the car's changed files, and the
//! files main changed since the car's gated base:
//!
//!   - the SAME PATH on both sides always touches;
//!   - Rust widens to the CRATE (`crates/<tier>/<name>/`): a crate is one
//!     compile unit, so a change anywhere in it can break the car's build;
//!   - TypeScript/Svelte widens to the file's DIRECTORY, the unit the web
//!     app co-locates a component, its types and its test in (CLAUDE.md
//!     §TypeScript): G's `phone-strip.test.ts` imports `regions.ts` from
//!     beside it, and that is the edge that broke;
//!   - but a LEAF main changed — a `*.test.*`/`*.spec.*` file, or a Rust
//!     integration test directly under a crate's `tests/` — touches only
//!     its own path. Nothing imports a test file, and without this cut
//!     the rule re-gated 81 of the 138 cars that landed on 2026-09-24
//!     rather than 47: most of the difference was one shared mocked-spec
//!     directory and `boss-testing`'s shell-test binaries, each touched by
//!     nearly every train.
//!
//! What it does NOT see, said so a reader does not assume it: a change to
//! a crate the car's crate DEPENDS on, a web module imported from another
//! directory, and a test main ADDED against code the car changes. The
//! train gate still judges every assembled tree; this rule only moves the
//! commonest clash out of it and onto a single car.
//!
//! THE BOUND. At most one re-gate per car per main move: the launch is
//! stamped on the car (`base_regate.main`) and a car already re-gated for
//! the main it would board on is never re-gated for it again. A car whose
//! files main did not touch boards exactly as before, however far behind.
//!
//! THE REPAIR IS THE ONE THAT ALREADY EXISTS. The car's branch is replayed
//! onto current main by `freshness::rebase_onto_main` (the `boss gate
//! --rebase` door), and a gate-run is filed for the new head carrying a
//! park intent. Its green reaches the auto-park handler, which finds the
//! car still at the dock and refreshes it in place — `ParkAction::Refresh`
//! in `jobs_auto_park.rs`, the path CLAUDE.md §Doors names as the repair
//! for a moved branch. Nothing here copies a receipt.
//!
//! AND IT MUST NEVER FREEZE A LANDING (the ordering edge's rule, in
//! `boarding.rs`). An unreadable base, an unreachable cluster, a push the
//! forge refused — every way the MEANS fail boards the car as gated,
//! loudly, because the train gate still stands behind it. Only an answer
//! about the CAR holds it: main moved into its files, a gate slot is busy
//! this window, or its replay onto main conflicts.

use super::*;

/// The job-metadata key that records a dock re-gate on the car. One
/// object, replaced whole on every write.
pub(crate) const BASE_REGATE: &str = "base_regate";

/// The gate-run key that says the dock filed this run, and for which car
/// and main — so a reader of the gate lane can tell it from a builder's.
pub(crate) const DOCK_REGATE: &str = "dock_regate";

/// How many of the touching paths ride in a skip reason (a chip) and in
/// the stamp (the record). The count is always exact.
const REASON_SAMPLE: usize = 4;
const STAMP_SAMPLE: usize = 20;

/// What a car's re-gate on current main is keyed on: the unit a change to
/// `path` can reach. A trailing `/` names a directory, so a unit can never
/// collide with a file of the same name.
pub(crate) fn neighbourhood(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.first() == Some(&"crates") && parts.len() >= 4 {
        return format!("{}/", parts[..3].join("/"));
    }
    if is_script(path)
        && let Some((dir, _)) = path.rsplit_once('/')
    {
        return format!("{dir}/");
    }
    path.to_string()
}

/// A TypeScript, JavaScript or Svelte file — the web's module shapes.
fn is_script(path: &str) -> bool {
    matches!(
        path.rsplit('/')
            .next()
            .and_then(|name| name.rsplit_once('.'))
            .map(|(_, ext)| ext),
        Some("ts" | "tsx" | "js" | "mjs" | "cjs" | "svelte")
    )
}

/// A file nothing imports: a web test or spec, or a Rust integration test
/// directly under a crate's `tests/` (a helper under `tests/common/` is
/// imported by its siblings, so it is not a leaf).
pub(crate) fn is_leaf(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.first() == Some(&"crates") && parts.len() == 5 && parts[3] == "tests" {
        return true;
    }
    is_script(path)
        && parts.last().is_some_and(|name| {
            name.rsplit_once('.')
                .is_some_and(|(stem, _)| stem.ends_with(".test") || stem.ends_with(".spec"))
        })
}

/// PURE: the paths main changed that touch the car — sorted, each once.
/// Empty means main moved nowhere near it, and the car boards as gated.
pub(crate) fn touched_by_main(car_files: &[String], main_files: &[String]) -> Vec<String> {
    let reach: BTreeSet<String> = car_files
        .iter()
        .flat_map(|f| [f.clone(), neighbourhood(f)])
        .collect();
    main_files
        .iter()
        .filter(|m| {
            reach.contains(m.as_str()) || (!is_leaf(m) && reach.contains(&neighbourhood(m)))
        })
        .cloned()
        .collect::<BTreeSet<String>>()
        .into_iter()
        .collect()
}

/// Where a car's boarding head stands against `origin/main`, read from
/// the conductor's clone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BaseReading {
    /// `origin/main` now.
    pub main: String,
    /// The car's gated base: `merge-base(origin/main, head)`.
    pub base: String,
    /// What the car changed since its base.
    pub car_files: Vec<String>,
    /// What main changed since the car's base.
    pub main_files: Vec<String>,
}

/// Read a car's base out of the clone. `Ok(None)` is CURRENT — main is an
/// ancestor of the head, so the gate already tested this tree. Every
/// failure is an error the caller boards through, never a finding.
pub(crate) fn read_base(clone: &str, head: &str) -> Result<Option<BaseReading>> {
    let line = |args: &[&str]| -> Result<String> {
        let mut full = vec!["git", "-C", clone];
        full.extend_from_slice(args);
        Ok(stdout_str(&sh(&full)?).trim().to_string())
    };
    let main = line(&["rev-parse", "--verify", "origin/main"])?;
    let ancestor = sh_unchecked(&[
        "git",
        "-C",
        clone,
        "merge-base",
        "--is-ancestor",
        &main,
        head,
    ])?;
    match crate::freshness::base_from_is_ancestor_code(ancestor.status.code()) {
        crate::freshness::Base::Current => return Ok(None),
        crate::freshness::Base::Behind => {}
        crate::freshness::Base::Unanswered => {
            bail!(
                "git merge-base --is-ancestor gave no answer for {}",
                &head[..8.min(head.len())]
            )
        }
    }
    let base = line(&["merge-base", &main, head])?;
    let files = |to: &str| -> Result<Vec<String>> {
        Ok(line(&["diff", "--name-only", &base, to])?
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    };
    Ok(Some(BaseReading {
        car_files: files(head)?,
        main_files: files(&main)?,
        main,
        base,
    }))
}

/// A dock re-gate as the car records it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct RegateStamp {
    /// The `origin/main` it was launched for — the bound's key.
    pub main: String,
    /// The base the car had been gated on.
    pub base: String,
    /// The head the car was replayed to; empty when the replay was refused.
    pub head: String,
    /// The gate-run filed for `head`; empty until one is filed.
    pub gate_run: String,
    /// Why the replay was refused; empty otherwise.
    pub refused: String,
    /// The paths main changed that touched the car (a sample).
    pub touched: Vec<String>,
    /// How many there were.
    pub touched_count: usize,
    /// How many departures have left this car behind while a re-gate of
    /// it was in flight (backlog d9530df2). Carried from launch to launch,
    /// because the re-launch on the next main is exactly what follows a
    /// miss; a car with one is waited for to its own verdict
    /// (`departure_hold`).
    pub missed: u32,
}

impl RegateStamp {
    /// A fresh stamp for a launch against `main`.
    pub(crate) fn for_main(main: &str, touched: &[String]) -> Self {
        RegateStamp {
            main: main.to_string(),
            touched: touched.iter().take(STAMP_SAMPLE).cloned().collect(),
            touched_count: touched.len(),
            ..Default::default()
        }
    }

    /// This stamp, keeping the misses the car's `prior` stamp counted.
    pub(crate) fn carrying(self, prior: Option<&RegateStamp>) -> Self {
        RegateStamp {
            missed: prior.map_or(0, |p| p.missed),
            ..self
        }
    }

    /// The stamp a car carries, or `None` when it carries none a reader
    /// can key on (no `main`).
    pub(crate) fn of(car: &Value) -> Option<Self> {
        let s = car.pointer(&format!("/metadata/{BASE_REGATE}"))?;
        let text = |k: &str| {
            s.get(k)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let main = text("main");
        if main.is_empty() {
            return None;
        }
        let touched: Vec<String> = s
            .get("touched")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        Some(RegateStamp {
            main,
            base: text("base"),
            head: text("head"),
            gate_run: text("gate_run"),
            refused: text("refused"),
            touched_count: s
                .get("touched_count")
                .and_then(Value::as_u64)
                .map_or(touched.len(), |n| n as usize),
            touched,
            missed: s
                .get("missed")
                .and_then(Value::as_u64)
                .map_or(0, |n| u32::try_from(n).unwrap_or(u32::MAX)),
        })
    }

    pub(crate) fn to_value(&self, at: DateTime<Utc>) -> Value {
        json!({
            "main": self.main,
            "base": self.base,
            "head": self.head,
            "gate_run": self.gate_run,
            "refused": self.refused,
            "touched": self.touched,
            "touched_count": self.touched_count,
            "missed": self.missed,
            "at": at.to_rfc3339(),
            "why": "backlog 969a1092: main moved into this car's files after its gate",
        })
    }
}

/// What the dock does about a car whose receipt vouches for the head it
/// would board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DockBase {
    /// Main is an ancestor of the head: the gate tested this tree.
    Current,
    /// Behind, but main changed nothing that touches the car: board.
    Untouched { main_changed: usize },
    /// Behind, and main moved into the car: hold it. `launch` is whether
    /// a re-gate is owed — false when one was already launched for this
    /// main (the bound).
    Touched { touched: Vec<String>, launch: bool },
}

/// PURE: the dock's judgement of a car's base. `None` reading = current.
pub(crate) fn judge(reading: Option<&BaseReading>, stamp: Option<&RegateStamp>) -> DockBase {
    let Some(r) = reading else {
        return DockBase::Current;
    };
    let touched = touched_by_main(&r.car_files, &r.main_files);
    if touched.is_empty() {
        return DockBase::Untouched {
            main_changed: r.main_files.len(),
        };
    }
    DockBase::Touched {
        touched,
        launch: stamp.is_none_or(|s| s.main != r.main),
    }
}

/// A dock re-gate whose replayed head is the head this car would board —
/// the branch moved because the DOCK moved it, and the receipt has not
/// caught up yet.
pub(crate) fn pending(car: &Value, boards: &str) -> Option<RegateStamp> {
    RegateStamp::of(car).filter(|s| !s.head.is_empty() && s.head == boards)
}

/// Where a pending re-gate stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InFlight {
    /// The branch was replayed and no gate-run was filed — file it.
    FileGate,
    /// The gate-run has no verdict yet.
    Running,
    /// Green, yet the car still does not vouch for the head: the refresh
    /// never landed.
    GreenNotCopied,
    /// Red or lost: the car breaks on current main.
    Failed(String),
}

/// PURE: a pending re-gate's standing, from the gate-run's verdict.
pub(crate) fn in_flight(stamp: &RegateStamp, verdict: Option<&str>) -> InFlight {
    if stamp.gate_run.is_empty() {
        return InFlight::FileGate;
    }
    match verdict {
        None => InFlight::Running,
        Some("green") => InFlight::GreenNotCopied,
        Some(v) => InFlight::Failed(v.to_string()),
    }
}

// ---------------------------------------------------------------------------
// THE ROUND A DEPARTURE WAITS FOR (backlog 4890165b, design 42279fb2, D2)
//
// Main moves only when a train merges, so a re-gate launched on main M is
// still the right test of its car until the NEXT departure — and that
// departure is exactly what used to make it stale. Measured at 19:26 on
// 2026-09-25: the dock replayed a car onto 22c1a876 in the same pass that
// departed train #686, whose merge (777a5888) changed four of that car's
// files. With a lone car still shipping (depth 1), each car that did come
// fresh left in its own one- or two-car train and re-staled the rest.
//
// So a board that has cars ready HOLDS while the dock has re-gates in
// flight on the current main, and departs once the oldest of them is
// `regate_hold_minutes` old (the registry's number, 15 — the median dock
// re-gate that day was 13.9 min). Counted from the OLDEST so a re-gate
// launched late in a round can never extend it: the hold is one gate,
// never a moving target. A re-gate on a main that has since moved is not
// this departure's round, and a stamp with no readable launch time cannot
// bound a wait, so it holds nothing.
//
// EXCEPT FOR A CAR THE ROUND HAS ALREADY FAILED (backlog d9530df2). The
// oldest-first bound never waits for a re-gate launched late, and a car
// in a busy crate is re-gated on every main, so it can be launched late
// every time. Measured 2026-09-26: car 70165082 was left behind by trains
// 14:39, 15:19 and 16:14 while its own re-gate ran each time — launched
// 2 to 17 minutes after main moved, running 17 to 24 minutes — and
// boarded at 17:09 only because that board happened to fire after the
// green. So a departure that leaves a car behind mid-re-gate counts it on
// the car (`base_regate.missed`), and from then on a departure on its
// main waits for that car's OWN verdict, bounded by TWICE the hold from
// its own launch, so even a re-gate that never answers is waited for a
// fixed time. A car in its first round keeps the oldest-first rule: one
// miss is the accepted cost of a bounded wait.
// ---------------------------------------------------------------------------

/// One re-gate the dock has in flight: whose car, the main it was
/// launched for, when, and how many departures have already left the car
/// behind mid-re-gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InRound {
    pub car: String,
    pub main: String,
    pub since: DateTime<Utc>,
    pub missed: u32,
}

/// Why a departure waits: the round on `main`, how many re-gates are in
/// it, how old the oldest is, the bound it waits against, how many of its
/// cars a departure already left behind mid-re-gate and are waited for to
/// their own verdict, and the most minutes the wait can still last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RoundHold {
    pub main: String,
    pub in_flight: usize,
    pub oldest_minutes: i64,
    pub hold_minutes: u32,
    pub missed: usize,
    pub more_minutes: i64,
}

/// PURE: does a departure on `main` wait for the dock's round? `None` =
/// depart.
pub(crate) fn departure_hold(
    round: &[InRound],
    main: &str,
    now: DateTime<Utc>,
    hold_minutes: u32,
) -> Option<RoundHold> {
    if hold_minutes == 0 {
        return None;
    }
    let age = |r: &InRound| (now - r.since).num_minutes().max(0);
    let hold = i64::from(hold_minutes);
    let on_main: Vec<&InRound> = round.iter().filter(|r| r.main == main).collect();
    let oldest_minutes = on_main.iter().map(|r| age(r)).max()?;
    // A car already left behind mid-re-gate, still inside its own bound.
    let owed: Vec<i64> = on_main
        .iter()
        .filter(|r| r.missed > 0)
        .map(|r| 2 * hold - age(r))
        .filter(|left| *left > 0)
        .collect();
    let more_minutes = owed
        .iter()
        .copied()
        .chain(std::iter::once(hold - oldest_minutes))
        .max()
        .unwrap_or(0);
    (more_minutes > 0).then(|| RoundHold {
        main: main.to_string(),
        in_flight: on_main.len(),
        oldest_minutes,
        hold_minutes,
        missed: owed.len(),
        more_minutes,
    })
}

/// PURE: the `base_regate` stamp a car carries, with one more miss
/// counted — what a departure that leaves it behind mid-re-gate writes.
/// Replaced whole, like every write of the stamp, so everything else it
/// said (its launch time above all) is kept. `None` when it carries none.
pub(crate) fn missed_stamp(car: &Value) -> Option<Value> {
    let mut stamp = car
        .pointer(&format!("/metadata/{BASE_REGATE}"))
        .filter(|s| s.is_object())?
        .clone();
    let missed = stamp.get("missed").and_then(Value::as_u64).unwrap_or(0);
    stamp["missed"] = json!(missed.saturating_add(1));
    Some(stamp)
}

/// PURE: what this pass writes to the car's claim on the next free gate
/// bay (`gate::DOCK_WAITING`, design 42279fb2 D3) — `waiting` is the main
/// the dock is waiting for a bay to re-gate it on, `None` when it is not
/// waiting. The claim records only its CHANGES (design 38f3a488 D1): a car
/// that starts waiting, or whose main moved, gets `{main, since: now}`; a
/// car already claiming a bay on the same main gets nothing, so a pass
/// that changes nothing writes nothing — builders read liveness off the
/// refresh rule's last firing, not off the claim. Not waiting deletes a
/// claim the car still carries (a merge deletes a `null`), so a re-gate
/// that has launched is never counted twice. `None` = write nothing.
pub(crate) fn waiting_write(
    car: &Value,
    waiting: Option<&str>,
    now: DateTime<Utc>,
) -> Option<Value> {
    match waiting {
        Some(main) => match crate::gate::dock_claim(car) {
            Some((held, _)) if held == main => None,
            _ => Some(crate::gate::dock_waiting_stamp(main, now)),
        },
        None => car
            .pointer(&format!("/metadata/{}", crate::gate::DOCK_WAITING))
            .filter(|v| !v.is_null())
            .map(|_| Value::Null),
    }
}

/// When the dock launched the re-gate a car's stamp records — the `at`
/// every stamp has carried since 969a1092. `None` when absent or
/// unreadable.
pub(crate) fn launched_at(car: &Value) -> Option<DateTime<Utc>> {
    car.pointer(&format!("/metadata/{BASE_REGATE}/at"))
        .and_then(Value::as_str)
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

/// The gate-run metadata a dock re-gate carries: a park intent, so its
/// green refreshes the parked car (`ParkAction::Refresh` keys on
/// `park_summary` alone), and the mark that says the dock filed it.
///
/// ONLY THE SUMMARY, and the car's own. The refresh writes each park
/// field it is given as `regate_*` beside the receipt and leaves the rest
/// alone (absent, never nulled), so restating the builder's test and
/// verified lines under the dock's name would put words in their mouth.
/// No item answer either: the car already carries its edge, and the
/// refresh re-routes an item only when one is given.
pub(crate) fn marks(car: &Value, car_id: &str, main: &str) -> Value {
    let summary = ["regate_summary", "summary"]
        .iter()
        .find_map(|k| {
            car.pointer(&format!("/metadata/{k}"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("re-gated on current main by the dock (backlog 969a1092)");
    json!({
        car::PARK_SUMMARY: summary,
        DOCK_REGATE: {"car": car_id, "main": main},
    })
}

/// Was a replay onto main REFUSED for a reason about the car (a conflict,
/// or every commit already landed) rather than failed on the way? The
/// first holds the car; the second boards it. Read off the refusal
/// `rebase_onto_main` builds, whose two car-shaped answers both say
/// `REFUSED`, and pinned by `a_replay_refusal_is_told_from_a_failure`.
pub(crate) fn replay_refused(err: &anyhow::Error) -> bool {
    err.chain().any(|c| c.to_string().contains("REFUSED"))
}

fn short(s: &str) -> &str {
    &s[..8.min(s.len())]
}

fn sample(touched: &[String], count: usize) -> String {
    let shown: Vec<&str> = touched
        .iter()
        .take(REASON_SAMPLE)
        .map(String::as_str)
        .collect();
    let more = count.saturating_sub(shown.len());
    if more == 0 {
        shown.join(", ")
    } else {
        format!("{} (+{more} more)", shown.join(", "))
    }
}

/// The skip reason for a car the dock just re-gated.
pub(crate) fn launched_reason(stamp: &RegateStamp) -> String {
    format!(
        "re-gating on current main before it boards — gated on {}, and main {} has since changed \
         {} path(s) beside this car's: {}. Replayed to {}; gate-run {} carries its park intent, \
         and its green refreshes this car in place (backlog 969a1092)",
        short(&stamp.base),
        short(&stamp.main),
        stamp.touched_count,
        sample(&stamp.touched, stamp.touched_count),
        short(&stamp.head),
        short(&stamp.gate_run),
    )
}

/// The skip reason while a gate slot is not free this window.
pub(crate) fn busy_reason(why: &str, reading: &BaseReading, touched: &[String]) -> String {
    format!(
        "waiting for a gate slot to re-gate on current main ({why}) — main {} has changed {} \
         path(s) beside this car's since its gated base {}: {}",
        short(&reading.main),
        touched.len(),
        short(&reading.base),
        sample(touched, touched.len()),
    )
}

/// The skip reason for a car whose replay onto main was refused.
pub(crate) fn refused_reason(car_id: &str, stamp: &RegateStamp) -> String {
    format!(
        "held: main {} moved into this car's files ({}), and replaying it onto that main was \
         refused — {}. `boss rerail {}` resolves it by hand",
        short(&stamp.main),
        sample(&stamp.touched, stamp.touched_count),
        stamp.refused,
        short(car_id),
    )
}

/// The skip reason for a replayed car whose gate-run could not be filed.
pub(crate) fn unfiled_reason(stamp: &RegateStamp, why: &str) -> String {
    format!(
        "replayed onto main {} as {} to re-gate it, but the gate-run could not be filed ({why}) \
         — the next window files it",
        short(&stamp.main),
        short(&stamp.head),
    )
}

/// The skip reason for a pending re-gate, by where it stands.
pub(crate) fn in_flight_reason(car_id: &str, stamp: &RegateStamp, standing: &InFlight) -> String {
    match standing {
        InFlight::FileGate => unfiled_reason(stamp, "not filed yet"),
        InFlight::Running => format!(
            "re-gating on current main as gate-run {} (replayed to {} on main {}) — boards once \
             its green refreshes this car",
            short(&stamp.gate_run),
            short(&stamp.head),
            short(&stamp.main),
        ),
        InFlight::GreenNotCopied => format!(
            "re-gate gate-run {} is green on {}, but its receipt never reached this car — `boss \
             rerail {} --finish` copies it",
            short(&stamp.gate_run),
            short(&stamp.head),
            short(car_id),
        ),
        InFlight::Failed(verdict) => format!(
            "the re-gate on current main went {verdict} (gate-run {}, head {} on main {}): this \
             car breaks on the main it would land on. Its builder repairs it and re-gates with \
             --park-*, which refreshes this car",
            short(&stamp.gate_run),
            short(&stamp.head),
            short(&stamp.main),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// D3 of design 42279fb2: the dock's claim on the next free bay is
    /// written when it starts waiting for one and removed the pass it
    /// stops, so a launched re-gate is never counted twice (once running,
    /// once waiting). A car that never waited is never written.
    #[test]
    fn the_dock_claims_a_bay_only_while_it_waits_for_one() {
        let now = DateTime::parse_from_rfc3339("2026-09-25T22:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let bare = json!({"id": "car-g", "metadata": {"branch": "feat/g"}});
        assert_eq!(
            waiting_write(&bare, Some("77bf499e"), now),
            Some(crate::gate::dock_waiting_stamp("77bf499e", now)),
            "starts waiting: the claim is written, naming the main it waits on"
        );
        assert_eq!(
            waiting_write(&bare, None, now),
            None,
            "a car that is not waiting and carries no claim writes nothing"
        );
        let claimed = json!({"id": "car-g", "metadata": {
            "branch": "feat/g",
            crate::gate::DOCK_WAITING: crate::gate::dock_waiting_stamp("77bf499e", now),
        }});
        assert_eq!(
            waiting_write(&claimed, None, now),
            Some(Value::Null),
            "no longer waiting: the claim is deleted (null deletes on a merge)"
        );
        let cleared = json!({"id": "car-g", "metadata": {
            "branch": "feat/g",
            crate::gate::DOCK_WAITING: null,
        }});
        assert_eq!(waiting_write(&cleared, None, now), None);
    }

    /// Design 38f3a488 D1 (backlog b15b0f4e): the claim records its own
    /// CHANGES, not the dock's liveness. It used to be re-stamped with
    /// `at = now` on every two-minute pass, so the unchanged-hold guard
    /// could never match and each waiting car cost a full job PUT and an
    /// audit event per pass — 7 cars x 30 passes = 210 events an hour on
    /// the evening of 2026-09-25, each saying only "still waiting".
    /// Liveness is read off the refresh rule's last firing instead.
    #[test]
    fn a_car_still_waiting_on_the_same_main_writes_nothing() {
        let began = DateTime::parse_from_rfc3339("2026-09-25T21:20:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let now = DateTime::parse_from_rfc3339("2026-09-25T22:30:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let waiting = json!({"id": "car-g", "metadata": {
            "branch": "feat/g",
            crate::gate::DOCK_WAITING: crate::gate::dock_waiting_stamp("77bf499e", began),
        }});
        assert_eq!(
            waiting_write(&waiting, Some("77bf499e"), now),
            None,
            "same main, still waiting: no write, and `since` keeps its instant"
        );
        assert_eq!(
            waiting_write(&waiting, Some("24ea4303"), now),
            Some(crate::gate::dock_waiting_stamp("24ea4303", now)),
            "main moved: a new claim, waiting since now, on the new main"
        );
        // The old heartbeat shape is no claim to a builder, so a car that
        // still carries one is rewritten once into the claim that counts.
        let heartbeat = json!({"id": "car-g", "metadata": {
            crate::gate::DOCK_WAITING: {"main": "77bf499e", "at": "2026-09-25T22:28:00Z"},
        }});
        assert_eq!(
            waiting_write(&heartbeat, Some("77bf499e"), now),
            Some(crate::gate::dock_waiting_stamp("77bf499e", now)),
        );
    }

    fn paths(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// Car G's own diff, read from its branch on 2026-09-24 (the same
    /// eleven paths on all three of its attempts).
    const CAR_G: &[&str] = &[
        "apps/web/src/it/yard/MapPage.svelte",
        "apps/web/src/it/yard/PhoneStrip.svelte",
        "apps/web/src/it/yard/phone-strip.test.ts",
        "apps/web/src/it/yard/phone-strip.ts",
        "apps/web/src/styles.css",
        "apps/web/tests/mocked/it-phone.mocked.spec.ts",
        "libs/web-kit/src/FeedbackControl.svelte",
        "libs/web-kit/src/GlobalSearch.svelte",
        "libs/web-kit/src/PerspectiveTabs.svelte",
        "libs/web-kit/src/ui/phone.test.ts",
        "libs/web-kit/src/ui/phone.ts",
    ];

    /// What main changed between G's base (bb6f4f18) and the main train
    /// 17:17 assembled on (a9028721) — car F's train, fourteen of the
    /// fifty paths `git diff --name-only bb6f4f18 a9028721` prints: every
    /// yard path, and a sample of the rest.
    const MAIN_AFTER_F: &[&str] = &[
        "apps/simulator/src/styles.css",
        "apps/web/src/enamel-tokens.test.ts",
        "apps/web/src/it/yard/HudFrame.svelte",
        "apps/web/src/it/yard/MapPage.svelte",
        "apps/web/src/it/yard/borders.test.ts",
        "apps/web/src/it/yard/borders.ts",
        "apps/web/src/it/yard/hud.test.ts",
        "apps/web/src/it/yard/hud.ts",
        "apps/web/src/it/yard/map-palette.test.ts",
        "apps/web/src/it/yard/regions.ts",
        "apps/web/src/styles.css",
        "apps/web/tests/mocked/it-map.mocked.spec.ts",
        "crates/core/boss-jobs/src/regions.rs",
        "infra/lint/a-colour-is-a-token.sh",
    ];

    /// What main changed between G-2's base (a9028721) and the main train
    /// 18:31 assembled on (d013ec41) — car E's train: ten of its 182
    /// paths, the yard ones among them.
    const MAIN_AFTER_E: &[&str] = &[
        "apps/web/src/it/yard/DepartureBoard.svelte",
        "apps/web/src/it/yard/HudFrame.svelte",
        "apps/web/src/it/yard/MapPage.svelte",
        "apps/web/src/it/yard/PlantStrip.svelte",
        "apps/web/src/it/yard/regions.test.ts",
        "apps/web/src/it/yard/regions.ts",
        "apps/web/src/it/yard/shop-floor.test.ts",
        "apps/web/src/it/yard/shop-floor.ts",
        "apps/web/tests/mocked/it-region-map.mocked.spec.ts",
        "crates/core/boss-jobs/src/regions.rs",
    ];

    /// THE MEASURED CASE, first leg. G was gated before F landed; F
    /// changed the Regions type in the directory G's test imports from,
    /// and train 17:17 went red on it. The dock must have held G.
    #[test]
    fn car_g_behind_car_f_is_touched_by_the_regions_type_it_imports() {
        let t = touched_by_main(&paths(CAR_G), &paths(MAIN_AFTER_F));
        assert!(
            t.contains(&"apps/web/src/it/yard/regions.ts".to_string()),
            "the Regions type G's test builds sits beside it: {t:?}"
        );
        assert!(
            t.contains(&"apps/web/src/it/yard/MapPage.svelte".to_string()),
            "and both cars edited the same page: {t:?}"
        );
        for leaf in [
            "apps/web/src/it/yard/borders.test.ts",
            "apps/web/src/it/yard/hud.test.ts",
            "apps/web/tests/mocked/it-map.mocked.spec.ts",
        ] {
            assert!(
                !t.contains(&leaf.to_string()),
                "nothing imports a test, so {leaf} touches only itself: {t:?}"
            );
        }
        assert!(
            !t.iter()
                .any(|p| p.starts_with("crates/") || p.starts_with("infra/")),
            "a Rust crate and a lint G never went near do not touch it: {t:?}"
        );
    }

    /// THE MEASURED CASE, second leg: G rebuilt on F's main as `-2`, then
    /// E landed and changed the same type again (train 18:31).
    #[test]
    fn car_g2_behind_car_e_is_touched_again() {
        let t = touched_by_main(&paths(CAR_G), &paths(MAIN_AFTER_E));
        assert!(t.contains(&"apps/web/src/it/yard/regions.ts".to_string()));
        assert!(
            !t.contains(&"apps/web/src/it/yard/regions.test.ts".to_string()),
            "E's own test is a leaf: {t:?}"
        );
    }

    /// A car whose files main did not touch boards exactly as today,
    /// however far behind it is.
    #[test]
    fn a_car_main_moved_nowhere_near_is_untouched() {
        let car = paths(&[
            "crates/modules/boss-ledger/src/rules.rs",
            "infra/lint/a-colour-is-a-token.sh.baseline",
        ]);
        assert!(touched_by_main(&car, &paths(MAIN_AFTER_F)).is_empty());
        let reading = BaseReading {
            main: "a9028721".into(),
            base: "bb6f4f18".into(),
            car_files: car,
            main_files: paths(MAIN_AFTER_F),
        };
        assert_eq!(
            judge(Some(&reading), None),
            DockBase::Untouched {
                main_changed: MAIN_AFTER_F.len()
            }
        );
    }

    /// Rust widens to the crate — one compile unit — but a sibling
    /// integration test is its own binary and touches only itself, while
    /// a helper those tests share is not a leaf.
    #[test]
    fn rust_widens_to_the_crate_but_not_to_a_sibling_test_binary() {
        let car = paths(&["crates/core/boss-jobs/src/car.rs"]);
        assert_eq!(
            touched_by_main(&car, &paths(&["crates/core/boss-jobs/src/regions.rs"])),
            paths(&["crates/core/boss-jobs/src/regions.rs"])
        );
        assert_eq!(
            touched_by_main(
                &car,
                &paths(&["crates/core/boss-jobs/seeds/step_types.toml"])
            ),
            paths(&["crates/core/boss-jobs/seeds/step_types.toml"]),
            "include_str! seeds are part of the crate"
        );
        assert!(
            touched_by_main(
                &car,
                &paths(&["crates/core/boss-jobs/tests/yard_regions_http.rs"])
            )
            .is_empty()
        );
        assert!(
            !touched_by_main(&car, &paths(&["crates/core/boss-jobs/tests/common/mod.rs"]))
                .is_empty(),
            "a shared test helper is imported, so it is not a leaf"
        );
        let car_test = paths(&["crates/core/boss-jobs/tests/yard_regions_http.rs"]);
        assert!(
            !touched_by_main(&car_test, &paths(&["crates/core/boss-jobs/src/regions.rs"]))
                .is_empty(),
            "a car's own test is reached by the crate it tests — train #244's shape"
        );
        assert!(
            touched_by_main(&car, &paths(&["crates/core/boss-events/src/lib.rs"])).is_empty(),
            "another crate is out of reach (said in the module doc)"
        );
    }

    /// A non-script file widens nowhere: the same path or nothing.
    #[test]
    fn a_non_script_file_touches_only_its_own_path() {
        let car = paths(&["apps/web/src/styles.css", "infra/lint/x.sh"]);
        assert!(touched_by_main(&car, &paths(&["apps/web/src/App.svelte"])).is_empty());
        assert!(touched_by_main(&car, &paths(&["infra/lint/y.sh"])).is_empty());
        assert_eq!(
            touched_by_main(&car, &paths(&["infra/lint/x.sh"])),
            paths(&["infra/lint/x.sh"])
        );
    }

    #[test]
    fn a_directory_unit_never_collides_with_a_file_of_its_name() {
        assert_eq!(neighbourhood("apps/web/src/x.ts"), "apps/web/src/");
        assert_eq!(
            neighbourhood("crates/core/boss-jobs/src/a.rs"),
            "crates/core/boss-jobs/"
        );
        assert_eq!(neighbourhood("Cargo.lock"), "Cargo.lock");
        assert_eq!(neighbourhood("crates/Cargo.toml"), "crates/Cargo.toml");
        assert!(is_leaf("apps/web/src/it/yard/regions.test.ts"));
        assert!(is_leaf("apps/web/tests/mocked/it-map.mocked.spec.ts"));
        assert!(!is_leaf("apps/web/tests/mocked/_mockApi.ts"));
        assert!(!is_leaf("apps/web/src/it/yard/regions.ts"));
    }

    fn reading_g_after_f() -> BaseReading {
        BaseReading {
            main: "a9028721aaaa".into(),
            base: "bb6f4f18bbbb".into(),
            car_files: paths(CAR_G),
            main_files: paths(MAIN_AFTER_F),
        }
    }

    /// THE BOUND: one re-gate per car per main move. A car already
    /// re-gated for the main it would board on is held, never re-gated
    /// again; a main that moved again owes it one more.
    #[test]
    fn a_car_is_regated_at_most_once_per_main_move() {
        let r = reading_g_after_f();
        match judge(Some(&r), None) {
            DockBase::Touched { launch, .. } => assert!(launch, "first sight owes a re-gate"),
            other => panic!("G behind F must be touched: {other:?}"),
        }
        let same_main = RegateStamp::for_main("a9028721aaaa", &[]);
        match judge(Some(&r), Some(&same_main)) {
            DockBase::Touched { launch, .. } => {
                assert!(!launch, "already re-gated for this main — never twice")
            }
            other => panic!("{other:?}"),
        }
        let older_main = RegateStamp::for_main("bb6f4f18bbbb", &[]);
        match judge(Some(&r), Some(&older_main)) {
            DockBase::Touched { launch, .. } => assert!(launch, "main moved again"),
            other => panic!("{other:?}"),
        }
        assert_eq!(judge(None, Some(&same_main)), DockBase::Current);
    }

    /// The stamp round-trips through the car's metadata, and a car with
    /// no stamp — or one with no `main` to key on — has none.
    #[test]
    fn the_stamp_round_trips_through_the_car() {
        let at: DateTime<Utc> = "2026-09-24T17:20:00Z".parse().unwrap();
        let touched = paths(&["apps/web/src/it/yard/regions.ts"; 25]);
        let s = RegateStamp {
            base: "bb6f4f18".into(),
            head: "cafef00d1234".into(),
            gate_run: "run-1".into(),
            ..RegateStamp::for_main("a9028721", &touched)
        };
        assert_eq!(
            s.touched.len(),
            STAMP_SAMPLE,
            "a sample rides, not the list"
        );
        assert_eq!(s.touched_count, 25, "the count is exact");
        let car = json!({"metadata": {BASE_REGATE: s.to_value(at)}});
        assert_eq!(RegateStamp::of(&car), Some(s.clone()));
        assert_eq!(pending(&car, "cafef00d1234"), Some(s));
        assert_eq!(pending(&car, "somethingelse"), None, "not the dock's head");
        assert_eq!(RegateStamp::of(&json!({"metadata": {}})), None);
        assert_eq!(
            RegateStamp::of(&json!({"metadata": {BASE_REGATE: {"head": "x"}}})),
            None
        );
    }

    /// A refused replay stamps no head, so it is never mistaken for a
    /// pending re-gate.
    #[test]
    fn a_refused_replay_is_not_pending() {
        let s = RegateStamp {
            refused: "conflict in apps/web/src/it/yard/MapPage.svelte".into(),
            ..RegateStamp::for_main("a9028721", &[])
        };
        let at: DateTime<Utc> = "2026-09-24T17:20:00Z".parse().unwrap();
        let car = json!({"metadata": {BASE_REGATE: s.to_value(at)}});
        assert_eq!(pending(&car, ""), None);
    }

    #[test]
    fn a_pending_regate_reads_its_gate_run() {
        let unfiled = RegateStamp {
            head: "h".into(),
            ..RegateStamp::for_main("m", &[])
        };
        assert_eq!(in_flight(&unfiled, None), InFlight::FileGate);
        let filed = RegateStamp {
            gate_run: "run-1".into(),
            ..unfiled
        };
        assert_eq!(in_flight(&filed, None), InFlight::Running);
        assert_eq!(in_flight(&filed, Some("green")), InFlight::GreenNotCopied);
        assert_eq!(
            in_flight(&filed, Some("failed")),
            InFlight::Failed("failed".into())
        );
        assert_eq!(
            in_flight(&filed, Some("lost")),
            InFlight::Failed("lost".into())
        );
    }

    /// The gate-run a dock re-gate files carries a park intent the
    /// auto-park handler reads (`park_summary` is the key its refresh
    /// keys on), in the car's own words, and nothing else of the
    /// builder's.
    #[test]
    fn the_regate_carries_the_cars_own_summary_as_its_park_intent() {
        let car = json!({"metadata": {"summary": "the phone strip", "branch": "feat/x"}});
        let m = marks(&car, "car-1", "a9028721");
        assert_eq!(m[car::PARK_SUMMARY], "the phone strip");
        assert_eq!(m[DOCK_REGATE]["car"], "car-1");
        assert_eq!(m[DOCK_REGATE]["main"], "a9028721");
        for k in [
            car::PARK_TEST,
            car::PARK_VERIFIED,
            car::PARK_EXCLUDES,
            car::PARK_BACKLOG_ITEM,
            car::PARK_PROBE,
        ] {
            assert!(m.get(k).is_none(), "{k} is the builder's to state");
        }
        let rebuilt = json!({"metadata": {"summary": "old", "regate_summary": "rebuilt"}});
        assert_eq!(marks(&rebuilt, "c", "m")[car::PARK_SUMMARY], "rebuilt");
        assert!(
            !marks(&json!({}), "c", "m")[car::PARK_SUMMARY]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            "a car with no summary still gets an intent the handler reads"
        );
    }

    fn in_round(main: &str, at: &str) -> InRound {
        InRound {
            car: format!("car-{at}"),
            main: main.into(),
            since: at.parse().unwrap(),
            missed: 0,
        }
    }

    /// A re-gate whose car a departure has already left behind `missed`
    /// times while a re-gate of it was running.
    fn missed_round(car: &str, main: &str, at: &str, missed: u32) -> InRound {
        InRound {
            car: car.into(),
            missed,
            ..in_round(main, at)
        }
    }

    /// Backlog d9530df2, replayed from the 15:19 departure of 2026-09-26.
    /// Car 70165082's re-gate 69236ad5 was still running when train 14:39
    /// departed (a miss). On main 604ed86f the oldest dock re-gate,
    /// 14458749, launched at 15:02:35Z, so the oldest-first bound ended at
    /// 15:17:35Z; the car's OWN re-gate 93ffd5ee launched only at
    /// 15:14:49Z and went green at 15:35:53Z. Train 15:19 opened at
    /// 15:21:00Z, six minutes into it — the second miss of three. A car
    /// already left behind mid-re-gate is waited for to its own verdict,
    /// bounded by twice the hold from its own launch.
    #[test]
    fn a_car_left_behind_mid_regate_holds_the_next_departure_for_its_own_verdict() {
        let main = "604ed86faaaa";
        let at = |t: &str| -> DateTime<Utc> { format!("2026-09-26T{t}Z").parse().unwrap() };
        let round = [
            in_round(main, "2026-09-26T15:02:35Z"),
            missed_round("70165082", main, "2026-09-26T15:14:49Z", 1),
        ];
        assert_eq!(
            departure_hold(&round, main, at("15:21:00"), 15),
            Some(RoundHold {
                main: main.into(),
                in_flight: 2,
                oldest_minutes: 18,
                hold_minutes: 15,
                missed: 1,
                more_minutes: 24,
            }),
            "the oldest is past its bound, but a car that already missed a train mid-re-gate \
             is six minutes into its own: hold, up to 30 minutes from ITS launch"
        );
        assert!(
            departure_hold(&round, main, at("15:35:00"), 15).is_some(),
            "still running at 15:35 — its green came at 15:35:53Z"
        );
        // 15:36: the verdict is in, so the car is no longer in the round
        // (a Running re-gate is the only kind that joins it), and the
        // oldest re-gate alone is long past its bound: depart, with the
        // car aboard once the refresh has copied its green.
        let after_green = [in_round(main, "2026-09-26T15:02:35Z")];
        assert_eq!(departure_hold(&after_green, main, at("15:36:00"), 15), None);
        // BOUNDED: a re-gate that never answers is waited for 2 x the
        // hold from its own launch, and not a minute more.
        assert!(departure_hold(&round, main, at("15:44:00"), 15).is_some());
        assert_eq!(
            departure_hold(&round, main, at("15:44:49"), 15),
            None,
            "thirty minutes from its own launch: the bound is reached, depart"
        );
    }

    /// A car in its FIRST round keeps the oldest-first rule — the 14:39
    /// departure of the same day, which the car missed with no miss behind
    /// it yet: its re-gate 69236ad5 was the only one on 564d044c, launched
    /// 14:22:58Z, and the train opened at 14:40:38Z. That miss is the
    /// accepted cost of a bounded wait; the stamp it leaves is what makes
    /// the next one wait.
    #[test]
    fn a_car_on_its_first_round_keeps_the_oldest_first_rule() {
        let main = "564d044caaaa";
        let now: DateTime<Utc> = "2026-09-26T14:40:38Z".parse().unwrap();
        let first = [missed_round("70165082", main, "2026-09-26T14:22:58Z", 0)];
        assert_eq!(departure_hold(&first, main, now, 15), None);
        // A late launch in its first round still cannot extend the wait.
        let late = [
            in_round(main, "2026-09-26T14:22:58Z"),
            missed_round("late", main, "2026-09-26T14:38:00Z", 0),
        ];
        assert_eq!(departure_hold(&late, main, now, 15), None);
        // A missed car on a main that has since moved is not this
        // departure's round, however starved.
        let stale = [missed_round(
            "70165082",
            "0ldma1n0bbbb",
            "2026-09-26T14:38:00Z",
            3,
        )];
        assert_eq!(departure_hold(&stale, main, now, 15), None);
        // And a registry that declares no hold holds nothing, missed or not.
        let missed = [missed_round("70165082", main, "2026-09-26T14:38:00Z", 1)];
        assert_eq!(departure_hold(&missed, main, now, 0), None);
    }

    /// The miss is recorded on the car's own stamp, and a stamp it
    /// carries survives the next launch on a new main — otherwise the
    /// re-launch that follows every miss would forget it.
    #[test]
    fn a_miss_is_counted_on_the_stamp_and_carried_to_the_next_launch() {
        let at: DateTime<Utc> = "2026-09-26T14:22:58Z".parse().unwrap();
        let s = RegateStamp {
            head: "cafef00d".into(),
            gate_run: "69236ad5".into(),
            ..RegateStamp::for_main("564d044c", &[])
        };
        assert_eq!(s.missed, 0, "a fresh stamp has missed nothing");
        let car = json!({"metadata": {BASE_REGATE: s.to_value(at)}});
        let once = missed_stamp(&car).expect("a car with a stamp can miss");
        assert_eq!(once["missed"], 1);
        assert_eq!(
            once["gate_run"], "69236ad5",
            "the rest of the stamp is kept"
        );
        assert_eq!(
            once["at"],
            s.to_value(at)["at"],
            "and so is its launch time"
        );
        let car = json!({"metadata": {BASE_REGATE: once}});
        assert_eq!(RegateStamp::of(&car).map(|s| s.missed), Some(1));
        assert_eq!(missed_stamp(&car).unwrap()["missed"], 2);
        assert_eq!(missed_stamp(&json!({"metadata": {}})), None);
        // The next launch, on the next main, inherits the count.
        let next = RegateStamp::for_main("604ed86f", &[]).carrying(RegateStamp::of(&car).as_ref());
        assert_eq!(next.missed, 1);
        assert_eq!(next.main, "604ed86f");
        assert_eq!(RegateStamp::for_main("m", &[]).carrying(None).missed, 0);
    }

    /// D2 of design 42279fb2, measured on its founding pass: at 19:26 on
    /// 2026-09-25 the dock replayed a car onto 22c1a876 and, in the SAME
    /// pass, train #686 departed and moved main to 777a5888 — four of that
    /// car's files. A departure must wait for the round started on the
    /// main it would move, bounded, and counted from the OLDEST re-gate in
    /// it so a late launch never extends the wait.
    #[test]
    fn a_departure_waits_bounded_for_the_round_on_the_current_main() {
        let now: DateTime<Utc> = "2026-09-25T19:40:00Z".parse().unwrap();
        let main = "22c1a876aaaa";
        let round = [
            in_round(main, "2026-09-25T19:35:00Z"),
            in_round(main, "2026-09-25T19:38:00Z"),
            in_round("0ldma1n0bbbb", "2026-09-25T19:10:00Z"),
        ];
        assert_eq!(
            departure_hold(&round, main, now, 15),
            Some(RoundHold {
                main: main.into(),
                in_flight: 2,
                oldest_minutes: 5,
                hold_minutes: 15,
                missed: 0,
                more_minutes: 10,
            }),
            "two re-gates on this main, the oldest five minutes in: hold"
        );
        let later: DateTime<Utc> = "2026-09-25T19:50:00Z".parse().unwrap();
        assert_eq!(
            departure_hold(&round, main, later, 15),
            None,
            "fifteen minutes from the OLDEST, not the newest: the bound is reached, depart"
        );
        assert_eq!(
            departure_hold(&round, "777a5888cccc", now, 15),
            None,
            "a round on a main that has since moved is not this departure's round"
        );
        assert_eq!(
            departure_hold(&[], main, now, 15),
            None,
            "no round, no hold"
        );
        assert_eq!(
            departure_hold(&round, main, now, 0),
            None,
            "a registry that declares no hold holds nothing"
        );
    }

    /// The hold is dated from the stamp the launch wrote (`at`), which is
    /// already on every re-gate stamp; a stamp without a readable one
    /// cannot bound a wait, so it holds nothing.
    #[test]
    fn a_launch_is_dated_by_its_stamp() {
        let at: DateTime<Utc> = "2026-09-25T19:35:00Z".parse().unwrap();
        let s = RegateStamp {
            head: "cafef00d".into(),
            gate_run: "run-1".into(),
            ..RegateStamp::for_main("22c1a876", &[])
        };
        let car = json!({"metadata": {BASE_REGATE: s.to_value(at)}});
        assert_eq!(launched_at(&car), Some(at));
        assert_eq!(
            launched_at(&json!({"metadata": {BASE_REGATE: {"main": "m", "at": "yesterday"}}})),
            None
        );
        assert_eq!(launched_at(&json!({"metadata": {}})), None);
    }

    /// The two car-shaped refusals `rebase_onto_main` builds hold the car;
    /// anything else is a failure of the means and boards it.
    #[test]
    fn a_replay_refusal_is_told_from_a_failure() {
        let conflict = anyhow!(
            "boss gate --rebase: REFUSED — replaying abcd1234 onto origin/main@a9028721 hit a \
             conflict in: apps/web/src/it/yard/MapPage.svelte. Nothing was pushed"
        );
        assert!(replay_refused(&conflict));
        let landed = anyhow!(
            "boss gate --rebase: REFUSED — feat/x is already landed: every commit it carries"
        );
        assert!(replay_refused(&landed));
        for failure in [
            anyhow!("boss gate --rebase: the rebase was NOT applied — feat/x on the forge"),
            anyhow!("fetching main and the branch: fatal: could not read Username"),
        ] {
            assert!(!replay_refused(&failure), "{failure}");
        }
    }

    /// Every reason names what an operator acts on: the gate-run, the
    /// heads, and the paths.
    #[test]
    fn the_reasons_name_the_run_the_heads_and_the_paths() {
        let s = RegateStamp {
            base: "bb6f4f18bbbb".into(),
            head: "cafef00d1234".into(),
            gate_run: "0badc0de5678".into(),
            ..RegateStamp::for_main(
                "a9028721aaaa",
                &touched_by_main(&paths(CAR_G), &paths(MAIN_AFTER_F)),
            )
        };
        let line = launched_reason(&s);
        for want in [
            "bb6f4f18",
            "a9028721",
            "cafef00d",
            "0badc0de",
            "MapPage.svelte",
            "(+2 more)",
        ] {
            assert!(line.contains(want), "{want}: {line}");
        }
        let red = in_flight_reason("car-1234567", &s, &InFlight::Failed("failed".into()));
        assert!(
            red.contains("went failed") && red.contains("--park-"),
            "{red}"
        );
        let green = in_flight_reason("car-1234567", &s, &InFlight::GreenNotCopied);
        assert!(green.contains("boss rerail car-1234 --finish"), "{green}");
        let refused = refused_reason(
            "car-1234567",
            &RegateStamp {
                refused: "conflict in MapPage.svelte".into(),
                ..s
            },
        );
        assert!(
            refused.contains("conflict in MapPage.svelte") && refused.contains("boss rerail"),
            "{refused}"
        );
    }

    /// The reader, against a real clone: a car cut before main moved is
    /// read with both file lists from its base; a car on current main is
    /// current.
    #[test]
    fn the_base_is_read_from_the_clone() {
        let (_g, clone) = super::super::test_support::clone_fixture("dock-regate");
        let c = clone.to_str().expect("utf8");
        let git = |args: &[&str]| super::super::test_support::git_ok(&clone, args);
        let base = super::super::test_support::rev(&clone, "main");
        // The car: its own file, cut from main.
        git(&["checkout", "-q", "-b", "feat/car"]);
        std::fs::create_dir_all(clone.join("apps/web/src/it/yard")).expect("mkdir");
        std::fs::write(clone.join("apps/web/src/it/yard/phone-strip.ts"), "car").expect("w");
        git(&["add", "-A"]);
        git(&["commit", "-qm", "car"]);
        let car_head = super::super::test_support::rev(&clone, "feat/car");
        git(&["checkout", "-q", "main"]);
        // Current: main is still its base.
        git(&["fetch", "-q", "origin"]);
        assert_eq!(read_base(c, &car_head).expect("reads"), None);
        // Main moves beside it.
        std::fs::create_dir_all(clone.join("apps/web/src/it/yard")).expect("mkdir");
        std::fs::write(clone.join("apps/web/src/it/yard/regions.ts"), "main").expect("w");
        git(&["add", "-A"]);
        git(&["commit", "-qm", "car F lands"]);
        git(&["push", "-q", "origin", "main"]);
        git(&["fetch", "-q", "origin"]);
        let r = read_base(c, &car_head)
            .expect("reads")
            .expect("behind main now");
        assert_eq!(r.base, base);
        assert_eq!(r.main, super::super::test_support::rev(&clone, "main"));
        assert_eq!(r.car_files, paths(&["apps/web/src/it/yard/phone-strip.ts"]));
        assert_eq!(r.main_files, paths(&["apps/web/src/it/yard/regions.ts"]));
        assert!(matches!(
            judge(Some(&r), None),
            DockBase::Touched { launch: true, .. }
        ));
        // An unreadable head is an error the caller boards through.
        assert!(read_base(c, "0000000000000000000000000000000000000000").is_err());
    }
}
