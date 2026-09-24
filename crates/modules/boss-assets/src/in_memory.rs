//! In-memory adapter for the `AssetsRepository` port.
//!
//! Used by tests and by dev/demo environments that don't need persistence.
//! Keeps events in a per-asset Vec and recomputes current state on read.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::port::{AssetsError, AssetsRepository};
use crate::project::project;
use crate::types::{AssetCurrentState, AssetEvent, AssetEventKind, AssetId};

#[derive(Default)]
pub struct InMemoryAssets {
    inner: Mutex<State>,
}

#[derive(Default)]
struct State {
    events: HashMap<AssetId, Vec<AssetEvent>>,
    seen_ids: HashSet<String>,
}

impl InMemoryAssets {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed from a prebuilt list of events (for demo data). Events are
    /// grouped by asset id; duplicates are rejected.
    pub fn with_events(events: impl IntoIterator<Item = AssetEvent>) -> Result<Self, AssetsError> {
        let assets = Self::new();
        {
            let mut state = assets.inner.lock().expect("poisoned lock");
            for e in events {
                if !state.seen_ids.insert(e.id.0.clone()) {
                    return Err(AssetsError::DuplicateEvent(e.id.0.clone()));
                }
                state.events.entry(e.asset_id.clone()).or_default().push(e);
            }
            // Keep per-asset logs chronologically sorted.
            for log in state.events.values_mut() {
                log.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.0.cmp(&b.id.0)));
            }
        }
        Ok(assets)
    }
}

#[async_trait]
impl AssetsRepository for InMemoryAssets {
    async fn append(&self, event: AssetEvent) -> Result<(), AssetsError> {
        let mut state = self.inner.lock().expect("poisoned lock");
        if !state.seen_ids.insert(event.id.0.clone()) {
            return Err(AssetsError::DuplicateEvent(event.id.0.clone()));
        }
        let log = state.events.entry(event.asset_id.clone()).or_default();
        log.push(event);
        log.sort_by(|a, b| a.ts.cmp(&b.ts).then_with(|| a.id.0.cmp(&b.id.0)));
        Ok(())
    }

    async fn events_for(&self, asset_id: &AssetId) -> Result<Vec<AssetEvent>, AssetsError> {
        let state = self.inner.lock().expect("poisoned lock");
        Ok(state.events.get(asset_id).cloned().unwrap_or_default())
    }

    async fn current_state(
        &self,
        asset_id: &AssetId,
    ) -> Result<Option<AssetCurrentState>, AssetsError> {
        let events = self.events_for(asset_id).await?;
        Ok(project(asset_id, &events))
    }

    async fn all_asset_ids(&self) -> Result<Vec<AssetId>, AssetsError> {
        let state = self.inner.lock().expect("poisoned lock");
        Ok(state.events.keys().cloned().collect())
    }

    async fn list_asset_ids(
        &self,
        limit: i64,
        offset: i64,
    ) -> Result<(Vec<AssetId>, i64), AssetsError> {
        let state = self.inner.lock().expect("poisoned lock");
        let mut all: Vec<AssetId> = state.events.keys().cloned().collect();
        all.sort_by(|a, b| a.0.cmp(&b.0));
        let total = all.len() as i64;
        let start = (offset as usize).min(all.len());
        let end = (start + limit as usize).min(all.len());
        Ok((all[start..end].to_vec(), total))
    }

    async fn list_assets(
        &self,
        limit: i64,
        offset: i64,
        account_id: Option<&str>,
    ) -> Result<(Vec<AssetCurrentState>, i64), AssetsError> {
        let state = self.inner.lock().expect("poisoned lock");
        let mut assets: Vec<AssetCurrentState> = Vec::new();
        for (asset_id, events) in &state.events {
            if let Some(cs) = project(asset_id, events) {
                if account_id.is_some_and(|p| {
                    cs.holder_kind.as_deref() != Some("account")
                        || cs.holder_id.as_deref() != Some(p)
                }) {
                    continue;
                }
                assets.push(cs);
            }
        }
        assets.sort_by_key(|s| std::cmp::Reverse(s.last_event_at));
        let total = assets.len() as i64;
        let start = (offset as usize).min(assets.len());
        let end = (start + limit as usize).min(assets.len());
        Ok((assets[start..end].to_vec(), total))
    }

