//! The yard status read-model — "what is the yard doing, and why?"
//! answered from live system-of-record data, server-side, in one shape.
//!
//! WHY THIS EXISTS. The operating model is registry data, but until this
//! surface an operator learning where a train sat — and, when it was
//! stuck, WHY — had to SSH and read a journal or a step's metadata by
//! hand. On 2026-09-02 two merged trains sat four hours at the
//! playground-deploy step because the deploy had silently refused a
//! dirty tree; the reason was written to `deployed.metadata.deploy_blocked`
//! and read by nobody. This module surfaces that reason, and the rest of
//! the yard's live state, as a computed payload the SPA renders directly.
//!
//! PURE BY DESIGN. Everything here is a function of data already in the
//! record — pr-train Jobs and their steps, the loading-dock queue, the
//! cadence rows, the delivery policy. It invents no thresholds of its
//! own: the boarding predicate's numbers come from the cadence rows and
//! the alarm thresholds from the delivery policy, so the page shows
//! current registry truth rather than folklore. The HTTP handler
//! (`http::yard`) is a thin adapter that reads those rows and calls
//! [`build_status`]; the decision logic lives here where a test can pin
//! it without a database.
//!
//! NOT A SECOND CONDUCTOR. This describes the cadence rules and the
//! current dock depth so an operator can read "boards at 4 parked or
//! 06:00/18:00 UTC; 2 parked now". It deliberately does NOT reimplement
//! the conductor's claim/cooldown decision (`boss train cadence`'s
//! `due_window`) — duplicating that decision would be a second copy that
//! drifts from the one that actually boards trains. It reports the rule,
//! not the verdict.

use boss_core::job::{Job, JobStatus, Step, StepStatus};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cadence::{CadenceRuleRow, LastFiring};
use crate::delivery::DeliveryPolicyRow;
use crate::stranded;

/// A pr-train's step vocabulary, addressed by spec slug with a title
/// fallback. The conductor writes these steps; the meaning is in the
/// slug/title, not the generic `trigger`/`task`/`outcome` kind. ONE
/// definition of each pair so a rename in the workflow is a rename here.
struct StepKey {
    slug: &'static str,
    title: &'static str,
}

const COLLECT: StepKey = StepKey {
    slug: "collect",
    title: "Collect what is ready to board",
};
const PR: StepKey = StepKey {
    slug: "pr",
    title: "Open the batched PR",
};
const CI: StepKey = StepKey {
    slug: "ci",
    title: "CI verdict",
};
const MERGED: StepKey = StepKey {
    slug: "merged",
    title: "Merged into main",
};
const DEPLOYED: StepKey = StepKey {
    slug: "deployed",
    title: "Deployed to the playground",
};
const CONVERGED: StepKey = StepKey {
    slug: "converged",
    title: "Cluster converged",
};
const ARRIVED: StepKey = StepKey {
    slug: "arrived",
    title: "Train arrived",
};
const CANCELLED: StepKey = StepKey {
    slug: "cancelled",
    title: "Cancelled",
};

/// Find a step by its slug, falling back to its title — the same
/// addressing the conductor's `find_step` uses, so the two ends agree on
/// which step is which.
fn find_step<'a>(steps: &'a [Step], key: &StepKey) -> Option<&'a Step> {
    steps
        .iter()
        .find(|s| s.spec_slug.as_deref() == Some(key.slug) || s.title == key.title)
}

fn is_done(step: Option<&Step>) -> bool {
    step.is_some_and(|s| s.status == StepStatus::Completed)
}

/// Does this open train hold the single track? Only while it is
/// PRE-MERGE — its `merged` step not completed. A merged train's content
/// is on main: the next consist merges on top of it and the earlier
/// train converges by ancestry, so a train the board already renders as
/// deploying or CONVERGING must not also read as a track hold. That
/// double reading is how a merged train whose boot failed held the
/// fix-forward car out of the yard, twice, on 2026-09-07 (f3796323).
/// Fails closed: no `merged` step is pre-merge.
///
/// Twin: the conductor's `train::holds_the_track` (boss-cli) reads the
/// same rule off the API's JSON — the typed read model and that JSON
/// cannot share one signature, so each side's test names the other
/// (CLAUDE.md §9a).
fn holds_the_track(steps: &[Step]) -> bool {
    !is_done(find_step(steps, &MERGED))
}

/// A step's `completed_at` metadata stamp — the conductor writes it on
/// completion. Not `completed_on` (date-only); the RFC3339 instant is
/// what makes journey timings derivable.
fn completed_at(step: Option<&Step>) -> Option<&str> {
    step?.metadata.get("completed_at").and_then(Value::as_str)
}

/// The boarding instant: the `collect` step's `completed_at`. That is
/// the stamp the conductor's arrival report reads as `boarded_at`
/// (boss-cli `train::arrival_report`), so the yard and the report name
/// one moment. `assemble` and `pr` complete in the same board pass and
/// carry the same stamp, but `collect` is the step that MEANS boarded.
fn boarded_at(steps: &[Step]) -> Option<&str> {
    completed_at(find_step(steps, &COLLECT))
}

fn meta_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

/// An RFC3339 stamp as a UTC instant. `None` for anything that does not
/// parse, so a malformed stamp reads as "no stamp", never as a guess.
fn parse_instant(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|t| t.with_timezone(&chrono::Utc))
}

/// Where a train sits and, if it is stuck, why — the whole point.
///
/// Ordered by distance travelled: the phase is the furthest step the
/// train has reached but not passed. `Blocked` is not a phase of its own
/// — a train blocked at deploy is still `Deploying`, with a `block`
/// attached — because the phase says WHERE and the block says WHY, and
/// conflating them would hide one behind the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TrainPhase {
    /// Assembling the consist — before the PR opens.
    Boarding,
    /// PR open, CI has not returned a verdict yet.
    AwaitingCi,
    /// CI green (or done), waiting for the merge to land.
    AwaitingMerge,
    /// Merged, deploying to the playground.
    Deploying,
    /// Deployed, waiting for the cluster to converge on the merge.
    Converging,
    /// Terminal — the train arrived (or this version's finish line,
    /// `deployed`, is reached on a pre-converged workflow).
    Arrived,
}

impl TrainPhase {
    pub fn label(self) -> &'static str {
        match self {
            TrainPhase::Boarding => "boarding",
            TrainPhase::AwaitingCi => "awaiting CI",
            TrainPhase::AwaitingMerge => "awaiting merge",
            TrainPhase::Deploying => "deploying",
            TrainPhase::Converging => "awaiting cluster convergence",
            TrainPhase::Arrived => "arrived",
        }
    }
}

/// The reason a train is not moving, surfaced from the record that held
/// it. Each variant names a fact the conductor wrote down somewhere an
/// operator had no reason to look.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TrainBlock {
    /// `deployed.metadata.deploy_blocked` — the deploy refused a dirty
    /// or off-main tree. `since` is when the refusal began. THE buried
    /// fact this whole surface exists to expose.
    DeployBlocked {
        reason: String,
        since: Option<String>,
    },
    /// `ci.metadata.result == "failing"` before the merge — a red PR
    /// does not merge, so the train cannot advance until someone looks.
    CiRed { checks: Option<String> },
    /// `job.metadata.converge_alarm_filed` — the cluster has not
    /// converged past the delivery policy's threshold; a packet was
    /// filed and had nowhere to show until now.
    ConvergeOverdue,
    /// `job.metadata.stalled_since` — no step completed inside the
    /// policy's stall window.
    Stalled { since: String },
}

/// One train's row in the yard status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrainStatus {
    pub id: String,
    pub title: String,
    pub phase: TrainPhase,
    /// The title of the step the train currently sits at (the first
    /// ready/active one), for the operator who wants the exact step.
    pub at_step: Option<String>,
    /// Why it is not moving, when it is not. Prominent by being its own
    /// field rather than buried in a step's metadata.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block: Option<TrainBlock>,
    /// The CI verdict recorded on the `ci` step: `green` / `failing` /
    /// `null` (no verdict yet).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci_result: Option<String>,
    /// The forge PR url, once the `pr` step opened it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,
    /// How many cars boarded (from `metadata.boarded_jobs`).
    pub car_count: usize,
    /// When the cars boarded — the `collect` step's `completed_at`, the
    /// conductor's own RFC3339 stamp — so the page can say "aboard
    /// since". `None` until the collect completes, or when it carries
    /// no stamp. The same instant `journey_seconds` starts from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boarded_at: Option<String>,
    /// When this train is expected to arrive — or why that cannot be
    /// said. The gates panel beside it has carried a measured
    /// `typical_seconds` for months; trains never got the equivalent,
    /// and the cost landed on the operator, who had to ask a human
    /// whether a 20-minute transit was normal. See [`train_eta`]: the
    /// figure is measured from arrived trains only, and it names the leg
    /// it covers. `#[serde(default)]` so an older payload deserializes
    /// to "no estimate was computed" rather than failing the page.
    #[serde(default)]
    pub eta: TrainEta,
}

/// The title of the first ready-or-active step — the exact place the
/// train sits, matching `boss orient`'s `at_step`.
fn at_step(steps: &[Step]) -> Option<String> {
    steps
        .iter()
        .find(|s| matches!(s.status, StepStatus::Ready | StepStatus::Active))
        .map(|s| s.title.clone())
}

/// The phase a train is in: the furthest step reached. A train whose
/// `converged` step is absent (a pre-converged workflow version) treats
/// `deployed` done as arrival — its finish line, honestly, so its
/// absence is not read as "stuck".
fn phase_of(steps: &[Step]) -> TrainPhase {
    let has_converged = find_step(steps, &CONVERGED).is_some();
    if is_done(find_step(steps, &ARRIVED))
        || (is_done(find_step(steps, &DEPLOYED)) && !has_converged)
        || (has_converged && is_done(find_step(steps, &CONVERGED)))
    {
        return TrainPhase::Arrived;
    }
    if is_done(find_step(steps, &DEPLOYED)) {
        return TrainPhase::Converging;
    }
    if is_done(find_step(steps, &MERGED)) {
        return TrainPhase::Deploying;
    }
    if is_done(find_step(steps, &PR)) {
        // CI and merge run in parallel off the PR. If CI has a verdict
        // but the merge hasn't landed, it is awaiting the merge;
        // otherwise it is awaiting CI.
        if is_done(find_step(steps, &CI)) {
            return TrainPhase::AwaitingMerge;
        }
        return TrainPhase::AwaitingCi;
    }
    TrainPhase::Boarding
}

/// The block on a train, if any — read from the exact record the
/// conductor wrote it to. Order matters: a deploy block is the most
/// specific and most-recently-buried, so it wins over the coarser
/// stall/converge latches when both are present.
fn block_of(
    job: &Job,
    steps: &[Step],
    phase: TrainPhase,
    stall_before: Option<chrono::DateTime<chrono::Utc>>,
) -> Option<TrainBlock> {
    // A deploy block lives on the `deployed` step and is meaningful only
    // while that step has not completed — a completed deploy cleared it
    // by advancing, even though the keys are not erased.
    let deployed = find_step(steps, &DEPLOYED);
    if !is_done(deployed)
        && let Some(reason) = deployed.and_then(|s| meta_str(&s.metadata, "deploy_blocked"))
    {
        return Some(TrainBlock::DeployBlocked {
            reason: reason.to_string(),
            since: deployed
                .and_then(|s| meta_str(&s.metadata, "deploy_blocked_since"))
                .map(str::to_string),
        });
    }
    // A returned red verdict is trouble until the train leaves — after
    // the merge the content has landed and the lamp is history.
    let ci = find_step(steps, &CI);
    if matches!(
        phase,
        TrainPhase::Boarding | TrainPhase::AwaitingCi | TrainPhase::AwaitingMerge
    ) && ci.and_then(|s| meta_str(&s.metadata, "result")) == Some("failing")
    {
        return Some(TrainBlock::CiRed {
            checks: ci
                .and_then(|s| meta_str(&s.metadata, "checks"))
                .map(str::to_string),
        });
    }
    // The conductor filed an urgent packet about this train's
    // convergence and then had nowhere to show it.
    let md = &job.metadata;
    if md.get("converge_alarm_filed").is_some_and(truthy) {
        return Some(TrainBlock::ConvergeOverdue);
    }
    // Stalled: no step completed inside the policy's stall window.
    if let Some(since) = meta_str(md, "stalled_since").filter(|s| !s.is_empty()) {
        return Some(TrainBlock::Stalled {
            since: since.to_string(),
        });
    }
    // DERIVED STALL — every check above reads a flag the CONDUCTOR
    // writes, so a conductor that cannot run makes every train it is
    // failing to move look healthy. On 2026-09-04 train #198 sat at
    // `converged` for 2h21m against a 2h policy showing block: None,
    // because the reconcile that stamps `stalled_since` was itself what
    // was down. CLAUDE.md: "an alarm that reports through its subject
    // dies with it."
    //
    // The read-model already holds the step stamps and the policy, so it
    // can answer without asking. An arrived train is never stalled; a
    // train with no completion stamps yields nothing rather than a guess.
    if phase != TrainPhase::Arrived
        && let Some(deadline) = stall_before
        && let Some(newest) = newest_completion(steps)
        && newest < deadline
    {
        return Some(TrainBlock::Stalled {
            since: newest.to_rfc3339(),
        });
    }
    None
}

/// The most recent `completed_at` across a train's steps — how long it
/// has been standing still. Steps without the stamp are skipped rather
/// than treated as ancient.
fn newest_completion(steps: &[Step]) -> Option<chrono::DateTime<chrono::Utc>> {
    steps
        .iter()
        .filter_map(|s| s.metadata.get("completed_at").and_then(Value::as_str))
        .filter_map(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&chrono::Utc))
        .max()
}

/// A metadata flag written as either `true` or the string `"true"`
/// (the conductor's PATCH merges write bools; older writes wrote
/// strings) — accept both.
fn truthy(v: &Value) -> bool {
    v.as_bool() == Some(true) || v.as_str() == Some("true")
}

/// Build one train row from its Job and steps.
///
/// `eta` is what the arrived population measured ([`eta_basis`]),
/// computed once per status build and passed in so every train on the
/// board is measured against the same history.
pub fn train_status(
    job: &Job,
    steps: &[Step],
    stall_before: Option<chrono::DateTime<chrono::Utc>>,
    eta: &EtaBasis,
    now: Option<chrono::DateTime<chrono::Utc>>,
) -> TrainStatus {
    let phase = phase_of(steps);
    let car_count = job
        .metadata
        .get("boarded_jobs")
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    TrainStatus {
        id: job.id.to_string(),
        title: job.title.clone(),
        phase,
        at_step: at_step(steps),
        block: block_of(job, steps, phase, stall_before),
        ci_result: find_step(steps, &CI)
            .and_then(|s| meta_str(&s.metadata, "result"))
            .map(str::to_string),
        pr_url: find_step(steps, &PR)
            .and_then(|s| meta_str(&s.metadata, "pr_url"))
            .map(str::to_string),
        car_count,
        boarded_at: boarded_at(steps).map(str::to_string),
        eta: train_eta(steps, eta, now),
    }
}

/// One parked car on the loading dock.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DockCar {
    pub id: String,
    pub title: String,
    pub branch: Option<String>,
    /// The car's `opened_on` — parked-since, date-level. The finer
    /// step-level "parked since review became ready" is the queue-age
    /// lens; the dock row carries the packet's own stamp.
    pub parked_since: String,
}

pub fn dock_car(job: &Job) -> DockCar {
    DockCar {
        id: job.id.to_string(),
        title: job.title.clone(),
        branch: meta_str(&job.metadata, "branch").map(str::to_string),
        parked_since: job.opened_on.to_string(),
    }
}

/// The boarding predicate, rendered from the live cadence rows — the
/// answer to "when and why will the next train board?".
///
/// Every number here comes from a `cadence_rules` row, never a constant:
/// the whole point is the page shows what the registry currently says,
/// so a threshold changed by an operator moves this line with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardingPredicate {
    /// The parked-car threshold that boards a train (the `queue-depth`
    /// rule's `min_dock_depth`), when one is configured.
    pub dock_threshold: Option<i32>,
    /// The cooldown after a depth-triggered board (`cooldown_minutes`).
    pub cooldown_minutes: Option<i32>,
    /// The times-of-day a train boards (the `clock` rule's `at_times`),
    /// verbatim from the row — e.g. `["06:00","18:00"]`.
    pub at_times: Vec<String>,
    /// How many cars are parked right now.
    pub dock_depth: usize,
    /// Whether the dock has reached the depth threshold this instant.
    /// `None` when no depth rule is configured.
    pub threshold_met: Option<bool>,
    /// A plain-language sentence an operator can read without knowing
    /// the rule shapes.
    pub summary: String,
    /// Why it is not boarding RIGHT NOW and what clears it — flattened,
    /// so `held_because` rides the wire beside `threshold_met`.
    #[serde(flatten)]
    pub hold: BoardHold,
}

/// Why the dock is not boarding RIGHT NOW, and what clears it.
///
/// The conductor decides this every tick (`boss-cli` `cadence::decide`:
/// the depth and cooldown guards in `due_window`, then the single-track
/// check) and until now wrote the answer only to its journal. Twice on
/// 2026-09-07 an operator watched a threshold-met dock not board and
/// had to ask why; both times the answer was a cooldown with minutes
/// left. So the read-model re-derives the decision from the same facts
/// the conductor reads — the depth rule, the board rule's last firing,
/// the dock, the open trains — and says it here.
///
/// A SECOND derivation of that decision (CLAUDE.md §9a): the conductor's
/// acts, this one only reports, and the two are pinned together by the
/// unit tests on `boarding_hold` (one per hold, one per release rule,
/// one for precedence) and the HTTP cases in `tests/yard_status_http.rs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardHold {
    /// The hold in force — `"track occupied (N open train(s))"` (N
    /// counts the open trains still before their merge — a merged train
    /// awaiting deploy or converge holds nothing, see `holds_the_track`),
    /// `"cooldown — M min left"`, or `"below threshold (depth D of T)"`
    /// — or `None` when the dock would board on the conductor's next
    /// tick. When several apply the line names one, in the order the
    /// conductor checks them: track, then cooldown, then depth.
    pub held_because: Option<String>,
    /// Minutes until the board cooldown clears; `None` when none is
    /// running — never fired, elapsed, or released by a firing that
    /// boarded nothing.
    pub cooldown_remaining_minutes: Option<u32>,
    /// When the board rule last fired, released or not.
    pub last_board_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The sentence. A depth rule has no clock (`next_due` promises it
    /// no window), so this is always "boards on the next tick once …",
    /// naming EVERY hold that has to clear — never a time of day.
    pub next_board: String,
}

/// A cadence rule fires by DEPTH when it declares `min_dock_depth`.
/// Public so the handler reads the board rule's last firing under the
/// same row the predicate reads the threshold from.
pub fn depth_rule(rules: &[CadenceRuleRow]) -> Option<&CadenceRuleRow> {
    rules.iter().find(|r| r.min_dock_depth.is_some())
}

/// A cadence rule fires by CLOCK when it declares `at_times` (and is not
/// the calendar basis, which also uses `at_times` but for whole days).
fn clock_rule(rules: &[CadenceRuleRow]) -> Option<&CadenceRuleRow> {
    rules
        .iter()
        .find(|r| r.basis == "clock" && r.at_times.is_some())
}

