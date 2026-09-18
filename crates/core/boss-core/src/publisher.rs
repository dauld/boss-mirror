//! Fire-and-forget domain event publisher + the [`EventStamp`]
//! envelope every outbox-recorded event is built from.
//!
//! **Record stamps are wall-clock.** Sim time is fully retired from
//! `Event.timestamp` / `audit_log.timestamp` (David, 2026-08-22,
//! packet a7a4cae5): a deployment whose clock-api runs in sim mode
//! still stamps every record with real time, so one incident's rows
//! read as one timeline. Sim time survives only as **payload data**
//! (business dates a handler reads from the clock port) and inside
//! the sim's own scheduling — never on the record. If a record ever
//! needs a sim-timeline annotation again, that returns as a protocol
//! field (explicitly deferred).
//!
//! The publisher wraps an `EventBus` and optionally an `AuditWriter`.
//! On error, returns false but never propagates — write operations
//! must not fail because the event bus or audit log is unavailable.
//!
//! "Never propagates" is NOT "silent": every bus or audit failure
//! logs at ERROR with the event identity. The audit log is the
//! system of record — a dropped write there means a state change
//! with no provenance, permanently unreproducible by
//! rebuild-from-log. The structural fix (audit insert joins the
//! domain transaction; the bus demotes to post-commit notification)
//! is tracked as its own workstream; this layer's job until then is
//! to make every hole loud the moment it is punched.

use std::sync::Arc;

use crate::actor::ActorId;
use crate::audit::AuditWriter;
use crate::event::Event;
use crate::partition::Partition;
use crate::port::EventBus;

/// Trait-erased clock probe — DomainPublisher only needs to
/// know whether the deploy is in sim mode, not the full ClockClient
/// surface. Keeping the trait local (no boss-clock-client dep
/// inside boss-core) preserves the existing crate hierarchy.
#[async_trait::async_trait]
pub trait SimulatedProbe: Send + Sync {
    /// Returns true when the deploy's clock is currently in sim
    /// mode (i.e., the audit_log row about to land represents
    /// simulated rather than real activity).
    async fn simulated(&self) -> bool;
}

/// Publishes domain events to the event bus and optionally to the audit log.
#[derive(Clone)]
pub struct DomainPublisher {
    bus: Arc<dyn EventBus>,
    audit: Option<Arc<dyn AuditWriter>>,
    source: String,
    /// Optional sim-mode probe. When set, every stamp carries
    /// `_simulated: bool` on the audit_log payload centrally, so no
    /// handler has to thread the bit through its call sites. When
    /// unset, stamps outside a sim chain don't carry `_simulated`.
    sim_probe: Option<Arc<dyn SimulatedProbe>>,
}

impl DomainPublisher {
    /// Create a publisher for the given service name (e.g., "catalog", "people").
    pub fn new(bus: Arc<dyn EventBus>, source: impl Into<String>) -> Self {
        Self {
            bus,
            audit: None,
            source: source.into(),
            sim_probe: None,
        }
    }

    /// Attach an audit writer for persisting events to the audit log.
    pub fn with_audit(mut self, writer: Arc<dyn AuditWriter>) -> Self {
        self.audit = Some(writer);
        self
    }

    /// Attach a sim-mode probe. Every subsequent
    /// [`Self::stamp_with_actor`] carries `_simulated: <bool>` on the
    /// payload it builds. Service binaries wire this at startup using
    /// their ClockClient (which exposes `now().simulated`); handlers
    /// don't have to thread the bit through their call sites.
    pub fn with_sim_probe(mut self, probe: Arc<dyn SimulatedProbe>) -> Self {
        self.sim_probe = Some(probe);
        self
    }

    /// The actor for an emit that didn't name one explicitly: the
    /// current request's authenticated identity if we're inside a
    /// request scope, otherwise this service's own automation identity
    /// (`automation:<source>`). Never anonymous — there is no `system`
    /// actor; a write with no traceable authority is still attributed
    /// to the process that made it.
    pub fn default_actor(&self) -> ActorId {
        crate::actor_context::current_actor()
            .unwrap_or_else(|| ActorId::Automation(self.source.clone()))
    }

