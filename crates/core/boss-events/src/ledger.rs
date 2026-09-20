//! In-memory [`CostLedger`] adapter.

use async_trait::async_trait;
use boss_core::agent::{AgentId, AgentLoad, AgentSpec, BudgetDecision, Cost, Window};
use boss_core::port::{CostLedger, LedgerError};
use chrono::{DateTime, Utc};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone)]
struct Entry {
    agent: AgentId,
    at: DateTime<Utc>,
    cost: Cost,
}

#[derive(Default)]
struct Inner {
    entries: Vec<Entry>,
}

#[derive(Clone, Default)]
pub struct InMemoryCostLedger {
    inner: Arc<Mutex<Inner>>,
}

impl InMemoryCostLedger {
    pub fn new() -> Self {
        Self::default()
    }

    fn cutoff(window: Window) -> DateTime<Utc> {
        window.cutoff(Utc::now())
    }

    /// The window's spend that is actually known: unpriced entries add
    /// nothing, because they have no number to add. See `check_budget`
    /// for why the desk reads this rather than the honest total.
    async fn priced_floor(&self, agent: &AgentId, window: Window) -> u64 {
        let cutoff = Self::cutoff(window);
        let inner = self.inner.lock().await;
        inner
            .entries
            .iter()
            .filter(|e| &e.agent == agent && e.at >= cutoff)
            .filter_map(|e| e.cost.usd_micros)
            .fold(0u64, u64::saturating_add)
    }
}

#[async_trait]
impl CostLedger for InMemoryCostLedger {
    async fn record(&self, agent: &AgentId, cost: Cost) -> Result<(), LedgerError> {
        let mut inner = self.inner.lock().await;
        inner.entries.push(Entry {
            agent: agent.clone(),
            at: Utc::now(),
            cost,
        });
        Ok(())
    }

    async fn spent(&self, agent: &AgentId, window: Window) -> Result<Cost, LedgerError> {
        let cutoff = Self::cutoff(window);
        let inner = self.inner.lock().await;
        let total = inner
            .entries
            .iter()
            .filter(|e| &e.agent == agent && e.at >= cutoff)
            .fold(Cost::ZERO, |acc, e| acc + e.cost);
        Ok(total)
    }

    async fn vm_spent(&self, window: Window) -> Result<Cost, LedgerError> {
        let cutoff = Self::cutoff(window);
        let inner = self.inner.lock().await;
        let total = inner
            .entries
            .iter()
            .filter(|e| e.at >= cutoff)
            .fold(Cost::ZERO, |acc, e| acc + e.cost);
        Ok(total)
    }