/// The `at_times` array as a list of `HH:MM` strings, dropping anything
/// that is not a string — a malformed row degrades to fewer times, never
/// a panic.
fn at_times_of(rule: Option<&CadenceRuleRow>) -> Vec<String> {
    rule.and_then(|r| r.at_times.as_ref())
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// `on_track` is the number of open trains still before their merge —
/// the ones that hold the track (`holds_the_track`).
pub fn boarding_predicate(
    rules: &[CadenceRuleRow],
    dock_depth: usize,
    last_board: Option<&LastFiring>,
    on_track: usize,
    now: Option<chrono::DateTime<chrono::Utc>>,
) -> BoardingPredicate {
    let depth = depth_rule(rules);
    let dock_threshold = depth.and_then(|r| r.min_dock_depth);
    let cooldown_minutes = depth.and_then(|r| r.cooldown_minutes);
    let at_times = at_times_of(clock_rule(rules));
    let threshold_met = dock_threshold.map(|t| dock_depth as i64 >= i64::from(t));

    let mut clauses: Vec<String> = Vec::new();
    if let Some(t) = dock_threshold {
        let mut c = format!("{t} parked cars");
        if let Some(cd) = cooldown_minutes {
            c.push_str(&format!(" (min {cd} min between boards)"));
        }
        clauses.push(c);
    }
    if !at_times.is_empty() {
        clauses.push(format!("{} UTC", at_times.join(" / ")));
    }
    let summary = if clauses.is_empty() {
        // No cadence rules readable — say so plainly rather than imply a
        // schedule the registry does not hold.
        format!(
            "No boarding cadence is configured; {dock_depth} car(s) parked. \
             The conductor boards on its own schedule."
        )
    } else {
        let met = match threshold_met {
            Some(true) => " — the dock threshold is met",
            Some(false) => " — below the dock threshold",
            None => "",
        };
        format!(
            "Boards at {}; {dock_depth} car(s) parked now{met}.",
            clauses.join(" or ")
        )
    };

    BoardingPredicate {
        dock_threshold,
        cooldown_minutes,
        at_times,
        dock_depth,
        threshold_met,
        summary,
        hold: boarding_hold(rules, last_board, dock_depth, on_track, now),
    }
}

/// The conductor's cooldown-release rule, verbatim from `due_window`: a
/// firing that boarded nothing — failed (`rc != 0`) or idle (the loop
/// records that as a non-zero `IDLE_BOARD_RC`) — releases the window
/// early. No recorded outcome (`rc == None`) is still in flight and
/// HOLDS, since re-firing under it would double-board.
fn cooldown_released(last: &LastFiring) -> bool {
    last.rc.is_some_and(|rc| rc != 0)
}

/// The boarding decision for the queue-depth rule, as the conductor
/// would make it on its next tick — see [`BoardHold`].
///
/// Pure: (rules, the board rule's last firing, dock depth, trains on the
/// track — open and still before their merge, see `holds_the_track` —
/// now) in, the hold out. The cooldown needs a clock: no clock, no
/// cooldown reading — the rule `build_status` keeps for stalls. Only the
/// clockless empty status takes that path, and it carries no firing.
pub fn boarding_hold(
    rules: &[CadenceRuleRow],
    last_board: Option<&LastFiring>,
    dock_depth: usize,
    on_track: usize,
    now: Option<chrono::DateTime<chrono::Utc>>,
) -> BoardHold {
    let depth = depth_rule(rules);
    let threshold = depth.and_then(|r| r.min_dock_depth);
    let cooldown_remaining = last_board
        .filter(|l| !cooldown_released(l))
        .zip(now)
        .zip(depth.and_then(|r| r.cooldown_minutes).filter(|cd| *cd > 0))
        .and_then(|((last, now), cd)| {
            let elapsed = now - last.fired_at;
            (elapsed < chrono::Duration::minutes(i64::from(cd)))
                .then(|| i64::from(cd) - elapsed.num_minutes())
                // Held means at least a minute left; a firing stamped in
                // the future (clock skew) reads as the whole cooldown.
                .and_then(|left| u32::try_from(left.clamp(1, i64::from(cd))).ok())
        });

    // (why it holds, what clears it) — in the conductor's order.
    // No depth rule → nothing boards on dock depth and nothing can HOLD a
    // boarding that does not exist: no "track occupied", no cooldown, no
    // threshold. The degraded case pinned by
    // no_cadence_or_policy_wired_degrades_gracefully.
    if threshold.is_none() {
        return BoardHold {
            held_because: None,
            cooldown_remaining_minutes: None,
            last_board_at: None,
            next_board: "no depth rule is configured — nothing boards on dock depth".to_string(),
        };
    }
    let mut holds: Vec<(String, String)> = Vec::new();
    if on_track > 0 {
        let s = if on_track == 1 { "" } else { "s" };
        holds.push((
            format!("track occupied ({on_track} open train{s})"),
            "the track clears".to_string(),
        ));
    }
    if let Some(m) = cooldown_remaining {
        holds.push((
            format!("cooldown — {m} min left"),
            format!("the cooldown clears ({m} min)"),
        ));
    }
    if let Some(t) = threshold
        && (dock_depth as i64) < i64::from(t)
    {
        holds.push((
            format!("below threshold (depth {dock_depth} of {t})"),
            format!("the dock reaches {t}"),
        ));
    }

    let next_board = match threshold {
        None => "no depth rule is configured — nothing boards on dock depth".to_string(),
        Some(_) if holds.is_empty() => "boards on the next tick".to_string(),
        Some(_) => format!(
            "boards on the next tick once {}",
            holds
                .iter()
                .map(|(_, clears)| clears.as_str())
                .collect::<Vec<_>>()
                .join(" and ")
        ),
    };
    BoardHold {
        held_because: holds.into_iter().next().map(|(why, _)| why),
        cooldown_remaining_minutes: cooldown_remaining,
        last_board_at: last_board.map(|l| l.fired_at),
        next_board,
    }
}

/// A recently-closed train and its outcome.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecentTrain {
    pub id: String,
    pub title: String,
    /// `arrived` / `cancelled` / `unknown` — from the terminal that
    /// completed (or `metadata.outcome`).
    pub outcome: String,
    /// Boarding → terminal, in seconds: the `collect` stamp to the
    /// instant the train arrived or was cancelled (see
    /// [`journey_seconds`]). `None` when either instant is missing —
    /// never estimated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub journey_seconds: Option<i64>,
}

/// The outcome of a closed train: the arrived terminal completed →
/// `arrived`; the cancelled terminal completed or `metadata.outcome`
/// says so → `cancelled`; otherwise `unknown`. Never a guess.
fn outcome_of(job: &Job, steps: &[Step]) -> String {
    if is_done(find_step(steps, &ARRIVED)) {
        return "arrived".to_string();
    }
    if let Some(o) = meta_str(&job.metadata, "outcome") {
        return o.to_string();
    }
    "unknown".to_string()
}

/// The instant a closed train reached its terminal. The terminal step's
/// own `completed_at` when some hand stamped one (`arrived`, else
/// `cancelled`); otherwise the job's `metadata.closed_at`, which is
/// where the server records the instant a declared terminal step
/// completed (`close_job_on_terminal`, http/steps.rs — the same `now`
/// that completed the step).
///
/// WHY THE FALLBACK. `arrived` is an OUTCOME step: the server's terminal
/// machinery completes it, not the conductor's `complete_step`, and only
/// the conductor stamps `completed_at`. Measured 2026-09-08 on every
/// closed train the board had ever shown: `collect` stamped, `arrived`
/// completed and bare, `closed_at` on the job — so a journey that
/// demanded the step stamp read `null` on all of them. The instant was
/// in the record the whole time, one level up.
fn terminal_instant(job: &Job, steps: &[Step]) -> Option<chrono::DateTime<chrono::Utc>> {
    let stamped = [ARRIVED, CANCELLED]
        .iter()
        .map(|key| find_step(steps, key))
        .filter(|s| is_done(*s))
        .find_map(completed_at);
    stamped
        .or_else(|| meta_str(&job.metadata, "closed_at"))
        .and_then(parse_instant)
}

/// Boarding → terminal, in seconds. Both ends are read, never
/// estimated: no boarding stamp (a train cancelled before it collected)
/// or no terminal instant (a train still open, or closed by a path that
/// stamped nothing) is `None`.
fn journey_seconds(job: &Job, steps: &[Step]) -> Option<i64> {
    let boarded = boarded_at(steps).and_then(parse_instant)?;
    let terminal = terminal_instant(job, steps)?;
    Some((terminal - boarded).num_seconds())
}

pub fn recent_train(job: &Job, steps: &[Step]) -> RecentTrain {
    RecentTrain {
        id: job.id.to_string(),
        title: job.title.clone(),
        outcome: outcome_of(job, steps),
        journey_seconds: journey_seconds(job, steps),
    }
}

/// How many measurable arrivals the ETA needs before it will publish a
/// number. Ten, because ten is the smallest population where the three
/// order statistics the field publishes are three DISTINCT observations
/// and the low one is not simply the minimum: at n=10 the indices are
/// p10→1, median→5, p90→9. Below that the "spread" would be the range
/// restated, and one wedged 25-hour arrival (the record holds a 90,204s
/// one) could carry the median on its own — it takes six of them to move
/// a median of ten. Measured 2026-09-10 the population is 143 / 105, so
/// the floor costs nothing at normal volume; it binds exactly where it
/// should, on a fresh deployment or after a rebuild, and then the field
/// says what it is short of instead of inventing a number.
pub const MIN_ETA_ARRIVALS: usize = 10;

/// What one arrived train measured, in seconds. The two legs a train
/// actually spends its time on, read from the arrival report the
/// conductor wrote and the `closed_at` the server stamped.
///
/// WHY NOT `total_s`. The arrival report declares `total_s` and
/// `arrived_at`, and both are NULL on every arrived train in the record
/// (measured 2026-09-10: 0 of 143). A reader leaning on them measures
/// nothing while looking like it measured everything, so the legs are
/// derived from the instants that are actually there.
///
/// WHY NOT `merge_to_deploy_s`. It is 0 on every arrival measured — the
/// deploy stamp and the merge stamp are the same instant — so it
/// describes a leg that takes no time while the merge→ARRIVAL leg takes
/// a median of 1,183s, half the journey. Measuring the declared leg
/// would under-state a merged train's remaining time by that half.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArrivedLegs {
    board_to_merge_s: Option<i64>,
    merge_to_arrival_s: Option<i64>,
}

/// One leg's measured distribution: the median and the 10th/90th
/// percentiles, so a surface can publish a spread instead of a point
/// estimate. The spread is the finding, not decoration — within a single
/// car count, arrivals ranged 770s to 4,274s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegSample {
    /// The 10th percentile — a fast-but-real arrival, never the minimum.
    pub low: i64,
    pub median: i64,
    /// The 90th percentile. Deliberately NOT the maximum: the record
    /// holds a 25-hour wedged train, and one of those is not a forecast.
    pub high: i64,
    /// How many arrivals this leg was measured from.
    pub n: usize,
}

/// What the arrived population measured — computed ONCE per status build
/// and shared by every in-flight train, so the sort happens once and
/// every train on the board is measured against the same history.
///
/// `Thin` carries the counts it found rather than a bare `None`: "9
/// arrived trains, 10 needed" sends nobody to re-derive why there is no
/// number (CLAUDE.md §Diagnosis — a verdict must name what failed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EtaBasis {
    Measured {
        board_to_merge: LegSample,
        merge_to_arrival: LegSample,
    },
    Thin {
        /// Trains in the window whose outcome is `arrived`.
        arrivals: usize,
        /// Of those, how many measured at least one leg.
        measurable: usize,
    },
}

/// One arrived train's legs — `None` for anything that did not ARRIVE.
///
/// THE FILTER LIVES HERE, not in the caller, and that is deliberate. A
/// board the consist check refuses still opens a pr-train Job and
/// cancels it, so the recent population is overwhelmingly zero-length
/// cancellations: measured 2026-09-10, 696 of 1,014 pr-trains are
/// `cancelled`, and of the 40 most recent — the window the departure
/// board fetches — exactly ONE had arrived. A population sampled on
/// recency would therefore average a pile of zeros and report a
/// confident, wrong, near-instant ETA. Making the outcome test part of
/// the extractor means a caller cannot forget it.
fn arrived_legs(job: &Job) -> Option<ArrivedLegs> {
    if meta_str(&job.metadata, "outcome") != Some("arrived") {
        return None;
    }
    let timings = job.metadata.get("arrival_report")?.get("timings")?;
    let board_to_merge_s = timings
        .get("board_to_merge_s")
        .and_then(Value::as_i64)
        .filter(|s| *s > 0);
    // The merge→arrival leg: the merge stamp in the report to the
    // `closed_at` the server wrote when the terminal step completed.
    // Both ends are read; a leg that runs backwards (clock skew, or a
    // `closed_at` from another path) is dropped rather than negated.
    let merge_to_arrival_s = meta_str(timings, "merged_at")
        .and_then(parse_instant)
        .zip(meta_str(&job.metadata, "closed_at").and_then(parse_instant))
        .map(|(merged, closed)| (closed - merged).num_seconds())
        .filter(|s| *s > 0);
    (board_to_merge_s.is_some() || merge_to_arrival_s.is_some()).then_some(ArrivedLegs {
        board_to_merge_s,
        merge_to_arrival_s,
    })
}

/// The order statistic at `fraction` of a sorted sample. Index by
/// truncation, clamped inside the slice — the same "an observation, not
/// an interpolation" convention [`typical_gate_seconds`] uses, so the
/// numbers on this page are all read off real arrivals.
fn quantile(sorted: &[i64], fraction: f64) -> Option<i64> {
    if sorted.is_empty() {
        return None;
    }
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
    let i = (sorted.len() as f64 * fraction) as usize;
    sorted.get(i.min(sorted.len() - 1)).copied()
}

/// One leg's distribution, or `None` when the leg has no samples. The
/// MIN_ETA_ARRIVALS floor is applied to the POPULATION in [`eta_basis`],
/// not per leg: the two legs have different counts (143 and 105 on the
/// record, because `closed_at` is missing on some arrivals) and the
/// binding constraint belongs in one place.
fn leg_sample(mut v: Vec<i64>) -> Option<LegSample> {
    if v.is_empty() {
        return None;
    }
    v.sort_unstable();
    Some(LegSample {
        low: quantile(&v, 0.10)?,
        median: quantile(&v, 0.50)?,
        high: quantile(&v, 0.90)?,
        n: v.len(),
    })
}

/// What the arrived population measured. Pure: a slice of Jobs in, a
/// distribution or a stated shortfall out.
///
/// CAR COUNT IS NOT A DIMENSION, and the reason is measured rather than
/// assumed. Over 143 arrivals the Spearman rank correlation between car
/// count and board→merge time is 0.167, and the bucket medians are
/// 1,140 / 1,197 / 1,256 / 1,259s for 1 / 2-3 / 4-6 / 7+ cars — a 10%
/// spread BETWEEN buckets against a 770..4,274s spread INSIDE a single
/// one. Slicing the population by car count would cut 143 samples into
/// an 8-sample bucket to move the median by 4%: strictly less
/// information, published with more confidence. So every train is
/// measured against the whole arrived population, and the spread — which
/// is where the real variation lives — is published beside the median.
pub fn eta_basis(arrived: &[Job]) -> EtaBasis {
    let arrivals = arrived
        .iter()
        .filter(|j| meta_str(&j.metadata, "outcome") == Some("arrived"))
        .count();
    let legs: Vec<ArrivedLegs> = arrived.iter().filter_map(arrived_legs).collect();
    let pick =
        |f: fn(&ArrivedLegs) -> Option<i64>| -> Vec<i64> { legs.iter().filter_map(f).collect() };
    let thin = EtaBasis::Thin {
        arrivals,
        measurable: legs.len(),
    };
    if legs.len() < MIN_ETA_ARRIVALS {
        return thin;
    }
    match (
        leg_sample(pick(|l| l.board_to_merge_s)),
        leg_sample(pick(|l| l.merge_to_arrival_s)),
    ) {
        (Some(board_to_merge), Some(merge_to_arrival)) => EtaBasis::Measured {
            board_to_merge,
            merge_to_arrival,
        },
        // One leg measured and the other not is not half an estimate: a
        // whole-journey figure built from one leg would under-state by
        // the other, which is the mixing this type exists to prevent.
        _ => thin,
    }
}

/// When a train in flight is expected to arrive — or why that cannot be
/// said. A tagged union rather than a nullable number, so a reader gets
/// either a figure or a reason and never a bare `null` to interpret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum TrainEta {
    Estimate {
        /// WHICH LEG the figure covers, in words: `boarding → arrival`
        /// for a train not yet merged, `merge → arrival` for one past
        /// it. On the wire because the two are materially different
        /// lengths (a median of 2,389s against 1,183s) and a reader who
        /// has to guess which one they are looking at has no estimate.
        leg: String,
        /// Seconds still to run, measured from this train's own elapsed
        /// time against the population's median. Never negative: a leg
        /// already over-spent contributes nothing, and `overdue` says so.
        remaining_seconds: i64,
        /// The same arithmetic on the population's 10th and 90th
        /// percentiles — the spread, published because one number would
        /// over-claim. Measured spread for a freshly boarded train:
        /// ~35 min typical, 20 to 103 min.
        remaining_low_seconds: i64,
        remaining_high_seconds: i64,
        /// How many arrivals the figure rests on — the binding leg's
        /// count, so it is the smaller of the two when they differ.
        sample_size: usize,
        /// The population and the legs, in words, so the number arrives
        /// with its provenance attached.
        basis: String,
        /// Elapsed has already passed the 90th percentile of everything
        /// measured: this train is outside the whole measured range, and
        /// the surface must look troubled rather than keep counting down
        /// (CLAUDE.md §Diagnosis — "a troubled packet must look
        /// troubled").
        overdue: bool,
    },
    /// No estimate, and why — a thin population, an arrived train, a
    /// missing stamp, or no clock. Never a number.
    Unknown { reason: String },
}

impl Default for TrainEta {
    fn default() -> Self {
        TrainEta::Unknown {
            reason: "no estimate was computed".to_string(),
        }
    }
}

/// The estimate for one train in flight. Pure: its steps, what the
/// arrived population measured, and a clock.
///
/// PHASE-AWARE, because the two legs are the same size. A train that has
/// not merged is estimated across BOTH legs from its boarding stamp; one
/// that has merged is estimated across the remaining leg only, from its
/// MERGE stamp. Telling a merged train the whole-journey figure would
/// roughly double what it has left.
///
/// Every refusal is explicit. A missing stamp is never filled in from
/// the other leg's stamp — that would silently mix the legs, which is the
/// one thing a figure like this must not do.
pub fn train_eta(
    steps: &[Step],
    basis: &EtaBasis,
    now: Option<chrono::DateTime<chrono::Utc>>,
) -> TrainEta {
    let unknown = |reason: String| TrainEta::Unknown { reason };
    let phase = phase_of(steps);
    if phase == TrainPhase::Arrived {
        return unknown("the train has arrived — nothing left to estimate".to_string());
    }
    let EtaBasis::Measured {
        board_to_merge,
        merge_to_arrival,
    } = basis
    else {
        let EtaBasis::Thin {
            arrivals,
            measurable,
        } = basis
        else {
            // Unreachable: `EtaBasis` has two variants and the first was
            // just matched. Written as a statement rather than an
            // `unreachable!()` because library code does not panic.
            return unknown("the arrival history could not be read".to_string());
        };
        return unknown(format!(
            "too little history to measure — {arrivals} arrived train(s) in the window, \
             {measurable} with a readable leg, {MIN_ETA_ARRIVALS} needed"
        ));
    };
    let Some(now) = now else {
        return unknown(
            "no clock — this read-model never reaches for wall time, so elapsed \
             cannot be measured"
                .to_string(),
        );
    };
    // Post-merge: only the remaining leg, measured from the merge.
    let post_merge = matches!(phase, TrainPhase::Deploying | TrainPhase::Converging);
    let (leg, from, legs): (&str, Option<&str>, Vec<&LegSample>) = if post_merge {
        (
            "merge → arrival",
            completed_at(find_step(steps, &MERGED)),
            vec![merge_to_arrival],
        )
    } else {
        (
            "boarding → arrival",
            boarded_at(steps),
            vec![board_to_merge, merge_to_arrival],
        )
    };
    let Some(started) = from.and_then(parse_instant) else {
        return unknown(format!(
            "no {} stamp on this train — elapsed time is unreadable, and an \
             estimate from the other leg's stamp would mix the legs",
            if post_merge { "merge" } else { "boarding" }
        ));
    };
    let elapsed = (now - started).num_seconds().max(0);
    // The leg under way is spent down by the elapsed time; every leg
    // after it is still whole. `legs[0]` is always the one under way.
    let project = |pick: fn(&LegSample) -> i64| -> i64 {
        legs.iter()
            .enumerate()
            .map(|(i, s)| {
                let v = pick(s);
                if i == 0 { (v - elapsed).max(0) } else { v }
            })
            .sum()
    };
    let high = legs.iter().map(|s| s.high).sum::<i64>();
    TrainEta::Estimate {
        leg: leg.to_string(),
        remaining_seconds: project(|s| s.median),
        remaining_low_seconds: project(|s| s.low),
        remaining_high_seconds: project(|s| s.high),
        sample_size: legs.iter().map(|s| s.n).min().unwrap_or(0),
        basis: if post_merge {
            format!(
                "median of {} measured merge→arrival legs (10th–90th: {}–{}s)",
                merge_to_arrival.n, merge_to_arrival.low, merge_to_arrival.high
            )
        } else {
            format!(
                "median of recent arrivals — boarding→merge from {}, merge→arrival from {}",
                board_to_merge.n, merge_to_arrival.n
            )
        },
        overdue: elapsed > high,
    }
}

/// The alarm thresholds the yard runs on, surfaced from the delivery
/// policy so the page names the same numbers the conductor enforces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyThresholds {
    pub stall_hours: Option<i32>,
    pub max_red_trains: Option<i32>,
}

pub fn policy_thresholds(policy: Option<&DeliveryPolicyRow>) -> PolicyThresholds {
    PolicyThresholds {
        stall_hours: policy.map(|p| p.stall_hours),
        max_red_trains: policy.map(|p| p.max_red_trains),
    }
}

/// A green gate-run whose branch has no car — gated green, never parked,
/// so it never reached the dock and cannot board. The read-model's cheap
/// stranded signal, the same cross-ref `boss orient` runs (§9a: one
/// derivation, not a second definition — this operates on `Job` structs
/// where orient's operates on JSON, because the two live in different
/// crates; the RULE is identical and pinned by a test each side).
///
/// Carries the PACKET, not just the branch: the web approach lane draws
/// a wagon per row and needs the gate-run's id to open it and its head
/// to label it. Those used to be recovered client-side by re-scanning a
/// window of gate-runs — which is exactly how a second, poorer copy of
/// "is this green spent?" grew in `apps/web/src/it/yard/yard.ts` and
/// drew a phantom wagon for a re-railed branch all day on 2026-09-10.
/// Served here, the lens has nothing left to judge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrandedGreen {
    pub branch: String,
    /// The gate-run packet this green belongs to.
    pub packet_id: String,
    /// The head it gated, from `metadata.sha`, when it named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    /// When the gate-run opened, as [`opened_since`] reads it.
    pub since: String,
}