    /// Resolve the enrichment envelope for IN-TRANSACTION (outbox)
    /// event recording — `_actor` / `_simulated` semantics captured
    /// as a value a domain repository can apply INSIDE its own
    /// transaction via [`EventStamp::event`] +
    /// `boss_events::outbox::record_event_in_tx`.
    ///
    /// The stamp's `timestamp` is **wall-clock now**, minted here.
    /// Sim time is retired from the record (David, 2026-08-22, packet
    /// a7a4cae5): `audit_log.timestamp` says when the fact was
    /// recorded, on the one real clock, regardless of the deploy's
    /// clock mode. Business dates that follow the sim timeline are
    /// payload fields, read from the clock port by the handler —
    /// never the stamp. Handlers that bind a row column the rebuilder
    /// reproduces from `audit_log.timestamp` (`created_at` /
    /// `updated_at`) must bind [`EventStamp::timestamp`], not their
    /// own clock read, so live row and replayed row stay identical.
    pub async fn stamp_with_actor(&self, actor: ActorId) -> EventStamp {
        // Origin, not clock mode: `_simulated` describes THIS event's
        // origin. An event is simulated when the task is part of a sim
        // chain — the request carried `x-sim-origin`, or the
        // dispatcher inherited it from the event it is reacting to.
        // The clock-mode probe only controls whether the key appears
        // at all when the chain is real.
        let simulated = crate::sim_origin::is_in_sim_chain();
        EventStamp {
            source: self.source.clone(),
            actor,
            partition: if simulated || self.sim_probe.is_some() {
                Some(Partition::from_legacy_simulated(simulated))
            } else {
                None
            },
            timestamp: chrono::Utc::now(),
        }
    }

    /// Publish a pre-built `Event`. Used by services like assets that
    /// already construct an `Event` via a domain bridge and need the
    /// id and timestamp on the wire to match what the rest of the
    /// service stores. Same fire-and-forget contract as `emit`:
    /// failures never propagate to the caller — but they are LOUD.
    /// A dropped audit write is a permanent hole in the system of
    /// record (the state change committed; replay-from-log cannot
    /// reproduce it), which is exactly how the 2026-07-13
    /// replay-divergence class stayed invisible: the audit trigger
    /// rejected events post-commit and the old `let _ =` here
    /// swallowed every rejection. Making the write transactional
    /// with the domain write is tracked separately; until then,
    /// every failure logs at ERROR with the event identity.
    pub async fn publish(&self, event: Event) -> bool {
        let bus_ok = match self.bus.publish(event.clone()).await {
            Ok(()) => true,
            Err(e) => {
                tracing::error!(
                    kind = %event.kind,
                    event_id = %event.id,
                    source = %self.source,
                    error = %e,
                    "event bus publish failed — downstream consumers will not see this event"
                );
                false
            }
        };
        if let Some(audit) = &self.audit
            && let Err(e) = audit.write(&event).await
        {
            tracing::error!(
                kind = %event.kind,
                event_id = %event.id,
                source = %self.source,
                error = %e,
                "audit_log write failed — state committed WITHOUT provenance; rebuild-from-log cannot reproduce this event"
            );
        }
        bus_ok
    }

    /// Publish a batch of pre-built `Event`s. Routes the audit-log
    /// writes through `AuditWriter::write_batch` so a Postgres-backed
    /// writer can collapse the per-event INSERTs into one bulk
    /// statement. Bus publishes still happen one at a time because
    /// every NATS subject is per-event.
    ///
    /// Same fire-and-forget contract as `publish`: bus or audit
    /// failures never fail the underlying domain write — but every
    /// failure logs at ERROR (see `publish` for why silence here
    /// punched holes in the system of record). Note a failed batch
    /// audit write loses the WHOLE batch's provenance in one shot.
    pub async fn publish_batch(&self, events: Vec<Event>) {
        for event in &events {
            if let Err(e) = self.bus.publish(event.clone()).await {
                tracing::error!(
                    kind = %event.kind,
                    event_id = %event.id,
                    source = %self.source,
                    error = %e,
                    "event bus publish failed — downstream consumers will not see this event"
                );
            }
        }
        if let Some(audit) = &self.audit
            && let Err(e) = audit.write_batch(&events).await
        {
            tracing::error!(
                batch_len = events.len(),
                first_kind = %events.first().map(|e| e.kind.as_str()).unwrap_or("<empty>"),
                source = %self.source,
                error = %e,
                "audit_log batch write failed — {} events committed WITHOUT provenance; rebuild-from-log cannot reproduce them",
                events.len()
            );
        }
    }

