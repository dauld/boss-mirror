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
//!    when it is measured, the total when it is all there is.
//!
//! 5. **A TOTAL IS PRICED AT A DECLARED BLEND, AND SAYS SO.** Rule 4
//!    used to end by refusing to price a total at all: the card charges
//!    the halves at different rates, so any blend rests on an assumed
//!    ratio, and an assumed ratio produces a figure with a
//!    measurement's authority and a guess's accuracy. That refusal cost
//!    more than it saved — 98% of the runs on the record carried no
//!    cost at all on 2026-09-20 (83 of 85), so both budget desks
//!    enforced against a fiftieth of the spend. Design 91a9bfe7,
//!    decided by David the same day, keeps the objection and removes
//!    the gap: the ratio is DECLARED, per model, in the registry
//!    ([`RateCardRow::blended_input_share_ppm`]) where a tenant can
//!    read and change it — never hardcoded, because an assumption
//!    nobody can see is the part that was wrong — and the record says
//!    which basis produced the figure ([`PricingBasis`]), carried into
//!    every roll-up so a bucket holding one blended run reports itself
//!    as blended. A measured split always wins over the blend, and a
//!    model whose row declares no ratio still prices a total at
//!    nothing: undeclared is unpriced, not assumed.
//!
//! 6. **A ROW THAT CANNOT BE CORRECTED IS MARKED, NEVER BACKFILLED.**
//!    This log is insert-once, so the rows written before the run
//!    recorded its effort and its real terminal cannot be repaired by
//!    re-reporting them. Reconstructing them from the run packets would
//!    hand a guess a measurement's authority; [`EffortEra`] leaves the
//!    record exactly as it is and derives which era a run belongs to,
//!    and the roll-up counts the excluded eras out loud beside the
//!    unpriced ones (backlog fd5ce137).

use std::collections::BTreeMap;

use boss_core::actor::ActorId;
use boss_core::agent::{AgentLoad, BudgetDecision, Cost, Window};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// How a run ended: the terminal state alone, with the tokens reported
/// beside it rather than inside it — a response body and a `Cost` here
/// would be the caller asserting what the record measures.
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
    /// The share of a bare TOTAL this model assumes was input, in parts
    /// per million: `Some(875_000)` is "87.5% input, 12.5% output".
    /// It is what makes a total priceable at all (see rule 5), and it
    /// is an ASSUMPTION, which is why it is a registry column and not a
    /// constant in this file — a tenant whose agents run a different
    /// shape of work can read it and change it.
    ///
    /// A RATIO rather than a blended rate: the two rates above are the
    /// prices, so a stored blend would be the same fact a third time
    /// and would go stale the day a price moves (CLAUDE.md §9a).
    /// [`RateCardRow::blended_usd_micros_per_mtok`] derives it on every
    /// read instead.
    ///
    /// `None` is the honest default and the seeded state of every row
    /// but one: no ratio declared, so a total-only run on that model
    /// stays unpriced rather than being blended against a number nobody
    /// measured.
    #[serde(default)]
    pub blended_input_share_ppm: Option<u64>,
    /// What a million prompt tokens READ FROM THE CACHE cost, and what
    /// a million WRITTEN TO IT cost (backlog e6b2066f) — the two rates
    /// a [`TokenUsage::Metered`] run needs beyond the two above. Prices
    /// as data, like the others, so a changed multiplier is a
    /// migration and never a constant here. `None` on a row that
    /// declares none, which leaves a metered run on that model
    /// unpriced: pricing three of four counts would read as the whole
    /// bill.
    #[serde(default)]
    pub cache_read_usd_micros_per_mtok: Option<u64>,
    #[serde(default)]
    pub cache_write_usd_micros_per_mtok: Option<u64>,
}

/// Parts per million, the unit [`RateCardRow::blended_input_share_ppm`]
/// is expressed in. Integer, for the same reason every price here is.
const PPM: u128 = 1_000_000;

impl RateCardRow {
    /// What one million tokens cost under this row's declared
    /// assumption: its two rates weighted by the ratio. `None` when the
    /// row declares no ratio — there is no blend to state.
    ///
    /// This is the number a surface shows beside the word *blended*,
    /// and [`price_run`] multiplies a total by this same value, so what
    /// a reader can check by hand is what the record charged.
    pub fn blended_usd_micros_per_mtok(&self) -> Option<u64> {
        let share = u128::from(self.blended_input_share_ppm?).min(PPM);
        let weighted = share * u128::from(self.input_usd_micros_per_mtok)
            + (PPM - share) * u128::from(self.output_usd_micros_per_mtok);
        Some(u64::try_from(rounded_div(weighted, PPM)).unwrap_or(u64::MAX))
    }
}

/// How a recorded price was arrived at — the condition David attached
/// to blended pricing (design 91a9bfe7, resolution *the basis*), and
/// not an optional one: "a blended number later read as measured is the
/// zero-means-unknown defect wearing better clothes". Three cars on
/// 2026-09-20 removed that defect from `total_tokens`, `Cost.usd_micros`
/// and the unpriced roll-up; a figure that cannot say what it rests on
/// would put it straight back.
///
/// **DERIVED, never stored** — from the token shape the run reported
/// and the price it carries ([`pricing_basis`]). A column would be the
/// same fact twice (§9a) and could drift from the arithmetic it
/// describes; deriving it also means the 83 runs already on the record
/// need no backfill, which is the design's third resolution (*existing
/// rows*): an unpriced run has no basis, because it has no figure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingBasis {
    /// Priced from a MEASURED input/output split at the card's two
    /// rates. The strong form, and the one a split always wins into.
    Split,
    /// Priced from a bare total at the model's DECLARED blended rate.
    /// Honest about being an estimate: the tokens are measured, the
    /// division between them is an assumption read out of the registry.
    Blended,
    /// Priced from the run's four MEASURED counts — uncached input,
    /// cache writes, cache reads, output — each at its own rate
    /// (backlog e6b2066f). The strongest basis: a `Split` carries no
    /// cache counts, and cache reads were a median 96.8% of what a
    /// dispatched run processed.
    Metered,
}

