//! Pure projection: fold an event log into a `AssetCurrentState`.
//!
//! Two entry points:
//!
//! - `project(asset_id, &events)` — full reprojection from a complete
//!   event log. Used for cold rebuilds and the rare out-of-order path.
//! - `apply_event(state, open_ticket_ids, event)` — incremental update
//!   from a stored projection plus one new event. Used by the hot
//!   write path so each append doesn't have to read the asset's full
//!   history.
//!
//! Both functions share the same per-event match logic via
//! `apply_one`, so they can never disagree.

use std::collections::HashSet;

use chrono::NaiveDate;

use crate::types::{AssetCurrentState, AssetEvent, AssetEventKind, AssetId, AssetLifecyclePhase};

/// In-memory state used while folding events. Carries the open ticket
/// id set explicitly because the projection row only stores the count;
/// the set is what we need to know whether a Close is a no-op.
#[derive(Debug, Clone)]
struct ProjectionAccumulator {
    phase: AssetLifecyclePhase,
    sku: Option<String>,
    holder_kind: Option<String>,
    holder_id: Option<String>,
    warranty_through: Option<NaiveDate>,
    open_ticket_ids: HashSet<String>,
    first_seen: NaiveDate,
    last_event_at: NaiveDate,
    oem_serial: Option<String>,
}

impl ProjectionAccumulator {
    fn into_state(self, asset_id: &AssetId) -> AssetCurrentState {
        AssetCurrentState {
            asset_id: asset_id.clone(),
            sku: self.sku,
            phase: self.phase,
            holder_kind: self.holder_kind,
            holder_id: self.holder_id,
            warranty_through: self.warranty_through,
            open_ticket_count: self.open_ticket_ids.len() as u32,
            first_seen: self.first_seen,
            last_event_at: self.last_event_at,
            oem_serial: self.oem_serial,
        }
    }
}

/// One event applied to the accumulator. Returns true if the
/// projection has reached the terminal `Decommissioned` state and the
/// caller should stop applying further events.
fn apply_one(acc: &mut ProjectionAccumulator, e: &AssetEvent) -> bool {
    if e.ts > acc.last_event_at {
        acc.last_event_at = e.ts;
    }
    match &e.kind {
        AssetEventKind::Registered { oem_serial } => {
            acc.phase = AssetLifecyclePhase::REGISTERED.into();
            if let Some(ser) = oem_serial {
                acc.oem_serial = Some(ser.clone());
            }
        }
        AssetEventKind::Identified { sku: s } => {
            // Pure identification — sets the catalog model without
            // advancing custody phase.
            acc.sku = Some(s.clone());
        }
        AssetEventKind::Received {
            sku: s, oem_serial, ..
        } => {
            acc.phase = AssetLifecyclePhase::RECEIVED.into();
            // Only set the model when receipt knows it; a `None` here is
            // "received, not yet identified" and must not clobber an sku
            // an earlier `Identified` event already set.
            if let Some(sk) = s {
                acc.sku = Some(sk.clone());
            }
            if let Some(ser) = oem_serial {
                acc.oem_serial = Some(ser.clone());
            }
        }
        AssetEventKind::Shipped {
            holder_kind,
            holder_id,
        } => {
            acc.phase = AssetLifecyclePhase::SHIPPED.into();
            acc.holder_kind = Some(holder_kind.clone());
            acc.holder_id = Some(holder_id.clone());
        }
        AssetEventKind::Installed {
            holder_kind,
            holder_id,
        } => {
            acc.phase = AssetLifecyclePhase::INSTALLED.into();
            acc.holder_kind = Some(holder_kind.clone());
            acc.holder_id = Some(holder_id.clone());
        }
        // Ownership events stay account-valued (the buyer IS an
        // account); ownership implies custody handover in the current
        // flows, so they set the holder to that account.
        AssetEventKind::Sold { account_id: c, .. } => {
            acc.holder_kind = Some("account".into());
            acc.holder_id = Some(c.clone());
        }
        AssetEventKind::OwnershipTransferred { to_account_id, .. } => {
            acc.holder_kind = Some("account".into());
            acc.holder_id = Some(to_account_id.clone());
        }
        AssetEventKind::WarrantyStarted { through, .. } => {
            acc.warranty_through = Some(*through);
        }
        AssetEventKind::WarrantyExpired => {
            acc.warranty_through = None;
        }
        AssetEventKind::ServiceJobOpened { job_id, .. } => {
            acc.open_ticket_ids.insert(job_id.clone());
            // OutForService means "deployed at a account and currently
            // needs service." A ticket opened on a asset that was
            // never installed somewhere doesn't put it in the field —
            // leave the phase alone in that case.
            if acc.holder_id.is_some() {
                acc.phase = AssetLifecyclePhase::OUT_FOR_SERVICE.into();
            }
        }
        AssetEventKind::ServiceJobClosed { job_id, .. } => {
            // Only decrement if the ticket was actually open. A Close
            // without a matching Open is a phantom (duplicate, replay,
            // or out-of-order delivery) and must be a no-op — otherwise
            // a later Open with the same id would restore the count
            // incorrectly.
            acc.open_ticket_ids.remove(job_id);
            if acc.open_ticket_ids.is_empty() && acc.holder_id.is_some() {
                acc.phase = AssetLifecyclePhase::INSTALLED.into();
            }
        }
        AssetEventKind::Decommissioned { .. } => {
            acc.phase = AssetLifecyclePhase::DECOMMISSIONED.into();
            // Decommissioned is terminal: phase, account, warranty,
            // and the open-ticket id set are all frozen at the
            // moment of retirement. We deliberately do NOT clear
            // open_ticket_ids — the count is a historical metric
            // ("opened without ever being closed") that survives
            // retirement, and the projection_properties.rs proptest
            // pins this contract.
            return true;
        }
        AssetEventKind::PutAway { .. }
        | AssetEventKind::PartReplaced { .. }
        | AssetEventKind::WarrantyClaimed { .. } => {
            // These don't change phase/account/warranty summaries.
        }
    }
    false
}

