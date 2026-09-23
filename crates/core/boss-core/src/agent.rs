//! Agent spend and admission — the value types an agent run's cost
//! and budget are measured in.
//!
//! This module used to be the domain vocabulary of the Cybernetics
//! agent stack as well: agent slugs, inbox messages and claims, run
//! handles and completions, a TOML-configured `AgentSpec`. Everything
//! that spoke that vocabulary lived in boss-cybernetics and the
//! adapters boss-events kept for it; the crate was retired in train
//! #582 and the vocabulary went with it (backlog 05a003da,
//! 2026-09-23). What remains is what the agent-runs record in
//! boss-jobs still measures with: [`TokenUsage`], [`Cost`],
//! [`Window`], and the one budget rule, [`BudgetDecision::decide`],
//! with its [`AgentCaps`] and [`AgentLoad`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What a run spent, in the three shapes a reporter can actually be in.
///
/// **The total is one fact with one definition.** For a [`Split`] it is
/// DERIVED from the halves, so a stored total cannot drift from them
/// (§9a); for [`TotalOnly`] it is the only thing measured. There is no
/// state where both are stored independently and can disagree, because
/// this type cannot express one.
///
/// A `TotalOnly` run is priceable only under an assumed ratio, which
/// the model's rate-card row either DECLARES or does not; an
/// undeclared one is never invented (design 91a9bfe7), and a figure
/// that rests on a declared one says so through its recorded basis.
///
/// **[`Unreported`] is UNKNOWN, never zero** (backlog 65c9c05a,
/// 2026-09-19). A harness that printed no usage line leaves the
/// reporter with nothing to say, and the record has to say that. Until
/// this variant existed, `boss dispatch --report` without `--tokens`
/// sent `total_tokens: 0` with a `detail.tokens_reported: false`
/// beside it, so 13 of 42 live rows read as a measurement of zero to
/// any query that did not know to check the companion flag — an
/// average over the column silently included 13 zeros. NULL for
/// unknown is the distinction `agent_runs.usd_micros` already draws in
/// that same table (unpriced, not free), followed here rather than a
/// third convention invented beside it.
///
/// It lived in `boss-jobs` beside the agent-runs record until backlog
/// e059a754, which needed [`Cost`] to be able to hold a blended total:
/// the record and the port value describe the same three shapes, and
/// one definition cannot drift from itself (CLAUDE.md §9a).
///
/// [`Split`]: TokenUsage::Split
/// [`TotalOnly`]: TokenUsage::TotalOnly
/// [`Unreported`]: TokenUsage::Unreported
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenUsage {
    /// Both halves measured. Priceable.
    Split { input: u64, output: u64 },
    /// One number, which is everything the reporter had. Recorded in
    /// full; priced only where the model's row declares a blend.
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

    /// A sum is only as measured as its least-measured term — the rule
    /// the agent-runs roll-up applies to a bucket's pricing basis,
    /// applied here to the counts (backlog e059a754). Two splits
    /// add half by half; a split and a bare total add to a bare total,
    /// because the sum's division between input and output is no
    /// longer known; anything plus [`Unreported`] is unreported,
    /// because a window containing a run nobody counted has no
    /// measured total, and answering with the rest would read as a
    /// measurement of all of it.
    ///
    /// [`Cost::ZERO`] is still the fold identity in all three cases:
    /// it is a measured `Split` of zeros, and adding it changes
    /// neither the shape nor the number of whatever it meets.
    ///
    /// [`Unreported`]: TokenUsage::Unreported
    pub fn saturating_sum(self, other: TokenUsage) -> TokenUsage {
        match (self, other) {
            (
                TokenUsage::Split {
                    input: a_in,
                    output: a_out,
                },
                TokenUsage::Split {
                    input: b_in,
                    output: b_out,
                },
            ) => TokenUsage::Split {
                input: a_in.saturating_add(b_in),
                output: a_out.saturating_add(b_out),
            },
            (TokenUsage::Unreported, _) | (_, TokenUsage::Unreported) => TokenUsage::Unreported,
            // One side lost its split, so the sum has none. Both
            // totals exist here: the `Unreported` arm above is the
            // only way `total()` answers `None`.
            (a, b) => TokenUsage::TotalOnly {
                total: a
                    .total()
                    .unwrap_or(0)
                    .saturating_add(b.total().unwrap_or(0)),
            },
        }
    }
}

