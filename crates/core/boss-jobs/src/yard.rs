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

use crate::cadence::CadenceRuleRow;
use crate::delivery::DeliveryPolicyRow;

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

/// A step's `completed_at` metadata stamp — the conductor writes it on
/// completion. Not `completed_on` (date-only); the RFC3339 instant is
/// what makes journey timings derivable.
fn completed_at(step: Option<&Step>) -> Option<&str> {
    step?.metadata.get("completed_at").and_then(Value::as_str)
}

fn meta_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
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
fn block_of(job: &Job, steps: &[Step], phase: TrainPhase) -> Option<TrainBlock> {
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
    None
}

/// A metadata flag written as either `true` or the string `"true"`
/// (the conductor's PATCH merges write bools; older writes wrote
/// strings) — accept both.
fn truthy(v: &Value) -> bool {
    v.as_bool() == Some(true) || v.as_str() == Some("true")
}

/// Build one train row from its Job and steps.
pub fn train_status(job: &Job, steps: &[Step]) -> TrainStatus {
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
        block: block_of(job, steps, phase),
        ci_result: find_step(steps, &CI)
            .and_then(|s| meta_str(&s.metadata, "result"))
            .map(str::to_string),
        pr_url: find_step(steps, &PR)
            .and_then(|s| meta_str(&s.metadata, "pr_url"))
            .map(str::to_string),
        car_count,
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
}

