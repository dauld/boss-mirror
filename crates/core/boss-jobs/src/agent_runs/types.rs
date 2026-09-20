//! Wire + domain types for the agent-run record.
//!
//! Three deliberate shapes here, each the answer to a way this could
//! have been got wrong:
//!
//! 1. **THE MODEL IS A FACT ABOUT THE RUN.** It is a column,
//!    `agent_runs.model`, resolved ONCE when the run is recorded and
//!    written down — design 6fda05ae, resolution model-on-run (decided
//!    2026-09-15): one registered agent (`agent-claude`, an `agents`
//!    row) runs different models over time, and cost is priced per
//!    run, so the run names the model and the agent row carries only
//!    the DEFAULT for a run that does not say. This rule used to read
//!    the other way — "the model is not a field, it is read out of
//!    `actor_id`" — and it was right while the actor id carried the
//!    model (`claude:opus-5`); a registered agent's id is model-free
//!    by decision, so the column is now the only place the fact lives.
//!    [`NewAgentRun::model`] is the one resolution: the column first,
//!    and the colon-form actor id ONLY as the fallback for rows and
//!    events written before the column existed. The `ActorId::Agent`
//!    arm survives for exactly those rows; a new run by a registered
//!    agent carries the registry id and the model beside it.
//!
//! 2. **THE PRICE IS NOT SUPPLIED BY THE CALLER.** A run reports
//!    tokens; the rate card turns tokens into micro-USD. A caller that
//!    could assert its own `usd_micros` would make the rate card
//!    decorative. [`price_run`] is the one pricing function and both
//!    adapters call it.
//!
//! 3. **AN UNPRICED RUN IS `None`, NEVER ZERO.** If no rate-card row
//!    covers the model, the run is still recorded and its cost reads
//!    as unknown. Zero would read as *free* — a component answering
//!    instead of erroring — and the roll-up counts unpriced runs out
//!    loud so the missing row is visible rather than absorbed.
//!
//! 4. **A RUN THAT ONLY KNOWS ITS TOTAL IS STILL A RECORD.** The first
//!    real caller could not fill shape 2 at all: the coding agents that
//!    build cars on this pod report `subagent_tokens`, ONE number, and
//!    no input/output split exists anywhere for them. The schema
//!    demanded a split, so the only ways forward were to invent one or
//!    to record nothing. [`TokenUsage`] is the third way — the split
//!    when it is measured, the total when it is all there is — and a
//!    total-only run falls straight into rule 3: the card prices the
//!    two halves at DIFFERENT rates, so a total genuinely cannot be
//!    priced, and it reads as unpriced rather than as a blended guess.
//!    A blend would be a fabricated number wearing a measurement's
//!    clothes, which is the failure this whole module exists to refuse.

use std::collections::BTreeMap;

use boss_core::actor::ActorId;
use boss_core::agent::{AgentLoad, BudgetDecision, Cost, Window};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How a run ended. Narrower than `boss_core::agent::Outcome`, which
/// carries a response body and a `Cost` the caller would be asserting;
/// this is the terminal state alone, with the tokens reported beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Success,
    Failed,
    Cancelled,
}

impl RunOutcome {
    pub fn as_str(&self) -> &'static str {
        match self {
            RunOutcome::Success => "success",
            RunOutcome::Failed => "failed",
            RunOutcome::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "success" => Some(RunOutcome::Success),
            "failed" => Some(RunOutcome::Failed),
            "cancelled" => Some(RunOutcome::Cancelled),
            _ => None,
        }
    }
}

/// One row of `agent_rate_card` — what a model's tokens cost.
///
/// Micro-USD per million tokens, integer on purpose (the same reason
/// `boss_core::agent::Cost` is integer): $5.00/MTok is `5_000_000`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateCardRow {
    /// The model half of an agent `ActorId` — `opus-5`, not
    /// `claude:opus-5` and not `claude-opus-5`. Matching is exact: a
    /// model string no row names is unpriced, which is visible, rather
    /// than fuzzily matched to a neighbour, which is not.
    pub model: String,
    pub input_usd_micros_per_mtok: u64,
    pub output_usd_micros_per_mtok: u64,
    /// Where the number came from, so a reader can check it.
    #[serde(default)]
    pub note: String,
}

/// What a run spent, in the three shapes a reporter can actually be in.
///
/// **The total is one fact with one definition.** For a [`Split`] it is
/// DERIVED from the halves, so a stored total cannot drift from them
/// (§9a); for [`TotalOnly`] it is the only thing measured. There is no
/// state where both are stored independently and can disagree, because
/// this type cannot express one.
///
/// A `TotalOnly` run is unpriceable by construction: the rate card
/// charges input and output at different rates. That is not a gap to
/// paper over with an assumed ratio — an assumed ratio is a number that
/// looks measured and is not.
///
/// **[`Unreported`] is UNKNOWN, never zero** (backlog 65c9c05a,
/// 2026-09-19). A harness that printed no usage line leaves the
/// reporter with nothing to say, and the record has to say that. Until
/// this variant existed, `boss dispatch --report` without `--tokens`
/// sent `total_tokens: 0` with a `detail.tokens_reported: false`
/// beside it, so 13 of 42 live rows read as a measurement of zero to
/// any query that did not know to check the companion flag — an
/// average over the column silently included 13 zeros. NULL for
/// unknown is the distinction [`AgentRun::usd_micros`] already draws
/// in this same table (unpriced, not free), followed here rather than
/// a third convention invented beside it.
///
/// [`Split`]: TokenUsage::Split
/// [`TotalOnly`]: TokenUsage::TotalOnly
/// [`Unreported`]: TokenUsage::Unreported
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenUsage {
    /// Both halves measured. Priceable.
    Split { input: u64, output: u64 },
    /// One number, which is everything the reporter had. Recorded in
    /// full; priced by nothing.
    TotalOnly { total: u64 },
    /// No count at all, stated as such: `"total_tokens": null` on the
    /// wire and a NULL column in the row. The run is recorded in full;
    /// what it spent is unknown, and unknown is not zero.
    Unreported,
}