/// The three token keys on the wire, defined once so the serializer and
/// the deserializer cannot disagree about their names. Flattened into
/// [`Cost`] and into the agent-runs report, so each stays one flat
/// JSON object.
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

/// Cost incurred by an agent run. All monetary values in micro-USD
/// (1_000_000 = $1.00) to avoid floating point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    /// What the run spent, in whichever of the three shapes its
    /// reporter was actually in. Two `u64` fields until backlog
    /// e059a754: a run that reported one blended number had no shape
    /// here at all, so `AgentRun::cost()` answered `None` for a run
    /// the record had already PRICED, and a reader going through this
    /// type disagreed with one reading `usd_micros` directly.
    /// Flattened on the wire, so the JSON keys are unchanged
    /// (`input_tokens` / `output_tokens`, plus `total_tokens`, which a
    /// split states rather than making every reader add the halves).
    #[serde(flatten)]
    pub tokens: TokenUsage,
    /// What the run cost, or `None` — **unpriced is not free**, the
    /// phrase the `agent_runs.usd_micros` column comment already uses
    /// for the same fact one layer down. Nothing on the rate card
    /// covered the model, or the reporter named no price: either way
    /// the number is not known, and a zero in its place is a
    /// measurement a later reader cannot tell from a real one. Was a
    /// plain `u64` until backlog c6e2341c, which is why
    /// `AgentRun::cost()` had to write a zero it knew was a lie.
    #[serde(default)]
    pub usd_micros: Option<u64>,
}

impl Cost {
    /// Nothing spent, and that is a measurement: `Some(0)`, not `None`.
    pub const ZERO: Cost = Cost {
        tokens: TokenUsage::Split {
            input: 0,
            output: 0,
        },
        usd_micros: Some(0),
    };

    /// A run whose spend nobody could read: no count, no price.
    /// Distinct from [`ZERO`] in exactly the way this type exists to
    /// express — and since backlog e059a754 the token side says
    /// unknown too, rather than the pair of zeros it used to carry for
    /// the same "nobody measured this".
    ///
    /// [`ZERO`]: Cost::ZERO
    pub const UNPRICED: Cost = Cost {
        tokens: TokenUsage::Unreported,
        usd_micros: None,
    };

    /// Both halves follow the same rule: a sum is only as measured as
    /// its least-measured term. One unpriced run takes the money to
    /// unknown and one uncounted run takes the tokens to unknown,
    /// because a sum that silently skipped either would read as a
    /// measurement of every run in the window — the rule the
    /// agent-runs roll-up applies to `usd_micros` and `total_tokens`
    /// one layer down. See [`TokenUsage::saturating_sum`].
    pub fn saturating_sum(self, other: Cost) -> Cost {
        Cost {
            tokens: self.tokens.saturating_sum(other.tokens),
            usd_micros: match (self.usd_micros, other.usd_micros) {
                (Some(a), Some(b)) => Some(a.saturating_add(b)),
                _ => None,
            },
        }
    }
}

impl Default for Cost {
    /// [`Cost::ZERO`], not a derived `None`: a default-constructed cost
    /// is the identity a fold starts from, and that is a measured zero.
    fn default() -> Cost {
        Cost::ZERO
    }
}

impl std::ops::Add for Cost {
    type Output = Cost;
    fn add(self, rhs: Cost) -> Cost {
        self.saturating_sum(rhs)
    }
}

/// Decision returned from a budget check.
///
/// A VALUE, not a failure — a denied run is a decision the desk can
/// show (which actor, refused for what), not an agent that quietly
/// stops mid-task (backlog 7dd9f28c: that happened for real on
/// 2026-09-08, and the only signal was the work stopping).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BudgetDecision {
    /// `remaining_usd_micros` is `None` when no hourly cap is declared:
    /// an unbudgeted agent is admitted with nothing to count down.
    Allow {
        remaining_usd_micros: Option<u64>,
    },
    Deny {
        reason: String,
    },
}

/// The caps an agent's registry row declares — `agents.hourly_budget_
/// usd_micros` and `agents.max_concurrent_runs` (20260915212644), both
/// nullable there and both optional here. `None` is "no cap declared"
/// and admits every run: a missing number must not stop the stack (the
/// boot-guard lesson of 2026-09-07), so unbudgeted is never a refusal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCaps {
    pub hourly_budget_usd_micros: Option<u64>,
    pub max_concurrent_runs: Option<u32>,
}