/// A green gate-run no car claims: gated green, never parked. Held or
/// stranded is decided by ONE predicate — whether the gate-run carries
/// a `hold` reason — so the two lists cannot overlap or leave a gap
/// between them. A `hold` written as `true` (no reason) is still a
/// hold; the reason then reads "no reason recorded", the same words the
/// web approach lens uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldGreen {
    pub branch: String,
    /// Why the operator gated without parking — the `hold` marker text.
    pub reason: String,
    /// When the gate-run opened, as [`opened_since`] reads it.
    pub since: String,
    /// The gate-run packet, so a surface can open it. See [`StrandedGreen`].
    pub packet_id: String,
    /// The head it gated, from `metadata.sha`, when it named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
}

/// A green gate-run that never became a car — the one classification
/// behind both `stranded` and `held`.
///
/// ONE DEFINITION, not a second copy (CLAUDE.md §9a): the rule lives in
/// [`crate::stranded`], which the CLI census and the conductor's
/// stranded-green ALARM read too. It drifted here once already — the
/// yard learned to exclude a re-railed or held green on 2026-09-08 and
/// the alarm did not, which filed four false STRANDED GREEN packets the
/// next morning (e60398dc). This function is now only the SHAPE
/// adapter: typed `Job`/`Step` in, the shared predicate's answer out.
fn unparked_green(
    gate_run: &Job,
    steps: &[Step],
    car_branches: &[String],
) -> Option<(String, Option<String>)> {
    let found =
        stranded::unparked_green(&gate_run.metadata, steps.iter().map(|s| &s.metadata), |b| {
            car_branches.iter().any(|c| c == b)
        })?;
    Some((found.branch, found.hold))
}

/// A gate-run is stranded when it is an unparked green WITHOUT a hold:
/// gated on purpose but forgotten. A held green is deliberately
/// waiting (a car that must land at a timed restart, or behind another
/// car) and lists under [`held_greens`] instead.
fn gate_run_is_stranded(gate_run: &Job, steps: &[Step], car_branches: &[String]) -> Option<String> {
    match unparked_green(gate_run, steps, car_branches) {
        Some((branch, None)) => Some(branch),
        _ => None,
    }
}

/// Every stranded green among `(gate_run, steps)` pairs, de-duped and
/// sorted — branch names an operator can rescue or drop.
pub fn stranded_greens(
    gate_runs: &[(Job, Vec<Step>)],
    car_branches: &[String],
) -> Vec<StrandedGreen> {
    let mut out: Vec<StrandedGreen> = Vec::new();
    for (g, steps) in gate_runs {
        if let Some(branch) = gate_run_is_stranded(g, steps, car_branches)
            && !out.iter().any(|s| s.branch == branch)
        {
            out.push(StrandedGreen {
                branch,
                packet_id: g.id.to_string(),
                sha: sha_of(g),
                since: opened_since(g),
            });
        }
    }
    out.sort_by(|a, b| a.branch.cmp(&b.branch));
    out
}

/// The head a gate-run gated, from `metadata.sha`. Blank is no sha — a
/// reaped or refused run carries the key with nothing in it, and a
/// surface drawing an empty head is worse than one drawing none.
fn sha_of(g: &Job) -> Option<String> {
    meta_str(&g.metadata, "sha")
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

/// Every HELD green among `(gate_run, steps)` pairs — unparked greens
/// carrying a `hold` reason — de-duped by branch (first seen wins) and
/// sorted. Rendered in its own list, in a neutral colour: a brake
/// deliberately on is not an alarm.
pub fn held_greens(gate_runs: &[(Job, Vec<Step>)], car_branches: &[String]) -> Vec<HeldGreen> {
    let mut out: Vec<HeldGreen> = Vec::new();
    for (g, steps) in gate_runs {
        if let Some((branch, Some(reason))) = unparked_green(g, steps, car_branches)
            && !out.iter().any(|h| h.branch == branch)
        {
            out.push(HeldGreen {
                branch,
                reason,
                since: opened_since(g),
                packet_id: g.id.to_string(),
                sha: sha_of(g),
            });
        }
    }
    out.sort_by(|a, b| a.branch.cmp(&b.branch));
    out
}

/// The verdict a gate-run recorded, read off any of its steps'
/// `metadata.verdict` — the same field `stranded_greens` reads for
/// green, generalized. `None` means no step has reported yet: the gate
/// is still running (in-flight). The runner writes exactly one verdict
/// onto the `record-verdict` step (`green` / `failed` / `lost` /
/// `unreadable`), so the first non-empty one is the answer.
fn gate_run_verdict(steps: &[Step]) -> Option<&str> {
    steps
        .iter()
        .find_map(|s| meta_str(&s.metadata, "verdict"))
        .filter(|v| !v.is_empty())
}

/// The failing check a red gate-run named, from its receipt — the
/// entries whose `result` is not `pass`, joined `a, b`. The receipt is a
/// JSON STRING in the `record-verdict` step's `metadata.receipt` (the
/// same encoding `boss receipt` parses), so it needs a second parse; a
/// runner that died before a receipt leaves prose there, which fails the
/// parse and correctly reads as "no named check". `None` when nothing
/// was named.
///
/// TWO SHAPES, BOTH REAL. `infra/gate.sh` writes `checks` — every check
/// with its result and its duration — but until 2026-09-09 the runner
/// reduced its receipt to `{verdict, head, mode, fails}` before
/// reporting, so no receipt reaching a packet HAD a `checks` array and
/// this reader was silent on every real gate-run. It now reports the
/// whole receipt. `checks` is the primary record and wins; `fails` is
/// read for the receipts already sitting on landed cars, which nothing
/// will rewrite.
fn failing_check(steps: &[Step]) -> Option<String> {
    let raw = steps
        .iter()
        .find_map(|s| meta_str(&s.metadata, "receipt"))?;
    let receipt: Value = serde_json::from_str(raw).ok()?;
    let failed: Vec<String> = match receipt.get("checks").and_then(Value::as_array) {
        Some(checks) => checks
            .iter()
            .filter(|c| c.get("result").and_then(Value::as_str) != Some("pass"))
            .filter_map(|c| c.get("name").and_then(Value::as_str).map(str::to_string))
            .collect(),
        None => receipt
            .get("fails")
            .and_then(Value::as_array)?
            .iter()
            .filter_map(|f| f.as_str().map(str::to_string))
            .collect(),
    };
    (!failed.is_empty()).then(|| failed.join(", "))
}

/// One gate currently being assessed — an open gate-run that has not
/// reported a verdict. The Approach draws these into its parallel gate
/// SLOTS so capacity and usage read at a glance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveGate {
    pub branch: String,
    pub packet_id: String,
    /// When the gate-run opened: the `opened_at` instant `boss gate`
    /// stamps (RFC3339) when the packet carries one, else the row's
    /// `opened_on` date (see [`opened_since`]). A string either way — a
    /// reader that parses an instant gets the elapsed time a gate bay
    /// needs; one that only knew the date keeps reading.
    pub since: String,
    /// True when this run has been "active" longer than a gate Job can
    /// physically live: it is not gating, it is a corpse holding a slot.
    /// A gate pod that dies without recording a verdict leaves its packet
    /// at `record-verdict` forever, and the surface then draws it exactly
    /// like a healthy run — on 2026-09-04 two such ghosts sat in the
    /// gates section for 17 hours with their branches long since landed,
    /// and a third silently ate a car that was never gated at all.
    #[serde(default)]
    pub stale: bool,
}

/// The metadata key a gate-run carries while it is WAITING for a
/// concurrency slot rather than running in one.
///
/// ONE DEFINITION, because two readers need it (CLAUDE.md §9a):
/// `boss gate` stamps it when the build node is at the bound and clears
/// it the instant it creates the runner Job, and this module reads it to
/// keep a waiting run out of the gate bays. A queued run has no Job, no
/// pod and no workspace — counted as active it would fill a bay that is
/// genuinely free, and past [`GATE_MAX_ACTIVE_HOURS`] it would render
/// `stale`, reporting a corpse for a run that never started. The value
/// is the RFC3339 instant the place in line was taken, which is also the
/// queue's ordering key: oldest first.
pub const QUEUED_AT: &str = "queued_at";

/// Is this gate-run waiting for a slot rather than holding one?
///
/// A BLANK marker is no marker: `queued_at: ""` is what a metadata merge
/// that wrote an empty string instead of deleting the key leaves behind,
/// and reading that as "queued" would hide a genuinely running gate from
/// the bays.
fn queued_for_a_slot(g: &Job) -> bool {
    meta_str(&g.metadata, QUEUED_AT).is_some_and(|s| !s.is_empty())
}

/// How long a gate-run may legitimately stay active before the surface
/// calls it dead. This is the gate Job's own `activeDeadlineSeconds`
/// (10800 = 3h) from `infra/gate-runner/gate-runner.yaml`: past it,
/// Kubernetes has already killed the Job, so a packet still claiming to
/// gate cannot be. CLAUDE.md §9a — the number lives in two places and
/// the manifest is the authority; move this if that moves. A ceiling,
/// not an expectation: a normal gate finishes in ~15-90 min.
pub const GATE_MAX_ACTIVE_HOURS: i64 = 3;

/// One gate-run WAITING for a slot: filed, ordered, but not running.
///
/// `boss gate --wait` takes a place in line when the build node is at
/// its concurrency bound rather than refusing (db7f7b73), and a queued
/// run is correctly kept out of the bays — but nothing showed it, so
/// three busy bays with two waiting looked exactly like three busy bays,
/// and an operator watching the floor could not tell a queued gate from
/// one that never launched. This is that missing lane.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueuedGate {
    pub branch: String,
    pub packet_id: String,
    /// The [`QUEUED_AT`] stamp — when the place in line was taken, which
    /// is also the ordering key.
    pub queued_at: String,
    /// Place in line: 1-based, oldest `queued_at` first.
    pub position: i32,
    /// How long it has waited so far, in seconds. `None` without a clock
    /// or with a stamp that does not parse — absence is not evidence.
    pub waiting_seconds: Option<i64>,
    /// How much longer it is expected to wait, in seconds — see
    /// [`estimated_waits`]. `None` when nothing has been measured: a
    /// wait nobody can derive is reported unknown, never invented.
    pub estimated_wait_seconds: Option<i64>,
}

/// The gate slots the Approach renders: how many gates run at once
/// (`capacity`, from the delivery policy — never a constant baked into
/// the page), which cars occupy them right now (`active`), and who is
/// waiting for one (`queued`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gates {
    pub capacity: i32,
    pub active: Vec<ActiveGate>,
    /// The line, in its own order. Empty on an older payload.
    #[serde(default)]
    pub queued: Vec<QueuedGate>,
    /// The gate duration this window MEASURED (median seconds), which
    /// every estimate above is derived from. `None` when no run in the
    /// window can be measured; a surface then says so rather than
    /// drawing a wait from a constant.
    #[serde(default)]
    pub typical_seconds: Option<i64>,
}

/// The instant a gate-run reported its verdict: the verdict step's
/// server-stamped `completed_at`, else the conductor's `completed_at`
/// metadata stamp, else the job's `closed_at` (a gate-run closes on its
/// verdict). Three readers because a run recorded by an older path
/// carries only the last of them, and a duration that cannot be read is
/// simply not measured.
fn verdict_instant(job: &Job, steps: &[Step]) -> Option<chrono::DateTime<chrono::Utc>> {
    steps
        .iter()
        .find(|s| meta_str(&s.metadata, "verdict").is_some_and(|v| !v.is_empty()))
        .and_then(|s| {
            s.completed_at
                .or_else(|| completed_at(Some(s)).and_then(parse_instant))
        })
        .or_else(|| meta_str(&job.metadata, "closed_at").and_then(parse_instant))
}

/// Every gate duration this window can MEASURE, in seconds: `opened_at`
/// to the verdict instant. Only a JUDGED run counts — `lost` and
/// `unreadable` measure a death, not a gate — and a span past
/// [`GATE_MAX_ACTIVE_HOURS`] is a corpse the reaper settled rather than
/// a slow gate, so it is dropped. A run missing either end is not
/// guessed at.
fn gate_durations(gate_runs: &[(Job, Vec<Step>)]) -> Vec<i64> {
    let ceiling = GATE_MAX_ACTIVE_HOURS * 3600;
    gate_runs
        .iter()
        .filter(|(_, steps)| matches!(gate_run_verdict(steps), Some("green" | "failed")))
        .filter_map(|(g, steps)| {
            let opened = meta_str(&g.metadata, "opened_at").and_then(parse_instant)?;
            let done = verdict_instant(g, steps)?;
            Some((done - opened).num_seconds())
        })
        .filter(|s| *s > 0 && *s <= ceiling)
        .collect()
}

/// How long a gate takes HERE, measured: the median of
/// [`gate_durations`]. The median, not the mean, because one reaped
/// three-hour run would drag an average across every estimate on the
/// floor. `None` when the window measured nothing.
fn typical_gate_seconds(gate_runs: &[(Job, Vec<Step>)]) -> Option<i64> {
    let mut d = gate_durations(gate_runs);
    if d.is_empty() {
        return None;
    }
    d.sort_unstable();
    d.get(d.len() / 2).copied()
}

/// The wait each place in line can expect, in seconds — one entry per
/// queued run, in queue order.
///
/// The model is the floor itself: each slot frees when the run in it
/// reaches the measured `typical` duration, a slot the policy allows but
/// nothing occupies frees now, and each queued run takes the earliest
/// free slot and holds it for a whole gate. A run whose start cannot be
/// read is assumed to have just started, so its slot's estimate is the
/// long one: over-stating a wait leaves an operator early, under-stating
/// it makes a working queue look stuck.
fn estimated_waits(
    active: &[ActiveGate],
    capacity: i32,
    queued: usize,
    typical: i64,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<i64> {
    let mut free: Vec<i64> = active
        .iter()
        .map(|a| {
            let elapsed = parse_instant(&a.since).map_or(0, |t| (now - t).num_seconds());
            (typical - elapsed).max(0)
        })
        .collect();
    // Slots the policy allows that nothing holds are free right now.
    free.resize(free.len().max(usize::try_from(capacity).unwrap_or(0)), 0);
    (0..queued)
        .map(|_| {
            let Some(i) = (0..free.len()).min_by_key(|i| free[*i]) else {
                return 0;
            };
            let wait = free[i];
            free[i] = wait + typical;
            wait
        })
        .collect()
}

/// The line waiting for a slot, in the order the system of record set:
/// oldest [`QUEUED_AT`] first, branch breaking a tie so the order is
/// total and the same across polls.
fn queued_gates(
    gate_runs: &[(Job, Vec<Step>)],
    active: &[ActiveGate],
    capacity: i32,
    now: Option<chrono::DateTime<chrono::Utc>>,
) -> (Vec<QueuedGate>, Option<i64>) {
    let mut waiting: Vec<((&str, &str), &Job)> = gate_runs
        .iter()
        .filter(|(g, _)| g.status == JobStatus::Open)
        .filter(|(_, steps)| gate_run_verdict(steps).is_none())
        .filter(|(g, _)| queued_for_a_slot(g))
        .filter_map(|(g, _)| {
            let stamp = meta_str(&g.metadata, QUEUED_AT).filter(|s| !s.is_empty())?;
            let branch = meta_str(&g.metadata, "branch").filter(|b| !b.is_empty())?;
            // The stamp is a whole-second `Z` instant, so string order is
            // time order — the same reading `active` sorts by; the branch
            // breaks a tie so the order is total across polls.
            Some(((stamp, branch), g))
        })
        .collect();
    waiting.sort_by_key(|(key, _)| *key);
    let typical = typical_gate_seconds(gate_runs);
    let waits = match (now, typical) {
        (Some(n), Some(t)) => estimated_waits(active, capacity, waiting.len(), t, n),
        _ => Vec::new(),
    };
    let queued = waiting
        .iter()
        .enumerate()
        .map(|(i, (_, g))| {
            let stamp = meta_str(&g.metadata, QUEUED_AT).unwrap_or_default();
            QueuedGate {
                branch: meta_str(&g.metadata, "branch")
                    .unwrap_or_default()
                    .to_string(),
                packet_id: g.id.to_string(),
                queued_at: stamp.to_string(),
                position: i32::try_from(i + 1).unwrap_or(i32::MAX),
                waiting_seconds: now
                    .zip(parse_instant(stamp))
                    .map(|(n, t)| (n - t).num_seconds()),
                estimated_wait_seconds: waits.get(i).copied(),
            }
        })
        .collect();
    (queued, typical)
}

/// A car that gated RED and is waiting for rework — the garage. Named
/// with its failing check (when the verdict recorded one) so an operator
/// reads WHAT to fix without opening the packet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GaragedCar {
    pub branch: String,
    /// The failing check the verdict named (`test`, `clippy`, …), or
    /// `None` when the receipt named none (a run that died outside a
    /// check).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_check: Option<String>,
    /// When the failing gate-run opened — the instant when stamped, else
    /// the date, as [`opened_since`] reads it.
    pub since: String,
    /// The gate-run packet, so a surface can open it. See [`StrandedGreen`].
    pub packet_id: String,
    /// The head it gated, from `metadata.sha`, when it named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
}

/// A car whose latest gate-run was NEVER JUDGED — the gate exit. The
/// reaper settles a dead runner as `lost` (no verdict was produced) and
/// a receipt that will not parse as `unreadable`; neither says anything
/// about the branch, which is why [`garage`] refuses to call them red.
/// They are not nothing, though: the change asked a question and got no
/// answer, so it is standing where an operator must re-gate it. Drawn on
/// its own siding for exactly that reason — "we do not know" must read
/// neither as rework nor as fine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimboCar {
    pub branch: String,
    /// The verdict as recorded — `lost` or `unreadable`.
    pub verdict: String,
    /// When the gate-run opened, as [`opened_since`] reads it.
    pub since: String,
    /// The gate-run packet, so a surface can open it.
    pub packet_id: String,
    /// The head it gated, from `metadata.sha`, when it named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
}

/// When a gate-run opened, as finely as the record knows it: the
/// `opened_at` instant `boss gate` stamps (RFC3339, whole seconds, `Z` —
/// `car::stamp`) when it is present and parses, else the row's
/// `opened_on` date. The date alone told a gate bay nothing about
/// elapsed time: every run opened "today" for a whole day. A stamp that
/// does not parse is no stamp.
fn opened_since(g: &Job) -> String {
    meta_str(&g.metadata, "opened_at")
        .filter(|s| parse_instant(s).is_some())
        .map_or_else(|| g.opened_on.to_string(), str::to_string)
}

/// The in-flight gates, sized to the policy's capacity. Active gate-runs
/// (open, no verdict yet) are sorted by `since` then `branch` so slot
/// assignment is deterministic — the same run lands in the same slot
/// across polls. (`since` is a whole-second `Z` stamp or a date, so the
/// string order is the time order.)
pub fn gates(
    gate_runs: &[(Job, Vec<Step>)],
    capacity: i32,
    now: Option<chrono::DateTime<chrono::Utc>>,
) -> Gates {
    // Past this instant a run has outlived the Job meant to be running it.
    let dead_before = now.map(|n| n - chrono::Duration::hours(GATE_MAX_ACTIVE_HOURS));
    let mut active: Vec<ActiveGate> = gate_runs
        .iter()
        .filter(|(g, _)| g.status == JobStatus::Open)
        .filter(|(_, steps)| gate_run_verdict(steps).is_none())
        // A run WAITING for a slot is not occupying one. See [`QUEUED_AT`].
        .filter(|(g, _)| !queued_for_a_slot(g))
        .filter_map(|(g, _)| {
            let branch = meta_str(&g.metadata, "branch").filter(|b| !b.is_empty())?;
            // `opened_at` is the RFC3339 instant; `opened_on` is only a
            // date, too coarse to tell a long-running gate from a dead
            // one on the same day. No stamp or no clock means no claim:
            // absence is not evidence.
            let opened_at = meta_str(&g.metadata, "opened_at").and_then(parse_instant);
            Some(ActiveGate {
                branch: branch.to_string(),
                packet_id: g.id.to_string(),
                since: opened_since(g),
                stale: match (dead_before, opened_at) {
                    (Some(cutoff), Some(at)) => at < cutoff,
                    _ => false,
                },
            })
        })
        .collect();
    active.sort_by(|a, b| a.since.cmp(&b.since).then_with(|| a.branch.cmp(&b.branch)));
    // The line waiting for one of those slots, and the measured gate
    // duration every estimate in it is derived from.
    let (queued, typical_seconds) = queued_gates(gate_runs, &active, capacity, now);
    Gates {
        capacity,
        active,
        queued,
        typical_seconds,
    }
}