impl TokenUsage {
    /// Build from the three wire keys, refusing every combination that
    /// is not one of the three shapes — and naming the fix, because the
    /// caller hitting this is a reporter that has to change what it
    /// sends.
    ///
    /// A supplied total beside a split is ACCEPTED only when it agrees
    /// with the halves; the split stays the definition either way. That
    /// is the validate half of §9a's "derive it or pin it", for callers
    /// that send all three.
    ///
    /// **`total` is a DOUBLE option and the two Nones are different
    /// facts.** `Some(None)` is a reporter that said in as many words
    /// that it has no count — an explicit `null` on the wire, a NULL
    /// column in the row — and records [`TokenUsage::Unreported`]. The
    /// outer `None` is a payload that never mentioned tokens, which is
    /// silence, and silence is refused here exactly as it is
    /// everywhere else: a report that forgot to say is not the same
    /// fact as one that says it does not know, and no caller should
    /// reach the honest shape by omission (backlog 65c9c05a).
    pub fn from_parts(
        input: Option<u64>,
        output: Option<u64>,
        total: Option<Option<u64>>,
    ) -> Result<Self, String> {
        // The stated "no count", ahead of the shapes that carry one.
        if let (None, None, Some(None)) = (input, output, total) {
            return Ok(TokenUsage::Unreported);
        }
        let total = total.flatten();
        match (input, output, total) {
            (Some(input), Some(output), supplied) => {
                let derived = input.saturating_add(output);
                match supplied {
                    Some(t) if t != derived => Err(format!(
                        "total_tokens is {t} but input_tokens + output_tokens is {derived} — \
                         send the two halves alone (the total is derived from them) or send \
                         total_tokens alone, but not two numbers that disagree"
                    )),
                    _ => Ok(TokenUsage::Split { input, output }),
                }
            }
            (None, None, Some(total)) => Ok(TokenUsage::TotalOnly { total }),
            (Some(_), None, _) => Err(
                "output_tokens is missing — report BOTH halves, or report total_tokens \
                 alone; deriving the other half by subtraction would invent a measurement"
                    .into(),
            ),
            (None, Some(_), _) => Err(
                "input_tokens is missing — report BOTH halves, or report total_tokens \
                 alone; deriving the other half by subtraction would invent a measurement"
                    .into(),
            ),
            (None, None, None) => Err(
                "a run reported no tokens at all — send total_tokens, or send both \
                 input_tokens and output_tokens; a run with neither is not a record of \
                 what it cost"
                    .into(),
            ),
        }
    }

    /// What the run spent, when anyone measured it. Derived for a
    /// split, so the two can never disagree; `None` for
    /// [`TokenUsage::Unreported`], which makes every caller that wants
    /// a number decide out loud what to do without one — the whole
    /// point of the variant.
    pub fn total(&self) -> Option<u64> {
        match self {
            TokenUsage::Split { input, output } => Some(input.saturating_add(*output)),
            TokenUsage::TotalOnly { total } => Some(*total),
            TokenUsage::Unreported => None,
        }
    }

    /// The input half, when it was measured.
    pub fn input(&self) -> Option<u64> {
        match self {
            TokenUsage::Split { input, .. } => Some(*input),
            TokenUsage::TotalOnly { .. } | TokenUsage::Unreported => None,
        }
    }

    /// The output half, when it was measured.
    pub fn output(&self) -> Option<u64> {
        match self {
            TokenUsage::Split { output, .. } => Some(*output),
            TokenUsage::TotalOnly { .. } | TokenUsage::Unreported => None,
        }
    }
}

/// The three token keys on the wire, defined once so the serializer and
/// the deserializer cannot disagree about their names. Flattened into
/// [`NewAgentRun`], so a report is still one flat JSON object.
#[derive(Serialize, Deserialize)]
struct TokenFields {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    /// A DOUBLE option, so the deserializer can tell an explicit
    /// `null` (`Some(None)` — no count, said out loud) from an absent
    /// key (`None` — a payload that forgot to say). `deserialize_with`
    /// runs only when the key is present, which is what makes the two
    /// readable apart; a plain `Option<u64>` collapses both to `None`
    /// (backlog 65c9c05a).
    #[serde(default, deserialize_with = "stated_total")]
    total_tokens: Option<Option<u64>>,
}

/// Called only when `total_tokens` IS present, so the wrapping `Some`
/// is what marks the value as stated — `null` included.
fn stated_total<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Option<u64>>, D::Error> {
    Ok(Some(Option::<u64>::deserialize(d)?))
}

impl Serialize for TokenUsage {
    /// Always states `total_tokens`, so no reader ever has to add the
    /// halves itself, and says an absent split with an explicit `null`
    /// rather than a missing key — "nothing measured this" is the fact,
    /// and a missing key reads as a payload that forgot to say.
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        TokenFields {
            input_tokens: self.input(),
            output_tokens: self.output(),
            // Always the key, and `null` as its value for an
            // unreported run: an absent key would read as a payload
            // that forgot to say.
            total_tokens: Some(self.total()),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for TokenUsage {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let f = TokenFields::deserialize(d)?;
        TokenUsage::from_parts(f.input_tokens, f.output_tokens, f.total_tokens)
            .map_err(serde::de::Error::custom)
    }
}

/// A finished agent run, as the caller reports it. No price: see the
/// module doc.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewAgentRun {
    /// Caller-supplied and the primary key, so a retried report
    /// records the run once (idempotence — the same contract the
    /// cadence claim rests on).
    pub run_id: String,
    /// The CPU that ran. An agent in either spelling: the registered
    /// id (`agent-claude`), which names no model and so relies on
    /// [`NewAgentRun::model`] or the agent row's default; or the legacy
    /// colon form (`claude:opus-5`), which carries its model.
    pub actor_id: ActorId,
    /// The model this run ran on, as `agent_rate_card.model` spells it.
    /// Optional on the WIRE only: a report may leave it out, and the
    /// recorder fills it — from the agent row's default when the actor
    /// is a registered agent, from the actor id when it is the colon
    /// form — or refuses. On the record it is always resolved.
    #[serde(default)]
    pub model: Option<String>,
    /// Both bound by the caller, never the database's `NOW()`: the run
    /// happened on the caller's clock, and a write-time reading would
    /// describe when the report arrived instead.
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub outcome: RunOutcome,
    #[serde(default)]
    pub error: Option<String>,
    /// What it spent. A split when the reporter measured both halves,
    /// a bare total when that is all it had — see [`TokenUsage`].
    #[serde(flatten)]
    pub tokens: TokenUsage,
    /// How many tool calls the run made — the other half of "what did
    /// this cost", and the one a token count cannot recover.
    #[serde(default)]
    pub tool_calls: u32,
    /// The packet the run was working, when there is one.
    #[serde(default)]
    pub job_id: Option<Uuid>,
    /// The car the run produced, when there is one. A branch name
    /// rather than a car id: the branch is what the builder knows at
    /// the moment it finishes, before any packet has been filed for it.
    #[serde(default)]
    pub branch: Option<String>,
    /// Anything else the reporter wants kept — worktree, host, the
    /// scope it was given. Free-form on purpose; a field that earns a
    /// column can graduate later.
    #[serde(default)]
    pub detail: serde_json::Value,
}

