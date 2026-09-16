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
        let spent = self.spent(agent, Window::LastHour).await?;
        Ok(BudgetDecision::decide(
            boss_core::agent::AgentCaps {
                max_concurrent_runs: None,
                ..spec.caps()
            },
            AgentLoad {
                spent_usd_micros: spent.usd_micros,
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
            usd_micros: usd,
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
    async fn spent_sums_only_matching_agent_within_window() {
        let l = InMemoryCostLedger::new();
        let a = agent("planner");
        let b = agent("doctor");
        l.record(&a, cost(100)).await.unwrap();
        l.record(&a, cost(50)).await.unwrap();
        l.record(&b, cost(200)).await.unwrap();

        assert_eq!(l.spent(&a, Window::LastHour).await.unwrap().usd_micros, 150);
        assert_eq!(l.vm_spent(Window::LastHour).await.unwrap().usd_micros, 350);
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
        assert_eq!(recent.usd_micros, 20);
    }
}