    /// The service name this publisher was created for.
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// Stamp the `_actor` field on an event payload so audit_log
/// readers can answer "who fired this transition" without joining.
/// `_actor` is the load-bearing field for the Level-B
/// actor-stamping invariant; the payload also gets `_source` so
/// debug consumers can correlate without walking back to the
/// audit_log row.
///
/// Object payloads gain the field; non-object payloads (legacy
/// arrays, scalars) are wrapped in a `{ _actor, value }` object so
/// the field is always reachable. The wrap path is rare — every
/// modern emitter uses an object — but it keeps the invariant
/// universal.
/// Add the partition markers to an event payload alongside `_actor`:
/// `_partition: "real" | "simulated" | "shadow"` and the legacy
/// `_simulated: bool`, derived as not-real (packet 508cc38c — a
/// reader that only knows the bool fails closed on a shadow event).
/// Same shape as `inject_actor` — mutates in place when the payload
/// is an object, wraps non-object payloads in `{ _partition,
/// _simulated, value }`. Replayed events that already carry an
/// explicit marker keep their original value (matching the rebuilder
/// semantics for `_actor`); the two keys are inserted independently,
/// so a pre-shadow event replayed with only `_simulated` gains the
/// `_partition` the stamp derived from it.
pub fn inject_partition(mut payload: serde_json::Value, partition: Partition) -> serde_json::Value {
    if let serde_json::Value::Object(ref mut map) = payload {
        map.entry("_partition".to_string())
            .or_insert_with(|| serde_json::Value::String(partition.as_str().to_string()));
        map.entry("_simulated".to_string())
            .or_insert(serde_json::Value::Bool(partition.fails_closed()));
        return payload;
    }
    serde_json::json!({
        "_partition": partition.as_str(),
        "_simulated": partition.fails_closed(),
        "value": payload,
    })
}

/// Public because it is THE actor-embedding shape: adapters that
/// mint events inside their own transactions (the workflow registry)
/// must produce payloads indistinguishable from EventStamp's.
pub fn inject_actor(mut payload: serde_json::Value, actor: &ActorId) -> serde_json::Value {
    let actor_value = serde_json::to_value(actor).unwrap_or(serde_json::Value::Null);
    if let serde_json::Value::Object(ref mut map) = payload {
        // Don't overwrite an explicit `_actor` already on the
        // payload — that's how a rebuilder replays a historical
        // event with the recorded actor preserved.
        map.entry("_actor".to_string()).or_insert(actor_value);
        return payload;
    }
    serde_json::json!({
        "_actor": actor_value,
        "value": payload,
    })
}

/// The enrichment envelope for in-transaction (outbox) event
/// recording. Resolved once per request — via
/// [`DomainPublisher::stamp_with_actor`] when a publisher (and
/// its sim probe) is wired, or [`EventStamp::new`] when not — and
/// handed into the domain repository, which applies it to each
/// payload it builds INSIDE its transaction.
///
/// `timestamp` is **wall-clock now**, minted at construction — the
/// one place a record's stamp comes from. Sim time is retired from
/// the record (David, 2026-08-22, packet a7a4cae5); the timeline a
/// forensic read walks is the real one. `timestamp` stays `pub` so
/// row binds reproduce it exactly (and so a deterministic test can
/// overwrite it on a stamp it built itself).
#[derive(Debug, Clone)]
pub struct EventStamp {
    source: String,
    actor: ActorId,
    /// `Some(p)` injects `_partition: p` + the derived `_simulated`;
    /// `None` leaves both keys off entirely (they appear when the
    /// chain is simulated or a probe is wired at all).
    partition: Option<Partition>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

impl EventStamp {
    /// Publisher-less construction (adapters without a publisher,
    /// trait-default overloads, CLI boundary tools). Still honors the
    /// task-local sim chain, so a sim-originated request stamps
    /// `_simulated: true` even here — and stamps wall-clock time,
    /// same as [`DomainPublisher::stamp_with_actor`].
    pub fn new(source: impl Into<String>, actor: ActorId) -> Self {
        let in_chain = crate::sim_origin::is_in_sim_chain();
        Self {
            source: source.into(),
            actor,
            partition: in_chain.then_some(Partition::Simulated),
            timestamp: chrono::Utc::now(),
        }
    }

    /// The actor every event this stamp builds is attributed to —
    /// what `_actor` will read on the payload. For a write whose
    /// record names the caller in its own words (`declared_by` on a
    /// tenant's `*.declared` events, backlog d9409039), so the named
    /// field and `_actor` are ONE value read from one place, never
    /// two arguments that can disagree.
    pub fn actor(&self) -> &ActorId {
        &self.actor
    }

    /// Override the minted wall timestamp. For deterministic tests
    /// and replay-shaped fixtures ONLY — production write paths never
    /// call this: the record stamp is wall time by decision (David,
    /// 2026-08-22, packet a7a4cae5), and threading a deploy-clock
    /// reading back in here would resurrect the mixed-clock log this
    /// API retires. (`timestamp` is a `pub` field; this builder just
    /// keeps fixture construction a single expression.)
    pub fn with_timestamp(mut self, timestamp: chrono::DateTime<chrono::Utc>) -> Self {
        self.timestamp = timestamp;
        self
    }