/// Per-event ticket-table mutation, returned by `apply_event` so the
/// caller can keep `asset_open_tickets` in sync without re-deriving
/// it from scratch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TicketOp {
    /// No change to the open-tickets table for this event.
    Noop,
    /// Insert a row for a newly opened ticket.
    Open {
        ticket_id: String,
        summary: String,
        opened_on: NaiveDate,
    },
    /// Delete the row for this ticket id (no-op at the table layer if
    /// it wasn't actually open — `DELETE WHERE` matches zero rows).
    Close { ticket_id: String },
    /// Delete every open ticket row for this asset id. Decommissioning
    /// a asset closes everything implicitly.
    ClearAll,
}

/// Apply a single new event to an existing projection state plus the
/// known open ticket ids. Returns the new state and the ticket-table
/// operation that should accompany the upsert. The returned state's
/// `open_ticket_count` already reflects the ticket op.
///
/// Pass `None` for `existing` when this is the very first event for a
/// asset id. Pass an empty set for `existing_open_ticket_ids` if the
/// asset has no open tickets recorded.
///
/// Caller responsibility: only invoke this on the in-order fast path.
/// If the new event has an earlier `ts` than `existing.last_event_at`,
/// fall back to `project()` over the full event log instead.
pub fn apply_event(
    asset_id: &AssetId,
    existing: Option<&AssetCurrentState>,
    existing_open_ticket_ids: &HashSet<String>,
    event: &AssetEvent,
) -> (AssetCurrentState, TicketOp) {
    // Decommissioned is terminal: ignore the new event entirely.
    if let Some(state) = existing
        && state.phase.is_decommissioned()
    {
        return (state.clone(), TicketOp::Noop);
    }

    let mut acc = match existing {
        Some(state) => ProjectionAccumulator {
            phase: state.phase.clone(),
            sku: state.sku.clone(),
            holder_kind: state.holder_kind.clone(),
            holder_id: state.holder_id.clone(),
            warranty_through: state.warranty_through,
            open_ticket_ids: existing_open_ticket_ids.clone(),
            first_seen: state.first_seen,
            last_event_at: state.last_event_at,
            oem_serial: state.oem_serial.clone(),
        },
        None => ProjectionAccumulator {
            phase: AssetLifecyclePhase::REGISTERED.into(),
            sku: None,
            holder_kind: None,
            holder_id: None,
            warranty_through: None,
            open_ticket_ids: HashSet::new(),
            first_seen: event.ts,
            last_event_at: event.ts,
            oem_serial: None,
        },
    };

    // Compute the ticket op for the table side. We do this from the
    // raw event before the apply runs so we can capture the summary
    // and opened_on (the apply only sees the id).
    let ticket_op = match &event.kind {
        AssetEventKind::ServiceJobOpened { job_id, summary } => TicketOp::Open {
            ticket_id: job_id.clone(),
            summary: summary.clone(),
            opened_on: event.ts,
        },
        AssetEventKind::ServiceJobClosed { job_id, .. } => TicketOp::Close {
            ticket_id: job_id.clone(),
        },
        // Decommissioning does NOT delete the open ticket rows.
        // They survive retirement as historical state, matching the
        // proptest contract that open_ticket_count is "opened
        // without ever being closed".
        AssetEventKind::Decommissioned { .. } => TicketOp::Noop,
        _ => TicketOp::Noop,
    };

    apply_one(&mut acc, event);

    (acc.into_state(asset_id), ticket_op)
}