/// What the ledger measured for an agent at the admission instant: the
/// micro-USD its priced runs spent in the window, and how many of its
/// runs were in flight.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentLoad {
    pub spent_usd_micros: u64,
    pub in_flight: u32,
}

impl BudgetDecision {
    /// The ONE budget rule (§9a): every reader that asks whether an
    /// agent is at its cap calls this, so "at the cap" cannot mean two
    /// things in two places. A pure function of the caps and
    /// the load — it reads no clock and no table — so it is exhaustively
    /// testable and a recorded decision replays without recomputing.
    ///
    /// Spend at or over the cap denies (a cap of zero denies every run:
    /// declared, not absent); in-flight at or over the concurrency cap
    /// denies; a budget denial is reported before a concurrency one,
    /// because the money is what the desk can act on. Otherwise allow,
    /// with the remainder when there is a cap to count down from.
    pub fn decide(caps: AgentCaps, load: AgentLoad) -> BudgetDecision {
        if let Some(cap) = caps.hourly_budget_usd_micros
            && load.spent_usd_micros >= cap
        {
            return BudgetDecision::Deny {
                reason: format!(
                    "hourly budget exhausted: spent {} of {} usd_micros in the last hour",
                    load.spent_usd_micros, cap
                ),
            };
        }
        if let Some(max) = caps.max_concurrent_runs
            && load.in_flight >= max
        {
            return BudgetDecision::Deny {
                reason: format!(
                    "concurrency cap reached: {} of {} runs in flight",
                    load.in_flight, max
                ),
            };
        }
        BudgetDecision::Allow {
            remaining_usd_micros: caps
                .hourly_budget_usd_micros
                .map(|cap| cap - load.spent_usd_micros),
        }
    }

    pub fn is_allowed(&self) -> bool {
        matches!(self, BudgetDecision::Allow { .. })
    }
}

/// Time window for cost queries.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Window {
    LastHour,
    LastDay,
    Since { at: DateTime<Utc> },
}