/// A recorded agent run: what the caller reported plus what the rate
/// card made of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRun {
    #[serde(flatten)]
    pub run: NewAgentRun,
    /// `None` means no rate-card row covered the model. NOT zero.
    #[serde(default)]
    pub usd_micros: Option<u64>,
    /// The rate-card model that priced it, so the arithmetic is
    /// checkable after the fact even if the card later changes.
    #[serde(default)]
    pub priced_by: Option<String>,
    /// The admission decision (backlog 7dd9f28c): what the actor's
    /// registry caps said when this run was measured against its spend
    /// in the hour before it started. Always `Allow` on a row — a
    /// refused run is not a row, it is an `agents.run.denied` event —
    /// and `None` only on a row recorded before budgets were consulted,
    /// which is "no decision was made", not "allowed".
    #[serde(default)]
    pub budget: Option<BudgetDecision>,
    /// The instant the record landed — the event's timestamp, so the
    /// row and the log entry share one instant.
    pub recorded_at: DateTime<Utc>,
}

/// The one window a run is admitted against: the actor's priced spend
/// in the hour before the run started. `agents.hourly_budget_usd_
/// micros` is an HOURLY cap, so this is the window it names.
pub const ADMISSION_WINDOW: Window = Window::LastHour;

/// Measure what the record holds against `run` at its admission
/// instant — the ONE definition of the load both adapters judge
/// (§9a): the Pg adapter fetches the actor's rows since the window's
/// cutoff and hands them here, the in-memory one hands its map.
///
/// The instant is the run's own `started_at`, not the report's
/// arrival: a run is admitted when it starts, and a report that
/// arrives an hour late must be judged against the hour it ran in.
/// Two measures, both over the SAME actor's other runs:
///   - spend: the priced cost (`usd_micros`, so an unpriced run adds
///     nothing — it cannot add a number it does not have) of runs that
///     FINISHED inside the window and before this run started;
///   - in flight: runs that had started and not yet finished at the
///     instant this one started.
/// A run the record does not hold yet (still running, or not reported)
/// is invisible to both, which is the honest limit of a record that is
/// written at finish.
pub fn measure_load(prior: &[AgentRun], run: &NewAgentRun) -> AgentLoad {
    let from = ADMISSION_WINDOW.cutoff(run.started_at);
    let others = prior
        .iter()
        .filter(|r| r.run.run_id != run.run_id && r.run.actor_id == run.actor_id);
    let spent_usd_micros = others
        .clone()
        .filter(|r| r.run.finished_at >= from && r.run.finished_at <= run.started_at)
        .filter_map(|r| r.usd_micros)
        .fold(0u64, u64::saturating_add);
    let in_flight = others
        .filter(|r| r.run.started_at <= run.started_at && r.run.finished_at > run.started_at)
        .count();
    AgentLoad {
        spent_usd_micros,
        in_flight: u32::try_from(in_flight).unwrap_or(u32::MAX),
    }
}

impl NewAgentRun {
    /// The model this run names — the ONE place it is derived, and the
    /// same rule the migration's backfill applied to the rows written
    /// before the column existed: the column when it is set; else the
    /// model half of a colon-form actor id; else `None`. A registered
    /// agent's id carries no model, so a run by one that has not been
    /// resolved through the registry (`port::resolve_model`) reads as
    /// naming none — which is why the recorder resolves before it
    /// prices or writes.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref().or(match &self.actor_id {
            ActorId::Agent { model, .. } => Some(model.as_str()),
            _ => None,
        })
    }
}

impl AgentRun {
    /// The model this run ran on — see [`NewAgentRun::model`].
    pub fn model(&self) -> Option<&str> {
        self.run.model()
    }

    /// Wall seconds the run took. Derived, never stored: `finished_at -
    /// started_at` is the same fact, and a stored copy could disagree
    /// with it.
    pub fn duration_secs(&self) -> i64 {
        (self.run.finished_at - self.run.started_at).num_seconds()
    }

    /// The run as a `boss_core::agent::Cost`, or `None` when the run
    /// reported only a total: `Cost` has an input field and an output
    /// field and no way to say "one number, unsplit", and putting the
    /// total in either would be a lie a later reader cannot detect.
    ///
    /// An unpriced run that DID report a split carries its price
    /// through as `None` — unpriced is not free. Until backlog
    /// c6e2341c this wrote `usd_micros: 0`, the last place in the
    /// system where a zero stood in for unknown, and only because
    /// `Cost.usd_micros` was a plain integer with no room to say
    /// otherwise; it is an `Option` now, so this method no longer has
    /// to lie and [`AgentRun::usd_micros`] is no longer the only
    /// reader that can tell.
    pub fn cost(&self) -> Option<Cost> {
        match self.run.tokens {
            TokenUsage::Split { input, output } => Some(Cost {
                input_tokens: input,
                output_tokens: output,
                usd_micros: self.usd_micros,
            }),
            TokenUsage::TotalOnly { .. } | TokenUsage::Unreported => None,
        }
    }
}