impl PricingBasis {
    pub fn as_str(&self) -> &'static str {
        match self {
            PricingBasis::Split => "split",
            PricingBasis::Blended => "blended",
            PricingBasis::Metered => "metered",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "split" => Some(PricingBasis::Split),
            "blended" => Some(PricingBasis::Blended),
            "metered" => Some(PricingBasis::Metered),
            _ => None,
        }
    }

    /// How measured a basis is: metered over split over blended.
    fn rank(self) -> u8 {
        match self {
            PricingBasis::Blended => 0,
            PricingBasis::Split => 1,
            PricingBasis::Metered => 2,
        }
    }

    /// A bucket is only as measured as its least-measured run: one
    /// blended figure in a sum makes the sum blended, one split among
    /// metered runs makes it split. The rule the roll-up folds with,
    /// written once here rather than at each call.
    pub fn least_measured(self, other: PricingBasis) -> PricingBasis {
        if other.rank() < self.rank() {
            other
        } else {
            self
        }
    }
}

/// What basis a price rests on, derived from the two facts that decide
/// it: the shape the reporter measured, and whether anything priced it.
///
/// `None` means there is no figure to describe — an unpriced run has no
/// basis, and saying `Split` for it would describe a number that does
/// not exist. This is the ONE definition; [`AgentRun::pricing_basis`]
/// and the roll-up both call it.
pub fn pricing_basis(tokens: TokenUsage, usd_micros: Option<u64>) -> Option<PricingBasis> {
    usd_micros?;
    match tokens {
        TokenUsage::Split { .. } => Some(PricingBasis::Split),
        TokenUsage::Metered { .. } => Some(PricingBasis::Metered),
        TokenUsage::TotalOnly { .. } => Some(PricingBasis::Blended),
        // Unreachable by construction — `price_run` prices no count at
        // all at nothing — and `None` rather than a panic if it ever
        // is: a library that cannot say gives no answer, not a wrong one.
        TokenUsage::Unreported => None,
    }
}

/// Which effort era a run belongs to — the answer to backlog fd5ce137,
/// and a DERIVED one.
///
/// `agent_runs` is insert-once (`ON CONFLICT (run_id) DO NOTHING`, the
/// same guard the replay and the rebuilder pass through), so a run
/// already on the record cannot be corrected by re-reporting it. The
/// packet offered two ways out — backfill the era from the run packets,
/// or mark it — and the choice made here is MARK, for the reason
/// CLAUDE.md gives: a reconstructed figure carries a measurement's
/// authority and a guess's accuracy, which is the defect the original
/// one was. Nothing is written to the 28 rows; they mark themselves,
/// and this is the one place that reads the mark.
///
/// Two boundaries, not one, because the era failed in two different
/// ways (measured on the live table, 2026-09-22, 142 rows):
///   - Before the effort was RECORDED at all — 28 rows, every one of
///     them declaring no effort, $33.88 of the $102.71 the record
///     holds, including a single row reading $23.00 against roughly
///     $1.50 of real spend. Those rows need no boundary instant: the
///     absent key is the evidence.
///   - Between that and the effort being APPLIED — 5 rows labelled
///     `high` that ran at the session default. This is the worse half,
///     because it LOOKS like data, and the row holds nothing that
///     distinguishes it. Only [`EFFORT_APPLIED_FROM_SHA`] does, which
///     is why the boundary is written down here rather than remembered
///     — a caveat in someone's head is exactly the mostly-sure failure
///     an effort-vs-reliability series exists to avoid.
///
/// Only [`EffortEra::Applied`] runs belong in a comparison of effort
/// against reliability or cost. The roll-up counts the other two out
/// loud ([`RunSummary::effort_unrecorded_runs`],
/// [`RunSummary::effort_unapplied_runs`]) for the same reason it counts
/// unpriced runs: a reader who can see the excluded set cannot silently
/// average over it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffortEra {
    /// The run declares no effort — it was recorded before anything
    /// wrote one down.
    Unrecorded,
    /// The run declares an effort that was never applied to the CPU
    /// that ran: a label, not a setting.
    Unapplied,
    /// The declared effort selected the agent definition that ran. The
    /// only era a series may read.
    Applied,
}

impl EffortEra {
    pub fn as_str(&self) -> &'static str {
        match self {
            EffortEra::Unrecorded => "unrecorded",
            EffortEra::Unapplied => "unapplied",
            EffortEra::Applied => "applied",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "unrecorded" => Some(EffortEra::Unrecorded),
            "unapplied" => Some(EffortEra::Unapplied),
            "applied" => Some(EffortEra::Applied),
            _ => None,
        }
    }
}

/// The car that made a run RECORD the effort it was dispatched at
/// (backlog 8f1de7bf), as a merge sha on `main`. Corroborating, not
/// load-bearing: the rows before it declare no effort and so identify
/// themselves. Measured 2026-09-22, the last effortless row was
/// recorded at 2026-09-19T16:30:24Z and this landed at 16:40:59Z.
pub const EFFORT_RECORDED_FROM_SHA: &str = "28937f2d5a557967903b263e5d53b38054dc2b34";