/// A cadence rule fires by DEPTH when it declares `min_dock_depth`.
fn depth_rule(rules: &[CadenceRuleRow]) -> Option<&CadenceRuleRow> {
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

pub fn boarding_predicate(rules: &[CadenceRuleRow], dock_depth: usize) -> BoardingPredicate {
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
    /// board → arrival, in seconds, when both stamps are present.
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

fn journey_seconds(steps: &[Step]) -> Option<i64> {
    let boarded = completed_at(find_step(steps, &COLLECT))?;
    let arrived = completed_at(find_step(steps, &ARRIVED))?;
    let b = chrono::DateTime::parse_from_rfc3339(boarded).ok()?;
    let a = chrono::DateTime::parse_from_rfc3339(arrived).ok()?;
    Some((a - b).num_seconds())
}

pub fn recent_train(job: &Job, steps: &[Step]) -> RecentTrain {
    RecentTrain {
        id: job.id.to_string(),
        title: job.title.clone(),
        outcome: outcome_of(job, steps),
        journey_seconds: journey_seconds(steps),
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StrandedGreen {
    pub branch: String,
}

/// A gate-run is stranded when: it is not marked `superseded`, one of its
/// steps recorded `verdict == "green"`, its branch is known, and no car
/// claims that branch.
fn gate_run_is_stranded(gate_run: &Job, steps: &[Step], car_branches: &[String]) -> Option<String> {
    if gate_run
        .metadata
        .get("superseded")
        .is_some_and(|v| !v.is_null() && v.as_bool() != Some(false))
    {
        return None;
    }
    let green = steps
        .iter()
        .any(|s| meta_str(&s.metadata, "verdict") == Some("green"));
    if !green {
        return None;
    }
    let branch = meta_str(&gate_run.metadata, "branch")?;
    if branch.is_empty() || car_branches.iter().any(|b| b == branch) {
        return None;
    }
    Some(branch.to_string())
}

/// Every stranded green among `(gate_run, steps)` pairs, de-duped and
/// sorted — branch names an operator can rescue or drop.
pub fn stranded_greens(
    gate_runs: &[(Job, Vec<Step>)],
    car_branches: &[String],
) -> Vec<StrandedGreen> {
    let mut out: Vec<String> = Vec::new();
    for (g, steps) in gate_runs {
        if let Some(b) = gate_run_is_stranded(g, steps, car_branches)
            && !out.contains(&b)
        {
            out.push(b);
        }
    }
    out.sort();
    out.into_iter()
        .map(|branch| StrandedGreen { branch })
        .collect()
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

/// The failing check a red gate-run named, from its receipt's `checks`
/// array — the entries whose `result` is not `pass`, joined `a, b`. The
/// receipt is a JSON STRING in the `record-verdict` step's
/// `metadata.receipt` (the same encoding `boss receipt` parses), so it
/// needs a second parse; a runner that died before a receipt leaves
/// prose there, which fails the parse and correctly reads as "no named
/// check". `None` when nothing was named.
fn failing_check(steps: &[Step]) -> Option<String> {
    let raw = steps
        .iter()
        .find_map(|s| meta_str(&s.metadata, "receipt"))?;
    let receipt: Value = serde_json::from_str(raw).ok()?;
    let checks = receipt.get("checks").and_then(Value::as_array)?;
    let failed: Vec<String> = checks
        .iter()
        .filter(|c| c.get("result").and_then(Value::as_str) != Some("pass"))
        .filter_map(|c| c.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    (!failed.is_empty()).then(|| failed.join(", "))
}

/// One gate currently being assessed — an open gate-run that has not
/// reported a verdict. The Approach draws these into its parallel gate
/// SLOTS so capacity and usage read at a glance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveGate {
    pub branch: String,
    pub packet_id: String,
    /// When the gate-run opened — date-level, its own `opened_on` stamp,
    /// the finest instant a gate-run Job carries (like `DockCar`).
    pub since: String,
}

/// The gate slots the Approach renders: how many gates run at once
/// (`capacity`, from the delivery policy — never a constant baked into
/// the page) and which cars occupy them right now (`active`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gates {
    pub capacity: i32,
    pub active: Vec<ActiveGate>,
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
    /// When the failing gate-run opened — date-level, as above.
    pub since: String,
}

/// The in-flight gates, sized to the policy's capacity. Active gate-runs
/// (open, no verdict yet) are sorted by `since` then `branch` so slot
/// assignment is deterministic — the same run lands in the same slot
/// across polls.
pub fn gates(gate_runs: &[(Job, Vec<Step>)], capacity: i32) -> Gates {
    let mut active: Vec<ActiveGate> = gate_runs
        .iter()
        .filter(|(g, _)| g.status == JobStatus::Open)
        .filter(|(_, steps)| gate_run_verdict(steps).is_none())
        .filter_map(|(g, _)| {
            meta_str(&g.metadata, "branch")
                .filter(|b| !b.is_empty())
                .map(|branch| ActiveGate {
                    branch: branch.to_string(),
                    packet_id: g.id.to_string(),
                    since: g.opened_on.to_string(),
                })
        })
        .collect();
    active.sort_by(|a, b| a.since.cmp(&b.since).then_with(|| a.branch.cmp(&b.branch)));
    Gates { capacity, active }
}

/// The garage: cars whose MOST-RECENT gate-run is red. A branch that
/// failed then re-gated green is fixed and does not show — so gate-runs
/// are grouped by branch, the latest kept (by `opened_on`, tie-broken by
/// packet id so the order is total), and the garage is every branch
/// whose latest is a non-green terminal verdict (`failed` / `lost` /
/// `unreadable`). In-flight runs (no verdict) do not count as the latest
/// state: a branch being re-gated right now is in the SLOTS, not the
/// garage, so a still-running retry is only kept as latest when it is the
/// sole run. Sorted by branch for a stable render.
pub fn garage(gate_runs: &[(Job, Vec<Step>)], settled_branches: &[String]) -> Vec<GaragedCar> {
    use std::collections::HashMap;
    // branch -> the latest (job, steps) seen for it.
    let mut latest: HashMap<&str, (&Job, &[Step])> = HashMap::new();
    for (g, steps) in gate_runs {
        let Some(branch) = meta_str(&g.metadata, "branch").filter(|b| !b.is_empty()) else {
            continue;
        };
        match latest.get(branch) {
            Some((prev, _))
                if (prev.opened_on, prev.id.to_string()) >= (g.opened_on, g.id.to_string()) => {}
            _ => {
                latest.insert(branch, (g, steps.as_slice()));
            }
        }
    }
    let mut out: Vec<GaragedCar> = latest
        .into_iter()
        .filter_map(|(branch, (g, steps))| {
            // A branch whose car has SETTLED is not awaiting rework — the
            // work finished, by landing or by being dropped. Its last
            // gate-run under that name stays red forever, because a car
            // fixed by re-railing onto a fresh branch, or squash-merged
            // and deleted, never re-gates under the old name to clear it.
            // Without this the garage accumulates ghosts: on 2026-09-04 all
            // three entries were landed work (branches deleted from the
            // forge, packets closed), which makes a rework queue nobody can
            // trust. A red branch with NO car at all is kept — red never
            // parks, so that is the ordinary case the garage exists for.
            if settled_branches.iter().any(|b| b == branch) {
                return None;
            }
            let verdict = gate_run_verdict(steps)?;
            // A terminal non-green verdict = red: awaiting rework. Green
            // is fixed; an in-flight run has no verdict and was filtered
            // above.
            (verdict != "green").then(|| GaragedCar {
                branch: branch.to_string(),
                failed_check: failing_check(steps),
                since: g.opened_on.to_string(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.branch.cmp(&b.branch));
    out
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
    /// The parallel gate slots: capacity (from the policy) + the cars
    /// occupying them right now.
    pub gates: Gates,
    /// Cars whose latest gate-run is red — waiting for rework.
    pub garage: Vec<GaragedCar>,
    /// The alarm thresholds the yard enforces, from the delivery policy.
    pub policy: PolicyThresholds,
}

/// How many recent trains the status carries. Enough to read a trend in
/// arrivals/cancellations without turning the surface into a history log
/// — the terminal report owns the long view.
pub const RECENT_LIMIT: usize = 8;

/// Assemble the full status from the rows the handler fetched.
///
/// - `open_trains` — open pr-train Jobs with their steps.
/// - `closed_trains` — recently-closed pr-train Jobs with their steps,
///   already ordered newest-first by the caller (the adapter orders by
///   `opened_on desc`, which for a batch of same-day trains is close
///   enough; the terminal report owns precise cycle stats).
/// - `dock_cars` — the loading-dock queue's parked cars.
/// - `rules` — the active cadence rows.
/// - `policy` — the active delivery policy, if any.
/// - `gate_runs` / `car_branches` — for the stranded cross-ref.
/// - `settled_car_branches` — branches whose car reached a terminal; the
///   garage drops these, since settled work is not awaiting rework.
#[allow(clippy::too_many_arguments)]
pub fn build_status(
    open_trains: &[(Job, Vec<Step>)],
    closed_trains: &[(Job, Vec<Step>)],
    dock_cars: &[Job],
    rules: &[CadenceRuleRow],
    policy: Option<&DeliveryPolicyRow>,
    gate_runs: &[(Job, Vec<Step>)],
    car_branches: &[String],
    settled_car_branches: &[String],
) -> YardStatus {
    let trains = open_trains
        .iter()
        .map(|(j, s)| train_status(j, s))
        .collect();
    let dock: Vec<DockCar> = dock_cars.iter().map(dock_car).collect();
    let boarding = boarding_predicate(rules, dock.len());
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
        gates: gates(gate_runs, capacity),
        garage: garage(gate_runs, settled_car_branches),
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
        let ts = train_status(&job, &steps);
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
        assert_eq!(block_of(&job, &steps, TrainPhase::Converging), None);
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
        let ts = train_status(&job, &steps);
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
        assert_eq!(block_of(&job, &steps, TrainPhase::Deploying), None);
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
            block_of(&job, &steps, TrainPhase::Converging),
            Some(TrainBlock::ConvergeOverdue)
        );
        // Also accepts the string form older writes used.
        let job2 = train(vec![], json!({ "converge_alarm_filed": "true" }));
        assert_eq!(
            block_of(&job2, &steps, TrainPhase::Converging),
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
            block_of(&job, &steps, TrainPhase::Boarding),
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
            block_of(&job, &steps, TrainPhase::Deploying),
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
        assert_eq!(train_status(&job, &steps).block, None);
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

        let p = boarding_predicate(&rules, 2);
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
        let p = boarding_predicate(&[depth], 5);
        assert_eq!(p.threshold_met, Some(true));
        assert!(p.summary.contains("the dock threshold is met"));
    }

    #[test]
    fn no_cadence_rules_reads_as_no_configured_cadence_not_a_fake_schedule() {
        let p = boarding_predicate(&[], 3);
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
        let p = boarding_predicate(&[clock], 0);
        assert_eq!(p.at_times, vec!["06:00"]);
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

    #[test]
    fn a_green_gate_run_with_no_car_is_stranded() {
        let g = gate_run("feat/x", json!({}));
        let out = stranded_greens(&[(g, vec![green_step()])], &[]);
        assert_eq!(
            out,
            vec![StrandedGreen {
                branch: "feat/x".into()
            }]
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
        let g = gates(&runs, 4);
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

    #[test]
    fn capacity_is_the_number_passed_never_a_constant() {
        // The same runs render into whatever capacity the policy set.
        let runs = vec![(gate_run_on("feat/x", 2), vec![in_flight_step()])];
        assert_eq!(gates(&runs, 3).capacity, 3);
        assert_eq!(gates(&runs, 7).capacity, 7);
    }

    #[test]
    fn a_closed_gate_run_never_occupies_a_slot() {
        // A gate-run whose Job has closed (verdict aside) is not in the
        // gates — a slot holds a car being assessed RIGHT NOW.
        let mut j = gate_run_on("feat/x", 2);
        j.status = JobStatus::Closed;
        let g = gates(&[(j, vec![in_flight_step()])], 3);
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
        // A run that died outside a check (headroom guard, crash) names
        // no failing check — the garage row shows the branch anyway.
        let runs = vec![(
            gate_run_on("feat/x", 3),
            vec![verdict_step("lost", json!([]))],
        )];
        let g = garage(&runs, &[]);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].failed_check, None);
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
        assert_eq!(gates(&runs, 3).active.len(), 1, "it occupies a slot");
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
            Some(&policy),
            &[stranded_run],
            &["feat/a".into(), "feat/b".into()],
            &[],
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
        assert_eq!(
            status.stranded,
            vec![StrandedGreen {
                branch: "feat/stranded".into()
            }]
        );

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
        let status = build_status(&[], &[], &[], &[], None, &[], &[], &[]);
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
            Some(&policy),
            &[in_flight, red],
            &[],
            &[],
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
        let status = build_status(&[], &many, &[], &[], None, &[], &[], &[]);
        assert_eq!(status.recent.len(), RECENT_LIMIT);
    }
}