impl Window {
    /// The instant this window opens, measured back from `at`. Takes
    /// the reference instant rather than reading a clock so a window
    /// can be asked at the moment a run STARTED (the admission instant)
    /// as well as at "now", and so the answer is deterministic.
    pub fn cutoff(self, at: DateTime<Utc>) -> DateTime<Utc> {
        match self {
            Window::LastHour => at - chrono::Duration::hours(1),
            Window::LastDay => at - chrono::Duration::days(1),
            Window::Since { at: since } => since,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_add_saturates_and_sums_fields() {
        let a = Cost {
            tokens: TokenUsage::Split {
                input: 10,
                output: 20,
            },
            usd_micros: Some(500),
        };
        let b = Cost {
            tokens: TokenUsage::Split {
                input: 5,
                output: 7,
            },
            usd_micros: Some(100),
        };
        let c = a + b;
        assert_eq!(c.tokens.input(), Some(15));
        assert_eq!(c.tokens.output(), Some(27));
        assert_eq!(c.usd_micros, Some(600));

        let max = Cost {
            tokens: TokenUsage::Split {
                input: u64::MAX,
                output: 0,
            },
            usd_micros: Some(0),
        };
        assert_eq!((max + a).tokens.input(), Some(u64::MAX));
    }

    #[test]
    fn an_unpriced_cost_poisons_a_sum_rather_than_adding_zero() {
        // The money is optional and the tokens are not: a run whose
        // split was reported but whose price nothing on the card could
        // supply still contributes its tokens, and takes the total's
        // money to unknown. A sum that quietly dropped it would read as
        // a measurement — the defect 65c9c05a closed one layer down,
        // which could not reach this type until it grew the room to say
        // so (backlog c6e2341c).
        let priced = Cost {
            tokens: TokenUsage::Split {
                input: 10,
                output: 20,
            },
            usd_micros: Some(500),
        };
        let unpriced = Cost {
            tokens: TokenUsage::Split {
                input: 5,
                output: 7,
            },
            usd_micros: None,
        };
        let sum = priced + unpriced;
        assert_eq!(sum.tokens.input(), Some(15));
        assert_eq!(sum.tokens.output(), Some(27));
        assert_eq!(sum.usd_micros, None, "unpriced is not free");
        assert_eq!(
            (unpriced + priced).usd_micros,
            None,
            "and the order the two arrive in cannot change that"
        );
        // A measured zero is still a measurement, and stays one.
        assert_eq!(Cost::ZERO.usd_micros, Some(0));
        assert_eq!((Cost::ZERO + priced).usd_micros, Some(500));
        assert_eq!(Cost::default(), Cost::ZERO);
    }

    #[test]
    fn an_unpriced_cost_writes_an_explicit_null() {
        // A reader must be able to tell "no price" from "this payload
        // is from before the field existed", so the field is written
        // even when it is null (the same distinction 2e4c200f drew for
        // the agent-runs serializer).
        let v = serde_json::to_value(Cost::UNPRICED).unwrap();
        assert_eq!(
            v.get("usd_micros"),
            Some(&serde_json::Value::Null),
            "the key is present and null"
        );
        assert_eq!(
            serde_json::to_value(Cost::ZERO).unwrap()["usd_micros"],
            serde_json::json!(0)
        );
        // And an older payload that never carried the key reads back as
        // unknown rather than as free.
        let back: Cost =
            serde_json::from_value(serde_json::json!({"input_tokens": 1, "output_tokens": 2}))
                .unwrap();
        assert_eq!(back.usd_micros, None);
        assert_eq!(
            back.tokens,
            TokenUsage::Split {
                input: 1,
                output: 2
            },
            "a payload written before the token side was a shape still reads as the split it is"
        );
    }

    #[test]
    fn a_blended_total_is_a_shape_this_type_can_hold() {
        // Backlog e059a754: a priced total-only run had nowhere to go
        // in this type, so `AgentRun::cost()` answered `None` for a run
        // the record HAD priced, and a consumer reading `usd_micros`
        // directly disagreed with one coming through here.
        let blended = Cost {
            tokens: TokenUsage::TotalOnly { total: 142_982 },
            usd_micros: Some(2_500_000),
        };
        assert_eq!(blended.tokens.total(), Some(142_982));
        assert_eq!(
            blended.tokens.input(),
            None,
            "the division between the halves is exactly what was not measured"
        );
        let v = serde_json::to_value(blended).unwrap();
        assert_eq!(v["total_tokens"], serde_json::json!(142_982));
        assert_eq!(v["input_tokens"], serde_json::Value::Null);
        assert_eq!(
            serde_json::from_value::<Cost>(v).unwrap(),
            blended,
            "and it round-trips, so a recorded outcome replays as what it was"
        );
    }

    #[test]
    fn a_sum_is_only_as_measured_as_its_least_measured_term() {
        // The rule the money half has followed since c6e2341c, applied
        // to the counts (backlog e059a754). Mixing a split with a bare
        // total leaves a total: the sum's own split is not known.
        let split = Cost {
            tokens: TokenUsage::Split {
                input: 10,
                output: 20,
            },
            usd_micros: Some(500),
        };
        let blended = Cost {
            tokens: TokenUsage::TotalOnly { total: 5 },
            usd_micros: Some(100),
        };
        assert_eq!(
            (split + blended).tokens,
            TokenUsage::TotalOnly { total: 35 }
        );
        assert_eq!((split + blended).usd_micros, Some(600));
        // An uncounted run takes the window's token total to unknown,
        // the same way an unpriced one takes its money there.
        let silent = Cost {
            tokens: TokenUsage::Unreported,
            usd_micros: Some(7),
        };
        assert_eq!((split + silent).tokens, TokenUsage::Unreported);
        assert_eq!((silent + blended).tokens, TokenUsage::Unreported);
        // ZERO is still the identity a fold starts from, whatever
        // shape it meets.
        for c in [split, blended, silent, Cost::UNPRICED] {
            assert_eq!((Cost::ZERO + c).tokens, c.tokens, "ZERO adds nothing");
            assert_eq!((c + Cost::ZERO).tokens, c.tokens, "from either side");
        }
    }

    #[test]
    fn budget_decision_is_allowed() {
        assert!(
            BudgetDecision::Allow {
                remaining_usd_micros: Some(100)
            }
            .is_allowed()
        );
        assert!(
            !BudgetDecision::Deny {
                reason: "cap".into()
            }
            .is_allowed()
        );
    }

    #[test]
    fn budget_decision_round_trips_serde() {
        let d = BudgetDecision::Allow {
            remaining_usd_micros: Some(42),
        };
        let json = serde_json::to_string(&d).unwrap();
        let back: BudgetDecision = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }

    // -- the one decision function, every case (backlog 7dd9f28c) ------

    fn capped(cap: u64) -> AgentCaps {
        AgentCaps {
            hourly_budget_usd_micros: Some(cap),
            max_concurrent_runs: None,
        }
    }

    fn spent(usd: u64) -> AgentLoad {
        AgentLoad {
            spent_usd_micros: usd,
            in_flight: 0,
        }
    }

    #[test]
    fn under_cap_allows_with_the_remainder() {
        assert_eq!(
            BudgetDecision::decide(capped(1_000), spent(300)),
            BudgetDecision::Allow {
                remaining_usd_micros: Some(700)
            }
        );
    }

    #[test]
    fn no_prior_spend_allows_the_whole_cap() {
        assert_eq!(
            BudgetDecision::decide(capped(1_000), spent(0)),
            BudgetDecision::Allow {
                remaining_usd_micros: Some(1_000)
            }
        );
    }

    #[test]
    fn at_cap_denies_naming_spend_and_cap() {
        let d = BudgetDecision::decide(capped(1_000), spent(1_000));
        let BudgetDecision::Deny { reason } = d else {
            panic!("at the cap there is nothing left to spend: {d:?}");
        };
        assert!(reason.contains("1000 of 1000"), "{reason}");
        assert!(reason.contains("hourly"), "{reason}");
    }

    #[test]
    fn over_cap_denies_and_never_underflows() {
        let d = BudgetDecision::decide(capped(1_000), spent(1_500));
        assert!(matches!(d, BudgetDecision::Deny { .. }), "{d:?}");
    }

    #[test]
    fn a_zero_cap_denies_every_run() {
        // A cap of zero is a declared cap, not an absent one: the agent
        // is switched off, and the reason says so.
        let d = BudgetDecision::decide(capped(0), spent(0));
        assert!(matches!(d, BudgetDecision::Deny { .. }), "{d:?}");
    }

    #[test]
    fn no_cap_allows_with_no_remainder_whatever_was_spent() {
        // NULL caps on the registry row are "unbudgeted", never a
        // refusal: a missing number must not stop the stack.
        let d = BudgetDecision::decide(AgentCaps::default(), spent(u64::MAX));
        assert_eq!(
            d,
            BudgetDecision::Allow {
                remaining_usd_micros: None
            }
        );
    }

    #[test]
    fn under_the_concurrency_cap_allows() {
        let caps = AgentCaps {
            hourly_budget_usd_micros: None,
            max_concurrent_runs: Some(2),
        };
        let d = BudgetDecision::decide(
            caps,
            AgentLoad {
                spent_usd_micros: 0,
                in_flight: 1,
            },
        );
        assert!(d.is_allowed(), "{d:?}");
    }

    #[test]
    fn at_the_concurrency_cap_denies_naming_the_count() {
        let caps = AgentCaps {
            hourly_budget_usd_micros: None,
            max_concurrent_runs: Some(2),
        };
        let d = BudgetDecision::decide(
            caps,
            AgentLoad {
                spent_usd_micros: 0,
                in_flight: 2,
            },
        );
        let BudgetDecision::Deny { reason } = d else {
            panic!("two in flight of two is full: {d:?}");
        };
        assert!(reason.contains("2 of 2"), "{reason}");
        assert!(reason.contains("in flight"), "{reason}");
    }

    #[test]
    fn a_budget_denial_is_reported_before_a_concurrency_one() {
        // Both exhausted: the reason names the money, because the
        // money is what the desk can do something about.
        let caps = AgentCaps {
            hourly_budget_usd_micros: Some(10),
            max_concurrent_runs: Some(1),
        };
        let d = BudgetDecision::decide(
            caps,
            AgentLoad {
                spent_usd_micros: 10,
                in_flight: 1,
            },
        );
        let BudgetDecision::Deny { reason } = d else {
            panic!("{d:?}");
        };
        assert!(reason.contains("hourly"), "{reason}");
    }

    #[test]
    fn a_window_cuts_off_relative_to_the_instant_it_is_asked_at() {
        let at: DateTime<Utc> = "2026-09-15T12:00:00Z".parse().unwrap();
        assert_eq!(
            Window::LastHour.cutoff(at),
            "2026-09-15T11:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        assert_eq!(
            Window::LastDay.cutoff(at),
            "2026-09-14T12:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        let since: DateTime<Utc> = "2026-09-01T00:00:00Z".parse().unwrap();
        assert_eq!(Window::Since { at: since }.cutoff(at), since);
    }
}
