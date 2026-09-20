//! Domain types for the Cybernetics agent stack.
//!
//! These types describe what flows through the system — messages to agents,
//! claims on those messages, costs, runs, budgets, and registry entries.
//! They have no behavior beyond construction and serialization; behavior lives
//! in adapters that implement the ports in [`crate::port`].

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::define_id;

define_id!(MessageId);
define_id!(RunId);
define_id!(ClaimId);

/// Stable, slug-based identifier for an agent on a VM.
///
/// Slugs are lowercase kebab-case: `[a-z][a-z0-9-]*`, 1..=64 chars, must not
/// start or end with a hyphen and must not contain consecutive hyphens.
/// Validation is enforced at construction.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct AgentId(String);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AgentIdError {
    #[error("agent id is empty")]
    Empty,
    #[error("agent id is longer than 64 characters")]
    TooLong,
    #[error("agent id must start with a lowercase letter")]
    BadStart,
    #[error("agent id must end with a lowercase letter or digit")]
    BadEnd,
    #[error("agent id contains invalid character '{0}'")]
    BadChar(char),
    #[error("agent id contains consecutive hyphens")]
    DoubleHyphen,
}

impl AgentId {
    pub fn try_new(s: impl Into<String>) -> Result<Self, AgentIdError> {
        let s: String = s.into();
        if s.is_empty() {
            return Err(AgentIdError::Empty);
        }
        if s.len() > 64 {
            return Err(AgentIdError::TooLong);
        }
        let bytes = s.as_bytes();
        let first = bytes[0] as char;
        if !first.is_ascii_lowercase() {
            return Err(AgentIdError::BadStart);
        }
        let last = bytes[bytes.len() - 1] as char;
        if !(last.is_ascii_lowercase() || last.is_ascii_digit()) {
            return Err(AgentIdError::BadEnd);
        }
        let mut prev_hyphen = false;
        for c in s.chars() {
            let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-';
            if !ok {
                return Err(AgentIdError::BadChar(c));
            }
            if c == '-' {
                if prev_hyphen {
                    return Err(AgentIdError::DoubleHyphen);
                }
                prev_hyphen = true;
            } else {
                prev_hyphen = false;
            }
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<String> for AgentId {
    type Error = AgentIdError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::try_new(s)
    }
}

impl From<AgentId> for String {
    fn from(id: AgentId) -> Self {
        id.0
    }
}

/// A message destined for an agent. Immutable once constructed.
///
/// Messages enter Cybernetics via the event bus (NATS) and are persisted
/// to the per-agent inbox (`MessageQueue`) before dispatch.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub id: MessageId,
    pub timestamp: DateTime<Utc>,
    pub target: AgentId,
    /// Dot-separated kind (e.g. `"work.plan-feature"`).
    pub kind: String,
    pub payload: serde_json::Value,
    /// Optional NATS subject for replies.
    pub reply_to: Option<String>,
    /// Correlation id for tracing a chain of messages.
    pub correlation_id: Option<Uuid>,
}

impl Message {
    pub fn new(target: AgentId, kind: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            id: MessageId::new(),
            timestamp: Utc::now(),
            target,
            kind: kind.into(),
            payload,
            reply_to: None,
            correlation_id: None,
        }
    }

    pub fn with_reply_to(mut self, subject: impl Into<String>) -> Self {
        self.reply_to = Some(subject.into());
        self
    }

    pub fn with_correlation(mut self, id: Uuid) -> Self {
        self.correlation_id = Some(id);
        self
    }
}

/// A message pulled from a queue and assigned to a dispatcher.
///
/// Holds a `claim_id` that the dispatcher must present to `ack`/`nack` the
/// message after the run completes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimedMessage {
    pub claim_id: ClaimId,
    pub message: Message,
    pub claimed_at: DateTime<Utc>,
    pub attempt: u32,
}

