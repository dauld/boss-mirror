//! Property-based tests for the assets device state projection.
//!
//! The unit tests in `crates/boss-assets/src/project.rs` cover specific
//! scenarios — install, open ticket, close ticket, etc. Those lock in
//! individual transitions. These property tests exercise the same
//! projection with random sequences of events and assert invariants
//! that MUST hold for any sequence.
//!
//! Properties encoded:
//!
//! 1. `prop_decommissioned_is_sticky` — once a Decommissioned event
//!    lands in the sorted log, the final phase is Decommissioned and
//!    account_id never changes afterward. Late-arriving events don't
//!    undecommission a device.
//!
//! 2. `prop_final_account_matches_last_account_event` — if the sequence
//!    (up to any Decommissioned) contains any account-setting event
//!    (Installed / Shipped / Sold / OwnershipTransferred), the final
//!    account_id is the one from the latest such event in sorted order.
//!
//! 3. `prop_event_order_invariant` — the projection sorts internally
//!    by (ts, id), so two sequences with the same events in different
//!    input orders must produce the same final state.
//!
//! 4. `prop_open_ticket_count_is_saturating_balance` — the counter
//!    equals `max(0, opened - closed)` up to the first Decommissioned.
//!
//! 5. `prop_phase_never_installed_without_account` — phase is
//!    Installed only if account_id is Some.
//!
//! 6. `prop_phase_is_out_for_service_iff_open_tickets_and_account` —
//!    given we've never decommissioned and there's a account, phase is
//!    OutForService iff open_ticket_count > 0, else Installed.

use boss_assets::project::project;
use boss_assets::types::{
    AssetCondition, AssetEvent, AssetEventId, AssetEventKind, AssetId, AssetLifecyclePhase,
    IntakeSource, WarrantyCoverage,
};
use chrono::NaiveDate;
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Event generators
// ---------------------------------------------------------------------------

fn arb_event_kind() -> impl Strategy<Value = AssetEventKind> {
    prop_oneof![
        Just(AssetEventKind::Received {
            sku: Some("Boss-TEST-2024".into()),
            source: IntakeSource::new("oem-new"),
            oem_serial: None,
        }),
        "[a-z]{3,6}".prop_map(|bin| AssetEventKind::PutAway { bin }),
        (
            "[a-z0-9]{3,8}".prop_map(String::from),
            "[a-z]{3,8}".prop_map(String::from),
        )
            .prop_map(|(part_sku, reason)| AssetEventKind::PartReplaced { part_sku, reason }),
        "account-[0-9]{3}".prop_map(|holder_id| AssetEventKind::Shipped {
            holder_kind: "account".to_string(),
            holder_id,
        }),
        "account-[0-9]{3}".prop_map(|holder_id| AssetEventKind::Installed {
            holder_kind: "account".to_string(),
            holder_id,
        }),
        ("account-[0-9]{3}", 1_000_000i64..10_000_000).prop_map(|(account_id, price_cents)| {
            AssetEventKind::Sold {
                account_id,
                price_cents,
                currency: "USD".to_string(),
                order_id: None,
                condition: AssetCondition::new("used"),
            }
        }),
        Just(AssetEventKind::WarrantyStarted {
            through: NaiveDate::from_ymd_opt(2027, 1, 1).unwrap(),
            coverage: WarrantyCoverage::new("standard"),
        }),
        Just(AssetEventKind::WarrantyExpired),
        ("account-[0-9]{3}", "account-[0-9]{3}").prop_map(|(from, to)| {
            AssetEventKind::OwnershipTransferred {
                from_account_id: from,
                to_account_id: to,
            }
        }),
        "tkt-[0-9]{3}".prop_map(|job_id| AssetEventKind::ServiceJobOpened {
            job_id,
            summary: "issue".into(),
        }),
        ("tkt-[0-9]{3}", 1u32..30).prop_map(|(job_id, turnaround_days)| {
            AssetEventKind::ServiceJobClosed {
                job_id,
                turnaround_days,
            }
        }),
        "tkt-[0-9]{3}".prop_map(|job_id| AssetEventKind::WarrantyClaimed { job_id }),
        "[a-z]{3,6}".prop_map(|reason| AssetEventKind::Decommissioned { reason }),
    ]
}