/// Replay `events` (any order; we sort by ts) to produce current state.
/// Returns None if the log is empty.
pub fn project(asset_id: &AssetId, events: &[AssetEvent]) -> Option<AssetCurrentState> {
    if events.is_empty() {
        return None;
    }

    let mut sorted: Vec<&AssetEvent> = events.iter().collect();
    sorted.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.0.cmp(&b.id.0)));

    let first_seen = sorted.first().unwrap().ts;
    let mut acc = ProjectionAccumulator {
        phase: AssetLifecyclePhase::REGISTERED.into(),
        sku: None,
        holder_kind: None,
        holder_id: None,
        warranty_through: None,
        open_ticket_ids: HashSet::new(),
        first_seen,
        last_event_at: first_seen,
        oem_serial: None,
    };

    for e in &sorted {
        if apply_one(&mut acc, e) {
            // Decommissioned is terminal: stop applying further
            // events. Late-arriving events after the decommission
            // date are ignored (the decommission "freezes" the
            // asset's observable state).
            break;
        }
    }

    // Re-pin last_event_at to the latest event we actually processed,
    // not necessarily the latest event in the input — if we hit
    // Decommissioned mid-stream we stopped early.
    Some(acc.into_state(asset_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AssetCondition, AssetEventId, IntakeSource, WarrantyCoverage};
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn evt(id: &str, ts: NaiveDate, kind: AssetEventKind) -> AssetEvent {
        AssetEvent {
            id: AssetEventId::new(id),
            asset_id: AssetId::new("LUM-3D-010000"),
            ts,
            actor_id: boss_core::actor::ActorId::Automation("test".into()),
            kind,
        }
    }

    #[test]
    fn empty_log_projects_to_none() {
        assert!(project(&AssetId::new("X"), &[]).is_none());
    }

    #[test]
    fn received_only_lands_in_received_phase() {
        let events = vec![evt(
            "1",
            d(2026, 1, 1),
            AssetEventKind::Received {
                sku: Some("Boss-TEST-2024".into()),
                source: IntakeSource::new("oem-new"),
                oem_serial: None,
            },
        )];
        let state = project(&AssetId::new("LUM-3D-010000"), &events).unwrap();
        assert_eq!(state.phase.as_str(), AssetLifecyclePhase::RECEIVED);
        assert_eq!(state.sku, Some("Boss-TEST-2024".into()));
        assert_eq!(state.holder_id, None);
        assert_eq!(state.first_seen, d(2026, 1, 1));
        assert_eq!(state.last_event_at, d(2026, 1, 1));
    }

    #[test]
    fn registered_event_creates_identity_only_asset() {
        // Identity-first: a Registered event with no sku materializes an
        // asset that exists at phase=Registered with no catalog model yet.
        let events = vec![evt(
            "1",
            d(2026, 1, 1),
            AssetEventKind::Registered { oem_serial: None },
        )];
        let state = project(&AssetId::new("SYS-00000001"), &events).unwrap();
        assert_eq!(state.phase.as_str(), AssetLifecyclePhase::REGISTERED);
        assert_eq!(state.sku, None, "an unidentified asset has no model");
        assert_eq!(state.first_seen, d(2026, 1, 1));
    }

    #[test]
    fn identified_event_sets_sku_after_registration() {
        // Register now, identify later — the identity-first path.
        let events = vec![
            evt(
                "1",
                d(2026, 1, 1),
                AssetEventKind::Registered { oem_serial: None },
            ),
            evt(
                "2",
                d(2026, 1, 3),
                AssetEventKind::Identified {
                    sku: "FERM-250".into(),
                },
            ),
        ];
        let state = project(&AssetId::new("SYS-00000001"), &events).unwrap();
        assert_eq!(state.sku, Some("FERM-250".into()));
        // Identification is not custody — phase stays Registered until a
        // Received (or later lifecycle) event advances it.
        assert_eq!(state.phase.as_str(), AssetLifecyclePhase::REGISTERED);
    }

    #[test]
    fn received_without_sku_stays_unidentified() {
        // Receipt is pure custody: a None sku does not clobber, and the
        // asset is in custody but still unidentified.
        let events = vec![evt(
            "1",
            d(2026, 1, 1),
            AssetEventKind::Received {
                sku: None,
                source: IntakeSource::new("used-trade-in"),
                oem_serial: Some("SN-ABC".into()),
            },
        )];
        let state = project(&AssetId::new("SYS-00000001"), &events).unwrap();
        assert_eq!(state.phase.as_str(), AssetLifecyclePhase::RECEIVED);
        assert_eq!(state.sku, None);
        assert_eq!(state.oem_serial, Some("SN-ABC".into()));
    }

    #[test]
    fn received_none_does_not_clobber_prior_identification() {
        // Identified before Received: the later None-sku receipt must
        // preserve the model set earlier.
        let events = vec![
            evt(
                "1",
                d(2026, 1, 1),
                AssetEventKind::Identified {
                    sku: "FERM-250".into(),
                },
            ),
            evt(
                "2",
                d(2026, 1, 2),
                AssetEventKind::Received {
                    sku: None,
                    source: IntakeSource::new("used-trade-in"),
                    oem_serial: None,
                },
            ),
        ];
        let state = project(&AssetId::new("SYS-00000001"), &events).unwrap();
        assert_eq!(state.sku, Some("FERM-250".into()));
        assert_eq!(state.phase.as_str(), AssetLifecyclePhase::RECEIVED);
    }

    #[test]
    fn full_sold_path_ends_installed_with_account_and_warranty() {
        let events = vec![
            evt(
                "1",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ),
            evt(
                "2",
                d(2026, 1, 14),
                AssetEventKind::PutAway { bin: "A-01".into() },
            ),
            evt(
                "3",
                d(2026, 1, 20),
                AssetEventKind::Sold {
                    account_id: "account-007".into(),
                    price_cents: 8_500_000,
                    currency: "USD".into(),
                    order_id: None,
                    condition: AssetCondition::new("new"),
                },
            ),
            evt(
                "4",
                d(2026, 1, 25),
                AssetEventKind::Shipped {
                    holder_kind: "account".into(),
                    holder_id: "account-007".into(),
                },
            ),
            evt(
                "5",
                d(2026, 2, 1),
                AssetEventKind::Installed {
                    holder_kind: "account".into(),
                    holder_id: "account-007".into(),
                },
            ),
            evt(
                "6",
                d(2026, 2, 1),
                AssetEventKind::WarrantyStarted {
                    through: d(2028, 2, 1),
                    coverage: WarrantyCoverage::new("standard"),
                },
            ),
        ];
        let state = project(&AssetId::new("LUM-3D-010000"), &events).unwrap();
        assert_eq!(state.phase.as_str(), AssetLifecyclePhase::INSTALLED);
        assert_eq!(state.holder_kind.as_deref(), Some("account"));
        assert_eq!(state.holder_id.as_deref(), Some("account-007"));
        assert_eq!(state.warranty_through, Some(d(2028, 2, 1)));
        assert_eq!(state.open_ticket_count, 0);
    }

    #[test]
    fn open_ticket_flips_phase_to_out_for_service_and_back() {
        let base_account = "account-007".to_string();
        let events = vec![
            evt(
                "1",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ),
            evt(
                "2",
                d(2026, 2, 1),
                AssetEventKind::Installed {
                    holder_kind: "account".into(),
                    holder_id: base_account.clone(),
                },
            ),
            evt(
                "3",
                d(2026, 3, 1),
                AssetEventKind::ServiceJobOpened {
                    job_id: "tkt-1".into(),
                    summary: "x".into(),
                },
            ),
        ];
        let after_open = project(&AssetId::new("x"), &events).unwrap();
        assert_eq!(
            after_open.phase.as_str(),
            AssetLifecyclePhase::OUT_FOR_SERVICE
        );
        assert_eq!(after_open.open_ticket_count, 1);

        let mut events2 = events.clone();
        events2.push(evt(
            "4",
            d(2026, 3, 5),
            AssetEventKind::ServiceJobClosed {
                job_id: "tkt-1".into(),
                turnaround_days: 4,
            },
        ));
        let after_close = project(&AssetId::new("x"), &events2).unwrap();
        assert_eq!(after_close.phase.as_str(), AssetLifecyclePhase::INSTALLED);
        assert_eq!(after_close.open_ticket_count, 0);
    }

    #[test]
    fn closing_every_ticket_reverts_phase_to_installed() {
        // The stated business invariant: "Assets in OutForService
        // with 0 open tickets revert to Installed." This test locks
        // it in for the multi-ticket case with out-of-order close
        // events, which the single-ticket round-trip test doesn't
        // exercise.
        let evts = vec![
            evt(
                "1",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ),
            evt(
                "2",
                d(2026, 2, 1),
                AssetEventKind::Installed {
                    holder_kind: "account".into(),
                    holder_id: "account-99".into(),
                },
            ),
            evt(
                "3",
                d(2026, 3, 1),
                AssetEventKind::ServiceJobOpened {
                    job_id: "a".into(),
                    summary: "cooling alarm".into(),
                },
            ),
            evt(
                "4",
                d(2026, 3, 2),
                AssetEventKind::ServiceJobOpened {
                    job_id: "b".into(),
                    summary: "power loss".into(),
                },
            ),
            evt(
                "5",
                d(2026, 3, 3),
                AssetEventKind::ServiceJobOpened {
                    job_id: "c".into(),
                    summary: "intermittent pulse".into(),
                },
            ),
            // Close in arbitrary order.
            evt(
                "6",
                d(2026, 3, 5),
                AssetEventKind::ServiceJobClosed {
                    job_id: "b".into(),
                    turnaround_days: 3,
                },
            ),
            evt(
                "7",
                d(2026, 3, 6),
                AssetEventKind::ServiceJobClosed {
                    job_id: "a".into(),
                    turnaround_days: 5,
                },
            ),
            evt(
                "8",
                d(2026, 3, 10),
                AssetEventKind::ServiceJobClosed {
                    job_id: "c".into(),
                    turnaround_days: 7,
                },
            ),
        ];
        let state = project(&AssetId::new("x"), &evts).unwrap();
        assert_eq!(state.open_ticket_count, 0);
        assert_eq!(
            state.phase.as_str(),
            AssetLifecyclePhase::INSTALLED,
            "asset with 0 open tickets must revert from OutForService to Installed"
        );
        assert_eq!(state.holder_id.as_deref(), Some("account-99"));
    }

    #[test]
    fn partial_close_keeps_out_for_service() {
        // Two open, one closed — phase must stay OutForService
        // because at least one ticket is still open. Paired with
        // the test above to pin both sides of the count>0/count==0
        // branch.
        let evts = vec![
            evt(
                "1",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ),
            evt(
                "2",
                d(2026, 2, 1),
                AssetEventKind::Installed {
                    holder_kind: "account".into(),
                    holder_id: "account-99".into(),
                },
            ),
            evt(
                "3",
                d(2026, 3, 1),
                AssetEventKind::ServiceJobOpened {
                    job_id: "a".into(),
                    summary: "".into(),
                },
            ),
            evt(
                "4",
                d(2026, 3, 2),
                AssetEventKind::ServiceJobOpened {
                    job_id: "b".into(),
                    summary: "".into(),
                },
            ),
            evt(
                "5",
                d(2026, 3, 5),
                AssetEventKind::ServiceJobClosed {
                    job_id: "a".into(),
                    turnaround_days: 4,
                },
            ),
        ];
        let state = project(&AssetId::new("x"), &evts).unwrap();
        assert_eq!(state.open_ticket_count, 1);
        assert_eq!(
            state.phase.as_str(),
            AssetLifecyclePhase::OUT_FOR_SERVICE,
            "asset with 1 remaining open ticket must stay OutForService"
        );
    }

    #[test]
    fn multiple_open_tickets_decrement_correctly() {
        let evts = vec![
            evt(
                "1",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ),
            evt(
                "2",
                d(2026, 2, 1),
                AssetEventKind::Installed {
                    holder_kind: "account".into(),
                    holder_id: "c".into(),
                },
            ),
            evt(
                "3",
                d(2026, 3, 1),
                AssetEventKind::ServiceJobOpened {
                    job_id: "a".into(),
                    summary: "".into(),
                },
            ),
            evt(
                "4",
                d(2026, 3, 2),
                AssetEventKind::ServiceJobOpened {
                    job_id: "b".into(),
                    summary: "".into(),
                },
            ),
            evt(
                "5",
                d(2026, 3, 5),
                AssetEventKind::ServiceJobClosed {
                    job_id: "a".into(),
                    turnaround_days: 4,
                },
            ),
        ];
        let state = project(&AssetId::new("x"), &evts).unwrap();
        assert_eq!(state.open_ticket_count, 1);
        assert_eq!(state.phase.as_str(), AssetLifecyclePhase::OUT_FOR_SERVICE);
    }

    #[test]
    fn installed_carries_a_typed_holder_the_brewery_shape() {
        // Q5: equipment installed AT a location — the pair the audit
        // found stuffed into account_id (170/170 brewery assets held
        // location ids in an account column).
        let events = vec![
            evt(
                "1",
                d(2026, 1, 1),
                AssetEventKind::Registered { oem_serial: None },
            ),
            evt(
                "2",
                d(2026, 1, 2),
                AssetEventKind::Installed {
                    holder_kind: "location".into(),
                    holder_id: "loc-brewery-brewhouse".into(),
                },
            ),
        ];
        let state = project(&AssetId::new("SYS-00000001"), &events).unwrap();
        assert_eq!(state.phase.as_str(), AssetLifecyclePhase::INSTALLED);
        assert_eq!(state.holder_kind.as_deref(), Some("location"));
        assert_eq!(state.holder_id.as_deref(), Some("loc-brewery-brewhouse"));
    }

    #[test]
    fn decommissioned_is_sticky() {
        let evts = vec![
            evt(
                "1",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("used-trade-in"),
                    oem_serial: None,
                },
            ),
            evt(
                "2",
                d(2026, 6, 1),
                AssetEventKind::Decommissioned {
                    reason: "eol".into(),
                },
            ),
        ];
        let state = project(&AssetId::new("x"), &evts).unwrap();
        assert_eq!(state.phase.as_str(), AssetLifecyclePhase::DECOMMISSIONED);
    }

    #[test]
    fn warranty_expired_clears_through_date() {
        let evts = vec![
            evt(
                "1",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ),
            evt(
                "2",
                d(2026, 2, 1),
                AssetEventKind::WarrantyStarted {
                    through: d(2028, 2, 1),
                    coverage: WarrantyCoverage::new("standard"),
                },
            ),
            evt("3", d(2028, 2, 1), AssetEventKind::WarrantyExpired),
        ];
        let state = project(&AssetId::new("x"), &evts).unwrap();
        assert_eq!(state.warranty_through, None);
    }

    #[test]
    fn events_are_sorted_before_projection() {
        // Give events in reverse order; projection should still work.
        let evts = vec![
            evt(
                "3",
                d(2026, 2, 1),
                AssetEventKind::Installed {
                    holder_kind: "account".into(),
                    holder_id: "c".into(),
                },
            ),
            evt(
                "1",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ),
            evt(
                "2",
                d(2026, 1, 14),
                AssetEventKind::PutAway { bin: "A-01".into() },
            ),
        ];
        let state = project(&AssetId::new("x"), &evts).unwrap();
        assert_eq!(state.phase.as_str(), AssetLifecyclePhase::INSTALLED);
        assert_eq!(state.first_seen, d(2026, 1, 1));
        assert_eq!(state.last_event_at, d(2026, 2, 1));
    }
}
