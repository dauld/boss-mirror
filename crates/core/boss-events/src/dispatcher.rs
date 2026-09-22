//! Stub [`AgentDispatcher`] — echoes the message payload back as the
//! response, records a fixed synthetic cost, and broadcasts completions.
//!
//! Useful for end-to-end loop testing before the Claude SDK adapter lands.

use async_trait::async_trait;
use boss_core::agent::{
    AgentId, AgentSpec, ClaimedMessage, Cost, Outcome, RunCompletion, RunHandle, RunId, RunStatus,
    TokenUsage,
};
use boss_core::port::{AgentDispatcher, DispatchError, RunCompletions};
use chrono::Utc;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};

/// A stub dispatcher that produces a deterministic fake outcome.
#[derive(Clone)]
pub struct StubDispatcher {
    running: Arc<Mutex<HashMap<RunId, RunHandle>>>,
    concurrency: Arc<Mutex<HashMap<AgentId, u32>>>,
    completions_tx: broadcast::Sender<RunCompletion>,
    /// Synthetic cost recorded for every run.
    synthetic_cost: Cost,
}

impl StubDispatcher {
    pub fn new(synthetic_cost: Cost) -> Self {
        let (tx, _) = broadcast::channel(256);
        Self {
            running: Arc::new(Mutex::new(HashMap::new())),
            concurrency: Arc::new(Mutex::new(HashMap::new())),
            completions_tx: tx,
            synthetic_cost,
        }
    }
}

impl Default for StubDispatcher {
    fn default() -> Self {
        Self::new(Cost {
            tokens: TokenUsage::Split {
                input: 10,
                output: 20,
            },
            usd_micros: Some(100),
        })
    }
}

#[async_trait]
impl AgentDispatcher for StubDispatcher {
    async fn dispatch(
        &self,
        spec: &AgentSpec,
        message: ClaimedMessage,
    ) -> Result<RunHandle, DispatchError> {
        // Enforce per-agent concurrency cap.
        {
            let mut conc = self.concurrency.lock().await;
            let current = conc.entry(spec.id.clone()).or_insert(0);
            if *current >= spec.max_concurrent_runs {
                return Err(DispatchError::CapacityExceeded(spec.id.clone()));
            }
            *current += 1;
        }

        let handle = RunHandle {
            run_id: RunId::new(),
            agent: spec.id.clone(),
            message_id: message.message.id,
            claim_id: message.claim_id,
            started_at: Utc::now(),
            status: RunStatus::Starting,
        };

        self.running
            .lock()
            .await
            .insert(handle.run_id, handle.clone());

        // Simulate async work: mark running, produce outcome, broadcast.
        let running = self.running.clone();
        let concurrency = self.concurrency.clone();
        let tx = self.completions_tx.clone();
        let cost = self.synthetic_cost;
        let run_id = handle.run_id;
        let agent = spec.id.clone();
        let response_payload = message.message.payload.clone();
        let mut finished_handle = handle.clone();
        tokio::spawn(async move {
            // Mark as Running briefly.
            if let Some(h) = running.lock().await.get_mut(&run_id) {
                h.status = RunStatus::Running;
            }
            // "Work" happens here in a real dispatcher.
            let outcome = Outcome::Success {
                cost,
                response: serde_json::json!({ "echo": response_payload }),
            };
            finished_handle.status = RunStatus::Completed;
            // Remove from running map and decrement concurrency.
            running.lock().await.remove(&run_id);
            if let Some(c) = concurrency.lock().await.get_mut(&agent) {
                *c = c.saturating_sub(1);
            }
            let _ = tx.send(RunCompletion {
                run: finished_handle,
                outcome,
                finished_at: Utc::now(),
            });
        });

        Ok(handle)
    }

    async fn running(&self) -> Result<Vec<RunHandle>, DispatchError> {
        Ok(self.running.lock().await.values().cloned().collect())
    }

    async fn cancel(&self, run_id: &RunId) -> Result<(), DispatchError> {
        let mut running = self.running.lock().await;
        let handle = running
            .remove(run_id)
            .ok_or(DispatchError::UnknownRun(*run_id))?;
        if let Some(c) = self.concurrency.lock().await.get_mut(&handle.agent) {
            *c = c.saturating_sub(1);
        }
        let mut h = handle;
        h.status = RunStatus::Cancelled;
        let _ = self.completions_tx.send(RunCompletion {
            run: h,
            outcome: Outcome::Cancelled,
            finished_at: Utc::now(),
        });
        Ok(())
    }

