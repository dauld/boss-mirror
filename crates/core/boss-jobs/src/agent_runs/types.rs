//! Wire + domain types for the agent-run record.
//!
//! Three deliberate shapes here, each the answer to a way this could
//! have been got wrong:
//!
//! 1. **THE MODEL IS NOT A FIELD.** It is read out of `actor_id`.
//!    `boss_core::actor::ActorId::Agent { mode, model }` already makes
//!    the model a groupable dimension of the actor id — that module's
//!    own doc comment says so and says no separate `_model` key should
//!    be added. A `model` column beside `actor_id` would be the same
//!    fact in two places (§9a), so [`AgentRun::model`] parses it and
//!    the roll-ups below group on the parse. One definition.
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

use std::collections::BTreeMap;

use boss_core::actor::ActorId;
use boss_core::agent::Cost;
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

/// A finished agent run, as the caller reports it. No price: see the
/// module doc.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewAgentRun {
    /// Caller-supplied and the primary key, so a retried report
    /// records the run once (idempotence — the same contract the
    /// cadence claim rests on).
    pub run_id: String,
    /// The CPU that ran. Must be the agent form `<mode>:<model>`.
    pub actor_id: ActorId,
    /// Both bound by the caller, never the database's `NOW()`: the run
    /// happened on the caller's clock, and a write-time reading would
    /// describe when the report arrived instead.
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub outcome: RunOutcome,
    #[serde(default)]
    pub error: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
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
    /// The instant the record landed — the event's timestamp, so the
    /// row and the log entry share one instant.
    pub recorded_at: DateTime<Utc>,
}

impl AgentRun {
    /// The model half of the actor id, or `None` if the actor is not
    /// an agent. The ONLY place a model is derived.
    pub fn model(&self) -> Option<&str> {
        match &self.run.actor_id {
            ActorId::Agent { model, .. } => Some(model.as_str()),
            _ => None,
        }
    }

    /// Wall seconds the run took. Derived, never stored: `finished_at -
    /// started_at` is the same fact, and a stored copy could disagree
    /// with it.
    pub fn duration_secs(&self) -> i64 {
        (self.run.finished_at - self.run.started_at).num_seconds()
    }

    /// The run as a `boss_core::agent::Cost`. An unpriced run reports
    /// its tokens with `usd_micros: 0` — the only place zero stands in
    /// for unknown, and only because `Cost` has no room to say
    /// otherwise. Ask [`AgentRun::usd_micros`] when the difference
    /// matters.
    pub fn cost(&self) -> Cost {
        Cost {
            input_tokens: self.run.input_tokens,
            output_tokens: self.run.output_tokens,
            usd_micros: self.usd_micros.unwrap_or(0),
        }
    }
}

/// Price a run against the card. `None` when no row covers the model,
/// or when the actor is not an agent and so names no model.
///
/// Integer arithmetic throughout, in `u128` so a run cannot overflow
/// the multiply before the divide, rounded to the nearest micro-USD
/// (half up) and saturated into `u64`.
pub fn price_run(card: &[RateCardRow], run: &NewAgentRun) -> Option<(u64, String)> {
    let model = match &run.actor_id {
        ActorId::Agent { model, .. } => model.as_str(),
        _ => return None,
    };
    let row = card.iter().find(|r| r.model == model)?;
    let micros = rounded_div(
        u128::from(run.input_tokens) * u128::from(row.input_usd_micros_per_mtok)
            + u128::from(run.output_tokens) * u128::from(row.output_usd_micros_per_mtok),
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupSpend {
    /// The model string, or the branch name.
    pub key: String,
    pub runs: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tool_calls: u64,
    pub wall_secs: i64,
    #[serde(default)]
    pub usd_micros: Option<u64>,
    pub unpriced_runs: u64,
}

/// The answer to "what did this cost to build".
///
/// `usd_micros` is `Some` only when EVERY run in the set was priced.
/// A partial total that looked complete is the failure this avoids:
/// one missing rate-card row would silently understate the bill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSummary {
    pub runs: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub tool_calls: u64,
    /// Summed run durations — agent time spent, not elapsed time,
    /// since runs may overlap.
    pub wall_secs: i64,
    #[serde(default)]
    pub usd_micros: Option<u64>,
    pub unpriced_runs: u64,
    pub by_model: Vec<GroupSpend>,
    pub by_branch: Vec<GroupSpend>,
}

/// Roll up a set of runs. A pure function of the rows — the projection
/// discipline applied one level up: group in Rust off the one model
/// parse rather than re-deriving the model split in SQL.
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
        input_tokens: total.input_tokens,
        output_tokens: total.output_tokens,
        tool_calls: total.tool_calls,
        wall_secs: total.wall_secs,
        usd_micros: total.usd_micros,
        unpriced_runs: total.unpriced_runs,
        by_model: by_model.into_values().collect(),
        by_branch: by_branch.into_values().collect(),
    }
}

fn blank(key: &str) -> GroupSpend {
    GroupSpend {
        key: key.to_string(),
        runs: 0,
        input_tokens: 0,
        output_tokens: 0,
        tool_calls: 0,
        wall_secs: 0,
        usd_micros: Some(0),
        unpriced_runs: 0,
    }
}

/// Add one run into a bucket. `usd_micros` goes to `None` and STAYS
/// `None` the moment an unpriced run joins — see [`RunSummary`].
fn fold(into: &mut GroupSpend, run: &AgentRun) {
    into.runs += 1;
    into.input_tokens = into.input_tokens.saturating_add(run.run.input_tokens);
    into.output_tokens = into.output_tokens.saturating_add(run.run.output_tokens);
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
        NewAgentRun {
            run_id: format!("run-{actor}-{input}"),
            actor_id: actor.parse().expect("ActorId::from_str is infallible"),
            started_at: "2026-09-10T01:00:00Z".parse().unwrap(),
            finished_at: "2026-09-10T01:10:00Z".parse().unwrap(),
            outcome: RunOutcome::Success,
            error: None,
            input_tokens: input,
            output_tokens: output,
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
            recorded_at: "2026-09-10T01:10:01Z".parse().unwrap(),
        }
    }

    #[test]
    fn model_is_read_from_the_actor_id_not_a_field() {
        let run = recorded(a_run("claude:opus-5", 100, 10), &card());
        assert_eq!(run.model(), Some("opus-5"));
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
        assert_eq!(run.run.input_tokens, 999);
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
        assert_eq!(s.input_tokens, 2_000_000);
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
        assert_eq!(cost.input_tokens, 1_000_000);
        assert_eq!(cost.usd_micros, 30_000_000);
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
    }
}