/// Price a run against the card. `None` — unpriced, never zero — in
/// three cases, and they are all the same case: nothing on the card
/// could turn these tokens into a number.
///
/// 1. The run names no model — a non-agent actor, or a registered
///    agent's run that was never resolved through the registry.
/// 2. No card row covers the model.
/// 3. **The run reported only a total, or no count at all.** The card
///    charges input and output at different rates, so there is no
///    arithmetic from one number to a price — and none whatsoever from
///    no number. Assuming a ratio would produce a figure with a
///    measurement's authority and a guess's accuracy; the roll-up
///    counts these out loud instead (`total_only_runs` and
///    `unreported_runs`).
///
/// Integer arithmetic throughout, in `u128` so a run cannot overflow
/// the multiply before the divide, rounded to the nearest micro-USD
/// (half up) and saturated into `u64`.
pub fn price_run(card: &[RateCardRow], run: &NewAgentRun) -> Option<(u64, String)> {
    let model = run.model()?;
    let TokenUsage::Split { input, output } = run.tokens else {
        return None;
    };
    let row = card.iter().find(|r| r.model == model)?;
    let micros = rounded_div(
        u128::from(input) * u128::from(row.input_usd_micros_per_mtok)
            + u128::from(output) * u128::from(row.output_usd_micros_per_mtok),
        1_000_000,
    );
    Some((u64::try_from(micros).unwrap_or(u64::MAX), row.model.clone()))
}

/// Nearest-integer division, half up. `divisor` is a non-zero constant
/// at every call site.
fn rounded_div(numerator: u128, divisor: u128) -> u128 {
    (numerator + divisor / 2) / divisor
}

/// What to select. Every field is AND-ed; `None` means "don't filter".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunFilter {
    #[serde(default)]
    pub job_id: Option<Uuid>,
    #[serde(default)]
    pub branch: Option<String>,
    /// Wire form of the actor (`claude:opus-5`).
    #[serde(default)]
    pub actor_id: Option<String>,
    /// Runs that finished at or after this instant.
    #[serde(default)]
    pub since: Option<DateTime<Utc>>,
    /// Row ceiling. A limit is not a filter — the roll-up below
    /// reports `runs` so a truncated page is visible as one.
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Spend for one model, or for one car.
///
/// The split halves go `None` the moment a total-only run joins, and
/// `total_tokens` the moment an unreported one does, for the same
/// reason `usd_micros` does — see [`RunSummary`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupSpend {
    /// The model string, or the branch name.
    pub key: String,
    pub runs: u64,
    #[serde(default)]
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    pub tool_calls: u64,
    pub wall_secs: i64,
    #[serde(default)]
    pub usd_micros: Option<u64>,
    pub unpriced_runs: u64,
    pub total_only_runs: u64,
    pub unreported_runs: u64,
}

/// The answer to "what did this cost to build".
///
/// **Four fields refuse to look more complete than the set they cover.**
/// `usd_micros` is `Some` only when EVERY run was priced; `input_tokens`
/// and `output_tokens` are `Some` only when every run reported a split;
/// `total_tokens` only when every run reported a count at all. A
/// partial figure that looked whole is the failure this avoids: one
/// missing rate-card row, one reporter with only a total, or one
/// harness that printed no usage line would otherwise understate the
/// bill while reading as an answer.
///
/// Three counters say WHY a total went missing, because the fixes
/// differ: `unpriced_runs` is how many could not be priced at all,
/// `total_only_runs` is how many of those reported no split to price,
/// and `unreported_runs` is how many reported no tokens at all
/// (backlog 65c9c05a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSummary {
    pub runs: u64,
    /// `None` once any run in the set reported no count — unknown, not
    /// zero, and a sum that quietly treated it as zero would be the
    /// defect this field exists to refuse.
    #[serde(default)]
    pub total_tokens: Option<u64>,
    /// `None` once any run in the set reported only a total.
    #[serde(default)]
    pub input_tokens: Option<u64>,
    #[serde(default)]
    pub output_tokens: Option<u64>,
    pub tool_calls: u64,
    /// Summed run durations — agent time spent, not elapsed time,
    /// since runs may overlap.
    pub wall_secs: i64,
    #[serde(default)]
    pub usd_micros: Option<u64>,
    pub unpriced_runs: u64,
    /// How many runs reported a bare total, which is why a price could
    /// not be computed for them. A subset of `unpriced_runs`.
    pub total_only_runs: u64,
    /// How many runs reported no token count at all. Also a subset of
    /// `unpriced_runs`, and disjoint from `total_only_runs`: the fixes
    /// differ, so the counters do.
    pub unreported_runs: u64,
    pub by_model: Vec<GroupSpend>,
    pub by_branch: Vec<GroupSpend>,
}

/// Roll up a set of runs. A pure function of the rows — the projection
/// discipline applied one level up: group in Rust off the one model
/// resolution rather than re-deriving it in SQL.
pub fn summarize(runs: &[AgentRun]) -> RunSummary {
    let mut by_model: BTreeMap<String, GroupSpend> = BTreeMap::new();
    let mut by_branch: BTreeMap<String, GroupSpend> = BTreeMap::new();
    let mut total = blank("");

    for run in runs {
        fold(&mut total, run);
        if let Some(model) = run.model() {
            fold(
                by_model
                    .entry(model.to_string())
                    .or_insert_with(|| blank(model)),
                run,
            );
        }
        if let Some(branch) = run.run.branch.as_deref() {
            fold(
                by_branch
                    .entry(branch.to_string())
                    .or_insert_with(|| blank(branch)),
                run,
            );
        }
    }

    RunSummary {
        runs: total.runs,
        total_tokens: total.total_tokens,
        input_tokens: total.input_tokens,
        output_tokens: total.output_tokens,
        tool_calls: total.tool_calls,
        wall_secs: total.wall_secs,
        usd_micros: total.usd_micros,
        unpriced_runs: total.unpriced_runs,
        total_only_runs: total.total_only_runs,
        unreported_runs: total.unreported_runs,
        by_model: by_model.into_values().collect(),
        by_branch: by_branch.into_values().collect(),
    }
}

fn blank(key: &str) -> GroupSpend {
    GroupSpend {
        key: key.to_string(),
        runs: 0,
        total_tokens: Some(0),
        input_tokens: Some(0),
        output_tokens: Some(0),
        tool_calls: 0,
        wall_secs: 0,
        usd_micros: Some(0),
        unpriced_runs: 0,
        total_only_runs: 0,
        unreported_runs: 0,
    }
}