/// The car that made a run's declared effort SELECT the agent
/// definition it runs as (backlog e720dd00), as a merge sha on `main`.
/// This boundary IS load-bearing — see [`EffortEra`].
pub const EFFORT_APPLIED_FROM_SHA: &str = "d0a307d8f9b8193c2b08f5619828df0dd8fb4915";

/// That sha's commit time, as epoch seconds — `2026-09-19T17:30:03Z`,
/// pinned by a test that spells the instant back out. Epoch rather than
/// an RFC3339 string so no parse can fail in library code, and a const
/// rather than a lookup because the boundary is a historical fact that
/// cannot move.
pub const EFFORT_APPLIED_FROM_EPOCH_SECS: i64 = 1_789_839_003;

/// [`EFFORT_APPLIED_FROM_EPOCH_SECS`] as an instant. `UNIX_EPOCH` is
/// unreachable — the constant is in range — and is the no-panic answer
/// rather than an `expect` in library code.
pub fn effort_applied_from() -> DateTime<Utc> {
    DateTime::from_timestamp(EFFORT_APPLIED_FROM_EPOCH_SECS, 0).unwrap_or(DateTime::UNIX_EPOCH)
}

/// The key a dispatched run's effort rides under in `detail`, written
/// by `boss dispatch`'s `run_record`. One spelling, here, because the
/// writer and this reader must agree (CLAUDE.md §9a).
pub const EFFORT_KEY: &str = "effort";

/// [`TokenUsage`] is `boss_core`'s: the shape a reporter measured is
/// what the port value [`Cost`] carries too, and one definition
/// cannot drift from itself (CLAUDE.md §9a, backlog e059a754). It
/// lived here until that packet, which needed `Cost` to be able to
/// say "one number, unsplit".
pub use boss_core::agent::TokenUsage;

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

    /// What this run's price rests on — `Split` when both halves were
    /// measured, `Blended` when a bare total was priced at the model's
    /// declared ratio, `None` when there is no price to describe.
    /// Derived from the record, so a rebuild replays it exactly.
    pub fn pricing_basis(&self) -> Option<PricingBasis> {
        pricing_basis(self.run.tokens, self.usd_micros)
    }

    /// The effort this run was dispatched at, as its reporter declared
    /// it — `detail.effort`, the one key `run_record` writes it under.
    /// `None` for a run recorded before anything wrote one, and for an
    /// explicit null, which says the same thing.
    pub fn declared_effort(&self) -> Option<&str> {
        self.run
            .detail
            .get(EFFORT_KEY)
            .and_then(serde_json::Value::as_str)
    }

    /// Which effort era this run belongs to — see [`EffortEra`]. Read
    /// off the row's own evidence first (a run that declared nothing
    /// has no label that could have been applied) and only then off the
    /// boundary instant.
    pub fn effort_era(&self) -> EffortEra {
        match self.declared_effort() {
            None => EffortEra::Unrecorded,
            Some(_) if self.recorded_at < effort_applied_from() => EffortEra::Unapplied,
            Some(_) => EffortEra::Applied,
        }
    }

    /// The run as a `boss_core::agent::Cost` — every run, in whatever
    /// shape its reporter was in.
    ///
    /// It returned `None` for a total-only run until backlog e059a754,
    /// because `Cost` had an input field and an output field and no
    /// way to say "one number, unsplit"; a run priced through its
    /// model's declared blend therefore read as UNPRICED to anyone
    /// coming through this method while the budget desks, which read
    /// `usd_micros` directly, saw the spend. Two readers of one
    /// record, disagreeing. `Cost.tokens` is the shape now, so there
    /// is nothing left for this method to refuse to answer.
    ///
    /// An unpriced run carries its price through as `None` — unpriced
    /// is not free. Until backlog c6e2341c this wrote `usd_micros: 0`,
    /// the last place in the system where a zero stood in for unknown,
    /// and only because `Cost.usd_micros` was a plain integer with no
    /// room to say otherwise.
    pub fn cost(&self) -> Cost {
        Cost {
            tokens: self.run.tokens,
            usd_micros: self.usd_micros,
        }
    }
}

/// ONE run as the jobs API serves it: the row, plus the basis its
/// figure rests on, derived on the way out (backlog 93fdb119).
///
/// The roll-ups ([`RunSummary`], [`GroupSpend`]) carried
/// `pricing_basis` from the day the blend landed (design 91a9bfe7) and
/// a single run's JSON carried none, so a per-row surface — a run list,
/// `boss dispatch --report` reading its POST's answer — could tell a
/// blended figure from a measured one only by re-deriving the rule
/// client-side: a second copy of a server judgement, free to drift.
/// The basis stays DERIVED, never stored, for the reason
/// [`PricingBasis`] gives; this type is where the derivation meets the
/// wire, through [`AgentRun::pricing_basis`], the one rule.
///
/// The key is always present, `null` included: `null` means no figure
/// to describe, which a reader must be able to tell from a server too
/// old to say. The flattened row keeps every existing key where it
/// was, and [`AgentRun`]'s own `Deserialize` ignores the extra one, so
/// a reader that parses rows back into `AgentRun` is unaffected.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentRunView {
    #[serde(flatten)]
    pub run: AgentRun,
    pub pricing_basis: Option<PricingBasis>,
}