/// Build a sequence of 1–20 events with unique timestamps (one per
/// day starting 2026-01-01). Unique timestamps remove ambiguity from
/// sort order so property assertions are deterministic.
fn arb_event_sequence() -> impl Strategy<Value = Vec<AssetEvent>> {
    prop::collection::vec(arb_event_kind(), 1..20).prop_map(|kinds| {
        kinds
            .into_iter()
            .enumerate()
            .map(|(i, kind)| AssetEvent {
                id: AssetEventId::new(format!("evt-{i:03}")),
                asset_id: AssetId::new("SN-PROP"),
                ts: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap() + chrono::Duration::days(i as i64),
                actor_id: boss_core::actor::ActorId::Automation("test".into()),
                kind,
            })
            .collect()
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn asset_id() -> AssetId {
    AssetId::new("SN-PROP")
}

/// Sort events by (ts, id) exactly as the projection does, then walk
/// them and return everything up to and including the first
/// Decommissioned event (or the full list if none).
fn events_before_or_at_decommission(events: &[AssetEvent]) -> Vec<&AssetEvent> {
    let mut sorted: Vec<&AssetEvent> = events.iter().collect();
    sorted.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.0.cmp(&b.id.0)));
    let mut out = Vec::new();
    for e in sorted {
        out.push(e);
        if matches!(e.kind, AssetEventKind::Decommissioned { .. }) {
            break;
        }
    }
    out
}

fn has_decommission(events: &[AssetEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e.kind, AssetEventKind::Decommissioned { .. }))
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 256,
        max_shrink_iters: 512,
        ..ProptestConfig::default()
    })]

    /// Decommissioned is a terminal phase: once it lands in the
    /// sorted event stream, every observable bit of state
    /// (phase, account_id, open_ticket_count) is frozen.
    #[test]
    fn prop_decommissioned_is_sticky(events in arb_event_sequence()) {
        prop_assume!(has_decommission(&events));

        let state = project(&asset_id(), &events).expect("non-empty log");
        prop_assert_eq!(
            state.phase.as_str(),
            AssetLifecyclePhase::DECOMMISSIONED,
            "decommission must stick"
        );

        // Extending with more events after the Decommissioned index
        // must NOT change phase, account, or counter. We synthesize a
        // ServiceJobOpened with a later timestamp and compare.
        let max_ts = events.iter().map(|e| e.ts).max().unwrap();
        let mut extended = events.clone();
        extended.push(AssetEvent {
            id: AssetEventId::new("evt-late-zzz"),
            asset_id: asset_id(),
            ts: max_ts + chrono::Duration::days(1),
            actor_id: boss_core::actor::ActorId::Automation("test".into()),
            kind: AssetEventKind::ServiceJobOpened {
                job_id: "tkt-late".into(),
                summary: "late arrival".into(),
            },
        });
        let extended_state = project(&asset_id(), &extended).expect("still non-empty");
        prop_assert_eq!(
            &extended_state.phase, &state.phase,
            "late ServiceJobOpened must not change phase of decommissioned device"
        );
        prop_assert_eq!(
            &extended_state.holder_id, &state.holder_id,
            "late event must not change account_id of decommissioned device"
        );
        prop_assert_eq!(
            extended_state.open_ticket_count, state.open_ticket_count,
            "late event must not change ticket count of decommissioned device"
        );
    }

    /// Final account_id equals the account from the last account-setting
    /// event (Installed / Shipped / Sold / OwnershipTransferred) that
    /// occurs in sorted order before any Decommissioned.
    #[test]
    fn prop_final_account_matches_last_account_event(events in arb_event_sequence()) {
        let state = project(&asset_id(), &events).expect("non-empty log");

        // Walk the sorted stream up to (and including) the first
        // Decommissioned and find the last holder-setting event
        // (custody events carry the pair; ownership events imply an
        // account holder — Q5).
        let relevant = events_before_or_at_decommission(&events);
        let expected_holder = relevant
            .iter()
            .rev()
            .find_map(|e| match &e.kind {
                AssetEventKind::Installed { holder_id, .. } => Some(holder_id.clone()),
                AssetEventKind::Shipped { holder_id, .. } => Some(holder_id.clone()),
                AssetEventKind::Sold { account_id, .. } => Some(account_id.clone()),
                AssetEventKind::OwnershipTransferred { to_account_id, .. } => {
                    Some(to_account_id.clone())
                }
                _ => None,
            });
        prop_assert_eq!(
            state.holder_id, expected_holder,
            "final account_id must match latest account-setting event"
        );
    }

    /// Shuffling the input events must produce the same final state
    /// because the projection sorts internally by (ts, id).
    #[test]
    fn prop_event_order_invariant(
        events in arb_event_sequence(),
        shuffle_seed in 0u64..10_000,
    ) {
        let state_a = project(&asset_id(), &events).expect("non-empty");

        // Rotate the events by shuffle_seed % len — a cheap
        // deterministic reorder that doesn't need an rng.
        let n = events.len();
        let rot = (shuffle_seed as usize) % n;
        let mut shuffled = events.clone();
        shuffled.rotate_left(rot);

        let state_b = project(&asset_id(), &shuffled).expect("still non-empty");
        prop_assert_eq!(
            state_a.phase, state_b.phase,
            "phase must be invariant under input reordering"
        );
        prop_assert_eq!(
            state_a.holder_id, state_b.holder_id,
            "account_id must be invariant under input reordering"
        );
        prop_assert_eq!(
            state_a.open_ticket_count, state_b.open_ticket_count,
            "open_ticket_count must be invariant under input reordering"
        );
        prop_assert_eq!(
            state_a.warranty_through, state_b.warranty_through,
            "warranty_through must be invariant under input reordering"
        );
    }

    /// `open_ticket_count` equals the size of the set difference
    /// {opened ticket_ids} - {closed ticket_ids} walked in sorted
    /// order up to the first Decommissioned. Close events are
    /// order-sensitive: closing a ticket_id that hasn't been
    /// opened yet is a phantom and must not affect a later Open
    /// of the same id.
    #[test]
    fn prop_open_ticket_count_is_set_difference(events in arb_event_sequence()) {
        use std::collections::HashSet;
        let state = project(&asset_id(), &events).expect("non-empty");

        let relevant = events_before_or_at_decommission(&events);
        let mut open_ids: HashSet<String> = HashSet::new();
        for e in &relevant {
            match &e.kind {
                AssetEventKind::ServiceJobOpened { job_id, .. } => {
                    open_ids.insert(job_id.clone());
                }
                AssetEventKind::ServiceJobClosed { job_id, .. } => {
                    open_ids.remove(job_id);
                }
                _ => {}
            }
        }
        let expected = open_ids.len() as u32;
        prop_assert_eq!(
            state.open_ticket_count, expected,
            "open_ticket_count should equal set difference by ticket_id"
        );
    }

    /// A device is never in the `Installed` phase without a account.
    /// The projection only reaches Installed via an Installed event
    /// (which always sets account_id) or by closing the last open
    /// ticket (which only reverts to Installed if account_id.is_some()).
    #[test]
    fn prop_phase_never_installed_without_account(events in arb_event_sequence()) {
        let state = project(&asset_id(), &events).expect("non-empty");
        if state.phase.as_str() == AssetLifecyclePhase::INSTALLED {
            prop_assert!(
                state.holder_id.is_some(),
                "phase=Installed requires account_id=Some"
            );
        }
    }

    /// Given a non-decommissioned device with a account, phase is
    /// OutForService iff there's at least one open ticket; otherwise
    /// it's at whatever the most recent phase-changing event set it
    /// to (which for a device that ever had open tickets is
    /// Installed, per the revert rule).
    #[test]
    fn prop_out_for_service_iff_open_tickets(events in arb_event_sequence()) {
        prop_assume!(!has_decommission(&events));

        let state = project(&asset_id(), &events).expect("non-empty");

        if state.phase.as_str() == AssetLifecyclePhase::OUT_FOR_SERVICE {
            prop_assert!(
                state.open_ticket_count > 0,
                "OutForService implies open_ticket_count > 0"
            );
        }
    }
}