    async fn check_budget(
        &self,
        agent: &AgentId,
        spec: &AgentSpec,
    ) -> Result<BudgetDecision, LedgerError> {
        // The one budget rule lives in boss-core (backlog 7dd9f28c):
        // this ledger measures the spend, `decide` judges it. The
        // concurrency half is the dispatcher's to count, so it is not
        // consulted here.
        // NOT `spent().usd_micros`: one unpriced entry takes that
        // whole window's total to unknown (backlog c6e2341c), and the
        // desk still has to decide. It counts the PRICED FLOOR — the
        // spend it can prove — because a missing number must not stop
        // the stack (the `AgentCaps` note, 2026-09-07) and a floor only
        // ever under-counts, so it never denies a run the full number
        // would have allowed. The same rule
        // `boss_jobs::agent_budget::spent_in` applies at the other desk.
        let spent_usd_micros = self.priced_floor(agent, Window::LastHour).await;
        Ok(BudgetDecision::decide(
            boss_core::agent::AgentCaps {
                max_concurrent_runs: None,
                ..spec.caps()
            },
            AgentLoad {
                spent_usd_micros,
                in_flight: 0,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(slug: &str) -> AgentId {
        AgentId::try_new(slug).unwrap()
    }

    fn spec(a: &AgentId, cap: u64) -> AgentSpec {
        AgentSpec {
            id: a.clone(),
            display_name: a.to_string(),
            system_prompt: String::new(),
            model: "test".into(),
            hourly_budget_usd_micros: cap,
            max_concurrent_runs: 1,
        }
    }

    fn cost(usd: u64) -> Cost {
        Cost {
            input_tokens: 0,
            output_tokens: 0,
            usd_micros: Some(usd),
        }
    }

    #[tokio::test]
    async fn empty_ledger_returns_zero() {
        let l = InMemoryCostLedger::new();
        let a = agent("planner");
        assert_eq!(l.spent(&a, Window::LastHour).await.unwrap(), Cost::ZERO);
        assert_eq!(l.vm_spent(Window::LastHour).await.unwrap(), Cost::ZERO);
    }

    #[tokio::test]
    async fn an_unpriced_entry_makes_the_window_unknown_and_still_admits() {
        // backlog c6e2341c: the window's TOTAL goes unknown, because an
        // entry with no number cannot be added; the admission desk
        // still decides, on the spend it can prove.
        let l = InMemoryCostLedger::new();
        let a = agent("planner");
        l.record(&a, cost(100)).await.unwrap();
        l.record(
            &a,
            Cost {
                input_tokens: 5,
                output_tokens: 5,
                usd_micros: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(
            l.spent(&a, Window::LastHour).await.unwrap().usd_micros,
            None,
            "unpriced is not free"
        );
        // The tokens are known on both entries and still add up.
        assert_eq!(l.spent(&a, Window::LastHour).await.unwrap().input_tokens, 5);
        assert!(
            l.check_budget(&a, &spec(&a, 150))
                .await
                .unwrap()
                .is_allowed(),
            "a proven floor of 100 is under a cap of 150"
        );
        assert!(
            !l.check_budget(&a, &spec(&a, 100))
                .await
                .unwrap()
                .is_allowed(),
            "and the proven 100 still denies at the cap"
        );
    }

    #[tokio::test]
    async fn spent_sums_only_matching_agent_within_window() {
        let l = InMemoryCostLedger::new();
        let a = agent("planner");
        let b = agent("doctor");
        l.record(&a, cost(100)).await.unwrap();
        l.record(&a, cost(50)).await.unwrap();
        l.record(&b, cost(200)).await.unwrap();

        assert_eq!(
            l.spent(&a, Window::LastHour).await.unwrap().usd_micros,
            Some(150)
        );
        assert_eq!(
            l.vm_spent(Window::LastHour).await.unwrap().usd_micros,
            Some(350)
        );
    }

    #[tokio::test]
    async fn budget_allow_returns_remaining() {
        let l = InMemoryCostLedger::new();
        let a = agent("planner");
        let s = spec(&a, 1_000);
        l.record(&a, cost(300)).await.unwrap();
        let d = l.check_budget(&a, &s).await.unwrap();
        assert_eq!(
            d,
            BudgetDecision::Allow {
                remaining_usd_micros: Some(700)
            }
        );
    }

    #[tokio::test]
    async fn budget_denies_when_exhausted() {
        let l = InMemoryCostLedger::new();
        let a = agent("planner");
        let s = spec(&a, 1_000);
        l.record(&a, cost(1_000)).await.unwrap();
        let d = l.check_budget(&a, &s).await.unwrap();
        assert!(matches!(d, BudgetDecision::Deny { .. }));
    }

    #[tokio::test]
    async fn window_since_filters_older_entries() {
        let l = InMemoryCostLedger::new();
        let a = agent("planner");
        l.record(&a, cost(10)).await.unwrap();
        let midpoint = Utc::now();
        // Small gap so the second record is strictly after the cutoff.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        l.record(&a, cost(20)).await.unwrap();

        let recent = l.spent(&a, Window::Since { at: midpoint }).await.unwrap();
        assert_eq!(recent.usd_micros, Some(20));
    }
}