impl From<AgentRun> for AgentRunView {
    fn from(run: AgentRun) -> Self {
        let pricing_basis = run.pricing_basis();
        AgentRunView { run, pricing_basis }
    }
}

/// Price a run against the card. `None` — unpriced, never zero — in
/// four cases, and they are all the same case: nothing on the card
/// could turn these tokens into a number.
///
/// 1. The run names no model — a non-agent actor, or a registered
///    agent's run that was never resolved through the registry.
/// 2. No card row covers the model.
/// 3. **The run reported only a total and the model's row declares no
///    blend.** The card charges input and output at different rates, so
///    a total is priceable only under an assumed ratio; the row either
///    declares one or it does not, and an undeclared one is not
///    invented here (design 91a9bfe7). The roll-up counts these out
///    loud, as before (`total_only_runs`).
/// 4. **The run reported no count at all.** No arithmetic reaches a
///    price from no number, declared ratio or not (`unreported_runs`).
///
/// A MEASURED SPLIT ALWAYS WINS: a run that reported both halves is
/// priced at the two rates even on a model that declares a blend, and
/// the blend is never consulted for it. That ordering is what makes
/// [`PricingBasis`] worth recording — the weaker basis is used only
/// where the stronger one does not exist.
///
/// Integer arithmetic throughout, in `u128` so a run cannot overflow
/// the multiply before the divide, rounded to the nearest micro-USD
/// (half up) and saturated into `u64`.
pub fn price_run(card: &[RateCardRow], run: &NewAgentRun) -> Option<(u64, String)> {
    let model = run.model()?;
    let row = card.iter().find(|r| r.model == model)?;
    let micros = match run.tokens {
        TokenUsage::Split { input, output } => rounded_div(
            u128::from(input) * u128::from(row.input_usd_micros_per_mtok)
                + u128::from(output) * u128::from(row.output_usd_micros_per_mtok),
            PPM,
        ),
        // Every count at its own rate, or nothing: a row that declares
        // no cache rate cannot price 97% of what the run processed,
        // and three of four terms would read as the whole bill.
        TokenUsage::Metered {
            input,
            cache_write,
            cache_read,
            output,
        } => rounded_div(
            u128::from(input) * u128::from(row.input_usd_micros_per_mtok)
                + u128::from(cache_write) * u128::from(row.cache_write_usd_micros_per_mtok?)
                + u128::from(cache_read) * u128::from(row.cache_read_usd_micros_per_mtok?)
                + u128::from(output) * u128::from(row.output_usd_micros_per_mtok),
            PPM,
        ),
        // The declared blend, through the same accessor a surface
        // shows, so the figure a reader checks by hand is the figure
        // charged.
        TokenUsage::TotalOnly { total } => rounded_div(
            u128::from(total) * u128::from(row.blended_usd_micros_per_mtok()?),
            PPM,
        ),
        TokenUsage::Unreported => return None,
    };
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
    /// What `usd_micros` rests on: `Blended` the moment one blended run
    /// joins, `None` once the figure itself is gone. Never read the
    /// figure without it.
    #[serde(default)]
    pub pricing_basis: Option<PricingBasis>,
    pub unpriced_runs: u64,
    pub total_only_runs: u64,
    pub unreported_runs: u64,
    /// How many of the priced runs were priced at a declared blend
    /// rather than a measured split — the basis with a size beside it.
    pub blended_runs: u64,
    /// How many runs in this bucket declared no effort at all
    /// ([`EffortEra::Unrecorded`]).
    pub effort_unrecorded_runs: u64,
    /// How many declared an effort that never reached the CPU
    /// ([`EffortEra::Unapplied`]).
    pub effort_unapplied_runs: u64,
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
/// `total_only_runs` is how many reported no split, and
/// `unreported_runs` is how many reported no tokens at all (backlog
/// 65c9c05a).
///
/// **And `pricing_basis` says what the figure that IS here rests on.**
/// A sum is only as measured as its least-measured member, so one
/// blended run makes the bucket `Blended` (design 91a9bfe7): a reader
/// who sees a number and no basis is back to reading an estimate as a
/// measurement, which is the defect the whole of this module's rule 3
/// exists to refuse. `blended_runs` says how many.
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
    /// What `usd_micros` rests on — see this type's doc. `None`
    /// whenever `usd_micros` is `None`: no figure, nothing to describe.
    #[serde(default)]
    pub pricing_basis: Option<PricingBasis>,
    pub unpriced_runs: u64,
    /// How many runs reported a bare total. NO LONGER a subset of
    /// `unpriced_runs`: since design 91a9bfe7 such a run IS priced when
    /// its model declares a blend, and the counter now says how many
    /// figures rest on an assumed ratio rather than how many are
    /// missing. `blended_runs` is the priced part of it; the remainder
    /// are totals on models that declare no ratio.
    pub total_only_runs: u64,
    /// How many runs reported no token count at all. A subset of
    /// `unpriced_runs`, and disjoint from `total_only_runs`: the fixes
    /// differ, so the counters do.
    pub unreported_runs: u64,
    /// How many runs were priced at a declared blend.
    pub blended_runs: u64,
    /// How many runs declared no effort, and how many declared one that
    /// was never applied — the two eras a series must exclude, counted
    /// rather than silently averaged over (backlog fd5ce137). See
    /// [`EffortEra`] for why the rows are marked and not backfilled.
    pub effort_unrecorded_runs: u64,
    pub effort_unapplied_runs: u64,
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
        pricing_basis: total.pricing_basis,
        unpriced_runs: total.unpriced_runs,
        total_only_runs: total.total_only_runs,
        unreported_runs: total.unreported_runs,
        blended_runs: total.blended_runs,
        effort_unrecorded_runs: total.effort_unrecorded_runs,
        effort_unapplied_runs: total.effort_unapplied_runs,
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
        // The identity a fold starts from, like the measured zero
        // beside it: an empty bucket has nothing blended in it, so it
        // starts at the strongest basis and each run can only lower it.
        pricing_basis: Some(PricingBasis::Metered),
        unpriced_runs: 0,
        total_only_runs: 0,
        unreported_runs: 0,
        blended_runs: 0,
        effort_unrecorded_runs: 0,
        effort_unapplied_runs: 0,
    }
}

