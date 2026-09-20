//! End-to-end test of the Cybernetics dispatch loop.
//!
//! Wires real in-memory adapters together and asserts that every expected
//! S3 telemetry event is emitted in the right order with the right shape.
//! This test is the observability contract: if you change telemetry event
//! kinds or payloads, you change this test.

use std::sync::Arc;
use std::time::Duration;

use boss_core::agent::{AgentId, AgentSpec, Cost, Message, Window};
use boss_core::event::Event;
use boss_core::port::{CostLedger, MessageQueue};
use boss_cybernetics::Cybernetics;
use boss_cybernetics::telemetry::{
    COST_RECORDED, DISPATCH_COMPLETED, DISPATCH_DENIED, DISPATCH_REQUESTED, DISPATCH_STARTED,
    MESSAGE_ENQUEUED, MESSAGE_REJECTED,
};
use boss_events::bus::InMemoryEventBus;
use boss_events::dispatcher::StubDispatcher;
use boss_events::ledger::InMemoryCostLedger;
use boss_events::queue::{InMemoryEventRecorder, InMemoryMessageQueue};
use boss_events::registry::InMemoryAgentRegistry;
use tokio::sync::watch;

fn planner_spec() -> AgentSpec {
    AgentSpec {
        id: AgentId::try_new("planner").unwrap(),
        display_name: "Planner".into(),
        system_prompt: "You are a planner.".into(),
        model: "test".into(),
        hourly_budget_usd_micros: 1_000_000,
        max_concurrent_runs: 1,
    }
}

struct Harness {
    vm: String,
    cyb: Arc<Cybernetics>,
    recorder: Arc<InMemoryEventRecorder>,
    queue: Arc<InMemoryMessageQueue>,
    ledger: Arc<InMemoryCostLedger>,
    cancel_tx: watch::Sender<bool>,
    loop_handle: tokio::task::JoinHandle<()>,
}

async fn boot(specs: Vec<AgentSpec>, synthetic_cost: Cost) -> Harness {
    let vm = "vm-test".to_string();
    let bus: Arc<InMemoryEventBus> = Arc::new(InMemoryEventBus::new(64));
    let queue = Arc::new(InMemoryMessageQueue::new());
    let ledger = Arc::new(InMemoryCostLedger::new());
    let dispatcher = Arc::new(StubDispatcher::new(synthetic_cost));
    let registry = Arc::new(InMemoryAgentRegistry::new(specs));

    let publisher = Arc::new(boss_core::publisher::DomainPublisher::new(
        bus.clone(),
        format!("cybernetics/{}", vm),
    ));
    // OUTBOX (phase 2): telemetry records via the EventRecorder port;
    // the in-memory recorder collects what the Pg impl would stage.
    let recorder = Arc::new(InMemoryEventRecorder::new());
    let cyb = Arc::new(Cybernetics::new(
        vm.clone(),
        queue.clone(),
        ledger.clone(),
        dispatcher,
        registry,
        publisher,
        recorder.clone() as Arc<dyn boss_core::port::EventRecorder>,
    ));

    let (cancel_tx, cancel_rx) = watch::channel(false);
    let runner = cyb.clone();
    let loop_handle = tokio::spawn(async move {
        let _ = runner.run(cancel_rx).await;
    });

    let _ = bus; // envelope-building only post-outbox
    Harness {
        vm,
        cyb,
        recorder,
        queue,
        ledger,
        cancel_tx,
        loop_handle,
    }
}

impl Harness {
    async fn shutdown(self) {
        let _ = self.cancel_tx.send(true);
        let _ = tokio::time::timeout(Duration::from_millis(500), self.loop_handle).await;
    }
}

/// Ordered cursor over the recorder — the outbox-era analogue of the
/// old bus-stream drain. Scans recorded events from the cursor until
/// `kind` appears (polling — the dispatch loop is async) or the
/// timeout expires; advances the cursor past the match so successive
/// calls assert ORDER, same as the stream did.
struct EventCursor {
    recorder: Arc<InMemoryEventRecorder>,
    idx: usize,
}

impl EventCursor {
    fn new(recorder: Arc<InMemoryEventRecorder>) -> Self {
        Self { recorder, idx: 0 }
    }