    /// Override the partition markers for every event this stamp
    /// builds. The job/step write paths use this to make the Job's
    /// admission-fixed partition — not the transport context of the
    /// current request — the source of the marker: a write to a
    /// simulated packet is simulated even when a human clicks it, a
    /// write to a shadow packet is shadow, and a sim-chain write to a
    /// real packet stays real. Events not scoped to a Job keep the
    /// task-local sim-chain default.
    pub fn with_partition(mut self, partition: Partition) -> Self {
        self.partition = Some(partition);
        self
    }

    /// Build the enriched `Event` for `kind` + `payload` — the in-tx
    /// analogue of the retired publisher emit path's construction.
    pub fn event(&self, kind: &str, payload: serde_json::Value) -> Event {
        let mut payload = inject_actor(payload, &self.actor);
        if let Some(partition) = self.partition {
            payload = inject_partition(payload, partition);
        }
        Event::new(&self.source, kind, payload, self.timestamp)
    }
}

#[cfg(test)]
mod inject_tests {
    use super::*;

    #[test]
    fn injects_into_object_payload() {
        let p = serde_json::json!({"foo": "bar"});
        let out = inject_actor(p, &ActorId::Human("emp-1".into()));
        assert_eq!(out["_actor"], serde_json::Value::String("emp-1".into()));
        assert_eq!(out["foo"], "bar");
    }

    /// An agent's `_actor` survives the payload round trip as the
    /// typed Agent, so a rebuilder replaying the log gets the same CPU
    /// class back — and the model is recoverable from `_actor` alone,
    /// with no `_model` key alongside it.
    #[test]
    fn agent_actor_roundtrips_through_an_event_payload() {
        let p = serde_json::json!({"job_id": "job-1"});
        let out = inject_actor(p, &ActorId::agent("claude", "opus-5"));
        assert_eq!(out["_actor"], "claude:opus-5");
        assert!(out.get("_model").is_none());
        let back: ActorId = serde_json::from_value(out["_actor"].clone()).unwrap();
        assert_eq!(back, ActorId::agent("claude", "opus-5"));
        assert!(!back.is_human());
    }

    #[test]
    fn preserves_explicit_actor_on_replay() {
        let p = serde_json::json!({"foo": "bar", "_actor": "emp-5"});
        let out = inject_actor(p, &ActorId::Automation("rebuilder".into()));
        // Replayed event keeps its original actor.
        assert_eq!(out["_actor"], "emp-5");
    }

    #[test]
    fn wraps_non_object_payload() {
        let p = serde_json::json!(["a", "b"]);
        let out = inject_actor(p, &ActorId::Automation("platform".into()));
        assert_eq!(out["_actor"], "automation:platform");
        assert_eq!(out["value"], serde_json::json!(["a", "b"]));
    }

    #[test]
    fn inject_partition_adds_both_keys_for_the_simulated_company() {
        let p = serde_json::json!({"foo": "bar"});
        let out = inject_partition(p, Partition::Simulated);
        assert_eq!(out["_partition"], "simulated");
        assert_eq!(out["_simulated"], true);
        assert_eq!(out["foo"], "bar");
    }

    #[test]
    fn inject_partition_adds_both_keys_for_real() {
        let p = serde_json::json!({"foo": "bar"});
        let out = inject_partition(p, Partition::Real);
        assert_eq!(out["_partition"], "real");
        assert_eq!(out["_simulated"], false);
    }

    #[test]
    fn inject_partition_marks_a_shadow_event_simulated_for_old_readers() {
        // The legacy bool is derived as not-real (packet 508cc38c): a
        // reader that only knows `_simulated` fails closed on a shadow
        // event exactly as it does on a simulated one.
        let out = inject_partition(serde_json::json!({}), Partition::Shadow);
        assert_eq!(out["_partition"], "shadow");
        assert_eq!(out["_simulated"], true);
    }

    #[test]
    fn inject_partition_preserves_replay_values() {
        // Replayed event keeps its original markers — same semantics
        // as the _actor preservation — and a pre-shadow event that
        // carried only the bool gains the `_partition` derived from it.
        let p = serde_json::json!({"foo": "bar", "_simulated": true});
        let out = inject_partition(p, Partition::Simulated);
        assert_eq!(out["_simulated"], true);
        assert_eq!(out["_partition"], "simulated");
        let p = serde_json::json!({"_partition": "shadow", "_simulated": true});
        let out = inject_partition(p, Partition::Real);
        assert_eq!(out["_partition"], "shadow");
        assert_eq!(out["_simulated"], true);
    }