/// Add one run into a bucket. `usd_micros`, `total_tokens` and the two
/// split halves go to `None` and STAY `None` the moment a run joins
/// that cannot supply them — see [`RunSummary`].
fn fold(into: &mut GroupSpend, run: &AgentRun) {
    into.runs += 1;
    // `and_then`, not `map`: a bucket that has already met an
    // unreported run stays unanswered, and a bucket meeting its first
    // one stops answering. Adding zero for it would be the projection
    // telling the same lie the column used to.
    into.total_tokens = match run.run.tokens.total() {
        Some(t) => into.total_tokens.map(|s| s.saturating_add(t)),
        None => None,
    };
    match run.run.tokens {
        TokenUsage::Split { input, output } => {
            into.input_tokens = into.input_tokens.map(|t| t.saturating_add(input));
            into.output_tokens = into.output_tokens.map(|t| t.saturating_add(output));
        }
        TokenUsage::TotalOnly { .. } => {
            into.input_tokens = None;
            into.output_tokens = None;
            into.total_only_runs += 1;
        }
        TokenUsage::Unreported => {
            into.input_tokens = None;
            into.output_tokens = None;
            into.unreported_runs += 1;
        }
    }
    into.tool_calls = into
        .tool_calls
        .saturating_add(u64::from(run.run.tool_calls));
    into.wall_secs = into.wall_secs.saturating_add(run.duration_secs());
    match run.usd_micros {
        Some(micros) => {
            into.usd_micros = into.usd_micros.map(|t| t.saturating_add(micros));
        }
        None => {
            into.usd_micros = None;
            into.unpriced_runs += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // An EXPLICIT null is the wire's "nothing measured this"; an absent
    // key is a payload that forgot to say. `total[key].is_null()` cannot
    // tell those apart (backlog 2e4c200f), and this serializer's whole
    // contract is that it writes the first and never the second.
    use boss_testing::assert_explicit_null;

    /// The four published Anthropic prices this test uses are the same
    /// ones the migration seeds; see that file for the source.
    fn card() -> Vec<RateCardRow> {
        vec![
            RateCardRow {
                model: "opus-5".into(),
                input_usd_micros_per_mtok: 5_000_000,
                output_usd_micros_per_mtok: 25_000_000,
                note: "test".into(),
            },
            RateCardRow {
                model: "haiku-4-5".into(),
                input_usd_micros_per_mtok: 1_000_000,
                output_usd_micros_per_mtok: 5_000_000,
                note: "test".into(),
            },
        ]
    }

    fn a_run(actor: &str, input: u64, output: u64) -> NewAgentRun {
        with_tokens(actor, TokenUsage::Split { input, output })
    }

    /// The same fixture for a reporter that only has a total — the
    /// shape every coding agent on this pod actually reports.
    fn a_total_run(actor: &str, total: u64) -> NewAgentRun {
        with_tokens(actor, TokenUsage::TotalOnly { total })
    }

    fn with_tokens(actor: &str, tokens: TokenUsage) -> NewAgentRun {
        NewAgentRun {
            run_id: format!("run-{actor}-{:?}", tokens.total()),
            actor_id: actor.parse().expect("ActorId::from_str is infallible"),
            started_at: "2026-09-10T01:00:00Z".parse().unwrap(),
            finished_at: "2026-09-10T01:10:00Z".parse().unwrap(),
            outcome: RunOutcome::Success,
            error: None,
            // Unset: these fixtures exercise the legacy fallback. The
            // column-first tests below set it.
            model: None,
            tokens,
            tool_calls: 7,
            job_id: None,
            branch: Some("feat/x".into()),
            detail: serde_json::json!({}),
        }
    }

    fn recorded(new: NewAgentRun, card: &[RateCardRow]) -> AgentRun {
        let priced = price_run(card, &new);
        AgentRun {
            run: new,
            usd_micros: priced.as_ref().map(|(m, _)| *m),
            priced_by: priced.map(|(_, m)| m),
            budget: None,
            recorded_at: "2026-09-10T01:10:01Z".parse().unwrap(),
        }
    }

    #[test]
    fn a_legacy_colon_form_run_reads_its_model_out_of_the_actor_id() {
        // The fallback, for rows and events written before the column.
        let run = recorded(a_run("claude:opus-5", 100, 10), &card());
        assert_eq!(run.model(), Some("opus-5"));
    }

    #[test]
    fn the_model_column_is_the_definition_when_it_is_set() {
        // A registered agent's id names no model; the column does, and
        // it is what the run is priced against.
        let mut new = a_run("agent-claude", 1_000_000, 0);
        new.model = Some("haiku-4-5".into());
        let run = recorded(new, &card());
        assert_eq!(run.model(), Some("haiku-4-5"));
        assert_eq!(run.priced_by.as_deref(), Some("haiku-4-5"));
        assert_eq!(run.usd_micros, Some(1_000_000));
    }

    #[test]
    fn the_column_wins_over_a_colon_form_actor_that_disagrees() {
        // A report that names its model outright is believed over the
        // spelling of its actor id: the column is the fact, the actor
        // id a legacy carrier of it.
        let mut new = a_run("claude:opus-5", 1_000_000, 0);
        new.model = Some("haiku-4-5".into());
        let run = recorded(new, &card());
        assert_eq!(run.model(), Some("haiku-4-5"));
        assert_eq!(run.usd_micros, Some(1_000_000));
    }

    #[test]
    fn an_unresolved_registered_agent_run_names_no_model_and_prices_to_nothing() {
        // `agent-claude` alone says nothing about the model; the
        // recorder resolves it through the registry before pricing, and
        // a run that skipped that step reads as unpriced, never as $0.
        let run = recorded(a_run("agent-claude", 100, 10), &card());
        assert_eq!(run.model(), None);
        assert_eq!(run.usd_micros, None);
    }

    #[test]
    fn a_non_agent_actor_names_no_model_and_prices_to_nothing() {
        let run = recorded(a_run("automation:train-conductor", 100, 10), &card());
        assert_eq!(run.model(), None);
        assert_eq!(run.usd_micros, None);
    }

    #[test]
    fn price_is_tokens_times_the_card_row() {
        // 1M input at $5 + 1M output at $25 = $30.00 = 30_000_000 micros.
        let (micros, by) = price_run(&card(), &a_run("claude:opus-5", 1_000_000, 1_000_000))
            .expect("opus-5 is on the card");
        assert_eq!(micros, 30_000_000);
        assert_eq!(by, "opus-5");
    }

    #[test]
    fn price_matches_the_evidence_this_packet_was_filed_on() {
        // The measured run on 2026-09-10: 142,982 tokens on opus-5.
        // Split 9:1 input:output, priced at $5/$25 per MTok:
        //   128,684 * 5 + 14,298 * 25 = 643,420 + 357,450 = 1,000,870
        let (micros, _) =
            price_run(&card(), &a_run("claude:opus-5", 128_684, 14_298)).expect("priced");
        assert_eq!(micros, 1_000_870);
    }

    #[test]
    fn price_rounds_to_the_nearest_micro_usd() {
        // 1 input token at $1/MTok = 1 micro-USD, which rounds up from
        // 0.5 rather than truncating to 0.
        let (micros, _) = price_run(&card(), &a_run("claude:haiku-4-5", 1, 0)).expect("priced");
        assert_eq!(micros, 1);
    }

    #[test]
    fn an_unknown_model_is_unpriced_not_free() {
        let run = recorded(
            a_run("claude:some-model-we-never-seeded", 999, 999),
            &card(),
        );
        assert_eq!(run.usd_micros, None, "unknown model must not read as $0");
        assert_eq!(run.priced_by, None);
        // The tokens are still recorded — the run is a fact either way.
        assert_eq!(run.run.tokens.input(), Some(999));
    }

    #[test]
    fn an_unpriced_split_run_reports_no_cost_rather_than_zero() {
        // 65c9c05a closed this defect one layer down and named where it
        // still lived: `cost()` wrote `usd_micros: 0` for a run nothing
        // on the card could price, because `Cost.usd_micros` was a plain
        // integer. It is an Option now, so unpriced is not free here
        // either (backlog c6e2341c).
        let run = recorded(
            a_run("claude:some-model-we-never-seeded", 999, 999),
            &card(),
        );
        let cost = run.cost().expect("a split run still maps onto Cost");
        assert_eq!(cost.input_tokens, 999, "the tokens are a fact either way");
        assert_eq!(cost.output_tokens, 999);
        assert_eq!(cost.usd_micros, None, "unpriced is not free");
    }

    #[test]
    fn a_huge_run_saturates_rather_than_overflowing() {
        let (micros, _) =
            price_run(&card(), &a_run("claude:opus-5", u64::MAX, u64::MAX)).expect("priced");
        assert_eq!(micros, u64::MAX);
    }

    #[test]
    fn duration_is_derived_from_the_two_instants() {
        let run = recorded(a_run("claude:opus-5", 1, 1), &card());
        assert_eq!(run.duration_secs(), 600);
    }

    #[test]
    fn summary_groups_by_model_and_by_branch() {
        let card = card();
        let mut cheap = a_run("claude:haiku-4-5", 1_000_000, 0);
        cheap.run_id = "run-2".into();
        cheap.branch = Some("feat/y".into());
        let runs = vec![
            recorded(a_run("claude:opus-5", 1_000_000, 0), &card),
            recorded(cheap, &card),
        ];
        let s = summarize(&runs);
        assert_eq!(s.runs, 2);
        assert_eq!(s.input_tokens, Some(2_000_000));
        assert_eq!(s.total_tokens, Some(2_000_000));
        assert_eq!(s.tool_calls, 14);
        assert_eq!(s.wall_secs, 1200);
        assert_eq!(s.usd_micros, Some(6_000_000));
        assert_eq!(s.unpriced_runs, 0);
        assert_eq!(
            s.by_model
                .iter()
                .map(|g| (g.key.as_str(), g.usd_micros))
                .collect::<Vec<_>>(),
            vec![("haiku-4-5", Some(1_000_000)), ("opus-5", Some(5_000_000))]
        );
        assert_eq!(
            s.by_branch
                .iter()
                .map(|g| g.key.as_str())
                .collect::<Vec<_>>(),
            vec!["feat/x", "feat/y"]
        );
    }

    #[test]
    fn one_unpriced_run_makes_the_total_unknown_rather_than_understated() {
        let card = card();
        let runs = vec![
            recorded(a_run("claude:opus-5", 1_000_000, 0), &card),
            recorded(a_run("claude:not-on-the-card", 1_000_000, 0), &card),
        ];
        let s = summarize(&runs);
        assert_eq!(s.runs, 2);
        assert_eq!(s.unpriced_runs, 1);
        assert_eq!(
            s.usd_micros, None,
            "a total that omitted an unpriced run would understate the bill while looking complete"
        );
        // The priced half is still readable per model: both models get a
        // bucket, and only the unpriced one says it cannot be totalled.
        assert_eq!(
            s.by_model
                .iter()
                .map(|g| (g.key.as_str(), g.usd_micros, g.unpriced_runs))
                .collect::<Vec<_>>(),
            vec![("not-on-the-card", None, 1), ("opus-5", Some(5_000_000), 0)]
        );
    }

    #[test]
    fn summary_of_nothing_is_zero_and_priced() {
        let s = summarize(&[]);
        assert_eq!(s.runs, 0);
        assert_eq!(s.usd_micros, Some(0));
        assert!(s.by_model.is_empty());
    }

    #[test]
    fn cost_reports_the_core_type() {
        let run = recorded(a_run("claude:opus-5", 1_000_000, 1_000_000), &card());
        let cost: Cost = run.cost().expect("a split run maps onto Cost");
        assert_eq!(cost.input_tokens, 1_000_000);
        assert_eq!(cost.usd_micros, Some(30_000_000));
    }

    #[test]
    fn outcome_round_trips_its_wire_form() {
        for o in [
            RunOutcome::Success,
            RunOutcome::Failed,
            RunOutcome::Cancelled,
        ] {
            assert_eq!(RunOutcome::parse(o.as_str()), Some(o));
        }
        assert_eq!(RunOutcome::parse("exploded"), None);
    }

    #[test]
    fn a_recorded_run_round_trips_serde() {
        let run = recorded(a_run("claude:opus-5", 5, 6), &card());
        let json = serde_json::to_string(&run).unwrap();
        let back: AgentRun = serde_json::from_str(&json).unwrap();
        assert_eq!(back, run);
        // The flattened wire form carries the actor in its bare form.
        assert!(json.contains("\"actor_id\":\"claude:opus-5\""));
        // And the model as its own key — an explicit null here, because
        // this fixture is the unresolved legacy shape; a recorded run
        // always carries it resolved (see the port tests).
        assert!(json.contains("\"model\":null"), "{json}");
    }

    #[test]
    fn a_payload_without_the_model_key_still_deserializes() {
        // Every `agents.run.recorded` event written before the column
        // existed has no `model` key; the rebuild reads them.
        let json = r#"{
            "run_id": "run-old",
            "actor_id": "claude:opus-5[1m]",
            "started_at": "2026-09-10T01:00:00Z",
            "finished_at": "2026-09-10T01:10:00Z",
            "outcome": "success",
            "total_tokens": 142982,
            "recorded_at": "2026-09-10T01:10:01Z"
        }"#;
        let run: AgentRun = serde_json::from_str(json).expect("an old payload parses");
        assert_eq!(run.run.model, None, "the key was absent");
        assert_eq!(
            run.model(),
            Some("opus-5[1m]"),
            "the fallback still answers"
        );
    }

    // ----------------------------------------------------------------
    // A run that only knows its total. The first real caller of this
    // surface could not fill the old shape at all: the coding agents
    // that build cars here report `subagent_tokens`, ONE number, with
    // no split available anywhere. See the migration header.
    // ----------------------------------------------------------------

    #[test]
    fn a_split_reports_a_derived_total_rather_than_a_stored_one() {
        let tokens = TokenUsage::Split {
            input: 128_684,
            output: 14_298,
        };
        assert_eq!(tokens.total(), Some(142_982));
        assert_eq!(tokens.input(), Some(128_684));
        assert_eq!(tokens.output(), Some(14_298));
    }

    #[test]
    fn a_total_only_run_reports_its_total_and_no_split() {
        let tokens = TokenUsage::TotalOnly { total: 142_982 };
        assert_eq!(tokens.total(), Some(142_982));
        assert_eq!(tokens.input(), None);
        assert_eq!(tokens.output(), None);
    }

    #[test]
    fn a_total_only_run_is_unpriced_even_on_a_model_the_card_names() {
        // The card prices input and output at DIFFERENT rates, so a
        // total cannot be priced — not even approximately, and a blend
        // would be a fabricated number wearing a measurement's clothes.
        let run = recorded(a_total_run("claude:opus-5", 142_982), &card());
        assert_eq!(
            run.usd_micros, None,
            "a total cannot be priced against per-half rates"
        );
        assert_eq!(run.priced_by, None, "nothing priced it");
        // The run is still a RECORD: tokens, tool calls and duration.
        assert_eq!(run.run.tokens.total(), Some(142_982));
        assert_eq!(run.duration_secs(), 600);
    }

    #[test]
    fn a_total_only_run_has_no_core_cost_because_cost_has_no_room_to_say_so() {
        let run = recorded(a_total_run("claude:opus-5", 142_982), &card());
        assert_eq!(
            run.cost(),
            None,
            "Cost carries an input/output split; reporting a total as input would be a lie"
        );
    }

    #[test]
    fn a_report_that_says_it_has_no_count_records_one_rather_than_a_zero() {
        // Backlog 65c9c05a: 13 of 42 live rows carried `total_tokens`
        // 0 for "the harness printed no usage line", distinguishable
        // from a genuine zero only by a companion `detail` flag. An
        // average over the column silently included them.
        let json = r#"{
            "run_id": "run-silent",
            "actor_id": "claude:opus-5",
            "started_at": "2026-09-19T01:00:00Z",
            "finished_at": "2026-09-19T01:10:00Z",
            "outcome": "success",
            "total_tokens": null
        }"#;
        let run: NewAgentRun = serde_json::from_str(json).expect("an explicit null is a shape");
        assert_eq!(run.tokens, TokenUsage::Unreported);
        assert_eq!(
            run.tokens.total(),
            None,
            "unknown, not zero — the same distinction usd_micros draws"
        );
        let back = serde_json::to_value(&run).expect("serializes");
        assert_explicit_null!(
            back,
            "total_tokens",
            "an absent key reads as a payload that forgot to say"
        );
    }

    #[test]
    fn an_unreported_run_is_unpriced_and_the_roll_up_reports_no_total_at_all() {
        let card = card();
        let mut silent = a_total_run("claude:opus-5", 0);
        silent.run_id = "run-silent".into();
        silent.tokens = TokenUsage::Unreported;
        let runs = vec![
            recorded(a_run("claude:opus-5", 9, 1), &card),
            recorded(silent, &card),
        ];
        let s = summarize(&runs);
        assert_eq!(
            s.total_tokens, None,
            "a sum over a set holding an unmeasured run understates it while looking whole"
        );
        assert_eq!(s.unreported_runs, 1);
        assert_eq!(s.unpriced_runs, 1, "no tokens is no price");
        assert_eq!(
            s.total_only_runs, 0,
            "a bare total and no count at all are different facts with different fixes"
        );
        assert_eq!(runs[1].usd_micros, None);
    }

    #[test]
    fn a_run_that_never_mentions_tokens_is_refused_where_a_stated_null_is_not() {
        // The two Nones differ: an absent key is silence and is
        // refused; `Some(None)` is a reporter saying it has no count
        // and is the honest record (backlog 65c9c05a).
        let err = TokenUsage::from_parts(None, None, None).expect_err("silence is not a run");
        assert!(err.contains("total_tokens"), "{err}");
        assert!(err.contains("input_tokens"), "{err}");
        assert_eq!(
            TokenUsage::from_parts(None, None, Some(None)),
            Ok(TokenUsage::Unreported),
            "said out loud, it is a shape"
        );
    }

    #[test]
    fn a_payload_that_never_mentions_tokens_is_refused_on_the_wire_too() {
        // The wire half of the rule above: the double option is only
        // worth having if serde can still tell the two apart after
        // flattening, and nothing else proves that.
        let json = r#"{
            "run_id": "run-quiet",
            "actor_id": "claude:opus-5",
            "started_at": "2026-09-19T01:00:00Z",
            "finished_at": "2026-09-19T01:10:00Z",
            "outcome": "success"
        }"#;
        let err = serde_json::from_str::<NewAgentRun>(json)
            .expect_err("a payload that says nothing about tokens is not a report");
        assert!(err.to_string().contains("total_tokens"), "{err}");
    }

    #[test]
    fn half_a_split_is_refused_rather_than_completed_by_subtraction() {
        let err = TokenUsage::from_parts(Some(10), None, Some(Some(12)))
            .expect_err("a half split must not be completed from the total");
        assert!(err.contains("output_tokens"), "{err}");
        let err = TokenUsage::from_parts(None, Some(2), None).expect_err("half a split");
        assert!(err.contains("input_tokens"), "{err}");
    }

    #[test]
    fn a_supplied_total_that_disagrees_with_the_split_is_refused_naming_both() {
        let err = TokenUsage::from_parts(Some(10), Some(2), Some(Some(11)))
            .expect_err("11 is not 10 + 2, and guessing which is right is not available");
        assert!(err.contains("11"), "{err}");
        assert!(err.contains("12"), "{err}");
    }

    #[test]
    fn a_supplied_total_that_agrees_is_accepted_and_the_split_remains_the_definition() {
        let tokens =
            TokenUsage::from_parts(Some(10), Some(2), Some(Some(12))).expect("12 is 10 + 2");
        assert_eq!(
            tokens,
            TokenUsage::Split {
                input: 10,
                output: 2
            }
        );
    }

    #[test]
    fn the_wire_form_accepts_a_bare_total() {
        // Exactly what tonight's reporter has to send.
        let json = r#"{
            "run_id": "run-1",
            "actor_id": "claude:opus-5",
            "started_at": "2026-09-10T01:00:00Z",
            "finished_at": "2026-09-10T01:10:00Z",
            "outcome": "success",
            "total_tokens": 142982,
            "tool_calls": 52
        }"#;
        let run: NewAgentRun = serde_json::from_str(json).expect("a total-only report parses");
        assert_eq!(run.tokens, TokenUsage::TotalOnly { total: 142_982 });
    }

    #[test]
    fn the_wire_form_still_accepts_the_split_it_always_did() {
        let json = r#"{
            "run_id": "run-1",
            "actor_id": "claude:opus-5",
            "started_at": "2026-09-10T01:00:00Z",
            "finished_at": "2026-09-10T01:10:00Z",
            "outcome": "success",
            "input_tokens": 10,
            "output_tokens": 2
        }"#;
        let run: NewAgentRun = serde_json::from_str(json).expect("a split report parses");
        assert_eq!(
            run.tokens,
            TokenUsage::Split {
                input: 10,
                output: 2
            }
        );
    }

    #[test]
    fn the_wire_refusal_names_the_two_ways_to_report_tokens() {
        let json = r#"{
            "run_id": "run-1",
            "actor_id": "claude:opus-5",
            "started_at": "2026-09-10T01:00:00Z",
            "finished_at": "2026-09-10T01:10:00Z",
            "outcome": "success"
        }"#;
        let err = serde_json::from_str::<NewAgentRun>(json)
            .expect_err("a report with no tokens at all is not a record");
        let msg = err.to_string();
        assert!(msg.contains("total_tokens"), "{msg}");
        assert!(msg.contains("input_tokens"), "{msg}");
    }

    #[test]
    fn a_serialized_run_always_states_its_total_and_says_null_for_an_absent_split() {
        let split = serde_json::to_value(a_run("claude:opus-5", 10, 2)).unwrap();
        assert_eq!(split["input_tokens"], 10);
        assert_eq!(split["output_tokens"], 2);
        assert_eq!(
            split["total_tokens"], 12,
            "a reader must never have to add the halves itself"
        );

        let total = serde_json::to_value(a_total_run("claude:opus-5", 142_982)).unwrap();
        assert_eq!(total["total_tokens"], 142_982);
        // `total["input_tokens"].is_null()` cannot state this: `Index` on
        // an object answers `Null` for a key that was never serialized,
        // so it is true for the explicit null AND for the missing key the
        // prose forbids — the two things this line exists to separate.
        assert_explicit_null!(total, "input_tokens", "an absent split is an EXPLICIT null");
        assert_explicit_null!(
            total,
            "output_tokens",
            "an absent split is an EXPLICIT null"
        );
    }

    #[test]
    fn a_total_only_run_round_trips_serde() {
        let run = recorded(a_total_run("claude:opus-5", 142_982), &card());
        let json = serde_json::to_string(&run).unwrap();
        let back: AgentRun = serde_json::from_str(&json).unwrap();
        assert_eq!(back, run);
    }

    #[test]
    fn a_set_of_total_only_runs_reports_no_usd_and_says_how_many() {
        // Tonight's real shape: every run reports one number.
        let card = card();
        let mut second = a_total_run("claude:opus-5", 218_218);
        second.run_id = "run-2".into();
        second.branch = Some("feat/y".into());
        let runs = vec![
            recorded(a_total_run("claude:opus-5", 142_982), &card),
            recorded(second, &card),
        ];
        let s = summarize(&runs);
        assert_eq!(s.runs, 2);
        assert_eq!(s.total_tokens, Some(361_200), "the totals still add up");
        assert_eq!(
            s.usd_micros, None,
            "a set of total-only runs must not report a confident cost"
        );
        assert_eq!(s.unpriced_runs, 2);
        assert_eq!(
            s.total_only_runs, 2,
            "the reader needs to know WHY it is unpriced: no split was reported"
        );
        assert_eq!(
            (s.input_tokens, s.output_tokens),
            (None, None),
            "summing a split nobody reported would understate it while looking complete"
        );
    }

    #[test]
    fn a_mixed_set_keeps_the_total_and_drops_the_split() {
        let card = card();
        let mut only_total = a_total_run("claude:opus-5", 100);
        only_total.run_id = "run-total".into();
        let runs = vec![
            recorded(a_run("claude:opus-5", 9, 1), &card),
            recorded(only_total, &card),
        ];
        let s = summarize(&runs);
        assert_eq!(s.total_tokens, Some(110));
        assert_eq!(s.input_tokens, None);
        assert_eq!(s.output_tokens, None);
        assert_eq!(s.usd_micros, None);
        assert_eq!(s.unpriced_runs, 1);
        assert_eq!(s.total_only_runs, 1);
        // The priced run is still priced on its own.
        assert_eq!(runs[0].usd_micros, Some(70));
    }

    #[test]
    fn an_unpriced_run_that_did_report_a_split_is_not_counted_as_total_only() {
        // The two reasons a run is unpriced are different facts: no card
        // row for the model, versus no split to price. A reader that
        // cannot tell them apart cannot tell which fix applies.
        let s = summarize(&[recorded(a_run("claude:not-on-the-card", 9, 1), &card())]);
        assert_eq!(s.unpriced_runs, 1);
        assert_eq!(s.total_only_runs, 0);
        assert_eq!(
            s.input_tokens,
            Some(9),
            "the split was reported, so it sums"
        );
        assert_eq!(s.total_tokens, Some(10));
    }
}
