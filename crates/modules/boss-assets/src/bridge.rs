//! AssetEvent <-> core Event conversion for NATS transport.
//!
//! The assets domain uses `AssetEvent` with a rich typed enum; the NATS bus
//! uses `boss_core::event::Event` with a JSON payload. This bridge converts
//! between the two so the assets service can publish/subscribe on NATS.

use boss_core::event::Event;

use crate::types::{AssetEvent, AssetEventKind};

/// NATS subject prefix for asset events.
const SUBJECT_PREFIX: &str = "asset";

/// Map a `AssetEventKind` variant to its snake_case subject suffix.
pub fn kind_to_subject(kind: &AssetEventKind) -> &'static str {
    match kind {
        AssetEventKind::Registered { .. } => "registered",
        AssetEventKind::Identified { .. } => "identified",
        AssetEventKind::Received { .. } => "received",
        AssetEventKind::PutAway { .. } => "put_away",
        AssetEventKind::PartReplaced { .. } => "part_replaced",
        AssetEventKind::Sold { .. } => "sold",
        AssetEventKind::Shipped { .. } => "shipped",
        AssetEventKind::Installed { .. } => "installed",
        AssetEventKind::WarrantyStarted { .. } => "warranty_started",
        AssetEventKind::WarrantyExpired => "warranty_expired",
        AssetEventKind::OwnershipTransferred { .. } => "ownership_transferred",
        // Wire string is `service_job_*` — what every downstream
        // consumer (boss-jobs, the SPA's Jobs list) calls it.
        AssetEventKind::ServiceJobOpened { .. } => "service_job_opened",
        AssetEventKind::ServiceJobClosed { .. } => "service_job_closed",
        AssetEventKind::WarrantyClaimed { .. } => "warranty_claimed",
        AssetEventKind::Decommissioned { .. } => "decommissioned",
    }
}

/// Wrap a `AssetEvent` into a core `Event` for NATS publishing.
///
/// The NATS subject becomes `asset.{suffix}` and the full
/// `AssetEvent` is embedded as the JSON payload. `event_time` is the
/// **definitive event timestamp** for the audit_log row — it must come
/// from the caller's clock service (`now_from(&clock)`), never from the
/// client. During a sim regen the clock returns sim-time, so events
/// land at sim-time; in prod it's wall-time. The AssetEvent's own `ts`
/// stays in the payload as domain data (the business date the lifecycle
/// event is recorded *for*, e.g. when a unit was received) and still
/// drives the projection's `first_seen`/ordering — it is not the
/// system event time.
pub fn asset_event_to_core(de: &AssetEvent, event_time: chrono::DateTime<chrono::Utc>) -> Event {
    let suffix = kind_to_subject(&de.kind);
    let kind = format!("{SUBJECT_PREFIX}.{suffix}");
    let mut payload = serde_json::to_value(de).unwrap_or_default();
    // Stamp the audit-log `_actor` convention from the event's own actor_id.
    // The AssetEvent already records who acted, but audit queries and
    // projections key on `_actor`; without mirroring it here, asset events
    // landed with a null `_actor` despite the actor being known (every
    // transition names its authority — invariant I-2).
    if let Some(obj) = payload.as_object_mut()
        && let Ok(actor) = serde_json::to_value(&de.actor_id)
    {
        obj.insert("_actor".to_string(), actor);
    }
    Event::new("assets", kind, payload, event_time)
}

/// Try to extract a `AssetEvent` from a core `Event`.
///
/// Returns `None` if the payload is not a valid `AssetEvent` or the event
/// kind does not start with `asset.`.
pub fn core_event_to_system(e: &Event) -> Option<AssetEvent> {
    if !e.kind.starts_with(SUBJECT_PREFIX) {
        return None;
    }
    serde_json::from_value(e.payload.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AssetEventId, AssetId, IntakeSource};
    use chrono::NaiveDate;

    fn sample_event() -> AssetEvent {
        AssetEvent {
            id: AssetEventId::new("evt-bridge-1"),
            asset_id: AssetId::new("SN-BRIDGE"),
            ts: NaiveDate::from_ymd_opt(2026, 4, 5).unwrap(),
            actor_id: boss_core::actor::ActorId::human("emp-1"),
            kind: AssetEventKind::Received {
                sku: Some("Boss-TEST-2024".into()),
                source: IntakeSource::new("oem-new"),
                oem_serial: None,
            },
        }
    }

    #[test]
    fn kind_to_subject_covers_all_variants() {
        // Ensure every variant maps to a non-empty string.
        let variants: Vec<AssetEventKind> = vec![
            AssetEventKind::Received {
                sku: Some("Boss-TEST-2024".into()),
                source: IntakeSource::new("oem-new"),
                oem_serial: None,
            },
            AssetEventKind::PartReplaced {
                part_sku: String::new(),
                reason: String::new(),
            },
            AssetEventKind::Sold {
                account_id: String::new(),
                price_cents: 0,
                currency: "USD".to_string(),
                order_id: None,
                condition: crate::types::AssetCondition::new("new"),
            },
            AssetEventKind::Shipped {
                holder_kind: String::new(),
                holder_id: String::new(),
            },
            AssetEventKind::Installed {
                holder_kind: String::new(),
                holder_id: String::new(),
            },
            AssetEventKind::WarrantyStarted {
                through: NaiveDate::from_ymd_opt(2028, 1, 1).unwrap(),
                coverage: crate::types::WarrantyCoverage::new("standard"),
            },
            AssetEventKind::WarrantyExpired,
            AssetEventKind::OwnershipTransferred {
                from_account_id: String::new(),
                to_account_id: String::new(),
            },
            AssetEventKind::ServiceJobOpened {
                job_id: String::new(),
                summary: String::new(),
            },
            AssetEventKind::ServiceJobClosed {
                job_id: String::new(),
                turnaround_days: 0,
            },
            AssetEventKind::WarrantyClaimed {
                job_id: String::new(),
            },
            AssetEventKind::Decommissioned {
                reason: String::new(),
            },
        ];
        for v in &variants {
            let s = kind_to_subject(v);
            assert!(!s.is_empty(), "empty subject for {v:?}");
        }
    }

    #[test]
    fn round_trip_through_core_event() {
        let de = sample_event();
        // The recording time is system-supplied (here a fixed instant);
        // it is NOT derived from the AssetEvent's payload `ts`.
        let event_time = chrono::DateTime::parse_from_rfc3339("2026-05-01T09:30:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let core = asset_event_to_core(&de, event_time);
        assert_eq!(core.kind, "asset.received");
        assert_eq!(core.source, "assets");
        // Recording time is the clock-supplied instant, distinct from the
        // payload's business date (`de.ts`, 2026-04-05).
        assert_eq!(core.timestamp, event_time);
        assert_ne!(core.timestamp.date_naive(), de.ts);

        let back = core_event_to_system(&core).expect("should decode back");
        assert_eq!(back, de);
    }

    #[test]
    fn non_asset_event_returns_none() {
        let e = Event::new(
            "other",
            "agent.health",
            serde_json::json!({}),
            chrono::Utc::now(),
        );
        assert!(core_event_to_system(&e).is_none());
    }

    #[test]
    fn malformed_payload_returns_none() {
        let e = Event::new(
            "assets",
            "asset.received",
            serde_json::json!(42),
            chrono::Utc::now(),
        );
        assert!(core_event_to_system(&e).is_none());
    }
}