    async fn active_asset_count_for_sku(&self, sku: &str) -> Result<u64, AssetsError> {
        let state = self.inner.lock().expect("poisoned lock");
        let mut count: u64 = 0;
        for events in state.events.values() {
            let Some(first) = events.first() else {
                continue;
            };
            let Some(cs) = project(&first.asset_id, events) else {
                continue;
            };
            if cs.sku.as_deref() == Some(sku) && !cs.phase.is_decommissioned() {
                count += 1;
            }
        }
        Ok(count)
    }

    async fn open_ticket_count_for_account(&self, account_id: &str) -> Result<u64, AssetsError> {
        let state = self.inner.lock().expect("poisoned lock");

        let mut closed_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for events in state.events.values() {
            for e in events {
                if let AssetEventKind::ServiceJobClosed { job_id, .. } = &e.kind {
                    closed_ids.insert(job_id);
                }
            }
        }

        let mut count: u64 = 0;
        for events in state.events.values() {
            let Some(first) = events.first() else {
                continue;
            };
            let cs = project(&first.asset_id, events);
            let held_by_account = cs.and_then(|c| {
                (c.holder_kind.as_deref() == Some("account"))
                    .then_some(c.holder_id)
                    .flatten()
            });
            if held_by_account.as_deref() != Some(account_id) {
                continue;
            }
            for e in events {
                if let AssetEventKind::ServiceJobOpened { job_id, .. } = &e.kind
                    && !closed_ids.contains(job_id.as_str())
                {
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    async fn assets_summary(
        &self,
        _today: chrono::NaiveDate,
    ) -> Result<crate::types::AssetsSummary, AssetsError> {
        // In-memory test stub: walk the in-memory projections and tally
        // phases + skus. Not used in production, but keeps the tests
        // runnable without requiring a Postgres backend.
        use crate::types::{AssetLifecyclePhase, AssetsSummary, PhaseRollup, SkuRollup};
        let state = self.inner.lock().expect("poisoned lock");
        let mut phase_map: std::collections::HashMap<String, i64> =
            std::collections::HashMap::new();
        let mut sku_map: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
        for events in state.events.values() {
            let Some(first) = events.first() else {
                continue;
            };
            let Some(cs) = project(&first.asset_id, events) else {
                continue;
            };
            *phase_map.entry(cs.phase.as_str().to_string()).or_insert(0) += 1;
            // Only identified assets bucket into a per-model rollup; an
            // unidentified (Registered) asset has no model to count under.
            if !cs.phase.is_decommissioned()
                && let Some(sku) = &cs.sku
            {
                *sku_map.entry(sku.clone()).or_insert(0) += 1;
            }
        }
        let phase_counts: Vec<PhaseRollup> = AssetLifecyclePhase::ORDER
            .iter()
            .map(|p| PhaseRollup {
                phase: p.to_string(),
                count: phase_map.get(*p).copied().unwrap_or(0),
            })
            .collect();
        let total_systems: i64 = phase_counts.iter().map(|p| p.count).sum();
        let in_field_count: i64 = phase_counts
            .iter()
            .filter(|p| p.phase != "decommissioned")
            .map(|p| p.count)
            .sum();
        let mut sku_counts: Vec<SkuRollup> = sku_map
            .into_iter()
            .map(|(sku, count)| SkuRollup { sku, count })
            .collect();
        sku_counts.sort_by_key(|s| std::cmp::Reverse(s.count));
        Ok(AssetsSummary {
            phase_counts,
            total_systems,
            in_field_count,
            open_tickets_total: 0,
            sku_counts,
            warranty_expiring_30d: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AssetCondition, AssetEventId, AssetEventKind, AssetLifecyclePhase, IntakeSource,
        WarrantyCoverage,
    };
    use chrono::NaiveDate;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn evt(id: &str, serial: &str, ts: NaiveDate, kind: AssetEventKind) -> AssetEvent {
        AssetEvent {
            id: AssetEventId::new(id),
            asset_id: AssetId::new(serial),
            ts,
            actor_id: boss_core::actor::ActorId::Automation("test".into()),
            kind,
        }
    }

    #[tokio::test]
    async fn append_then_read_back_events() {
        let assets = InMemoryAssets::new();
        let s = AssetId::new("SN-1");
        assets
            .append(evt(
                "e1",
                "SN-1",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ))
            .await
            .unwrap();

        let events = assets.events_for(&s).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].id.0, "e1");
    }

    #[tokio::test]
    async fn duplicate_event_id_is_rejected() {
        let assets = InMemoryAssets::new();
        assets
            .append(evt(
                "e1",
                "A",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ))
            .await
            .unwrap();
        let err = assets
            .append(evt(
                "e1",
                "B",
                d(2026, 1, 2),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("buyback"),
                    oem_serial: None,
                },
            ))
            .await
            .unwrap_err();
        assert!(matches!(err, AssetsError::DuplicateEvent(_)));
    }

    #[tokio::test]
    async fn unknown_asset_id_returns_empty_log_and_none_state() {
        let assets = InMemoryAssets::new();
        let unknown = AssetId::new("ghost");
        assert!(assets.events_for(&unknown).await.unwrap().is_empty());
        assert!(assets.current_state(&unknown).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn current_state_projects_from_appended_events() {
        let assets = InMemoryAssets::new();
        let s = AssetId::new("SN-1");
        for (id, ts, kind) in [
            (
                "e1",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ),
            (
                "e2",
                d(2026, 1, 14),
                AssetEventKind::PutAway { bin: "A-01".into() },
            ),
            (
                "e3",
                d(2026, 2, 1),
                AssetEventKind::Sold {
                    account_id: "account-1".into(),
                    price_cents: 8_500_000,
                    currency: "USD".into(),
                    order_id: None,
                    condition: AssetCondition::new("new"),
                },
            ),
            (
                "e4",
                d(2026, 2, 10),
                AssetEventKind::Installed {
                    holder_kind: "account".into(),
                    holder_id: "account-1".into(),
                },
            ),
            (
                "e5",
                d(2026, 2, 10),
                AssetEventKind::WarrantyStarted {
                    through: d(2028, 2, 10),
                    coverage: WarrantyCoverage::new("standard"),
                },
            ),
        ] {
            assets.append(evt(id, "SN-1", ts, kind)).await.unwrap();
        }

        let state = assets.current_state(&s).await.unwrap().unwrap();
        assert_eq!(state.phase.as_str(), AssetLifecyclePhase::INSTALLED);
        assert_eq!(state.holder_id.as_deref(), Some("account-1"));
        assert_eq!(state.warranty_through, Some(d(2028, 2, 10)));
    }

    #[tokio::test]
    async fn all_asset_ids_returns_every_known_id() {
        let assets = InMemoryAssets::new();
        assets
            .append(evt(
                "e1",
                "A",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ))
            .await
            .unwrap();
        assets
            .append(evt(
                "e2",
                "B",
                d(2026, 1, 2),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("buyback"),
                    oem_serial: None,
                },
            ))
            .await
            .unwrap();

        let mut ids = assets.all_asset_ids().await.unwrap();
        ids.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(ids, vec![AssetId::new("A"), AssetId::new("B")]);
    }

    #[tokio::test]
    async fn with_events_bulk_loads_deterministically() {
        let events = vec![
            evt(
                "e1",
                "SN-1",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ),
            evt(
                "e2",
                "SN-2",
                d(2026, 1, 2),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("buyback"),
                    oem_serial: None,
                },
            ),
        ];
        let assets = InMemoryAssets::with_events(events).unwrap();
        assert_eq!(assets.all_asset_ids().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn events_are_returned_in_chronological_order() {
        let assets = InMemoryAssets::new();
        // Append out of order.
        assets
            .append(evt(
                "e2",
                "SN-1",
                d(2026, 2, 1),
                AssetEventKind::Installed {
                    holder_kind: "account".into(),
                    holder_id: "c".into(),
                },
            ))
            .await
            .unwrap();
        assets
            .append(evt(
                "e1",
                "SN-1",
                d(2026, 1, 1),
                AssetEventKind::Received {
                    sku: Some("Boss-TEST-2024".into()),
                    source: IntakeSource::new("oem-new"),
                    oem_serial: None,
                },
            ))
            .await
            .unwrap();

        let events = assets.events_for(&AssetId::new("SN-1")).await.unwrap();
        assert_eq!(events[0].id.0, "e1");
        assert_eq!(events[1].id.0, "e2");
    }
}