    #[test]
    fn inject_partition_wraps_non_object_payload() {
        let p = serde_json::json!(["a", "b"]);
        let out = inject_partition(p, Partition::Simulated);
        assert_eq!(out["_partition"], "simulated");
        assert_eq!(out["_simulated"], true);
        assert_eq!(out["value"], serde_json::json!(["a", "b"]));
    }

    #[test]
    fn stamp_with_partition_overrides_the_chain_default() {
        // A stamp built outside any sim chain carries no markers; the
        // job write paths override it from the packet's admission-fixed
        // partition, in every direction.
        let stamp = EventStamp::new("jobs", ActorId::Human("emp-1".into()));
        let bare = stamp
            .clone()
            .event("jobs.job.updated", serde_json::json!({}));
        assert!(
            bare.payload.get("_simulated").is_none() && bare.payload.get("_partition").is_none(),
            "outside a sim chain the default stamp leaves the keys off"
        );
        let sim = stamp
            .clone()
            .with_partition(Partition::Simulated)
            .event("jobs.job.updated", serde_json::json!({}));
        assert_eq!(sim.payload["_partition"], "simulated");
        assert_eq!(sim.payload["_simulated"], true);
        let shadow = stamp
            .clone()
            .with_partition(Partition::Shadow)
            .event("jobs.job.updated", serde_json::json!({}));
        assert_eq!(shadow.payload["_partition"], "shadow");
        assert_eq!(shadow.payload["_simulated"], true);
        let real = stamp
            .with_partition(Partition::Real)
            .event("jobs.job.updated", serde_json::json!({}));
        assert_eq!(real.payload["_partition"], "real");
        assert_eq!(real.payload["_simulated"], false);
    }
}

#[cfg(test)]
mod wall_stamp_tests {
    use super::*;

    /// Sim time is retired from the record (David, 2026-08-22, packet
    /// a7a4cae5). A stamp minted inside a sim chain — the exact
    /// condition under which the old API was handed a skewed sim
    /// instant — still carries wall time; the chain reaches the
    /// payload as `_simulated`, never the timestamp.
    #[tokio::test]
    async fn a_stamp_minted_inside_a_sim_chain_carries_wall_time() {
        let (event, before, after) = crate::sim_origin::with_sim_chain(true, async {
            let before = chrono::Utc::now();
            let stamp = EventStamp::new("jobs", ActorId::Human("emp-1".into()));
            let event = stamp.event("jobs.job.updated", serde_json::json!({}));
            (event, before, chrono::Utc::now())
        })
        .await;
        assert!(
            event.timestamp >= before && event.timestamp <= after,
            "the record stamp must be wall-clock now, got {}",
            event.timestamp
        );
        assert_eq!(
            event.payload["_simulated"], true,
            "the sim chain still marks origin — just not the clock"
        );
    }

    /// Same invariant through the publisher path (the sim probe says
    /// the deploy's clock runs simulated — the stamp is wall anyway).
    #[tokio::test]
    async fn publisher_stamp_is_wall_time_even_when_the_deploy_clock_is_simulated() {
        struct AlwaysSim;
        #[async_trait::async_trait]
        impl SimulatedProbe for AlwaysSim {
            async fn simulated(&self) -> bool {
                true
            }
        }
        struct NullBus;
        #[async_trait::async_trait]
        impl crate::port::EventBus for NullBus {
            async fn publish(&self, _e: Event) -> Result<(), crate::port::EventBusError> {
                Ok(())
            }
            async fn subscribe(
                &self,
                _p: &str,
            ) -> Result<Box<dyn crate::port::EventStream>, crate::port::EventBusError> {
                Err(crate::port::EventBusError::SubscribeFailed("stub".into()))
            }
        }
        let publisher =
            DomainPublisher::new(Arc::new(NullBus), "jobs").with_sim_probe(Arc::new(AlwaysSim));
        let before = chrono::Utc::now();
        let stamp = publisher
            .stamp_with_actor(ActorId::Human("emp-1".into()))
            .await;
        let after = chrono::Utc::now();
        assert!(
            stamp.timestamp >= before && stamp.timestamp <= after,
            "stamp must be wall-clock now, got {}",
            stamp.timestamp
        );
        // Outside a sim chain the probe only makes the key appear —
        // with the honest value: this activity is not synthetic.
        let event = stamp.event("jobs.job.updated", serde_json::json!({}));
        assert_eq!(event.payload["_simulated"], false);
    }
}