/// Cost incurred by an agent run. All monetary values in micro-USD
/// (1_000_000 = $1.00) to avoid floating point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    pub input_tokens: u64,
    pub output_tokens: u64,
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
        input_tokens: 0,
        output_tokens: 0,
        usd_micros: Some(0),
    };

    /// A run that reported tokens but no price. Distinct from [`ZERO`]
    /// in exactly the way this type exists to express.
    ///
    /// [`ZERO`]: Cost::ZERO
    pub const UNPRICED: Cost = Cost {
        input_tokens: 0,
        output_tokens: 0,
        usd_micros: None,
    };

    /// Tokens always add; money adds only when BOTH sides know their
    /// own. One unpriced run takes the total to unknown, because a sum
    /// that silently skipped it would read as a measurement of every
    /// run in the window — the rule the agent-runs roll-up applies to
    /// `usd_micros` and `total_tokens` one layer down.
    pub fn saturating_sum(self, other: Cost) -> Cost {
        Cost {
            input_tokens: self.input_tokens.saturating_add(other.input_tokens),
            output_tokens: self.output_tokens.saturating_add(other.output_tokens),
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

/// Outcome reported by an agent after a dispatch completes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outcome {
    Success {
        cost: Cost,
        response: serde_json::Value,
    },
    Failed {
        cost: Cost,
        error: String,
    },
    Cancelled,
}

impl Outcome {
    pub fn cost(&self) -> Cost {
        match self {
            Outcome::Success { cost, .. } | Outcome::Failed { cost, .. } => *cost,
            Outcome::Cancelled => Cost::ZERO,
        }
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
    /// The ONE budget rule (§9a): the cybernetics ledger and the
    /// jobs-API run recorder both call this, so "at the cap" cannot
    /// mean two things in two places. A pure function of the caps and
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

/// Static per-agent configuration held by the registry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentSpec {
    pub id: AgentId,
    pub display_name: String,
    pub system_prompt: String,
    pub model: String,
    /// Hard hourly cap; runs are denied if recording would exceed this.
    pub hourly_budget_usd_micros: u64,
    /// Max in-flight runs for this agent on this VM.
    pub max_concurrent_runs: u32,
}

impl AgentSpec {
    /// The spec's caps in the registry row's shape. A TOML-configured
    /// spec always declares both, so both are `Some`.
    pub fn caps(&self) -> AgentCaps {
        AgentCaps {
            hourly_budget_usd_micros: Some(self.hourly_budget_usd_micros),
            max_concurrent_runs: Some(self.max_concurrent_runs),
        }
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

/// Lifecycle status of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Starting,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// Handle to an in-flight or finished agent run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunHandle {
    pub run_id: RunId,
    pub agent: AgentId,
    pub message_id: MessageId,
    pub claim_id: ClaimId,
    pub started_at: DateTime<Utc>,
    pub status: RunStatus,
}

/// Notification that a dispatched run reached a terminal state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunCompletion {
    pub run: RunHandle,
    pub outcome: Outcome,
    pub finished_at: DateTime<Utc>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_id_accepts_slug() {
        let id = AgentId::try_new("planner").unwrap();
        assert_eq!(id.as_str(), "planner");
        assert_eq!(id.to_string(), "planner");
    }

    #[test]
    fn agent_id_accepts_hyphenated_slug_with_digits() {
        AgentId::try_new("code-reviewer-2").unwrap();
    }

    #[test]
    fn agent_id_rejects_empty() {
        assert_eq!(AgentId::try_new(""), Err(AgentIdError::Empty));
    }

    #[test]
    fn agent_id_rejects_uppercase() {
        assert!(matches!(
            AgentId::try_new("Planner"),
            Err(AgentIdError::BadStart)
        ));
    }

    #[test]
    fn agent_id_rejects_starting_digit() {
        assert!(matches!(
            AgentId::try_new("1planner"),
            Err(AgentIdError::BadStart)
        ));
    }

    #[test]
    fn agent_id_rejects_trailing_hyphen() {
        assert!(matches!(
            AgentId::try_new("planner-"),
            Err(AgentIdError::BadEnd)
        ));
    }

    #[test]
    fn agent_id_rejects_double_hyphen() {
        assert_eq!(
            AgentId::try_new("plan--ner"),
            Err(AgentIdError::DoubleHyphen)
        );
    }

    #[test]
    fn agent_id_rejects_invalid_char() {
        assert_eq!(
            AgentId::try_new("plan_ner"),
            Err(AgentIdError::BadChar('_'))
        );
    }

    #[test]
    fn agent_id_rejects_too_long() {
        let s: String = "a".repeat(65);
        assert_eq!(AgentId::try_new(s), Err(AgentIdError::TooLong));
    }

    #[test]
    fn agent_id_serde_round_trips_as_string() {
        let id = AgentId::try_new("planner").unwrap();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "\"planner\"");
        let back: AgentId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }

    #[test]
    fn agent_id_serde_rejects_invalid_string() {
        let bad = "\"Planner\"";
        assert!(serde_json::from_str::<AgentId>(bad).is_err());
    }

    #[test]
    fn message_builder_defaults() {
        let agent = AgentId::try_new("planner").unwrap();
        let msg = Message::new(agent.clone(), "work.plan", serde_json::json!({"x": 1}));
        assert_eq!(msg.target, agent);
        assert_eq!(msg.kind, "work.plan");
        assert!(msg.reply_to.is_none());
        assert!(msg.correlation_id.is_none());
    }

    #[test]
    fn message_with_reply_to_and_correlation() {
        let agent = AgentId::try_new("planner").unwrap();
        let corr = Uuid::new_v4();
        let msg = Message::new(agent, "k", serde_json::json!({}))
            .with_reply_to("boss.s1.vm1.planner.out.done")
            .with_correlation(corr);
        assert_eq!(
            msg.reply_to.as_deref(),
            Some("boss.s1.vm1.planner.out.done")
        );
        assert_eq!(msg.correlation_id, Some(corr));
    }

    #[test]
    fn message_round_trips_serde() {
        let agent = AgentId::try_new("planner").unwrap();
        let msg = Message::new(agent, "work.plan", serde_json::json!({"x": 1}));
        let json = serde_json::to_string(&msg).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn cost_add_saturates_and_sums_fields() {
        let a = Cost {
            input_tokens: 10,
            output_tokens: 20,
            usd_micros: Some(500),
        };
        let b = Cost {
            input_tokens: 5,
            output_tokens: 7,
            usd_micros: Some(100),
        };
        let c = a + b;
        assert_eq!(c.input_tokens, 15);
        assert_eq!(c.output_tokens, 27);
        assert_eq!(c.usd_micros, Some(600));

        let max = Cost {
            input_tokens: u64::MAX,
            output_tokens: 0,
            usd_micros: Some(0),
        };
        assert_eq!((max + a).input_tokens, u64::MAX);
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
            input_tokens: 10,
            output_tokens: 20,
            usd_micros: Some(500),
        };
        let unpriced = Cost {
            input_tokens: 5,
            output_tokens: 7,
            usd_micros: None,
        };
        let sum = priced + unpriced;
        assert_eq!(sum.input_tokens, 15);
        assert_eq!(sum.output_tokens, 27);
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
    }

    #[test]
    fn outcome_cost_returns_zero_for_cancelled() {
        assert_eq!(Outcome::Cancelled.cost(), Cost::ZERO);
        let c = Cost {
            input_tokens: 1,
            output_tokens: 2,
            usd_micros: Some(3),
        };
        assert_eq!(
            Outcome::Success {
                cost: c,
                response: serde_json::json!({})
            }
            .cost(),
            c
        );
        assert_eq!(
            Outcome::Failed {
                cost: c,
                error: "boom".into()
            }
            .cost(),
            c
        );
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
    fn a_spec_derives_its_caps_as_declared() {
        let spec = AgentSpec {
            id: AgentId::try_new("planner").unwrap(),
            display_name: "p".into(),
            system_prompt: String::new(),
            model: "m".into(),
            hourly_budget_usd_micros: 5,
            max_concurrent_runs: 3,
        };
        assert_eq!(
            spec.caps(),
            AgentCaps {
                hourly_budget_usd_micros: Some(5),
                max_concurrent_runs: Some(3),
            }
        );
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