/// The garage: cars whose MOST-RECENT gate-run is red. A branch that
/// failed then re-gated green is fixed and does not show — so gate-runs
/// are grouped by branch, the latest kept (by the `opened_at` instant,
/// falling back to `opened_on`, tie-broken by packet id so the order is
/// total), and the garage is every branch
/// whose latest verdict is `failed`: a red that a check actually judged.
/// `lost` and `unreadable` are NOT reds — a lost run says "no verdict was
/// produced" (the runner died, the reaper buried it), an unreadable one
/// says the receipt could not be parsed — and a branch nobody judged is
/// not awaiting rework. Measured 2026-09-05: the garage held
/// fix/lean-ci-builds on a reaper-settled `lost` run while the branch's
/// head was already reachable from main, drawing rework where there was
/// none. In-flight runs (no verdict) do not count as the latest state: a
/// branch being re-gated right now is in the SLOTS, not the garage, so a
/// still-running retry is only kept as latest when it is the sole run.
/// Sorted by branch for a stable render.
pub fn garage(gate_runs: &[(Job, Vec<Step>)], settled_branches: &[String]) -> Vec<GaragedCar> {
    let mut out: Vec<GaragedCar> = unsettled_latest(gate_runs, settled_branches)
        .into_iter()
        .filter_map(|(branch, g, steps)| {
            let verdict = gate_run_verdict(steps)?;
            // Only a JUDGED red is rework. Green is fixed; lost and
            // unreadable were never judged (they are [`limbo`]); an
            // in-flight run has no verdict and was filtered above.
            (verdict == "failed").then(|| GaragedCar {
                branch: branch.to_string(),
                failed_check: failing_check(steps),
                since: opened_since(g),
                packet_id: g.id.to_string(),
                sha: sha_of(g),
            })
        })
        .collect();
    out.sort_by(|a, b| a.branch.cmp(&b.branch));
    out
}

/// The gate exit: cars whose most-recent gate-run was never judged —
/// `lost` (the runner died, the reaper buried it) or `unreadable` (the
/// receipt would not parse). The other half of [`garage`]'s partition
/// over the same latest-run-per-branch grouping, so a branch can never
/// be in both and no judged-or-not state falls between them.
/// Sorted by branch for a stable render.
pub fn limbo(gate_runs: &[(Job, Vec<Step>)], settled_branches: &[String]) -> Vec<LimboCar> {
    let mut out: Vec<LimboCar> = unsettled_latest(gate_runs, settled_branches)
        .into_iter()
        .filter_map(|(branch, g, steps)| {
            let verdict = gate_run_verdict(steps)?;
            matches!(verdict, "lost" | "unreadable").then(|| LimboCar {
                branch: branch.to_string(),
                verdict: verdict.to_string(),
                since: opened_since(g),
                packet_id: g.id.to_string(),
                sha: sha_of(g),
            })
        })
        .collect();
    out.sort_by(|a, b| a.branch.cmp(&b.branch));
    out
}

/// The latest gate-run per branch, minus the branches whose car has
/// settled — the one grouping [`garage`] and [`limbo`] both partition,
/// so the two cannot disagree about which run is a branch's current
/// state (CLAUDE.md §9a).
///
/// A branch whose car has SETTLED is not awaiting anything — the work
/// finished, by landing or by being dropped. Its last gate-run under
/// that name keeps its verdict forever, because a car fixed by
/// re-railing onto a fresh branch, or squash-merged and deleted, never
/// re-gates under the old name to clear it. Without this the garage
/// accumulates ghosts: on 2026-09-04 all three entries were landed work
/// (branches deleted from the forge, packets closed), which makes a
/// rework queue nobody can trust. A branch with NO car at all is kept —
/// a red never parks, so that is the ordinary case the garage exists for.
fn unsettled_latest<'a>(
    gate_runs: &'a [(Job, Vec<Step>)],
    settled_branches: &[String],
) -> Vec<(&'a str, &'a Job, &'a [Step])> {
    use std::collections::HashMap;
    // The instant a run opened: `metadata.opened_at` (RFC3339, stamped
    // by `boss gate`) when present, else the row's `opened_on` at
    // midnight. `opened_on` alone has DAY resolution, so two gates of
    // one branch on one day tied on it and the packet-id tiebreak — a
    // random UUID — chose between a morning red and an afternoon green
    // by luck: on 2026-09-05 feat/the-ci-host-check-is-live sat in the
    // garage after its second gate went green. The id still breaks a
    // true tie, so the order stays total.
    let opened_instant = |g: &Job| -> (chrono::NaiveDateTime, String) {
        let at = meta_str(&g.metadata, "opened_at")
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.naive_utc())
            .unwrap_or_else(|| g.opened_on.and_hms_opt(0, 0, 0).unwrap_or_default());
        (at, g.id.to_string())
    };
    let mut latest: HashMap<&str, (&Job, &[Step])> = HashMap::new();
    for (g, steps) in gate_runs {
        let Some(branch) = meta_str(&g.metadata, "branch").filter(|b| !b.is_empty()) else {
            continue;
        };
        if settled_branches.iter().any(|b| b == branch) {
            continue;
        }
        match latest.get(branch) {
            Some((prev, _)) if opened_instant(prev) >= opened_instant(g) => {}
            _ => {
                latest.insert(branch, (g, steps.as_slice()));
            }
        }
    }
    latest
        .into_iter()
        .map(|(branch, (g, steps))| (branch, g, steps))
        .collect()
}

/// What the conductor is doing, in its own record — and, crucially,
/// whether it has been heard from at all.
///
/// WHY THIS EXISTS. Every trouble signal on this board is written by
/// some component, and the conductor writes most of them. So when the
/// conductor stops, its failures render as HEALTH: on 2026-09-04 it was
/// dead for two and a half hours (a fresh pod lost the git credential
/// its `--global` config held) while the yard drew full docks, healthy
/// trains and live gates. Eleven distinct "the board said fine and it
/// was not" incidents in two days share this one shape — absence of a
/// trouble flag rendered as absence of trouble.
///
/// The fix is to make SILENCE a first-class reading rather than a gap.
/// The conductor already records every cadence firing — rule, verb,
/// instant, and (since the 2026-09-04 cooldown fix) its exit code — so
/// "when did it last act, and did that act succeed" is answerable from
/// the record without asking the conductor anything.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConductorHealth {
    /// When it last fired the rule this health is measured against.
    /// `None` means never — a conductor that has not acted in the
    /// window we keep, which is itself the loudest possible reading.
    pub last_seen: Option<String>,
    /// Minutes since `last_seen`. `None` when there is no firing or no
    /// clock: no reading, rather than a fabricated zero.
    pub silent_for_minutes: Option<i64>,
    /// The rule's OWN declared interval — the conductor's stated
    /// heartbeat, taken from the cadence registry rather than a
    /// constant here, so changing the cadence moves the expectation
    /// with it.
    pub expected_every_minutes: Option<i64>,
    /// True when silence has run past `SILENT_AFTER_INTERVALS` of that
    /// declared heartbeat. THE FIELD THE BOARD SHOULD DEFER TO: while
    /// this is true, every section whose truth the conductor writes is
    /// last-known-good, not current.
    pub silent: bool,
    /// The verb it last ran, and how that went. A conductor that is
    /// running but FAILING every pass looks identical to a healthy one
    /// unless the exit code is on the surface — 2026-09-04 again, where
    /// `rc=3` (preflight refusing) repeated for hours in a log nobody
    /// was reading.
    pub last_verb: Option<String>,
    pub last_rc: Option<i32>,
}

/// How many declared intervals of silence before the board stops
/// trusting what the conductor wrote. Two, not one: a single missed
/// tick is a slow pass or a restart, and crying wolf at every blip
/// trains an operator to ignore the signal — which is how the real
/// outage stayed invisible.
pub const SILENT_AFTER_INTERVALS: i64 = 2;

/// Read the conductor's liveness from its own firing record.
///
/// Pure, so the decision is testable without a database or a clock.
/// Every unknown stays unknown: no firing, or no clock, yields `None`
/// readings and `silent: false` — because "I cannot tell" must not be
/// dressed up as either health or alarm.
pub fn conductor_health(
    last_fired_at: Option<chrono::DateTime<chrono::Utc>>,
    last_verb: Option<&str>,
    last_rc: Option<i32>,
    expected_every_minutes: Option<i64>,
    now: Option<chrono::DateTime<chrono::Utc>>,
) -> ConductorHealth {
    let silent_for = last_fired_at
        .zip(now)
        .map(|(f, n)| (n - f).num_minutes().max(0));
    let silent = match (silent_for, expected_every_minutes) {
        (Some(mins), Some(every)) if every > 0 => mins > every * SILENT_AFTER_INTERVALS,
        _ => false,
    };
    ConductorHealth {
        last_seen: last_fired_at.map(|t| t.to_rfc3339()),
        silent_for_minutes: silent_for,
        expected_every_minutes,
        silent,
        last_verb: last_verb.map(str::to_string),
        last_rc,
    }
}

/// The whole yard status — the one payload the surface renders. Every
/// field is a function of the inputs; nothing is fetched here, so the
/// assembly is testable without a database. The HTTP handler reads the
/// rows and calls this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct YardStatus {
    /// Open pr-trains, each with where it sits and (if stuck) why.
    pub trains: Vec<TrainStatus>,
    /// Parked cars ready to board.
    pub dock: Vec<DockCar>,
    /// The boarding predicate, from the live cadence rows.
    pub boarding: BoardingPredicate,
    /// The last few closed trains, newest first, with outcome + journey.
    pub recent: Vec<RecentTrain>,
    /// Green gate-runs no car claims — cheap stranded signal.
    pub stranded: Vec<StrandedGreen>,
    /// Green gate-runs an operator HELD off the dock on purpose, with
    /// the reason — the other half of the stranded predicate, so a
    /// deliberate hold never reads as a forgotten green. Absent on an
    /// older payload → empty.
    #[serde(default)]
    pub held: Vec<HeldGreen>,
    /// The parallel gate slots: capacity (from the policy) + the cars
    /// occupying them right now.
    pub gates: Gates,
    /// Cars whose latest gate-run is red — waiting for rework.
    pub garage: Vec<GaragedCar>,
    /// Cars whose latest gate-run was never judged — the gate exit.
    /// Absent on an older payload → empty.
    #[serde(default)]
    pub limbo: Vec<LimboCar>,
    /// The alarm thresholds the yard enforces, from the delivery policy.
    pub policy: PolicyThresholds,
}

/// How many trains each of the handler's two train reads fetches: every
/// in-flight train (a handful, bounded by policy) and the recent tail.
/// Named here, beside `RECENT_LIMIT`, so the test that pins the window
/// (`a_day_of_arrivals_does_not_push_the_open_train_off_the_board`)
/// seeds exactly one window of arrivals rather than a second copy of
/// this number (CLAUDE.md §9a).
pub const TRAIN_WINDOW: i64 = 60;

/// How many recent trains the status carries. Enough to read a trend in
/// arrivals/cancellations without turning the surface into a history log
/// — the terminal report owns the long view.
pub const RECENT_LIMIT: usize = 8;

/// Assemble the full status from the rows the handler fetched.
///
/// - `open_trains` — open pr-train Jobs with their steps. Only those
///   still before their merge count as the track hold (`holds_the_track`).
/// - `closed_trains` — recently-closed pr-train Jobs with their steps,
///   already ordered newest-first by the caller (the adapter orders by
///   `opened_on desc`, which for a batch of same-day trains is close
///   enough; the terminal report owns precise cycle stats).
/// - `dock_cars` — the loading-dock queue's parked cars.
/// - `rules` — the active cadence rows.
/// - `last_board` — the board rule's last firing, which the cooldown
///   hold is read from.
/// - `policy` — the active delivery policy, if any.
/// - `gate_runs` / `car_branches` — for the stranded cross-ref.
/// - `settled_car_branches` — branches whose car reached a terminal; the
///   garage drops these, since settled work is not awaiting rework.
/// - `arrived_trains` — pr-train Jobs whose outcome is `arrived`, the
///   population every in-flight train's ETA is measured against. A
///   SEPARATE read from `closed_trains` on purpose: the recent window is
///   overwhelmingly refused boards (696 of 1,014 pr-trains on record are
///   cancelled, and only ONE of the 40 most recent had arrived), so the
///   arrived population has to be narrowed in the query. Steps are not
///   needed — every timing is in the Job's own metadata.
#[allow(clippy::too_many_arguments)]
pub fn build_status(
    open_trains: &[(Job, Vec<Step>)],
    closed_trains: &[(Job, Vec<Step>)],
    dock_cars: &[Job],
    rules: &[CadenceRuleRow],
    last_board: Option<&LastFiring>,
    policy: Option<&DeliveryPolicyRow>,
    gate_runs: &[(Job, Vec<Step>)],
    car_branches: &[String],
    settled_car_branches: &[String],
    arrived_trains: &[Job],
    now: Option<chrono::DateTime<chrono::Utc>>,
) -> YardStatus {
    // The instant a train must have completed SOMETHING after, or it is
    // standing still. Computed once so train_status stays a pure
    // comparison, and only when the policy declares a window.
    // Both derivations below follow one rule: no clock means no claim.
    // A caller without a clock (the denied-reader's empty status) gets a
    // status that asserts no trouble rather than inventing a reading —
    // and this keeps wall-clock out of the read-model entirely, which is
    // what `no-wallclock` protects (sim runs must not see real time).
    let stall_before = now
        .zip(policy.map(|p| p.stall_hours).filter(|h| *h > 0))
        .map(|(n, h)| n - chrono::Duration::hours(i64::from(h)));
    // What the arrived population measured, once: the sort happens here
    // rather than per train, and every train on the board is measured
    // against the same history.
    let eta = eta_basis(arrived_trains);
    let trains = open_trains
        .iter()
        .map(|(j, s)| train_status(j, s, stall_before, &eta, now))
        .collect();
    let dock: Vec<DockCar> = dock_cars.iter().map(dock_car).collect();
    // The track: open trains still before their merge. A merged train
    // waiting to deploy or converge is rendered as such above and holds
    // nothing — the next consist merges on top of it.
    let on_track = open_trains
        .iter()
        .filter(|(_, s)| holds_the_track(s))
        .count();
    let boarding = boarding_predicate(rules, dock.len(), last_board, on_track, now);
    let recent = closed_trains
        .iter()
        .take(RECENT_LIMIT)
        .map(|(j, s)| recent_train(j, s))
        .collect();
    // Gate capacity is the policy's `gate_max_concurrent` — the number
    // `boss gate` enforces, drawn as slots here so the two never drift.
    // No policy → the CLI's own compiled fallback, so the page shows the
    // same bound a gate would obey with an unreachable registry.
    let capacity = policy.map_or(COMPILED_GATE_MAX_CONCURRENT, |p| p.gate_max_concurrent);
    YardStatus {
        trains,
        dock,
        boarding,
        recent,
        stranded: stranded_greens(gate_runs, car_branches),
        held: held_greens(gate_runs, car_branches),
        gates: gates(gate_runs, capacity, now),
        garage: garage(gate_runs, settled_car_branches),
        limbo: limbo(gate_runs, settled_car_branches),
        policy: policy_thresholds(policy),
    }
}