/// Add one run into a bucket. `usd_micros`, `total_tokens` and the two
/// split halves go to `None` and STAY `None` the moment a run joins
/// that cannot supply them — see [`RunSummary`].
fn fold(into: &mut GroupSpend, run: &AgentRun) {
    into.runs += 1;
    // Counted, never dropped: the excluded eras are part of the record
    // and a bucket that quietly omitted them would be the same missing
    // caveat this counter exists to publish (backlog fd5ce137).
    match run.effort_era() {
        EffortEra::Unrecorded => into.effort_unrecorded_runs += 1,
        EffortEra::Unapplied => into.effort_unapplied_runs += 1,
        EffortEra::Applied => {}
    }
    // `and_then`, not `map`: a bucket that has already met an
    // unreported run stays unanswered, and a bucket meeting its first
    // one stops answering. Adding zero for it would be the projection
    // telling the same lie the column used to.
    into.total_tokens = match run.run.tokens.total() {
        Some(t) => into.total_tokens.map(|s| s.saturating_add(t)),
        None => None,
    };
    match run.run.tokens {
        // A metered run's halves are its uncached input and its output;
        // its cache counts ride the row, and its total above holds them.
        TokenUsage::Split { input, output } | TokenUsage::Metered { input, output, .. } => {
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
            if let Some(basis) = run.pricing_basis() {
                // `map`, so a bucket that has already lost its figure
                // does not describe one, and one blended run carries
                // the whole bucket to blended.
                into.pricing_basis = into.pricing_basis.map(|acc| acc.least_measured(basis));
                if basis == PricingBasis::Blended {
                    into.blended_runs += 1;
                }
            }
        }
        None => {
            into.usd_micros = None;
            // No figure, no basis: `Split` here would describe a number
            // that is not there.
            into.pricing_basis = None;
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
                blended_input_share_ppm: None,
                cache_read_usd_micros_per_mtok: None,
                cache_write_usd_micros_per_mtok: None,
            },
            RateCardRow {
                model: "haiku-4-5".into(),
                input_usd_micros_per_mtok: 1_000_000,
                output_usd_micros_per_mtok: 5_000_000,
                note: "test".into(),
                blended_input_share_ppm: None,
                cache_read_usd_micros_per_mtok: None,
                cache_write_usd_micros_per_mtok: None,
            },
            // The one row that declares a blend, as the migration
            // seeds it: the model 99 of the last 100 recorded runs ran
            // on, and the only one with measured splits behind its
            // ratio. The two rows above declare none on purpose —
            // undeclared is unpriced, not assumed.
            RateCardRow {
                model: "opus-5[1m]".into(),
                input_usd_micros_per_mtok: 5_000_000,
                output_usd_micros_per_mtok: 25_000_000,
                note: "test".into(),
                blended_input_share_ppm: Some(875_000),
                // The cache rates the migration seeds (backlog
                // e6b2066f): 0.1x input to read, 1.25x to write. The
                // two rows above carry none, which leaves a metered
                // run on them unpriced rather than half-priced.
                cache_read_usd_micros_per_mtok: Some(500_000),
                cache_write_usd_micros_per_mtok: Some(6_250_000),
            },
        ]
    }

    fn metered(actor: &str) -> NewAgentRun {
        with_tokens(
            actor,
            TokenUsage::Metered {
                input: 40,
                cache_write: 30_000,
                cache_read: 1_470_000,
                output: 9_000,
            },
        )
    }

    /// Backlog e6b2066f: every count at its own rate. 40 x $5 + 30,000
    /// x $6.25 + 1,470,000 x $0.50 + 9,000 x $25 per MTok = $1.1477,
    /// the shape of the operator's 2026-09-23 spot check: a run whose
    /// harness reported ~150k tokens (its final context, $1.13 at the
    /// $7.50 blend) processed ~1.5M.
    #[test]
    fn a_metered_run_is_priced_at_four_rates_and_says_so() {
        let mut new = metered("agent-claude");
        new.model = Some("opus-5[1m]".into());
        let run = recorded(new, &card());
        assert_eq!(run.usd_micros, Some(1_147_700));
        assert_eq!(run.pricing_basis(), Some(PricingBasis::Metered));
    }

    #[test]
    fn a_metered_run_on_a_row_without_cache_rates_is_unpriced_not_half_priced() {
        let mut new = metered("agent-claude");
        new.model = Some("haiku-4-5".into());
        let run = recorded(new, &card());
        assert_eq!(run.usd_micros, None);
    }

    #[test]
    fn a_bucket_is_metered_only_while_every_run_in_it_is() {
        let mut m = metered("agent-claude");
        m.model = Some("opus-5[1m]".into());
        let alone = summarize(&[recorded(m.clone(), &card())]);
        assert_eq!(alone.pricing_basis, Some(PricingBasis::Metered));
        assert_eq!(alone.total_tokens, Some(1_509_040));
        let mut s = a_run("agent-claude", 100, 10);
        s.run_id = "run-split".into();
        s.model = Some("opus-5[1m]".into());
        let mixed = summarize(&[recorded(m, &card()), recorded(s, &card())]);
        assert_eq!(mixed.pricing_basis, Some(PricingBasis::Split));
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
        let cost = run.cost();
        assert_eq!(
            cost.tokens,
            TokenUsage::Split {
                input: 999,
                output: 999
            },
            "the tokens are a fact either way"
        );
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
        let cost: Cost = run.cost();
        assert_eq!(cost.tokens.input(), Some(1_000_000));
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
    fn a_priced_total_only_run_reports_that_price_through_cost() {
        // Backlog e059a754: `Cost` had an input field and an output
        // field and no shape for a blended total, so this run — priced
        // through the declared blend, with `usd_micros` set — read as
        // unpriced to anyone going through `cost()` while the budget
        // desks reading `usd_micros` directly saw the spend. Two
        // readers of one record, disagreeing.
        let run = recorded(a_total_run("claude:opus-5[1m]", 142_982), &card());
        let cost = run.cost();
        assert_eq!(
            cost.tokens,
            TokenUsage::TotalOnly { total: 142_982 },
            "the shape the reporter measured, carried through unsplit"
        );
        assert_eq!(
            cost.usd_micros, run.usd_micros,
            "the same number both readers of this run must see"
        );
        assert!(cost.usd_micros.is_some(), "the blend priced it");
    }

    #[test]
    fn an_unreported_run_still_maps_onto_cost_as_unknown() {
        // Backlog e059a754: unknown tokens are a fact the type can
        // hold now, so `cost()` no longer refuses to answer for them.
        let mut new = a_total_run("claude:opus-5", 1);
        new.tokens = TokenUsage::Unreported;
        let run = recorded(new, &card());
        let cost = run.cost();
        assert_eq!(cost.tokens, TokenUsage::Unreported);
        assert_eq!(cost.usd_micros, None, "no count reaches no price");
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

    // ----------------------------------------------------------------
    // A total priced at a DECLARED blend, and a record that says so.
    // Design 91a9bfe7, decided by David 2026-09-20 on the measurement
    // that 83 of 85 recorded runs carried no cost at all, because the
    // harness reports `<subagent_tokens>` as one number and the card
    // prices two.
    // ----------------------------------------------------------------

    #[test]
    fn a_declared_blend_is_derived_from_the_two_rates_it_sits_between() {
        let row = card()
            .into_iter()
            .find(|r| r.model == "opus-5[1m]")
            .expect("the fixture declares one");
        // 87.5% at $5 + 12.5% at $25 = $7.50 per MTok, and it sits
        // between the two rates because it is a weighting of them.
        assert_eq!(row.blended_usd_micros_per_mtok(), Some(7_500_000));
        let plain = card()
            .into_iter()
            .find(|r| r.model == "opus-5")
            .expect("on the fixture card");
        assert_eq!(
            plain.blended_usd_micros_per_mtok(),
            None,
            "a row that declares no ratio states no blend"
        );
    }

    #[test]
    fn a_total_only_run_is_priced_at_the_declared_blend() {
        // 182,068 tokens — the `<subagent_tokens>` line quoted in the
        // packet — at $7.50 per MTok is 1,365,510 micro-USD.
        let run = recorded(a_total_run("claude:opus-5[1m]", 182_068), &card());
        assert_eq!(run.usd_micros, Some(1_365_510));
        assert_eq!(run.priced_by.as_deref(), Some("opus-5[1m]"));
        assert_eq!(
            run.pricing_basis(),
            Some(PricingBasis::Blended),
            "the figure must say it rests on the declared ratio, not on a measurement"
        );
    }

    #[test]
    fn a_blended_figure_reports_itself_as_blended_through_a_roll_up() {
        // The condition David attached to the whole change: a bucket
        // holding any blended run reports itself as blended, so no
        // reader of the sum can take it for a measurement.
        let card = card();
        let mut split = a_run("claude:opus-5[1m]", 1_000_000, 0);
        split.run_id = "run-split".into();
        let runs = vec![
            recorded(split, &card),
            recorded(a_total_run("claude:opus-5[1m]", 1_000_000), &card),
        ];
        let s = summarize(&runs);
        assert_eq!(s.unpriced_runs, 0, "both are priced now");
        assert_eq!(s.usd_micros, Some(5_000_000 + 7_500_000));
        assert_eq!(
            s.pricing_basis,
            Some(PricingBasis::Blended),
            "one blended run makes the sum blended"
        );
        assert_eq!(s.blended_runs, 1);
        assert_eq!(s.total_only_runs, 1);
        // And through the grouped roll-ups, which is where a surface
        // reads a per-model or per-car figure.
        assert_eq!(
            s.by_model
                .iter()
                .map(|g| (g.key.as_str(), g.pricing_basis, g.blended_runs))
                .collect::<Vec<_>>(),
            vec![("opus-5[1m]", Some(PricingBasis::Blended), 1)]
        );
        // The wire says it too — a surface reads the basis off the JSON.
        let json = serde_json::to_string(&s).expect("serializes");
        assert!(json.contains("\"pricing_basis\":\"blended\""), "{json}");
    }

    #[test]
    fn a_bucket_of_measured_splits_says_split() {
        let s = summarize(&[recorded(a_run("claude:opus-5[1m]", 1_000_000, 0), &card())]);
        assert_eq!(s.pricing_basis, Some(PricingBasis::Split));
        assert_eq!(s.blended_runs, 0);
    }

    #[test]
    fn a_measured_split_wins_over_the_blend() {
        // Same model, same token count, both priceable. The split is
        // priced at the two rates and reads as measured; blending it
        // would throw away the measurement that exists.
        let run = recorded(a_run("claude:opus-5[1m]", 875_000, 125_000), &card());
        assert_eq!(
            run.usd_micros,
            Some(875_000 * 5 + 125_000 * 25),
            "the two rates, not the blend"
        );
        assert_eq!(run.pricing_basis(), Some(PricingBasis::Split));
    }

    #[test]
    fn a_single_runs_wire_shape_names_its_basis_and_reads_back() {
        // Backlog 93fdb119: the row a reader gets carries the word the
        // roll-up carries, derived by the same rule — and a reader that
        // parses it back into `AgentRun` loses nothing and trips on
        // nothing (the flattened token shape included).
        let card = card();
        for (run, want) in [
            (
                recorded(a_run("claude:opus-5[1m]", 875_000, 125_000), &card),
                serde_json::json!("split"),
            ),
            (
                recorded(a_total_run("claude:opus-5[1m]", 1_000_000), &card),
                serde_json::json!("blended"),
            ),
            (
                recorded(a_total_run("claude:opus-5", 142_982), &card),
                serde_json::Value::Null,
            ),
        ] {
            let wire = serde_json::to_value(AgentRunView::from(run.clone())).expect("serializes");
            assert_eq!(wire["pricing_basis"], want, "{wire}");
            let back: AgentRun = serde_json::from_value(wire).expect("reads back");
            assert_eq!(back, run);
        }
    }

    #[test]
    fn a_total_on_a_model_that_declares_no_ratio_stays_unpriced() {
        // Undeclared is unpriced, not assumed: the assumption lives in
        // the registry, and a row that does not carry one is not given
        // a neighbour's.
        let run = recorded(a_total_run("claude:opus-5", 142_982), &card());
        assert_eq!(run.usd_micros, None, "no declared ratio, no blend");
        assert_eq!(run.priced_by, None);
        assert_eq!(run.pricing_basis(), None, "no figure, no basis");
    }

    #[test]
    fn a_run_with_no_count_is_unpriced_even_where_a_ratio_is_declared() {
        let mut silent = a_total_run("claude:opus-5[1m]", 0);
        silent.tokens = TokenUsage::Unreported;
        let run = recorded(silent, &card());
        assert_eq!(
            run.usd_micros, None,
            "a ratio multiplies a count; there is no count"
        );
        assert_eq!(run.pricing_basis(), None);
    }

    #[test]
    fn an_unpriced_run_takes_the_buckets_basis_with_its_figure() {
        // The grain `usd_micros` already follows: the moment the sum is
        // gone, so is the word describing it.
        let card = card();
        let runs = vec![
            recorded(a_total_run("claude:opus-5[1m]", 1_000_000), &card),
            recorded(a_run("claude:not-on-the-card", 1, 1), &card),
        ];
        let s = summarize(&runs);
        assert_eq!(s.usd_micros, None);
        assert_eq!(
            s.pricing_basis, None,
            "there is no figure left for a basis to describe"
        );
        assert_eq!(s.blended_runs, 1, "the count of blended runs survives");
    }

    #[test]
    fn an_already_recorded_unpriced_run_is_left_exactly_as_it_was() {
        // Design resolution three (*existing rows*): the 83 runs on the
        // record before this change stay unpriced. Nothing re-prices
        // them, because pricing happens once at record time and a
        // rebuild replays the recorded figure — and the basis is
        // derived from that figure, so an unpriced row reads as having
        // no basis rather than as blended, which would record assurance
        // nobody has.
        let old: AgentRun = serde_json::from_str(
            r#"{
                "run_id": "run-old",
                "actor_id": "agent-claude",
                "model": "opus-5[1m]",
                "started_at": "2026-09-19T01:00:00Z",
                "finished_at": "2026-09-19T01:10:00Z",
                "outcome": "success",
                "total_tokens": 182068,
                "usd_micros": null,
                "recorded_at": "2026-09-19T01:10:01Z"
            }"#,
        )
        .expect("an existing row parses");
        assert_eq!(old.usd_micros, None);
        assert_eq!(old.pricing_basis(), None);
        let s = summarize(&[old]);
        assert_eq!(s.unpriced_runs, 1);
        assert_eq!(s.pricing_basis, None);
        assert_eq!(s.blended_runs, 0);
    }

    #[test]
    fn the_basis_round_trips_its_wire_form() {
        for b in [PricingBasis::Split, PricingBasis::Blended] {
            assert_eq!(PricingBasis::parse(b.as_str()), Some(b));
            assert_eq!(
                serde_json::to_string(&b).expect("serializes"),
                format!("\"{}\"", b.as_str())
            );
        }
        assert_eq!(PricingBasis::parse("measured"), None);
    }

    #[test]
    fn every_price_the_card_computes_agrees_with_the_basis_it_reports() {
        // The pin for the one place these two could drift: `price_run`
        // decides WHICH arithmetic ran, `pricing_basis` decides what
        // the record calls it, and they are separate functions. Over
        // every shape and every fixture row, a priced total is blended
        // and a priced split is not.
        let card = card();
        for model in ["opus-5", "haiku-4-5", "opus-5[1m]", "not-on-the-card"] {
            let actor = format!("claude:{model}");
            for tokens in [
                TokenUsage::Split {
                    input: 10,
                    output: 1,
                },
                TokenUsage::TotalOnly { total: 11 },
                TokenUsage::Unreported,
                TokenUsage::Metered {
                    input: 1,
                    cache_write: 2,
                    cache_read: 3,
                    output: 4,
                },
            ] {
                let run = recorded(with_tokens(&actor, tokens), &card);
                match (run.usd_micros, run.run.tokens) {
                    (Some(_), TokenUsage::Split { .. }) => {
                        assert_eq!(run.pricing_basis(), Some(PricingBasis::Split), "{model}")
                    }
                    (Some(_), TokenUsage::Metered { .. }) => {
                        assert_eq!(run.pricing_basis(), Some(PricingBasis::Metered), "{model}")
                    }
                    (Some(_), TokenUsage::TotalOnly { .. }) => {
                        assert_eq!(run.pricing_basis(), Some(PricingBasis::Blended), "{model}")
                    }
                    (Some(_), TokenUsage::Unreported) => {
                        panic!("{model}: no count cannot produce a price")
                    }
                    (None, _) => assert_eq!(run.pricing_basis(), None, "{model}"),
                }
            }
        }
    }

    /// A run recorded in the era, from the fixture's `detail` up: the
    /// declared effort (or none) and the instant the record landed are
    /// the two facts an era is read off.
    fn in_era(effort: Option<&str>, recorded_at: &str) -> AgentRun {
        let mut new = a_run("claude:opus-5", 1_000, 1_000);
        new.run_id = format!("run-{effort:?}-{recorded_at}");
        if let Some(effort) = effort {
            new.detail = serde_json::json!({ "effort": effort });
        }
        let mut run = recorded(new, &card());
        run.recorded_at = recorded_at.parse().expect("an RFC3339 instant");
        run
    }

    #[test]
    fn a_run_that_declares_no_effort_is_the_unrecorded_era() {
        // The 28 rows measured on the live table on 2026-09-22: the
        // absent key IS the marker, so nothing has to be written to
        // them to make them excludable.
        let run = in_era(None, "2026-09-19T08:54:00Z");
        assert_eq!(run.declared_effort(), None);
        assert_eq!(run.effort_era(), EffortEra::Unrecorded);
    }

    #[test]
    fn an_effort_label_recorded_before_it_selected_the_cpu_reads_unapplied() {
        // The worse half: a label that looks like data and was never
        // applied. Only the boundary instant can tell it apart.
        let run = in_era(Some("high"), "2026-09-19T17:00:00Z");
        assert_eq!(run.declared_effort(), Some("high"));
        assert_eq!(run.effort_era(), EffortEra::Unapplied);
    }

    #[test]
    fn an_effort_label_recorded_after_the_boundary_is_applied() {
        let run = in_era(Some("high"), "2026-09-22T19:00:00Z");
        assert_eq!(run.effort_era(), EffortEra::Applied);
    }

    #[test]
    fn a_run_with_no_effort_after_the_boundary_is_still_unrecorded() {
        // The row's own evidence beats the clock: a report that
        // declared nothing has no label that could have been applied.
        let run = in_era(None, "2026-09-22T19:00:00Z");
        assert_eq!(run.effort_era(), EffortEra::Unrecorded);
    }

    #[test]
    fn the_applied_boundary_is_the_instant_its_sha_landed() {
        assert_eq!(
            effort_applied_from().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "2026-09-19T17:30:03Z",
            "the commit time of {EFFORT_APPLIED_FROM_SHA}"
        );
    }

    #[test]
    fn a_roll_up_counts_both_excluded_eras_rather_than_averaging_over_them() {
        let runs = vec![
            in_era(None, "2026-09-19T08:54:00Z"),
            in_era(Some("high"), "2026-09-19T17:00:00Z"),
            in_era(Some("high"), "2026-09-22T19:00:00Z"),
        ];
        let s = summarize(&runs);
        assert_eq!(s.runs, 3);
        assert_eq!(s.effort_unrecorded_runs, 1);
        assert_eq!(s.effort_unapplied_runs, 1);
        // Per bucket too: a model's or a car's figure carries the same
        // warning the total does.
        let by_model = &s.by_model[0];
        assert_eq!(by_model.key, "opus-5");
        assert_eq!(by_model.effort_unrecorded_runs, 1);
        assert_eq!(by_model.effort_unapplied_runs, 1);
        // The wire spelling, pinned: `GET /api/agent-runs/cost` is
        // where an operator reads this, and a recorded probe greps the
        // key by name.
        let wire = serde_json::to_value(&s).expect("a summary serializes");
        assert_eq!(wire["effort_unrecorded_runs"], 1);
        assert_eq!(wire["effort_unapplied_runs"], 1);
    }

    #[test]
    fn an_empty_roll_up_excludes_nothing() {
        let s = summarize(&[]);
        assert_eq!(s.effort_unrecorded_runs, 0);
        assert_eq!(s.effort_unapplied_runs, 0);
    }

    #[test]
    fn the_era_round_trips_its_wire_form() {
        for e in [
            EffortEra::Unrecorded,
            EffortEra::Unapplied,
            EffortEra::Applied,
        ] {
            assert_eq!(EffortEra::parse(e.as_str()), Some(e));
        }
        assert_eq!(EffortEra::parse("medium-ish"), None);
    }
}
