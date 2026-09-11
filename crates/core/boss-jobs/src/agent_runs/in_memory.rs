//! In-memory `AgentRunLog` — the port-level test double.
//!
//! Mirrors the Postgres semantics that matter: the insert collapses on
//! a duplicate `run_id` and returns the run already held, pricing runs
//! off the same [`price_run`] the Pg adapter calls, and listing newest
//! finish first. Events are collected rather than delivered, so a test
//! can assert that recording a run put a fact on the log.

use std::collections::HashMap;

use async_trait::async_trait;
use boss_core::actor::ActorId;
use boss_core::event::Event;
use tokio::sync::RwLock;

use super::port::{AgentRunError, AgentRunLog, RecordedRun, validate};
use super::types::{AgentRun, NewAgentRun, RateCardRow, RunFilter, price_run};

pub struct InMemoryAgentRuns {
    card: Vec<RateCardRow>,
    runs: RwLock<HashMap<String, AgentRun>>,
    events: RwLock<Vec<Event>>,
}

impl InMemoryAgentRuns {
    pub fn new(card: Vec<RateCardRow>) -> Self {
        Self {
            card,
            runs: RwLock::new(HashMap::new()),
            events: RwLock::new(Vec::new()),
        }
    }

    /// Every event this log recorded, in order.
    pub async fn recorded_events(&self) -> Vec<Event> {
        self.events.read().await.clone()
    }
}

#[async_trait]
impl AgentRunLog for InMemoryAgentRuns {
    async fn record_run(
        &self,
        run: &NewAgentRun,
        recorded_by: &ActorId,
    ) -> Result<RecordedRun, AgentRunError> {
        validate(run)?;
        let mut guard = self.runs.write().await;
        if let Some(held) = guard.get(&run.run_id) {
            return Ok(RecordedRun {
                recorded: false,
                run: held.clone(),
            });
        }
        let priced = price_run(&self.card, run);
        let event = super::events::run_recorded_event(recorded_by, run, &priced);
        let recorded = AgentRun {
            run: run.clone(),
            usd_micros: priced.as_ref().map(|(m, _)| *m),
            priced_by: priced.map(|(_, m)| m),
            // The row and the record share ONE instant: the event's.
            recorded_at: event.timestamp,
        };
        guard.insert(run.run_id.clone(), recorded.clone());
        self.events.write().await.push(event);
        Ok(RecordedRun {
            recorded: true,
            run: recorded,
        })
    }

    async fn list_runs(&self, filter: &RunFilter) -> Result<Vec<AgentRun>, AgentRunError> {
        let guard = self.runs.read().await;
        let mut out: Vec<AgentRun> = guard
            .values()
            .filter(|r| filter.job_id.is_none_or(|id| r.run.job_id == Some(id)))
            .filter(|r| {
                filter
                    .branch
                    .as_deref()
                    .is_none_or(|b| r.run.branch.as_deref() == Some(b))
            })
            .filter(|r| {
                filter
                    .actor_id
                    .as_deref()
                    .is_none_or(|a| r.run.actor_id.to_string() == a)
            })
            .filter(|r| filter.since.is_none_or(|t| r.run.finished_at >= t))
            .cloned()
            .collect();
        // Newest finish first, `run_id` breaking ties so the order is
        // total — an arbitrary tiebreak is what made a LIMIT cut a busy
        // window at an unpredictable point (#262).
        out.sort_by(|a, b| {
            b.run
                .finished_at
                .cmp(&a.run.finished_at)
                .then_with(|| a.run.run_id.cmp(&b.run.run_id))
        });
        if let Some(limit) = filter.limit {
            out.truncate(usize::try_from(limit.max(0)).unwrap_or(usize::MAX));
        }
        Ok(out)
    }

    async fn rate_card(&self) -> Result<Vec<RateCardRow>, AgentRunError> {
        let mut out = self.card.clone();
        out.sort_by(|a, b| a.model.cmp(&b.model));
        Ok(out)
    }
}