/// The compiled gate-concurrency fallback, mirrored here for the
/// no-policy case. It equals `boss-cli`'s `DEFAULT_MAX_CONCURRENT` /
/// `COMPILED_GATE_MAX_CONCURRENT` (3): a page with no policy shows the
/// same bound a gate obeys with an unreachable registry, and the pin
/// `the_no_policy_capacity_matches_the_cli_compiled_fallback` names this
/// if it ever drifts (CLAUDE.md §9a — the two live in different crates,
/// so equality is the mechanism).
pub const COMPILED_GATE_MAX_CONCURRENT: i32 = 3;

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::job::{Job, JobStatus, Priority, Step, StepStatus, Subject};
    use serde_json::json;

    fn train(_steps: Vec<Step>, metadata: Value) -> Job {
        Job {
            id: Default::default(),
            kind: "pr-train".into(),
            workflow_version: 16,
            subject: Subject::new("custom", "train/x"),
            title: "train x".into(),
            owner_id: "emp-david".into(),
            status: JobStatus::Open,
            priority: Priority::Standard,
            opened_on: chrono::NaiveDate::from_ymd_opt(2026, 9, 3).unwrap(),
            due_on: None,
            closed_on: None,
            metadata,
            tags: vec![],
            simulated: false,
        }
    }

    fn at(s: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    /// 2026-09-04: the conductor died at 17:16 and the yard drew full
    /// docks and healthy trains for two and a half hours. Silence has to
    /// be a reading, not a gap.
    #[test]
    fn a_silent_conductor_reads_as_silent() {
        let fired = at("2026-09-04T17:16:00Z");
        // 20-minute heartbeat, 2h39m of silence: well past 2 intervals.
        let h = conductor_health(
            Some(fired),
            Some("reconcile"),
            Some(0),
            Some(20),
            Some(at("2026-09-04T19:55:00Z")),
        );
        assert!(h.silent, "2h39m against a 20m heartbeat must read silent");
        assert_eq!(h.silent_for_minutes, Some(159));
        assert_eq!(h.expected_every_minutes, Some(20));

        // One missed tick is a slow pass or a restart, not an outage —
        // crying wolf at every blip is how the real one gets ignored.
        let h = conductor_health(
            Some(fired),
            Some("reconcile"),
            Some(0),
            Some(20),
            Some(at("2026-09-04T17:41:00Z")),
        );
        assert!(!h.silent, "25m against a 20m heartbeat is one slow tick");
    }

    /// A conductor that RUNS but fails every pass looks identical to a
    /// healthy one unless its exit code is on the surface. rc=3 was the
    /// preflight refusing, repeating for hours in a log nobody read.
    #[test]
    fn a_failing_conductor_carries_its_exit_code() {
        let h = conductor_health(
            Some(at("2026-09-04T19:40:00Z")),
            Some("reconcile"),
            Some(3),
            Some(20),
            Some(at("2026-09-04T19:45:00Z")),
        );
        assert!(
            !h.silent,
            "it is running — the trouble is the rc, not silence"
        );
        assert_eq!(h.last_rc, Some(3));
        assert_eq!(h.last_verb.as_deref(), Some("reconcile"));
    }

    /// "I cannot tell" must not be dressed up as health OR as alarm.
    #[test]
    fn no_firing_and_no_clock_assert_nothing() {
        let none = conductor_health(None, None, None, Some(20), Some(at("2026-09-04T19:00:00Z")));
        assert_eq!(none.last_seen, None);
        assert_eq!(none.silent_for_minutes, None);
        assert!(!none.silent, "never-seen is not a silence measurement");

        let no_clock = conductor_health(
            Some(at("2026-09-04T17:16:00Z")),
            Some("reconcile"),
            Some(0),
            Some(20),
            None,
        );
        assert_eq!(no_clock.silent_for_minutes, None);
        assert!(!no_clock.silent);

        // No declared heartbeat: nothing to measure against.
        let no_rule = conductor_health(
            Some(at("2026-09-04T10:00:00Z")),
            Some("reconcile"),
            Some(0),
            None,
            Some(at("2026-09-04T19:00:00Z")),
        );
        assert_eq!(no_rule.silent_for_minutes, Some(540));
        assert!(
            !no_rule.silent,
            "9h of silence, but no declared interval to call it late"
        );
    }

    /// A fixed instant for status assembly in tests. Never the wall
    /// clock: `no-wallclock` forbids it, and a test that reads real time
    /// is a test that changes answer overnight.
    fn fixed_now() -> Option<chrono::DateTime<chrono::Utc>> {
        Some(
            chrono::DateTime::parse_from_rfc3339("2026-09-04T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc),
        )
    }

    fn step(slug: &str, title: &str, status: StepStatus, metadata: Value) -> Step {
        let mut s = Step::new(Default::default(), "task", title, 0);
        s.spec_slug = Some(slug.into());
        s.status = status;
        s.metadata = metadata;
        s
    }

    fn done(slug: &str, title: &str, at: &str) -> Step {
        step(
            slug,
            title,
            StepStatus::Completed,
            json!({ "completed_at": at }),
        )
    }

    /// The basis a test that is not exercising the ETA passes: no
    /// arrival history read, so every train reads "no estimate" with a
    /// stated reason — the honest value, not a stand-in number.
    fn no_history() -> EtaBasis {
        EtaBasis::Thin {
            arrivals: 0,
            measurable: 0,
        }
    }

    fn rule(basis: &str) -> CadenceRuleRow {
        CadenceRuleRow {
            name: "r".into(),
            verb: "run".into(),
            basis: basis.into(),
            every_minutes: None,
            at_times: None,
            min_dock_depth: None,
            cooldown_minutes: None,
            cadence: None,
            anchor_date: None,
            business_calendar: None,
        }
    }

    // ---- phase ----

    #[test]
    fn a_fresh_train_is_boarding() {
        let steps = vec![step(
            "collect",
            "Collect what is ready to board",
            StepStatus::Ready,
            json!({}),
        )];
        assert_eq!(phase_of(&steps), TrainPhase::Boarding);
    }

    #[test]
    fn pr_open_without_a_ci_verdict_is_awaiting_ci() {
        let steps = vec![
            done(
                "collect",
                "Collect what is ready to board",
                "2026-09-03T06:00:00Z",
            ),
            done("pr", "Open the batched PR", "2026-09-03T06:05:00Z"),
            step("ci", "CI verdict", StepStatus::Active, json!({})),
        ];
        assert_eq!(phase_of(&steps), TrainPhase::AwaitingCi);
    }

    #[test]
    fn ci_done_but_unmerged_is_awaiting_merge() {
        let steps = vec![
            done("pr", "Open the batched PR", "2026-09-03T06:05:00Z"),
            done("ci", "CI verdict", "2026-09-03T06:40:00Z"),
            step("merged", "Merged into main", StepStatus::Ready, json!({})),
        ];
        assert_eq!(phase_of(&steps), TrainPhase::AwaitingMerge);
    }

    #[test]
    fn merged_but_undeployed_is_deploying() {
        let steps = vec![
            done("merged", "Merged into main", "2026-09-03T06:45:00Z"),
            step(
                "deployed",
                "Deployed to the playground",
                StepStatus::Ready,
                json!({}),
            ),
        ];
        assert_eq!(phase_of(&steps), TrainPhase::Deploying);
    }

    #[test]
    fn deployed_but_unconverged_is_converging() {
        let steps = vec![
            done(
                "deployed",
                "Deployed to the playground",
                "2026-09-03T06:50:00Z",
            ),
            step(
                "converged",
                "Cluster converged",
                StepStatus::Ready,
                json!({}),
            ),
        ];
        assert_eq!(phase_of(&steps), TrainPhase::Converging);
    }

    #[test]
    fn a_preconverged_train_with_no_converged_step_arrives_at_deploy() {
        // Older in-flight trains pinned to a pre-converged workflow have
        // no `converged` step; its absence is that version's finish
        // line, not a stuck train.
        let steps = vec![done(
            "deployed",
            "Deployed to the playground",
            "2026-09-03T06:50:00Z",
        )];
        assert_eq!(phase_of(&steps), TrainPhase::Arrived);
    }

    #[test]
    fn converged_done_is_arrived() {
        let steps = vec![
            done(
                "deployed",
                "Deployed to the playground",
                "2026-09-03T06:50:00Z",
            ),
            done("converged", "Cluster converged", "2026-09-03T07:10:00Z"),
        ];
        assert_eq!(phase_of(&steps), TrainPhase::Arrived);
    }

    #[test]
    fn find_step_falls_back_to_title_when_slug_is_absent() {
        // A step materialized before spec_slug existed is addressed by
        // title — the conductor's own fallback.
        let mut s = step(
            "",
            "Deployed to the playground",
            StepStatus::Ready,
            json!({}),
        );
        s.spec_slug = None;
        assert!(find_step(&[s], &DEPLOYED).is_some());
    }

    // ---- the buried block reason ----

    #[test]
    fn a_blocked_deploy_surfaces_its_reason_and_since() {
        // The 2026-09-02 incident: the reason was in the step metadata
        // and read by nobody. The status payload names it.
        let steps = vec![
            done("merged", "Merged into main", "2026-09-03T06:45:00Z"),
            step(
                "deployed",
                "Deployed to the playground",
                StepStatus::Ready,
                json!({
                    "deploy_blocked": "deploy tree busy (branch=main, dirty=True) — will retry",
                    "deploy_blocked_since": "2026-09-03T06:46:00Z",
                }),
            ),
        ];
        let job = train(vec![], json!({}));
        let ts = train_status(&job, &steps, None, &no_history(), None);
        assert_eq!(ts.phase, TrainPhase::Deploying);
        assert_eq!(
            ts.block,
            Some(TrainBlock::DeployBlocked {
                reason: "deploy tree busy (branch=main, dirty=True) — will retry".into(),
                since: Some("2026-09-03T06:46:00Z".into()),
            })
        );
    }

    #[test]
    fn a_completed_deploy_clears_the_block_even_with_stale_keys() {
        // deploy_blocked keys are not erased on success; a completed
        // deploy step means the block is history.
        let steps = vec![step(
            "deployed",
            "Deployed to the playground",
            StepStatus::Completed,
            json!({
                "completed_at": "2026-09-03T06:50:00Z",
                "deploy_blocked": "old reason",
                "deploy_blocked_since": "2026-09-03T06:46:00Z",
            }),
        )];
        let job = train(vec![], json!({}));
        assert_eq!(block_of(&job, &steps, TrainPhase::Converging, None), None);
    }

    #[test]
    fn a_red_ci_before_the_merge_is_a_block_naming_the_check() {
        let steps = vec![
            done("pr", "Open the batched PR", "2026-09-03T06:05:00Z"),
            step(
                "ci",
                "CI verdict",
                StepStatus::Completed,
                json!({
                    "completed_at": "2026-09-03T06:40:00Z",
                    "result": "failing",
                    "checks": "test:FAILURE, build:SUCCESS",
                }),
            ),
            step("merged", "Merged into main", StepStatus::Ready, json!({})),
        ];
        let job = train(vec![], json!({}));
        let ts = train_status(&job, &steps, None, &no_history(), None);
        assert_eq!(
            ts.block,
            Some(TrainBlock::CiRed {
                checks: Some("test:FAILURE, build:SUCCESS".into())
            })
        );
        assert_eq!(ts.ci_result.as_deref(), Some("failing"));
    }

    #[test]
    fn a_red_ci_after_the_merge_is_history_not_a_block() {
        // Post-merge the content has landed; the red lamp is history.
        let steps = vec![
            done("pr", "Open the batched PR", "2026-09-03T06:05:00Z"),
            step(
                "ci",
                "CI verdict",
                StepStatus::Completed,
                json!({ "result": "failing" }),
            ),
            done("merged", "Merged into main", "2026-09-03T06:45:00Z"),
            step(
                "deployed",
                "Deployed to the playground",
                StepStatus::Ready,
                json!({}),
            ),
        ];
        let job = train(vec![], json!({}));
        assert_eq!(block_of(&job, &steps, TrainPhase::Deploying, None), None);
    }

    #[test]
    fn converge_overdue_is_read_from_the_job_latch() {
        let steps = vec![
            done(
                "deployed",
                "Deployed to the playground",
                "2026-09-03T06:50:00Z",
            ),
            step(
                "converged",
                "Cluster converged",
                StepStatus::Ready,
                json!({}),
            ),
        ];
        let job = train(vec![], json!({ "converge_alarm_filed": true }));
        assert_eq!(
            block_of(&job, &steps, TrainPhase::Converging, None),
            Some(TrainBlock::ConvergeOverdue)
        );
        // Also accepts the string form older writes used.
        let job2 = train(vec![], json!({ "converge_alarm_filed": "true" }));
        assert_eq!(
            block_of(&job2, &steps, TrainPhase::Converging, None),
            Some(TrainBlock::ConvergeOverdue)
        );
    }

    #[test]
    fn a_stalled_train_surfaces_its_stamp() {
        let steps = vec![step(
            "pr",
            "Open the batched PR",
            StepStatus::Ready,
            json!({}),
        )];
        let job = train(vec![], json!({ "stalled_since": "2026-09-03T00:00:00Z" }));
        assert_eq!(
            block_of(&job, &steps, TrainPhase::Boarding, None),
            Some(TrainBlock::Stalled {
                since: "2026-09-03T00:00:00Z".into()
            })
        );
    }

    #[test]
    fn a_deploy_block_wins_over_a_coarser_latch() {
        // Both present: the specific, recently-buried deploy reason is
        // the one an operator needs first.
        let steps = vec![
            done("merged", "Merged into main", "2026-09-03T06:45:00Z"),
            step(
                "deployed",
                "Deployed to the playground",
                StepStatus::Ready,
                json!({ "deploy_blocked": "tree busy", "deploy_blocked_since": "2026-09-03T06:46:00Z" }),
            ),
        ];
        let job = train(vec![], json!({ "stalled_since": "2026-09-03T00:00:00Z" }));
        assert!(matches!(
            block_of(&job, &steps, TrainPhase::Deploying, None),
            Some(TrainBlock::DeployBlocked { .. })
        ));
    }

    #[test]
    fn a_healthy_train_has_no_block() {
        let steps = vec![
            done("merged", "Merged into main", "2026-09-03T06:45:00Z"),
            step(
                "deployed",
                "Deployed to the playground",
                StepStatus::Active,
                json!({}),
            ),
        ];
        let job = train(vec![], json!({}));
        assert_eq!(
            train_status(&job, &steps, None, &no_history(), None).block,
            None
        );
    }

    // ---- boarding predicate, from live rules ----

    #[test]
    fn the_boarding_predicate_reads_the_depth_and_clock_rules() {
        let mut depth = rule("queue-depth");
        depth.min_dock_depth = Some(4);
        depth.cooldown_minutes = Some(120);
        let mut clock = rule("clock");
        clock.at_times = Some(json!(["06:00", "18:00"]));
        let rules = vec![depth, clock];

        let p = boarding_predicate(&rules, 2, None, 0, None);
        assert_eq!(p.dock_threshold, Some(4));
        assert_eq!(p.cooldown_minutes, Some(120));
        assert_eq!(p.at_times, vec!["06:00", "18:00"]);
        assert_eq!(p.dock_depth, 2);
        assert_eq!(p.threshold_met, Some(false));
        assert!(p.summary.contains("4 parked cars"));
        assert!(p.summary.contains("06:00 / 18:00 UTC"));
        assert!(p.summary.contains("2 car(s) parked now"));
        assert!(p.summary.contains("below the dock threshold"));
    }

    #[test]
    fn the_predicate_reports_the_threshold_met_when_the_dock_is_deep() {
        let mut depth = rule("queue-depth");
        depth.min_dock_depth = Some(4);
        let p = boarding_predicate(&[depth], 5, None, 0, None);
        assert_eq!(p.threshold_met, Some(true));
        assert!(p.summary.contains("the dock threshold is met"));
    }

    #[test]
    fn no_cadence_rules_reads_as_no_configured_cadence_not_a_fake_schedule() {
        let p = boarding_predicate(&[], 3, None, 0, None);
        assert_eq!(p.dock_threshold, None);
        assert_eq!(p.threshold_met, None);
        assert!(p.at_times.is_empty());
        assert!(p.summary.contains("No boarding cadence is configured"));
        assert!(p.summary.contains("3 car(s) parked"));
    }

    #[test]
    fn a_malformed_at_times_degrades_to_the_string_entries_only() {
        let mut clock = rule("clock");
        clock.at_times = Some(json!(["06:00", 18, null]));
        let p = boarding_predicate(&[clock], 0, None, 0, None);
        assert_eq!(p.at_times, vec!["06:00"]);
    }

    // ---- the boarding hold: why it is not boarding, from the conductor's facts ----

    fn board_rule(threshold: i32, cooldown: i32) -> CadenceRuleRow {
        let mut r = rule("queue-depth");
        r.name = "train-board-on-dock-depth".into();
        r.verb = "board".into();
        r.min_dock_depth = Some(threshold);
        r.cooldown_minutes = Some(cooldown);
        r
    }

    fn fired(at_rfc3339: &str, rc: Option<i32>) -> LastFiring {
        LastFiring {
            firing_id: "f".into(),
            fired_at: at(at_rfc3339),
            rc,
        }
    }

    /// 2026-09-07, twice: a threshold-met dock, no board, and the only
    /// answer was in the conductor's journal — "45-min cooldown, N left".
    #[test]
    fn a_recent_board_holds_the_dock_for_the_minutes_left_in_the_cooldown() {
        let last = fired("2026-09-07T20:00:00Z", Some(0));
        let h = boarding_hold(
            &[board_rule(4, 45)],
            Some(&last),
            5,
            0,
            Some(at("2026-09-07T20:33:30Z")),
        );
        assert_eq!(h.held_because.as_deref(), Some("cooldown — 12 min left"));
        assert_eq!(h.cooldown_remaining_minutes, Some(12));
        assert_eq!(h.last_board_at, Some(at("2026-09-07T20:00:00Z")));
        assert_eq!(
            h.next_board,
            "boards on the next tick once the cooldown clears (12 min)"
        );
    }

    #[test]
    fn a_board_that_boarded_nothing_releases_the_cooldown() {
        // The conductor's own release rule (`due_window`): a failed board
        // (rc 1) or an idle one (IDLE_BOARD_RC, -2) has nothing to re-fire
        // against, so it holds nothing — but it is still the last board.
        let now = Some(at("2026-09-07T20:05:00Z"));
        for rc in [1, -2] {
            let last = fired("2026-09-07T20:00:00Z", Some(rc));
            let h = boarding_hold(&[board_rule(4, 45)], Some(&last), 5, 0, now);
            assert_eq!(h.held_because, None, "rc={rc}");
            assert_eq!(h.cooldown_remaining_minutes, None, "rc={rc}");
            assert_eq!(h.last_board_at, Some(at("2026-09-07T20:00:00Z")));
            assert_eq!(h.next_board, "boards on the next tick");
        }
    }

    #[test]
    fn a_board_with_no_outcome_yet_still_holds() {
        // rc == None is in flight, not released: re-firing under it would
        // double-board, so the conductor holds, and so does the reading.
        let last = fired("2026-09-07T20:00:00Z", None);
        let h = boarding_hold(
            &[board_rule(4, 45)],
            Some(&last),
            5,
            0,
            Some(at("2026-09-07T20:05:00Z")),
        );
        assert_eq!(h.held_because.as_deref(), Some("cooldown — 40 min left"));
        assert_eq!(h.cooldown_remaining_minutes, Some(40));
    }

    #[test]
    fn an_elapsed_cooldown_holds_nothing() {
        let last = fired("2026-09-07T19:00:00Z", Some(0));
        let h = boarding_hold(
            &[board_rule(4, 45)],
            Some(&last),
            5,
            0,
            Some(at("2026-09-07T20:00:00Z")),
        );
        assert_eq!(h.held_because, None);
        assert_eq!(h.cooldown_remaining_minutes, None);
        assert_eq!(h.last_board_at, Some(at("2026-09-07T19:00:00Z")));
        assert_eq!(h.next_board, "boards on the next tick");
    }

    #[test]
    fn a_shallow_dock_is_held_below_threshold() {
        let h = boarding_hold(
            &[board_rule(4, 45)],
            None,
            2,
            0,
            Some(at("2026-09-07T20:00:00Z")),
        );
        assert_eq!(
            h.held_because.as_deref(),
            Some("below threshold (depth 2 of 4)")
        );
        assert_eq!(h.cooldown_remaining_minutes, None);
        assert_eq!(h.last_board_at, None);
        assert_eq!(
            h.next_board,
            "boards on the next tick once the dock reaches 4"
        );
    }

    #[test]
    fn an_open_train_holds_every_departure_first() {
        // Single-track railway: a train on the main holds a departure
        // whatever the dock says, so the track is named before the
        // cooldown or the depth — and the sentence names all three.
        let last = fired("2026-09-07T20:00:00Z", Some(0));
        let now = Some(at("2026-09-07T20:10:00Z"));
        let h = boarding_hold(&[board_rule(4, 45)], Some(&last), 2, 2, now);
        assert_eq!(
            h.held_because.as_deref(),
            Some("track occupied (2 open trains)")
        );
        assert_eq!(h.cooldown_remaining_minutes, Some(35));
        assert_eq!(
            h.next_board,
            "boards on the next tick once the track clears and the cooldown clears (35 min) \
             and the dock reaches 4"
        );
        let one = boarding_hold(&[board_rule(4, 45)], None, 5, 1, now);
        assert_eq!(
            one.held_because.as_deref(),
            Some("track occupied (1 open train)")
        );
        assert_eq!(
            one.next_board,
            "boards on the next tick once the track clears"
        );
    }

    /// Twin of boss-cli `track_tests::a_merged_train_waiting_to_converge_does_not_hold_the_track`
    /// — the conductor reads the same rule off the API's JSON.
    #[test]
    fn a_merged_train_waiting_to_converge_does_not_hold_the_track() {
        // 2026-09-07 (f3796323), twice: a merged train whose sha bricked
        // its boot sat at `converged`, and the board said the fix-forward
        // car was held by the track — the very train it would have
        // converged. Merged content is on main; the next consist merges
        // on top and converges it by ancestry. A CONVERGING train must
        // not also read as a track hold.
        let converging = vec![
            done("merged", "Merged into main", "2026-09-07T20:00:00Z"),
            done(
                "deployed",
                "Deployed to the playground",
                "2026-09-07T20:05:00Z",
            ),
            step(
                "converged",
                "Cluster converged",
                StepStatus::Ready,
                json!({}),
            ),
        ];
        let open = vec![(train(vec![], json!({})), converging)];
        let dock: Vec<Job> = (0..5)
            .map(|i| {
                let mut car = train(vec![], json!({ "branch": format!("fix/{i}") }));
                car.kind = "ship-a-change".into();
                car
            })
            .collect();
        let rules = vec![board_rule(4, 45)];

        let status = build_status(
            &open,
            &[],
            &dock,
            &rules,
            None,
            None,
            &[],
            &[],
            &[],
            &[],
            fixed_now(),
        );
        assert_eq!(status.trains[0].phase, TrainPhase::Converging);
        assert_eq!(status.boarding.hold.held_because, None);
        assert_eq!(status.boarding.hold.next_board, "boards on the next tick");
    }

    /// Twin of boss-cli `track_tests::the_pre_merge_train_is_named_when_a_merged_one_is_also_open`.
    #[test]
    fn only_the_pre_merge_train_counts_as_the_track() {
        // Two open trains: one converging (merged), one still awaiting
        // its merge. The track is held by exactly one — the count says
        // so, not "2 open trains".
        let converging = vec![
            done("merged", "Merged into main", "2026-09-07T20:00:00Z"),
            done(
                "deployed",
                "Deployed to the playground",
                "2026-09-07T20:05:00Z",
            ),
            step(
                "converged",
                "Cluster converged",
                StepStatus::Ready,
                json!({}),
            ),
        ];
        let pre_merge = vec![
            done(
                "collect",
                "Collect what is ready to board",
                "2026-09-07T20:30:00Z",
            ),
            done("pr", "Open the batched PR", "2026-09-07T20:31:00Z"),
            step("ci", "CI verdict", StepStatus::Ready, json!({})),
            step("merged", "Merged into main", StepStatus::Ready, json!({})),
        ];
        let open = vec![
            (train(vec![], json!({})), converging),
            (train(vec![], json!({})), pre_merge),
        ];
        let dock: Vec<Job> = (0..5)
            .map(|i| {
                let mut car = train(vec![], json!({ "branch": format!("fix/{i}") }));
                car.kind = "ship-a-change".into();
                car
            })
            .collect();
        let rules = vec![board_rule(4, 45)];

        let status = build_status(
            &open,
            &[],
            &dock,
            &rules,
            None,
            None,
            &[],
            &[],
            &[],
            &[],
            fixed_now(),
        );
        assert_eq!(
            status.boarding.hold.held_because.as_deref(),
            Some("track occupied (1 open train)")
        );
    }

    #[test]
    fn the_cooldown_outranks_the_depth() {
        let last = fired("2026-09-07T20:00:00Z", Some(0));
        let h = boarding_hold(
            &[board_rule(4, 45)],
            Some(&last),
            2,
            0,
            Some(at("2026-09-07T20:10:00Z")),
        );
        assert_eq!(h.held_because.as_deref(), Some("cooldown — 35 min left"));
        assert_eq!(
            h.next_board,
            "boards on the next tick once the cooldown clears (35 min) and the dock reaches 4"
        );
    }

    #[test]
    fn a_clear_dock_boards_on_the_next_tick_never_at_a_time() {
        let h = boarding_hold(
            &[board_rule(4, 45)],
            None,
            4,
            0,
            Some(at("2026-09-07T20:10:00Z")),
        );
        assert_eq!(h.held_because, None);
        assert_eq!(h.cooldown_remaining_minutes, None);
        assert_eq!(h.next_board, "boards on the next tick");
        // The depth rule has no clock; a time of day here would be invented.
        assert!(!h.next_board.contains(':'), "{}", h.next_board);
    }

    #[test]
    fn no_depth_rule_says_so_rather_than_inventing_a_hold() {
        let mut clock = rule("clock");
        clock.at_times = Some(json!(["06:00"]));
        let h = boarding_hold(&[clock], None, 3, 0, Some(at("2026-09-07T20:10:00Z")));
        assert_eq!(h.held_because, None);
        assert_eq!(
            h.next_board,
            "no depth rule is configured — nothing boards on dock depth"
        );
    }

    #[test]
    fn the_hold_rides_the_predicate_flat_on_the_wire() {
        let last = fired("2026-09-07T20:00:00Z", Some(0));
        let p = boarding_predicate(
            &[board_rule(4, 45)],
            5,
            Some(&last),
            0,
            Some(at("2026-09-07T20:33:00Z")),
        );
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["threshold_met"], true);
        assert_eq!(v["held_because"], "cooldown — 12 min left");
        assert_eq!(v["cooldown_remaining_minutes"], 12);
        assert_eq!(v["last_board_at"], "2026-09-07T20:00:00Z");
        assert_eq!(
            v["next_board"],
            "boards on the next tick once the cooldown clears (12 min)"
        );
    }

    // ---- recent trains ----

    #[test]
    fn an_arrived_train_reports_its_journey_time() {
        let steps = vec![
            done(
                "collect",
                "Collect what is ready to board",
                "2026-09-03T06:00:00Z",
            ),
            done("arrived", "Train arrived", "2026-09-03T07:00:00Z"),
        ];
        let job = train(vec![], json!({}));
        let r = recent_train(&job, &steps);
        assert_eq!(r.outcome, "arrived");
        assert_eq!(r.journey_seconds, Some(3600));
    }

    #[test]
    fn a_cancelled_train_reads_its_outcome_from_metadata() {
        let job = train(vec![], json!({ "outcome": "cancelled" }));
        let r = recent_train(&job, &[]);
        assert_eq!(r.outcome, "cancelled");
        assert_eq!(r.journey_seconds, None);
    }

    #[test]
    fn a_train_with_no_terminal_evidence_is_unknown_never_a_guess() {
        let job = train(vec![], json!({}));
        assert_eq!(recent_train(&job, &[]).outcome, "unknown");
    }

    /// The live shape, measured 2026-09-08 on every closed train: the
    /// conductor stamps `collect`, but `arrived` is an outcome step the
    /// server completes, bare — the terminal instant is the job's
    /// `closed_at`. This is why `journey_seconds` was null on every
    /// train the board had ever shown.
    #[test]
    fn a_terminal_the_server_closed_reads_its_journey_from_the_job_closed_at() {
        let steps = vec![
            done(
                "collect",
                "Collect what is ready to board",
                "2026-09-08T02:03:05Z",
            ),
            done("converged", "Cluster converged", "2026-09-08T02:30:24Z"),
            step(
                "arrived",
                "Train arrived",
                StepStatus::Completed,
                json!({ "outcome_kind": "completed" }),
            ),
        ];
        let job = train(
            vec![],
            json!({
                "outcome": "arrived",
                "closed_at": "2026-09-08T02:30:25.263551771+00:00",
            }),
        );
        let r = recent_train(&job, &steps);
        assert_eq!(r.outcome, "arrived");
        assert_eq!(r.journey_seconds, Some(1640), "02:03:05 → 02:30:25");
    }

    #[test]
    fn a_cancelled_train_measures_boarding_to_its_cancellation() {
        // Cancelled AFTER boarding: the time it spent in the yard is a
        // journey too, read from the cancelled terminal's own stamp.
        let steps = vec![
            done(
                "collect",
                "Collect what is ready to board",
                "2026-09-08T01:18:00Z",
            ),
            done("cancelled", "Cancelled", "2026-09-08T01:46:42Z"),
        ];
        let job = train(vec![], json!({ "outcome": "cancelled" }));
        let r = recent_train(&job, &steps);
        assert_eq!(r.outcome, "cancelled");
        assert_eq!(r.journey_seconds, Some(1722));
    }

    #[test]
    fn a_train_that_never_boarded_has_no_journey() {
        // Closed (stamped) but the collect never completed: nothing to
        // measure from, so null — not zero, not opened_at.
        let steps = vec![step(
            "collect",
            "Collect what is ready to board",
            StepStatus::Skipped,
            json!({}),
        )];
        let job = train(
            vec![],
            json!({ "outcome": "cancelled", "closed_at": "2026-09-08T01:46:42Z" }),
        );
        assert_eq!(recent_train(&job, &steps).journey_seconds, None);
    }

    #[test]
    fn a_train_with_no_terminal_instant_has_no_journey() {
        // Boarded, terminal completed, but nothing stamped the instant
        // and the job carries no `closed_at`: no estimate.
        let steps = vec![
            done(
                "collect",
                "Collect what is ready to board",
                "2026-09-08T02:03:05Z",
            ),
            step(
                "arrived",
                "Train arrived",
                StepStatus::Completed,
                json!({ "outcome_kind": "completed" }),
            ),
        ];
        let job = train(vec![], json!({ "outcome": "arrived" }));
        assert_eq!(recent_train(&job, &steps).journey_seconds, None);
    }

    // ---- boarding instant ----

    #[test]
    fn an_open_train_reads_its_boarding_instant_from_the_collect_stamp() {
        let steps = vec![
            done(
                "collect",
                "Collect what is ready to board",
                "2026-09-08T02:03:05Z",
            ),
            done(
                "assemble",
                "Assemble the train branch",
                "2026-09-08T02:03:05Z",
            ),
            step("pr", "Open the batched PR", StepStatus::Ready, json!({})),
        ];
        let job = train(vec![], json!({}));
        let t = train_status(&job, &steps, None, &no_history(), None);
        assert_eq!(t.phase, TrainPhase::Boarding);
        assert_eq!(t.boarded_at.as_deref(), Some("2026-09-08T02:03:05Z"));
        // Additive on the wire: present as a string when known.
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(v["boarded_at"], "2026-09-08T02:03:05Z");
    }

    #[test]
    fn a_train_still_collecting_has_no_boarding_instant() {
        let steps = vec![step(
            "collect",
            "Collect what is ready to board",
            StepStatus::Ready,
            json!({}),
        )];
        let job = train(vec![], json!({}));
        let t = train_status(&job, &steps, None, &no_history(), None);
        assert_eq!(t.boarded_at, None);
        // And absent from the wire, like the other unknowns on the row.
        let v = serde_json::to_value(&t).unwrap();
        assert!(v.get("boarded_at").is_none());
    }

    // ---- dock + policy ----

    #[test]
    fn a_dock_car_carries_branch_and_parked_since() {
        let mut job = train(vec![], json!({ "branch": "feat/x" }));
        job.kind = "ship-a-change".into();
        job.title = "A fix".into();
        let c = dock_car(&job);
        assert_eq!(c.branch.as_deref(), Some("feat/x"));
        assert_eq!(c.parked_since, "2026-09-03");
        assert_eq!(c.title, "A fix");
    }

    #[test]
    fn policy_thresholds_surface_the_registry_numbers() {
        let policy = DeliveryPolicyRow {
            name: "train-conductor".into(),
            version: 3,
            max_red_trains: 2,
            stall_hours: 6,
            consist_excluded_lints: json!([]),
            consist_budget_secs: 600,
            consist_output_budget: 2000,
            consist_files_named: 5,
            skip_reason_file_budget: 200,
            blip_cause_budget: 200,
            ci_host_floor_gb: 10,
            gate_max_concurrent: 3,
        };
        let t = policy_thresholds(Some(&policy));
        assert_eq!(t.stall_hours, Some(6));
        assert_eq!(t.max_red_trains, Some(2));
        // None policy → all None, never a fabricated default.
        assert_eq!(
            policy_thresholds(None),
            PolicyThresholds {
                stall_hours: None,
                max_red_trains: None
            }
        );
    }

    // ---- stranded greens ----

    fn gate_run(branch: &str, metadata: Value) -> Job {
        let mut j = train(vec![], metadata);
        j.kind = "gate-run".into();
        j.metadata["branch"] = json!(branch);
        j
    }

    fn green_step() -> Step {
        step(
            "gate",
            "Gate",
            StepStatus::Completed,
            json!({ "verdict": "green" }),
        )
    }

    /// The branches a lane names, in its order — what these tests are
    /// about. The packet fields each row also carries are pinned once,
    /// by `a_stranded_green_carries_its_packet_and_head`.
    fn stranded_branches(out: &[StrandedGreen]) -> Vec<&str> {
        out.iter().map(|s| s.branch.as_str()).collect()
    }

    #[test]
    fn a_green_gate_run_with_no_car_is_stranded() {
        let g = gate_run("feat/x", json!({}));
        let out = stranded_greens(&[(g, vec![green_step()])], &[]);
        assert_eq!(stranded_branches(&out), vec!["feat/x"]);
    }

    /// A stranded row names its PACKET and its head, not only its
    /// branch. The web approach lane draws a wagon per row and needs
    /// both — and used to recover them by re-scanning a window of
    /// gate-runs client-side, which is how a second, weaker copy of
    /// "is this green spent?" grew there and drew a phantom wagon for a
    /// re-railed branch all day on 2026-09-10 (CLAUDE.md §9a).
    #[test]
    fn a_stranded_green_carries_its_packet_and_head() {
        let mut g = gate_run("feat/x", json!({}));
        g.metadata["sha"] = json!("deadbeef");
        g.metadata["opened_at"] = json!("2026-09-09T18:00:00Z");
        let id = g.id.to_string();
        let out = stranded_greens(&[(g, vec![green_step()])], &[]);
        assert_eq!(
            out,
            vec![StrandedGreen {
                branch: "feat/x".into(),
                packet_id: id,
                sha: Some("deadbeef".into()),
                since: "2026-09-09T18:00:00Z".into(),
            }]
        );
        // A blank sha is no sha — a drawn head must be a head that is
        // on the record.
        let mut blank = gate_run("feat/y", json!({}));
        blank.metadata["sha"] = json!("");
        assert_eq!(
            stranded_greens(&[(blank, vec![green_step()])], &[])[0].sha,
            None
        );
    }

    #[test]
    fn a_green_gate_run_whose_branch_is_a_car_is_not_stranded() {
        let g = gate_run("feat/x", json!({}));
        let out = stranded_greens(&[(g, vec![green_step()])], &["feat/x".into()]);
        assert!(out.is_empty());
    }

    #[test]
    fn a_superseded_green_is_not_stranded() {
        let g = gate_run("feat/x", json!({ "superseded": true }));
        let out = stranded_greens(&[(g, vec![green_step()])], &[]);
        assert!(out.is_empty());
    }

    /// A green marked `hold` is deliberately waiting, not stranded: the
    /// operator gated it and chose not to park. Only the unheld green
    /// beside it is stranded.
    #[test]
    fn a_held_green_is_not_stranded() {
        let held = gate_run("feat/held", json!({ "hold": "lands at the next restart" }));
        let free = gate_run("feat/free", json!({}));
        let out = stranded_greens(
            &[(held, vec![green_step()]), (free, vec![green_step()])],
            &[],
        );
        assert_eq!(stranded_branches(&out), vec!["feat/free"]);
    }

    /// `boss rerail --finish` repoints a car at `<branch>-rerail` and
    /// stamps the ORIGINAL branch's gate-run `rerailed_to`. That green
    /// is spent — its car rides another branch — so it is not stranded
    /// (69daaba2: it read as stranded forever, even after the branch was
    /// deleted on the forge). The `rerailed_from` stamp on the new
    /// branch's gate-run is informational and changes nothing here.
    #[test]
    fn a_rerailed_green_is_not_stranded() {
        let old = gate_run("fix/x", json!({ "rerailed_to": "fix/x-rerail" }));
        let new = gate_run("fix/x-rerail", json!({ "rerailed_from": "fix/x" }));
        let out = stranded_greens(&[(old, vec![green_step()]), (new, vec![green_step()])], &[]);
        assert_eq!(stranded_branches(&out), vec!["fix/x-rerail"]);
        // An empty or null stamp is no stamp.
        for v in [json!(""), Value::Null] {
            let g = gate_run("fix/y", json!({ "rerailed_to": v }));
            assert_eq!(stranded_greens(&[(g, vec![green_step()])], &[]).len(), 1);
        }
    }

    /// `jobs.auto-park` writes `park_skipped` on a green it DECIDED not
    /// to file a car for — the branch had already landed, or its car was
    /// already aboard a train. The handler ran, looked and declined, so
    /// the green is spent, not forgotten; reading it as stranded put a
    /// landed branch on an operator's rescue list (e60398dc).
    #[test]
    fn an_auto_park_skipped_green_is_not_stranded() {
        let skipped = gate_run("fix/landed", json!({ "park_skipped": "landed" }));
        let free = gate_run("fix/free", json!({}));
        let out = stranded_greens(
            &[(skipped, vec![green_step()]), (free, vec![green_step()])],
            &[],
        );
        assert_eq!(stranded_branches(&out), vec!["fix/free"]);
    }

    /// The held list is the other half of the same predicate: a green
    /// no car claims, with a `hold` reason. It carries the reason and
    /// when the gate opened, so the surface can say "held — <why>" in
    /// its own colour instead of amber. Superseded, rerailed, red and
    /// car-claimed runs are not held any more than they are stranded.
    #[test]
    fn a_held_green_lists_as_held_with_its_reason_and_since() {
        let mut held = gate_run("feat/held", json!({ "hold": "lands at the next restart" }));
        held.metadata["opened_at"] = json!("2026-09-08T18:00:00Z");
        let bare = gate_run("feat/bare", json!({ "hold": true }));
        let claimed = gate_run("feat/claimed", json!({ "hold": "x" }));
        let free = gate_run("feat/free", json!({}));
        let rerailed = gate_run(
            "feat/old",
            json!({ "hold": "x", "rerailed_to": "feat/old-rerail" }),
        );
        let out = held_greens(
            &[
                (free, vec![green_step()]),
                (held, vec![green_step()]),
                (bare, vec![green_step()]),
                (claimed, vec![green_step()]),
                (rerailed, vec![green_step()]),
            ],
            &["feat/claimed".into()],
        );
        assert_eq!(
            out.iter()
                .map(|h| (h.branch.as_str(), h.reason.as_str(), h.since.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("feat/bare", "no reason recorded", "2026-09-03"),
                (
                    "feat/held",
                    "lands at the next restart",
                    "2026-09-08T18:00:00Z"
                ),
            ]
        );
    }

    #[test]
    fn a_red_gate_run_is_not_stranded() {
        let g = gate_run("feat/x", json!({}));
        let red = step(
            "gate",
            "Gate",
            StepStatus::Completed,
            json!({ "verdict": "red" }),
        );
        assert!(stranded_greens(&[(g, vec![red])], &[]).is_empty());
    }

    // ---- gate slots + garage ----

    /// A gate-run opened on a given day, so "latest wins" is testable.
    fn gate_run_on(branch: &str, day: u32) -> Job {
        let mut j = gate_run(branch, json!({}));
        j.opened_on = chrono::NaiveDate::from_ymd_opt(2026, 9, day).unwrap();
        j
    }

    /// A `record-verdict` step carrying a verdict and a receipt string
    /// with a `checks` array — the shape the runner writes.
    fn verdict_step(verdict: &str, checks: Value) -> Step {
        let receipt = serde_json::to_string(&json!({
            "verdict": verdict,
            "head": "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8",
            "checks": checks,
        }))
        .unwrap();
        step(
            "record-verdict",
            "Record the receipt",
            StepStatus::Completed,
            json!({ "verdict": verdict, "receipt": receipt }),
        )
    }

    /// An in-flight gate-run has an unreported record-verdict step (no
    /// verdict yet). Kept Open by the shared `train()` helper.
    fn in_flight_step() -> Step {
        step(
            "record-verdict",
            "Record the receipt",
            StepStatus::Active,
            json!({}),
        )
    }

    #[test]
    fn active_gates_are_open_runs_with_no_verdict_sorted_deterministically() {
        // Two in-flight (different days), one already-green (excluded).
        let runs = vec![
            (gate_run_on("feat/later", 3), vec![in_flight_step()]),
            (gate_run_on("feat/early", 2), vec![in_flight_step()]),
            (
                gate_run_on("feat/done", 2),
                vec![verdict_step("green", json!([]))],
            ),
        ];
        let g = gates(&runs, 4, None);
        assert_eq!(g.capacity, 4);
        // Sorted by `since` then branch: the day-2 run precedes the day-3.
        let branches: Vec<&str> = g.active.iter().map(|a| a.branch.as_str()).collect();
        assert_eq!(branches, vec!["feat/early", "feat/later"]);
        assert_eq!(g.active[0].since, "2026-09-02");
        assert!(
            !g.active[0].packet_id.is_empty(),
            "the slot names its packet"
        );
    }

    /// A QUEUED gate-run HOLDS NO SLOT. `boss gate` files the packet
    /// when it is waiting for a slot (so the queue has an order the
    /// system of record owns, not a per-builder retry loop), and a
    /// waiting run has no Job, no pod and no workspace. Counted as
    /// active it would fill a gate bay that is genuinely free, and past
    /// [`GATE_MAX_ACTIVE_HOURS`] it would render `stale` — the surface
    /// reporting a corpse for a run that has not started. This is the
    /// same defect the refusal-as-`lost` packet was, one shape along.
    #[test]
    fn a_queued_gate_run_holds_no_slot() {
        let mut waiting = gate_run_on("feat/waiting", 2);
        waiting.metadata[QUEUED_AT] = json!("2026-09-02T01:00:00Z");
        let runs = vec![
            (gate_run_on("feat/running", 2), vec![in_flight_step()]),
            (waiting, vec![in_flight_step()]),
        ];
        let g = gates(&runs, 3, None);
        let branches: Vec<&str> = g.active.iter().map(|a| a.branch.as_str()).collect();
        assert_eq!(
            branches,
            vec!["feat/running"],
            "a run waiting for a slot is not occupying one"
        );
    }

    /// An EMPTY marker is not a queued run. A `queued_at: ""` is the
    /// shape a cleared key leaves behind on a metadata merge that wrote
    /// a blank instead of deleting, and reading it as "queued" would
    /// hide a real running gate from the bays.
    #[test]
    fn a_blank_queued_marker_still_occupies_its_slot() {
        let mut launched = gate_run_on("feat/launched", 2);
        launched.metadata[QUEUED_AT] = json!("");
        let g = gates(&[(launched, vec![in_flight_step()])], 3, None);
        assert_eq!(g.active.len(), 1, "a blank marker is no marker");
    }

    /// A gate-run waiting in line, stamped the way `boss gate` stamps it.
    fn queued_run(branch: &str, queued_at: &str) -> Job {
        let mut j = gate_run_on(branch, 2);
        j.metadata[QUEUED_AT] = json!(queued_at);
        j
    }

    /// A gate-run that RAN: an `opened_at` instant and a verdict step
    /// completed at `done_at` — the pair a measured duration is read from.
    fn finished_run(
        branch: &str,
        opened_at: &str,
        verdict: &str,
        done_at: &str,
    ) -> (Job, Vec<Step>) {
        let mut j = gate_run_on(branch, 2);
        j.metadata["opened_at"] = json!(opened_at);
        let mut s = verdict_step(verdict, json!([]));
        s.completed_at = parse_instant(done_at);
        (j, vec![s])
    }

    /// The queue is a LIST, in the order the system of record set:
    /// oldest [`QUEUED_AT`] first, positions 1..n. Without this the floor
    /// showed three busy bays and nothing else, and a queued run was
    /// indistinguishable from a gate that never launched.
    #[test]
    fn queued_runs_are_listed_oldest_first_with_their_place_in_line() {
        let runs = vec![
            (
                queued_run("feat/third", "2026-09-02T01:20:00Z"),
                vec![in_flight_step()],
            ),
            (
                queued_run("feat/first", "2026-09-02T01:00:00Z"),
                vec![in_flight_step()],
            ),
            (
                queued_run("feat/second", "2026-09-02T01:10:00Z"),
                vec![in_flight_step()],
            ),
            (gate_run_on("feat/running", 2), vec![in_flight_step()]),
        ];
        let g = gates(&runs, 3, None);
        let branches: Vec<&str> = g.queued.iter().map(|q| q.branch.as_str()).collect();
        assert_eq!(branches, vec!["feat/first", "feat/second", "feat/third"]);
        let places: Vec<i32> = g.queued.iter().map(|q| q.position).collect();
        assert_eq!(places, vec![1, 2, 3], "positions are 1-based and dense");
        assert_eq!(g.queued[0].queued_at, "2026-09-02T01:00:00Z");
        assert!(
            !g.queued[0].packet_id.is_empty(),
            "the lane names its packet"
        );
        assert_eq!(
            g.active.len(),
            1,
            "a queued run is listed, never drawn into a bay"
        );
    }

    /// How long it has waited is read from the stamp against the clock.
    /// No clock is no claim — the same rule the rest of this module obeys.
    #[test]
    fn a_queued_run_reports_how_long_it_has_waited() {
        let runs = vec![(
            queued_run("feat/waiting", "2026-09-02T01:00:00Z"),
            vec![in_flight_step()],
        )];
        let g = gates(&runs, 3, Some(at("2026-09-02T01:10:00Z")));
        assert_eq!(g.queued[0].waiting_seconds, Some(600));
        assert_eq!(
            gates(&runs, 3, None).queued[0].waiting_seconds,
            None,
            "no clock, no elapsed"
        );
    }

    /// The estimate is MEASURED, not a constant: the median duration of
    /// the runs this window judged, less what the slot ahead has already
    /// spent. Here one run took 20 minutes, the occupied slot is 5 minutes
    /// in, and capacity is 1 — so the first in line waits ~15 minutes and
    /// the second that plus a whole gate.
    #[test]
    fn the_estimated_wait_comes_from_measured_gate_duration() {
        let mut running = gate_run_on("feat/running", 2);
        running.metadata["opened_at"] = json!("2026-09-02T01:55:00Z");
        let runs = vec![
            finished_run(
                "feat/measured",
                "2026-09-02T00:00:00Z",
                "green",
                "2026-09-02T00:20:00Z",
            ),
            (running, vec![in_flight_step()]),
            (
                queued_run("feat/one", "2026-09-02T01:56:00Z"),
                vec![in_flight_step()],
            ),
            (
                queued_run("feat/two", "2026-09-02T01:57:00Z"),
                vec![in_flight_step()],
            ),
        ];
        let g = gates(&runs, 1, Some(at("2026-09-02T02:00:00Z")));
        assert_eq!(g.typical_seconds, Some(1200), "the measured median gate");
        assert_eq!(g.queued[0].estimated_wait_seconds, Some(900));
        assert_eq!(
            g.queued[1].estimated_wait_seconds,
            Some(900 + 1200),
            "the second waits for the first to run too"
        );
    }

    /// Nothing measured means no estimate — a wait nobody can derive is
    /// reported as unknown, never as a number the page invented.
    #[test]
    fn no_measured_run_means_no_estimate() {
        let runs = vec![(
            queued_run("feat/waiting", "2026-09-02T01:00:00Z"),
            vec![in_flight_step()],
        )];
        let g = gates(&runs, 3, Some(at("2026-09-02T01:10:00Z")));
        assert_eq!(g.typical_seconds, None);
        assert_eq!(g.queued[0].estimated_wait_seconds, None);
    }

    /// A free bay is a wait of nothing: `boss gate` queues on the count
    /// it saw, and a slot can free before its next poll.
    #[test]
    fn a_free_bay_makes_the_estimated_wait_nil() {
        let runs = vec![
            finished_run(
                "feat/measured",
                "2026-09-02T00:00:00Z",
                "green",
                "2026-09-02T00:20:00Z",
            ),
            (
                queued_run("feat/waiting", "2026-09-02T01:00:00Z"),
                vec![in_flight_step()],
            ),
        ];
        let g = gates(&runs, 3, Some(at("2026-09-02T01:10:00Z")));
        assert_eq!(g.queued[0].estimated_wait_seconds, Some(0));
    }

    /// A run that never reached a check measures a DEATH, not a gate: a
    /// `lost` verdict, and a duration past the Job's own deadline, are
    /// both dropped from the measurement.
    #[test]
    fn a_lost_or_impossible_run_is_not_a_measurement() {
        let lost = finished_run(
            "feat/lost",
            "2026-09-02T00:00:00Z",
            "lost",
            "2026-09-02T00:05:00Z",
        );
        let corpse = finished_run(
            "feat/corpse",
            "2026-09-01T00:00:00Z",
            "failed",
            "2026-09-02T00:00:00Z",
        );
        let g = gates(&[lost, corpse], 3, Some(at("2026-09-02T01:00:00Z")));
        assert_eq!(g.typical_seconds, None);
    }

    #[test]
    fn capacity_is_the_number_passed_never_a_constant() {
        // The same runs render into whatever capacity the policy set.
        let runs = vec![(gate_run_on("feat/x", 2), vec![in_flight_step()])];
        assert_eq!(gates(&runs, 3, None).capacity, 3);
        assert_eq!(gates(&runs, 7, None).capacity, 7);
    }

    #[test]
    fn a_closed_gate_run_never_occupies_a_slot() {
        // A gate-run whose Job has closed (verdict aside) is not in the
        // gates — a slot holds a car being assessed RIGHT NOW.
        let mut j = gate_run_on("feat/x", 2);
        j.status = JobStatus::Closed;
        let g = gates(&[(j, vec![in_flight_step()])], 3, None);
        assert!(g.active.is_empty());
    }

    #[test]
    fn the_garage_holds_a_branch_whose_latest_gate_is_red() {
        let runs = vec![(
            gate_run_on("feat/x", 3),
            vec![verdict_step(
                "failed",
                json!([
                    {"name": "clippy", "result": "pass"},
                    {"name": "test", "result": "fail"},
                ]),
            )],
        )];
        let g = garage(&runs, &[]);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].branch, "feat/x");
        assert_eq!(g[0].failed_check.as_deref(), Some("test"));
        assert_eq!(g[0].since, "2026-09-03");
    }

    /// 2026-09-04: every one of the garage's three entries was landed
    /// work. A car fixed by re-railing onto a fresh branch, or squash-
    /// merged and its branch deleted, never re-gates under the old name —
    /// so its last red run sits there forever and the rework queue fills
    /// with ghosts nobody can act on.
    #[test]
    fn a_branch_whose_car_settled_leaves_the_garage() {
        let runs = vec![(
            gate_run_on("feat/x", 3),
            vec![verdict_step(
                "failed",
                json!([{"name": "test", "result": "fail"}]),
            )],
        )];
        assert!(
            garage(&runs, &["feat/x".to_string()]).is_empty(),
            "a settled car is finished work, not work awaiting rework"
        );
        // ... and the same red WITHOUT a settled car still garages: red
        // never parks, so a red branch with no car is the ordinary case
        // the garage exists to show.
        assert_eq!(garage(&runs, &[]).len(), 1);
    }

    /// Train #198, 2026-09-04: merged and deployed at 17:10:17Z, still at
    /// `converged` at 19:31Z — 2h21m against a 2h policy — and the board
    /// showed it healthy, because `stalled_since` is stamped by the
    /// conductor's reconcile and the conductor was what was down.
    #[test]
    fn a_train_standing_still_past_the_window_is_stalled_without_the_conductor() {
        let steps = vec![done(DEPLOYED.slug, DEPLOYED.title, "2026-09-04T17:10:17Z")];
        let job = train(vec![], json!({}));
        let past = chrono::DateTime::parse_from_rfc3339("2026-09-04T17:31:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        match block_of(&job, &steps, TrainPhase::Converging, Some(past)) {
            Some(TrainBlock::Stalled { since }) => assert!(since.starts_with("2026-09-04T17:10")),
            other => panic!("a train past its stall window must look troubled, got {other:?}"),
        }
        // Deadline BEFORE the last completion: it moved recently enough.
        let fresh = chrono::DateTime::parse_from_rfc3339("2026-09-04T17:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            block_of(&job, &steps, TrainPhase::Converging, Some(fresh)),
            None
        );
        // An arrived train is never stalled, however long ago it landed.
        assert_eq!(
            block_of(&job, &steps, TrainPhase::Arrived, Some(past)),
            None
        );
    }

    /// Two gate-runs whose pods were evicted sat in the gates section for
    /// 17 hours on 2026-09-04, drawn exactly like live ones, while their
    /// branches had already landed.
    #[test]
    fn a_gate_run_older_than_the_job_deadline_is_stale() {
        let mut g = gate_run_on("feat/x", 3);
        g.metadata = json!({ "branch": "feat/x", "opened_at": "2026-09-03T20:00:00Z" });
        let runs = vec![(g, vec![in_flight_step()])];
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-04T13:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let out = gates(&runs, 3, Some(now));
        assert!(out.active[0].stale, "17h active is past any gate deadline");

        // Inside the deadline it is simply a gate that is running.
        let soon = chrono::DateTime::parse_from_rfc3339("2026-09-03T21:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert!(!gates(&runs, 3, Some(soon)).active[0].stale);
        // No clock: no claim either way.
        assert!(!gates(&runs, 3, None).active[0].stale);
    }

    #[test]
    fn an_active_gate_reads_since_as_the_opened_at_instant() {
        // The gate bay needs an instant to draw elapsed time; the
        // packet's `opened_at` is that instant, verbatim.
        let mut g = gate_run_on("feat/x", 3);
        g.metadata = json!({ "branch": "feat/x", "opened_at": "2026-09-03T11:40:00Z" });
        let out = gates(&[(g, vec![in_flight_step()])], 3, None);
        assert_eq!(out.active[0].since, "2026-09-03T11:40:00Z");

        // A stamp that is not an instant is no stamp: the date stands.
        let mut bad = gate_run_on("feat/y", 3);
        bad.metadata = json!({ "branch": "feat/y", "opened_at": "yesterday-ish" });
        let out = gates(&[(bad, vec![in_flight_step()])], 3, None);
        assert_eq!(out.active[0].since, "2026-09-03");
    }

    #[test]
    fn the_garage_reads_since_as_the_opened_at_instant_too() {
        let mut g = gate_run_on("feat/broken", 3);
        g.metadata = json!({ "branch": "feat/broken", "opened_at": "2026-09-03T11:40:00Z" });
        let runs = vec![(
            g,
            vec![verdict_step(
                "failed",
                json!([{"name": "test", "result": "fail"}]),
            )],
        )];
        assert_eq!(garage(&runs, &[])[0].since, "2026-09-03T11:40:00Z");
    }

    #[test]
    fn a_branch_that_regated_green_leaves_the_garage() {
        // Day 2 red, day 3 green for the SAME branch: fixed, so absent.
        let runs = vec![
            (
                gate_run_on("feat/x", 2),
                vec![verdict_step(
                    "failed",
                    json!([{"name": "test", "result": "fail"}]),
                )],
            ),
            (
                gate_run_on("feat/x", 3),
                vec![verdict_step("green", json!([]))],
            ),
        ];
        assert!(
            garage(&runs, &[]).is_empty(),
            "a later green for the same branch clears the red"
        );
    }

    #[test]
    fn a_green_then_a_later_red_puts_the_branch_in_the_garage() {
        // The reverse order: latest is red, so it IS garaged.
        let runs = vec![
            (
                gate_run_on("feat/x", 2),
                vec![verdict_step("green", json!([]))],
            ),
            (
                gate_run_on("feat/x", 3),
                vec![verdict_step(
                    "failed",
                    json!([{"name": "fmt", "result": "fail"}]),
                )],
            ),
        ];
        let g = garage(&runs, &[]);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].failed_check.as_deref(), Some("fmt"));
    }

    #[test]
    fn a_red_with_no_named_check_still_garages_without_a_check() {
        // A judged red that names no failing check (a receipt with no
        // `checks`, a runner that failed outside a check) — the garage
        // row shows the branch anyway. `failed`, not `lost`: a lost run
        // was never judged and does not garage.
        let runs = vec![(
            gate_run_on("feat/x", 3),
            vec![verdict_step("failed", json!([]))],
        )];
        let g = garage(&runs, &[]);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].failed_check, None);
    }

    #[test]
    fn two_runs_on_one_day_are_ordered_by_instant_not_by_id() {
        // A branch gated red at 05:14 and green at 05:29 the same day.
        // `opened_on` has day resolution, so the two tie on it, and the
        // packet-id tiebreak is a random UUID — here rigged so the RED
        // run's id sorts higher. The later INSTANT must win: the branch
        // was fixed and re-gated, it is not awaiting rework. Measured
        // 2026-09-05: feat/the-ci-host-check-is-live sat in the garage
        // after its second gate went green.
        let mut red = gate_run_on("feat/the-ci-host-check-is-live", 5);
        red.id = boss_core::job::JobId::from_uuid(
            uuid::Uuid::parse_str("ffffffff-ffff-4fff-8fff-ffffffffffff").unwrap(),
        );
        red.metadata["opened_at"] = json!("2026-09-05T05:14:00Z");
        let mut green = gate_run_on("feat/the-ci-host-check-is-live", 5);
        green.id = boss_core::job::JobId::from_uuid(
            uuid::Uuid::parse_str("00000000-0000-4000-8000-000000000000").unwrap(),
        );
        green.metadata["opened_at"] = json!("2026-09-05T05:29:00Z");
        let runs = vec![
            (red, vec![verdict_step("failed", json!([]))]),
            (green, vec![verdict_step("green", json!([]))]),
        ];
        assert!(
            garage(&runs, &[]).is_empty(),
            "the later instant is the latest run"
        );
        // And the other way round: green first, red later — garaged.
        let mut green2 = gate_run_on("feat/y", 5);
        green2.metadata["opened_at"] = json!("2026-09-05T05:14:00Z");
        let mut red2 = gate_run_on("feat/y", 5);
        red2.metadata["opened_at"] = json!("2026-09-05T05:29:00Z");
        let runs = vec![
            (green2, vec![verdict_step("green", json!([]))]),
            (red2, vec![verdict_step("failed", json!([]))]),
        ];
        assert_eq!(garage(&runs, &[]).len(), 1);
    }

    #[test]
    fn a_lost_run_is_not_a_red_and_leaves_the_garage() {
        // The reaper settles a dead runner as `lost`: NO VERDICT WAS
        // PRODUCED. That says nothing about the branch — it was never
        // judged — so it is not rework awaiting a fix. Measured
        // 2026-09-05: fix/lean-ci-builds sat in the garage on a lost
        // run while its head was reachable from main.
        let runs = vec![(
            gate_run_on("fix/lean-ci-builds", 4),
            vec![verdict_step("lost", json!([]))],
        )];
        assert!(
            garage(&runs, &[]).is_empty(),
            "a lost run is unknown, not red"
        );
        let runs = vec![(
            gate_run_on("fix/x", 4),
            vec![verdict_step("unreadable", json!([]))],
        )];
        assert!(
            garage(&runs, &[]).is_empty(),
            "an unreadable run is unknown, not red"
        );
    }

    /// …and an unjudged run is not NOTHING either: it stands at the gate
    /// exit, on its own siding, waiting to be re-gated. The web lens
    /// drew this lane from its own gate-run window until 2026-09-10;
    /// served here it is the same partition of the same grouping the
    /// garage reads, so a branch can never be on both sidings.
    #[test]
    fn an_unjudged_run_stands_at_the_gate_exit() {
        let mut g = gate_run_on("fix/lean-ci-builds", 4);
        g.metadata["sha"] = json!("c0ffee");
        let id = g.id.to_string();
        let runs = vec![(g, vec![verdict_step("lost", json!([]))])];
        assert_eq!(
            limbo(&runs, &[]),
            vec![LimboCar {
                branch: "fix/lean-ci-builds".into(),
                verdict: "lost".into(),
                since: "2026-09-04".into(),
                packet_id: id,
                sha: Some("c0ffee".into()),
            }]
        );
        // An unreadable receipt is the same kind of unknown.
        let runs = vec![(
            gate_run_on("fix/x", 4),
            vec![verdict_step("unreadable", json!([]))],
        )];
        assert_eq!(limbo(&runs, &[])[0].verdict, "unreadable");
        // A judged run is the garage's business, not limbo's; and a
        // branch whose car settled has left the floor entirely.
        let judged = vec![(
            gate_run_on("fix/red", 4),
            vec![verdict_step("failed", json!([]))],
        )];
        assert!(limbo(&judged, &[]).is_empty());
        let settled = vec![(
            gate_run_on("fix/gone", 4),
            vec![verdict_step("lost", json!([]))],
        )];
        assert!(limbo(&settled, &["fix/gone".to_string()]).is_empty());
    }

    /// A branch being re-gated right now is in the SLOTS, not at the
    /// gate exit: the in-flight run is its latest state and has no
    /// verdict to read.
    #[test]
    fn a_branch_regating_after_a_lost_run_is_not_in_limbo() {
        let runs = vec![
            (
                gate_run_on("fix/x", 4),
                vec![verdict_step("lost", json!([]))],
            ),
            (gate_run_on("fix/x", 5), vec![in_flight_step()]),
        ];
        assert!(limbo(&runs, &[]).is_empty());
    }

    #[test]
    fn a_branch_being_regated_right_now_is_a_slot_not_the_garage() {
        // Day 2 red, day 3 IN FLIGHT (no verdict). The latest run has no
        // terminal verdict, so the branch is not garaged — it is being
        // reworked, and shows in the slots instead.
        let runs = vec![
            (
                gate_run_on("feat/x", 2),
                vec![verdict_step(
                    "failed",
                    json!([{"name": "test", "result": "fail"}]),
                )],
            ),
            (gate_run_on("feat/x", 3), vec![in_flight_step()]),
        ];
        assert!(
            garage(&runs, &[]).is_empty(),
            "an in-flight retry is not garaged"
        );
        assert_eq!(gates(&runs, 3, None).active.len(), 1, "it occupies a slot");
    }

    #[test]
    fn the_garage_is_sorted_by_branch() {
        let runs = vec![
            (
                gate_run_on("feat/z", 3),
                vec![verdict_step(
                    "failed",
                    json!([{"name": "a", "result": "fail"}]),
                )],
            ),
            (
                gate_run_on("feat/a", 3),
                vec![verdict_step(
                    "failed",
                    json!([{"name": "b", "result": "fail"}]),
                )],
            ),
        ];
        let branches: Vec<String> = garage(&runs, &[])
            .iter()
            .map(|c| c.branch.clone())
            .collect();
        assert_eq!(branches, vec!["feat/a".to_string(), "feat/z".to_string()]);
    }

    // ---- the whole payload ----

    #[test]
    fn build_status_names_the_block_computes_the_predicate_and_limits_recents() {
        // One blocked train, a dock of two, a depth+clock cadence, one
        // arrived and one cancelled recent, one stranded green.
        let blocked_steps = vec![
            done("merged", "Merged into main", "2026-09-03T06:45:00Z"),
            step(
                "deployed",
                "Deployed to the playground",
                StepStatus::Ready,
                json!({ "deploy_blocked": "tree busy", "deploy_blocked_since": "2026-09-03T06:46:00Z" }),
            ),
        ];
        let open = vec![(train(vec![], json!({})), blocked_steps)];

        let arrived = (
            train(vec![], json!({})),
            vec![
                done(
                    "collect",
                    "Collect what is ready to board",
                    "2026-09-03T05:00:00Z",
                ),
                done("arrived", "Train arrived", "2026-09-03T05:30:00Z"),
            ],
        );
        let cancelled = (train(vec![], json!({ "outcome": "cancelled" })), vec![]);
        let closed = vec![arrived, cancelled];

        let mut dock_a = train(vec![], json!({ "branch": "feat/a" }));
        dock_a.kind = "ship-a-change".into();
        let mut dock_b = train(vec![], json!({ "branch": "feat/b" }));
        dock_b.kind = "ship-a-change".into();
        let dock = vec![dock_a, dock_b];

        let mut depth = rule("queue-depth");
        depth.min_dock_depth = Some(4);
        let mut clock = rule("clock");
        clock.at_times = Some(json!(["06:00", "18:00"]));
        let rules = vec![depth, clock];

        let policy = DeliveryPolicyRow {
            name: "train-conductor".into(),
            version: 1,
            max_red_trains: 2,
            stall_hours: 6,
            consist_excluded_lints: json!([]),
            consist_budget_secs: 600,
            consist_output_budget: 2000,
            consist_files_named: 5,
            skip_reason_file_budget: 200,
            blip_cause_budget: 200,
            ci_host_floor_gb: 10,
            gate_max_concurrent: 3,
        };

        let stranded_run = (gate_run("feat/stranded", json!({})), vec![green_step()]);

        let status = build_status(
            &open,
            &closed,
            &dock,
            &rules,
            None,
            Some(&policy),
            &[stranded_run],
            &["feat/a".into(), "feat/b".into()],
            &[],
            &[],
            fixed_now(),
        );

        // The block is named, prominently, on the train row.
        assert_eq!(status.trains.len(), 1);
        assert!(matches!(
            status.trains[0].block,
            Some(TrainBlock::DeployBlocked { .. })
        ));

        // The boarding predicate is computed from the live rules + depth.
        assert_eq!(status.boarding.dock_threshold, Some(4));
        assert_eq!(status.boarding.dock_depth, 2);
        assert_eq!(status.boarding.threshold_met, Some(false));
        assert_eq!(status.boarding.at_times, vec!["06:00", "18:00"]);

        // The dock carries both cars.
        assert_eq!(status.dock.len(), 2);

        // Recent outcomes are read, not guessed.
        assert_eq!(status.recent.len(), 2);
        assert_eq!(status.recent[0].outcome, "arrived");
        assert_eq!(status.recent[0].journey_seconds, Some(1800));
        assert_eq!(status.recent[1].outcome, "cancelled");

        // The stranded green shows (its branch is not a car).
        assert_eq!(stranded_branches(&status.stranded), vec!["feat/stranded"]);

        // The policy thresholds come from the registry row.
        assert_eq!(status.policy.stall_hours, Some(6));
        assert_eq!(status.policy.max_red_trains, Some(2));

        // Gate capacity is the policy's gate_max_concurrent, drawn as
        // slots — not a constant. The one gate-run here is green, so it
        // occupies no slot and garages nothing.
        assert_eq!(status.gates.capacity, 3);
        assert!(status.gates.active.is_empty());
        assert!(status.garage.is_empty());
    }

    #[test]
    fn no_policy_draws_the_compiled_gate_capacity() {
        // With no delivery policy the page shows the same bound a gate
        // obeys against an unreachable registry — never a fabricated
        // number.
        let status = build_status(
            &[],
            &[],
            &[],
            &[],
            None,
            None,
            &[],
            &[],
            &[],
            &[],
            fixed_now(),
        );
        assert_eq!(status.gates.capacity, COMPILED_GATE_MAX_CONCURRENT);
    }

    #[test]
    fn build_status_fills_the_slots_and_the_garage_from_the_gate_runs() {
        // One in-flight gate (a slot) and one red (the garage), plus a
        // policy sizing the capacity to 4.
        let in_flight = (gate_run_on("feat/gating", 3), vec![in_flight_step()]);
        let red = (
            gate_run_on("feat/broken", 3),
            vec![verdict_step(
                "failed",
                json!([{"name": "test", "result": "fail"}]),
            )],
        );
        let policy = DeliveryPolicyRow {
            name: "train-conductor".into(),
            version: 1,
            max_red_trains: 2,
            stall_hours: 6,
            consist_excluded_lints: json!([]),
            consist_budget_secs: 600,
            consist_output_budget: 2000,
            consist_files_named: 5,
            skip_reason_file_budget: 200,
            blip_cause_budget: 200,
            ci_host_floor_gb: 10,
            gate_max_concurrent: 4,
        };
        let status = build_status(
            &[],
            &[],
            &[],
            &[],
            None,
            Some(&policy),
            &[in_flight, red],
            &[],
            &[],
            &[],
            fixed_now(),
        );
        assert_eq!(status.gates.capacity, 4);
        assert_eq!(status.gates.active.len(), 1);
        assert_eq!(status.gates.active[0].branch, "feat/gating");
        assert_eq!(status.garage.len(), 1);
        assert_eq!(status.garage[0].branch, "feat/broken");
        assert_eq!(status.garage[0].failed_check.as_deref(), Some("test"));
    }

    #[test]
    fn recents_are_capped_at_the_limit() {
        let many: Vec<(Job, Vec<Step>)> = (0..RECENT_LIMIT + 5)
            .map(|_| (train(vec![], json!({ "outcome": "arrived" })), vec![]))
            .collect();
        let status = build_status(
            &[],
            &many,
            &[],
            &[],
            None,
            None,
            &[],
            &[],
            &[],
            &[],
            fixed_now(),
        );
        assert_eq!(status.recent.len(), RECENT_LIMIT);
    }

    /// BOTH RECEIPT SHAPES, because both are on real cars.
    ///
    /// The gate runner used to reduce `infra/gate.sh`'s account of a run
    /// to `{verdict, head, mode, fails}` before reporting it, so every
    /// receipt written before 2026-09-09 names its failures in `fails`
    /// and carries no `checks` at all — which means this reader, written
    /// against `checks`, returned None for every gate-run that ever ran.
    /// The runner now reports the whole receipt and `checks` is the
    /// primary record (it carries each check's result AND its duration);
    /// `fails` stays readable because landed cars are not rewritten.
    #[test]
    fn a_four_field_receipt_still_names_its_failing_checks() {
        let raw = serde_json::to_string(&json!({
            "verdict": "failed",
            "head": "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8",
            "mode": "full",
            "fails": ["clippy", "test"],
        }))
        .unwrap();
        let steps = vec![step(
            "record-verdict",
            "Record the receipt",
            StepStatus::Completed,
            json!({ "verdict": "failed", "receipt": raw }),
        )];
        assert_eq!(failing_check(&steps).as_deref(), Some("clippy, test"));
    }

    /// A wide receipt names them from `checks`, ignoring the durations
    /// and every other field it now carries.
    #[test]
    fn a_wide_receipt_names_its_failing_checks_from_checks() {
        let raw = serde_json::to_string(&json!({
            "verdict": "failed",
            "mode": "full",
            "scope": "",
            "head": "e16708f69bc5b0a0a3f4bd1572f9db6dec76e7c8",
            "dirty": false,
            "host": "gate-runner-abc",
            "ci": true,
            "free_gb": 91,
            "unverifiable": [],
            "report": { "attempts": 4, "waited_s": 63, "sor_unreachable": true },
            "checks": [
                { "name": "fmt", "result": "pass", "seconds": 3 },
                { "name": "clippy", "result": "fail", "seconds": 44 },
                { "name": "test", "result": "fail", "seconds": 812 },
            ],
        }))
        .unwrap();
        let steps = vec![step(
            "record-verdict",
            "Record the receipt",
            StepStatus::Completed,
            json!({ "verdict": "failed", "receipt": raw }),
        )];
        assert_eq!(failing_check(&steps).as_deref(), Some("clippy, test"));
    }

    // ---- the ETA on a train in flight ----
    //
    // MEASURED 2026-09-10 against the live record (1,014 pr-trains):
    // 696 cancelled, 254 arrived, 143 of those with a readable
    // `board_to_merge_s`, 105 with a readable merge→arrival leg.
    // board→merge p10/median/p90 = 890 / 1206 / 1709s; merge→arrival =
    // 589 / 1183 / 4799s. The fixtures below are shaped to those.

    /// An arrived train as the conductor records one: `outcome` stamped,
    /// the arrival report's timings, and the server's `closed_at`.
    fn arrived(board_to_merge_s: i64, merge_to_arrival_s: i64) -> Job {
        let merged = at("2026-09-04T10:00:00Z");
        let closed = merged + chrono::Duration::seconds(merge_to_arrival_s);
        let mut j = train(vec![], json!({}));
        j.status = JobStatus::Closed;
        j.metadata = json!({
            "outcome": "arrived",
            "closed_at": closed.to_rfc3339(),
            "arrival_report": {
                "timings": {
                    "board_to_merge_s": board_to_merge_s,
                    "merged_at": merged.to_rfc3339(),
                    // Measured null on ALL 143 arrived trains — the
                    // fixture carries the same hole the record has, so a
                    // reader that leans on them fails here.
                    "total_s": null,
                    "arrived_at": null,
                }
            }
        });
        j
    }

    /// The same arrival, carrying `n` cars.
    fn with_cars(mut j: Job, n: usize) -> Job {
        let cars: Vec<Value> = (0..n).map(|i| json!(format!("car-{i}"))).collect();
        j.metadata["boarded_jobs"] = json!(cars);
        j
    }

    /// A refused board: the consist check turned it away, so the Job
    /// opened, cancelled, carried no cars and measured nothing. 696 of
    /// the 1,014 pr-trains on record are this.
    fn refused() -> Job {
        let mut j = train(vec![], json!({}));
        j.status = JobStatus::Closed;
        j.metadata = json!({
            "outcome": "cancelled",
            "closed_at": "2026-09-04T10:00:00Z",
            "boarded_jobs": [],
        });
        j
    }

    /// A population big enough to measure: `n` arrivals spread across
    /// the measured range, so p10 < median < p90 are distinct.
    fn population(n: usize) -> Vec<Job> {
        (0..n)
            .map(|i| {
                let k = i64::try_from(i).unwrap_or(0);
                arrived(900 + k * 100, 600 + k * 100)
            })
            .collect()
    }

    fn pre_merge_steps(boarded: &str) -> Vec<Step> {
        vec![
            done("collect", "Collect what is ready to board", boarded),
            done("pr", "Open the batched PR", boarded),
            step("ci", "CI verdict", StepStatus::Active, json!({})),
            step("merged", "Merged into main", StepStatus::Ready, json!({})),
        ]
    }

    fn post_merge_steps(boarded: &str, merged: &str) -> Vec<Step> {
        vec![
            done("collect", "Collect what is ready to board", boarded),
            done("pr", "Open the batched PR", boarded),
            done("ci", "CI verdict", merged),
            done("merged", "Merged into main", merged),
            step(
                "deployed",
                "Deployed to the playground",
                StepStatus::Ready,
                json!({}),
            ),
            step(
                "converged",
                "Cluster converged",
                StepStatus::Pending,
                json!({}),
            ),
        ]
    }

    /// THE TRAP. A board the consist check refused still opened a
    /// pr-train Job and cancelled it, so the recent population is mostly
    /// zero-length cancellations — at one point every row of page one.
    /// A sample taken on RECENCY would hand an operator a confident
    /// near-instant ETA. The population is chosen by OUTCOME, so a pile
    /// of refusals measures nothing and the field says so.
    #[test]
    fn a_pile_of_refused_boards_measures_nothing() {
        let noise: Vec<Job> = (0..200).map(|_| refused()).collect();
        let basis = eta_basis(&noise);
        assert_eq!(
            basis,
            EtaBasis::Thin {
                arrivals: 0,
                measurable: 0
            },
            "200 cancelled trains are 0 arrivals, not a 0-second journey"
        );
        let eta = train_eta(
            &pre_merge_steps("2026-09-04T11:55:00Z"),
            &basis,
            fixed_now(),
        );
        match eta {
            TrainEta::Unknown { reason } => assert!(
                reason.contains("0 arrived"),
                "the reason must name the population it found: {reason}"
            ),
            other => panic!("a pile of refusals must not yield an estimate: {other:?}"),
        }
    }

    /// And the same population mixed with a couple of real arrivals must
    /// still refuse: two measurable arrivals is an anecdote.
    #[test]
    fn a_thin_history_refuses_with_its_count() {
        let mut pop: Vec<Job> = (0..200).map(|_| refused()).collect();
        pop.extend(population(MIN_ETA_ARRIVALS - 1));
        let basis = eta_basis(&pop);
        assert_eq!(
            basis,
            EtaBasis::Thin {
                arrivals: MIN_ETA_ARRIVALS - 1,
                measurable: MIN_ETA_ARRIVALS - 1
            }
        );
        let eta = train_eta(
            &pre_merge_steps("2026-09-04T11:55:00Z"),
            &basis,
            fixed_now(),
        );
        match eta {
            TrainEta::Unknown { reason } => {
                assert!(reason.contains("9"), "name the count: {reason}");
                assert!(
                    reason.contains(&MIN_ETA_ARRIVALS.to_string()),
                    "name the floor it fell short of: {reason}"
                );
            }
            other => panic!("9 arrivals is below the floor: {other:?}"),
        }

        // One more crosses it.
        pop.push(arrived(1206, 1183));
        assert!(matches!(eta_basis(&pop), EtaBasis::Measured { .. }));
    }

    /// A pre-merge train's estimate covers BOTH legs and names that, so
    /// a reader never has to guess which leg the number measures.
    #[test]
    fn a_pre_merge_train_estimates_the_whole_journey_and_says_so() {
        let basis = eta_basis(&population(20));
        // Boarded 5 minutes before `fixed_now` (12:00:00Z).
        let eta = train_eta(
            &pre_merge_steps("2026-09-04T11:55:00Z"),
            &basis,
            fixed_now(),
        );
        let TrainEta::Estimate {
            leg,
            remaining_seconds,
            remaining_low_seconds,
            remaining_high_seconds,
            sample_size,
            overdue,
            ..
        } = eta
        else {
            panic!("20 measured arrivals must yield an estimate: {eta:?}");
        };
        assert_eq!(leg, "boarding → arrival", "the leg is named on the wire");
        assert_eq!(sample_size, 20);
        assert!(!overdue, "5 minutes in is not overdue");
        // population(20): board→merge 900..2800 step 100, merge→arrival
        // 600..2500 step 100. median index 10 → 1900 / 1600; p10 index 2
        // → 1100 / 800; p90 index 18 → 2700 / 2400.
        assert_eq!(remaining_seconds, (1900 - 300) + 1600);
        assert_eq!(remaining_low_seconds, (1100 - 300) + 800);
        assert_eq!(remaining_high_seconds, (2700 - 300) + 2400);
        assert!(
            remaining_low_seconds < remaining_seconds && remaining_seconds < remaining_high_seconds,
            "the spread must straddle the median — a point estimate over-claims"
        );
    }

    /// PHASE AWARENESS. merge→arrival measured a median of 1,183s — half
    /// the journey — so a merged train told the whole-journey figure is
    /// told roughly double what it has left. Its estimate covers the
    /// remaining leg only, measured from the MERGE stamp, and names it.
    #[test]
    fn a_merged_train_estimates_only_the_leg_it_is_on() {
        let basis = eta_basis(&population(20));
        let steps = post_merge_steps("2026-09-04T11:00:00Z", "2026-09-04T11:55:00Z");
        assert_eq!(phase_of(&steps), TrainPhase::Deploying);
        let eta = train_eta(&steps, &basis, fixed_now());
        let TrainEta::Estimate {
            leg,
            remaining_seconds,
            remaining_low_seconds,
            remaining_high_seconds,
            sample_size,
            ..
        } = eta
        else {
            panic!("expected an estimate: {eta:?}");
        };
        assert_eq!(leg, "merge → arrival");
        assert_eq!(sample_size, 20);
        // 5 minutes past the merge, against the merge→arrival leg only.
        assert_eq!(remaining_seconds, 1600 - 300);
        assert_eq!(remaining_low_seconds, 800 - 300);
        assert_eq!(remaining_high_seconds, 2400 - 300);
        // It must NOT be the whole-journey figure the pre-merge train got.
        assert!(
            remaining_seconds < (1900 - 300) + 1600,
            "a merged train has less left than a boarding one — mixing \
             the legs is what this test exists to catch"
        );
    }

    /// A train past the 90th percentile of everything measured is
    /// overdue, and says so rather than counting down below zero. "A
    /// troubled packet must look troubled" (CLAUDE.md §Diagnosis).
    #[test]
    fn a_train_past_the_measured_spread_reads_overdue() {
        let basis = eta_basis(&population(20));
        // p90 of the whole journey is 2700 + 2400 = 5100s = 85 min.
        // Boarded 3 hours before `fixed_now`.
        let eta = train_eta(
            &pre_merge_steps("2026-09-04T09:00:00Z"),
            &basis,
            fixed_now(),
        );
        let TrainEta::Estimate {
            remaining_seconds,
            overdue,
            remaining_high_seconds,
            ..
        } = eta
        else {
            panic!("expected an estimate: {eta:?}");
        };
        assert!(overdue, "3h against an 85-minute p90 is overdue");
        assert_eq!(
            remaining_seconds, 1600,
            "the elapsed leg is spent, never negative — what is left is \
             the leg not yet started"
        );
        assert_eq!(remaining_high_seconds, 2400);
    }

    /// The spread is WIDE and that is the finding, not a flaw: within one
    /// car count arrivals ranged 770s to 4,274s. A single number would
    /// over-claim, so the field publishes p10 and p90 beside the median.
    #[test]
    fn a_wide_spread_is_published_not_flattened() {
        // Ten arrivals whose board→merge spans the measured 770..7391,
        // with merge→arrival fixed so the spread is attributable.
        let pop: Vec<Job> = [770, 890, 1070, 1140, 1206, 1318, 1439, 1709, 4274, 7391]
            .into_iter()
            .map(|b| arrived(b, 1183))
            .collect();
        let EtaBasis::Measured {
            board_to_merge,
            merge_to_arrival,
        } = eta_basis(&pop)
        else {
            panic!("10 arrivals is the floor, exactly");
        };
        // n=10 → p10 index 1, median index 5, p90 index 9: three
        // distinct observations, and p10 is not the minimum.
        assert_eq!(board_to_merge.low, 890);
        assert_eq!(board_to_merge.median, 1318);
        assert_eq!(board_to_merge.high, 7391);
        assert_eq!(board_to_merge.n, 10);
        assert_eq!(merge_to_arrival.median, 1183);
        assert!(
            board_to_merge.high > board_to_merge.median * 5,
            "the tail is real — flattening it to the median would hide \
             that a train CAN take two hours"
        );
    }

    /// An arrived train is not waiting for anything, and a train whose
    /// stamps or clock cannot be read is told so — never handed a number
    /// derived from a missing instant.
    #[test]
    fn an_unreadable_train_is_told_why_not_guessed() {
        let basis = eta_basis(&population(20));

        let arrived_steps = vec![
            done(
                "collect",
                "Collect what is ready to board",
                "2026-09-04T10:00:00Z",
            ),
            done("merged", "Merged into main", "2026-09-04T10:20:00Z"),
            done(
                "deployed",
                "Deployed to the playground",
                "2026-09-04T10:30:00Z",
            ),
            done("converged", "Cluster converged", "2026-09-04T10:40:00Z"),
        ];
        match train_eta(&arrived_steps, &basis, fixed_now()) {
            TrainEta::Unknown { reason } => {
                assert!(reason.contains("arrived"), "{reason}")
            }
            other => panic!("an arrived train needs no ETA: {other:?}"),
        }

        // No clock: the read-model never reaches for wall time.
        match train_eta(&pre_merge_steps("2026-09-04T11:55:00Z"), &basis, None) {
            TrainEta::Unknown { reason } => assert!(reason.contains("clock"), "{reason}"),
            other => panic!("no clock means no estimate: {other:?}"),
        }

        // Boarded, but the collect step carries no stamp: elapsed is
        // unreadable, so there is nothing to subtract.
        let unstamped = vec![
            step(
                "collect",
                "Collect what is ready to board",
                StepStatus::Completed,
                json!({}),
            ),
            step(
                "pr",
                "Open the batched PR",
                StepStatus::Completed,
                json!({}),
            ),
        ];
        match train_eta(&unstamped, &basis, fixed_now()) {
            TrainEta::Unknown { reason } => assert!(reason.contains("stamp"), "{reason}"),
            other => panic!("no boarding stamp means no estimate: {other:?}"),
        }

        // Merged, but the merge step carries no stamp. Falling back to
        // the whole-journey figure here would SILENTLY MIX the legs.
        let merged_unstamped = vec![
            done(
                "collect",
                "Collect what is ready to board",
                "2026-09-04T11:00:00Z",
            ),
            done("pr", "Open the batched PR", "2026-09-04T11:00:00Z"),
            step(
                "merged",
                "Merged into main",
                StepStatus::Completed,
                json!({}),
            ),
            step(
                "deployed",
                "Deployed to the playground",
                StepStatus::Ready,
                json!({}),
            ),
        ];
        match train_eta(&merged_unstamped, &basis, fixed_now()) {
            TrainEta::Unknown { reason } => assert!(reason.contains("stamp"), "{reason}"),
            other => panic!("a merged train with no merge stamp must refuse: {other:?}"),
        }
    }

    /// An arrival with a zero or backwards leg is not a 0-second train:
    /// `merge_to_deploy_s` is 0 on every arrival measured, which is why
    /// nothing here reads it. Non-positive legs are dropped, not averaged.
    #[test]
    fn a_zero_length_leg_is_dropped_not_averaged() {
        let mut pop = population(MIN_ETA_ARRIVALS);
        // A backwards pair: closed BEFORE the merge (a clock skew, or a
        // `closed_at` from a different path).
        let mut broken = arrived(1206, 0);
        broken.metadata = json!({
            "outcome": "arrived",
            "closed_at": "2026-09-04T09:00:00Z",
            "arrival_report": { "timings": {
                "board_to_merge_s": 0,
                "merged_at": "2026-09-04T10:00:00Z",
            }},
        });
        pop.push(broken);
        let EtaBasis::Measured {
            board_to_merge,
            merge_to_arrival,
        } = eta_basis(&pop)
        else {
            panic!("the good arrivals still measure");
        };
        assert_eq!(board_to_merge.n, MIN_ETA_ARRIVALS, "the 0 leg is dropped");
        assert_eq!(
            merge_to_arrival.n, MIN_ETA_ARRIVALS,
            "the backwards leg too"
        );
    }

    /// The wire shape: a tagged union, so the SPA reads one field and
    /// gets either a number or a reason — never a bare null.
    #[test]
    fn the_eta_serializes_as_a_tagged_union() {
        let basis = eta_basis(&population(20));
        let v = serde_json::to_value(train_eta(
            &pre_merge_steps("2026-09-04T11:55:00Z"),
            &basis,
            fixed_now(),
        ))
        .unwrap();
        assert_eq!(v["kind"], "estimate");
        assert_eq!(v["leg"], "boarding → arrival");
        assert!(v["remaining_seconds"].is_i64());
        assert!(v["basis"].is_string(), "the basis is stated, not implied");

        let v = serde_json::to_value(train_eta(
            &pre_merge_steps("2026-09-04T11:55:00Z"),
            &eta_basis(&[]),
            fixed_now(),
        ))
        .unwrap();
        assert_eq!(v["kind"], "unknown");
        assert!(v["reason"].is_string());
    }

    /// The status carries the estimate on the train row — the whole
    /// point: David should not have to ask whether a 20-minute transit
    /// is normal.
    #[test]
    fn the_status_carries_an_eta_on_the_train_in_flight() {
        let job = train(vec![], json!({ "boarded_jobs": ["a", "b"] }));
        let steps = pre_merge_steps("2026-09-04T11:55:00Z");
        let status = build_status(
            &[(job, steps)],
            &[],
            &[],
            &[],
            None,
            None,
            &[],
            &[],
            &[],
            &population(20),
            fixed_now(),
        );
        assert!(
            matches!(status.trains[0].eta, TrainEta::Estimate { .. }),
            "the in-flight train states its ETA: {:?}",
            status.trains[0].eta
        );
    }

    /// Car count is NOT a dimension of the estimate, and this test is the
    /// record of why. MEASURED 2026-09-10 over 143 arrivals: Spearman rho
    /// between car count and board→merge is 0.167, and the bucket medians
    /// are 1,140 / 1,197 / 1,256 / 1,259s for 1 / 2-3 / 4-6 / 7+ cars — a
    /// 10% spread BETWEEN buckets against a 770..4,274s spread INSIDE
    /// one. Splitting 143 samples into an 8-sample bucket to move the
    /// median 4% is strictly worse than measuring the whole population,
    /// so the estimate is blind to car count by design.
    #[test]
    fn the_estimate_does_not_vary_with_car_count() {
        let light: Vec<Job> = population(20)
            .into_iter()
            .map(|j| with_cars(j, 1))
            .collect();
        let heavy: Vec<Job> = population(20)
            .into_iter()
            .map(|j| with_cars(j, 16))
            .collect();
        assert_eq!(
            eta_basis(&light),
            eta_basis(&heavy),
            "same timings, different consists — the basis must not move; \
             see the measurement above for why car count is not a dimension"
        );
    }
}