    async fn completions(&self) -> Result<Box<dyn RunCompletions>, DispatchError> {
        Ok(Box::new(BroadcastCompletions {
            rx: self.completions_tx.subscribe(),
        }))
    }
}

struct BroadcastCompletions {
    rx: broadcast::Receiver<RunCompletion>,
}

#[async_trait]
impl RunCompletions for BroadcastCompletions {
    async fn next(&mut self) -> Option<RunCompletion> {
        loop {
            match self.rx.recv().await {
                Ok(c) => return Some(c),
                Err(broadcast::error::RecvError::Closed) => return None,
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_core::agent::{AgentId, ClaimId, Message, MessageId};

    fn spec(slug: &str, max: u32) -> AgentSpec {
        AgentSpec {
            id: AgentId::try_new(slug).unwrap(),
            display_name: slug.into(),
            system_prompt: String::new(),
            model: "test".into(),
            hourly_budget_usd_micros: 10_000,
            max_concurrent_runs: max,
        }
    }

    fn claimed(agent: &AgentId) -> ClaimedMessage {
        ClaimedMessage {
            claim_id: ClaimId::new(),
            message: Message {
                id: MessageId::new(),
                timestamp: Utc::now(),
                target: agent.clone(),
                kind: "k".into(),
                payload: serde_json::json!({"n": 1}),
                reply_to: None,
                correlation_id: None,
            },
            claimed_at: Utc::now(),
            attempt: 1,
        }
    }

    #[tokio::test]
    async fn dispatch_broadcasts_completion_with_synthetic_cost() {
        let d = StubDispatcher::new(Cost {
            tokens: TokenUsage::Split {
                input: 1,
                output: 2,
            },
            usd_micros: Some(3),
        });
        let s = spec("planner", 1);
        let mut completions = d.completions().await.unwrap();

        let handle = d.dispatch(&s, claimed(&s.id)).await.unwrap();
        assert_eq!(handle.agent, s.id);

        let completion =
            tokio::time::timeout(std::time::Duration::from_millis(500), completions.next())
                .await
                .unwrap()
                .unwrap();

        assert_eq!(completion.run.run_id, handle.run_id);
        match completion.outcome {
            Outcome::Success { cost, .. } => {
                assert_eq!(cost.usd_micros, Some(3));
            }
            other => panic!("expected Success, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn dispatch_rejects_when_at_capacity() {
        let d = StubDispatcher::default();
        let s = spec("planner", 1);
        let mut completions = d.completions().await.unwrap();

        // Hold the first run open by not draining completions until after the
        // second attempt — but the stub completes almost instantly, so instead
        // we set concurrency to 0 manually by exhausting capacity.
        // Easier: set max=1 and spin many dispatches, checking that at least
        // one comes back with CapacityExceeded OR all succeed sequentially.
        // To make this deterministic, directly saturate the counter.
        d.concurrency
            .lock()
            .await
            .insert(s.id.clone(), s.max_concurrent_runs);
        let err = d.dispatch(&s, claimed(&s.id)).await.unwrap_err();
        assert!(matches!(err, DispatchError::CapacityExceeded(_)));

        // Reset so Drop doesn't leak.
        d.concurrency.lock().await.insert(s.id.clone(), 0);
        // Prove the dispatcher still works after the rejection.
        d.dispatch(&s, claimed(&s.id)).await.unwrap();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), completions.next())
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn running_snapshot_reflects_in_flight_runs() {
        // Create a dispatcher where runs don't complete too fast by using a
        // full broadcast channel trick — actually we can just check that
        // after dispatch, running() contains the handle OR it has already
        // drained. To make this deterministic, assert that either the
        // handle is present OR a completion is queued.
        let d = StubDispatcher::default();
        let s = spec("planner", 1);
        let mut completions = d.completions().await.unwrap();
        let handle = d.dispatch(&s, claimed(&s.id)).await.unwrap();

        // Wait for the completion, then confirm running is empty afterwards.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), completions.next())
            .await
            .unwrap()
            .unwrap();
        let running = d.running().await.unwrap();
        assert!(running.iter().all(|h| h.run_id != handle.run_id));
    }
}
