use crate::event::Event;
use async_trait::async_trait;

/// Port: publish and subscribe to events.
///
/// This is the central nervous system of Boss.
/// Adapters can implement this with in-memory channels,
/// NATS, Kafka, Redis Streams — domain doesn't care.
#[async_trait]
pub trait EventBus: Send + Sync {
    /// Publish an event to the bus
    async fn publish(&self, event: Event) -> Result<(), EventBusError>;

    /// Subscribe to events matching a kind pattern (e.g., "agent.*")
    async fn subscribe(&self, pattern: &str) -> Result<Box<dyn EventStream>, EventBusError>;
}

/// A stream of events from a subscription.
#[async_trait]
pub trait EventStream: Send + Sync {
    /// Receive the next event. Returns None if the stream is closed.
    async fn next(&mut self) -> Option<Event>;
}

/// Port: persist and retrieve events.
#[async_trait]
pub trait EventStore: Send + Sync {
    /// Append an event to the store
    async fn append(&self, event: &Event) -> Result<(), EventStoreError>;

    /// Retrieve events by kind, ordered by timestamp
    async fn query_by_kind(&self, kind: &str) -> Result<Vec<Event>, EventStoreError>;

    /// Retrieve all events from a given source
    async fn query_by_source(&self, source: &str) -> Result<Vec<Event>, EventStoreError>;
}

#[derive(Debug, thiserror::Error)]
pub enum EventBusError {
    #[error("failed to publish event: {0}")]
    PublishFailed(String),
    #[error("failed to subscribe: {0}")]
    SubscribeFailed(String),
    #[error("connection lost: {0}")]
    ConnectionLost(String),
}

#[derive(Debug, thiserror::Error)]
pub enum EventStoreError {
    #[error("failed to append event: {0}")]
    AppendFailed(String),
    #[error("query failed: {0}")]
    QueryFailed(String),
}

// ---------------------------------------------------------------------------
// Record-only events
//
// This section held five more ports — MessageQueue, CostLedger,
// AgentDispatcher, RunCompletions, AgentRegistry — whose only
// implementations were boss-events' in-memory queue, ledger, stub and
// `claude --print` dispatchers and TOML registry, and whose only caller
// was boss-cybernetics. That crate was retired in train #582 and the
// ports went with their adapters (backlog 05a003da, 2026-09-23). What
// an agent run costs is recorded by boss-jobs' `agent_runs` module.
// ---------------------------------------------------------------------------

/// Port: transactional event recorder — the outbox-backed sink for
/// telemetry/domain events that have NO accompanying row write to
/// join (outbox phase 2). The Pg implementation stages the event on
/// `event_outbox` in a small transaction of its own; boss-event-relay
/// delivers to audit_log + NATS. In-memory implementations collect
/// for test assertion. This is how a component whose events ARE its
/// state (the gateway's auth events — a session is a cookie, not a
/// row) gets the same delivery guarantee as a domain write without
/// pretending it has a domain table.
#[async_trait]
pub trait EventRecorder: Send + Sync {
    async fn record(&self, event: &Event) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ports must be object-safe — the whole point is polymorphism via trait
    /// objects. These compile-only assertions lock that in.
    #[test]
    fn ports_are_object_safe() {
        fn takes<T: ?Sized>() {}
        takes::<dyn EventBus>();
        takes::<dyn EventStream>();
        takes::<dyn EventStore>();
    }
}