    async fn expect_kind(&mut self, kind: &str) -> Event {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let events = self.recorder.events();
            if let Some(pos) = events[self.idx.min(events.len())..]
                .iter()
                .position(|e| e.kind == kind)
            {
                let abs = self.idx + pos;
                self.idx = abs + 1;
                return events[abs].clone();
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("timed out waiting for {kind}");
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

#[tokio::test]
async fn happy_path_emits_full_telemetry_sequence() {
    let cost = Cost {
        input_tokens: 10,
        output_tokens: 20,
        usd_micros: Some(500),
    };
    let h = boot(vec![planner_spec()], cost).await;
    let mut cursor = EventCursor::new(h.recorder.clone());

    let agent = AgentId::try_new("planner").unwrap();
    let msg = Message::new(agent.clone(), "work.plan", serde_json::json!({"n": 1}));
    let msg_id = h.cyb.submit(msg).await.unwrap();

    // Every lifecycle event must fire, in order.
    let enq = cursor.expect_kind(MESSAGE_ENQUEUED).await;
    assert_eq!(enq.source, format!("cybernetics/{}", h.vm));
    assert_eq!(enq.payload["agent"], "planner");
    assert_eq!(enq.payload["kind"], "work.plan");
    assert_eq!(enq.payload["depth"], 1);
    assert_eq!(
        enq.payload["message_id"].as_str().unwrap(),
        msg_id.to_string()
    );

    let req = cursor.expect_kind(DISPATCH_REQUESTED).await;
    assert_eq!(req.payload["agent"], "planner");
    assert_eq!(req.payload["attempt"], 1);

    let started = cursor.expect_kind(DISPATCH_STARTED).await;
    assert_eq!(started.payload["agent"], "planner");
    assert!(started.payload.get("run_id").is_some());

    let completed = cursor.expect_kind(DISPATCH_COMPLETED).await;
    assert_eq!(completed.payload["agent"], "planner");
    assert_eq!(completed.payload["status"], "completed");
    assert_eq!(completed.payload["outcome"]["kind"], "success");

    let cost_evt = cursor.expect_kind(COST_RECORDED).await;
    assert_eq!(cost_evt.payload["agent"], "planner");
    assert_eq!(cost_evt.payload["cost"]["usd_micros"], 500);

    // Final state: queue drained, ledger has one entry.
    assert_eq!(h.queue.depth(&agent).await.unwrap(), 0);
    let spent = h.ledger.spent(&agent, Window::LastHour).await.unwrap();
    assert_eq!(spent.usd_micros, Some(500));

    h.shutdown().await;
}

#[tokio::test]
async fn unknown_agent_is_rejected_with_telemetry() {
    let h = boot(vec![], Cost::ZERO).await;
    let mut cursor = EventCursor::new(h.recorder.clone());

    let ghost = AgentId::try_new("ghost").unwrap();
    let msg = Message::new(ghost, "any", serde_json::json!({}));
    let err = h.cyb.submit(msg).await.unwrap_err();
    assert!(matches!(
        err,
        boss_cybernetics::CyberneticsError::UnknownAgent(_)
    ));

    let evt = cursor.expect_kind(MESSAGE_REJECTED).await;
    assert_eq!(evt.payload["agent"], "ghost");
    assert_eq!(evt.payload["reason"], "unknown_agent");

    h.shutdown().await;
}

#[tokio::test]
async fn budget_exhaustion_denies_dispatch() {
    // Spec with a tiny cap, dispatcher that charges 100 usd_micros per run.
    let mut spec = planner_spec();
    spec.hourly_budget_usd_micros = 50; // less than one run.
    let h = boot(
        vec![spec],
        Cost {
            input_tokens: 0,
            output_tokens: 0,
            usd_micros: Some(100),
        },
    )
    .await;
    let mut cursor = EventCursor::new(h.recorder.clone());

    let agent = AgentId::try_new("planner").unwrap();
    // Pre-fund the ledger past the cap.
    h.ledger
        .record(
            &agent,
            Cost {
                input_tokens: 0,
                output_tokens: 0,
                usd_micros: Some(50),
            },
        )
        .await
        .unwrap();

    let msg = Message::new(agent.clone(), "work", serde_json::json!({}));
    h.cyb.submit(msg).await.unwrap();

    // Enqueued + denied, and we must NOT see dispatch.started.
    cursor.expect_kind(MESSAGE_ENQUEUED).await;
    let denied = cursor.expect_kind(DISPATCH_DENIED).await;
    assert_eq!(denied.payload["agent"], "planner");
    assert!(
        denied.payload["reason"]
            .as_str()
            .unwrap()
            .contains("budget")
    );

    // Message remains in the queue (nothing was claimed).
    assert_eq!(h.queue.depth(&agent).await.unwrap(), 1);

    h.shutdown().await;
}
