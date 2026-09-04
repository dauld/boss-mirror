//! In-memory adapter for `CadenceRepository` — the port-level test
//! double. Mirrors the Pg semantics that matter: the claim collapses
//! on a duplicate `firing_id`, and the outcome MERGES into `detail`
//! rather than replacing it.

use std::collections::HashMap;

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::port::{CadenceError, CadenceRepository};
use super::types::{CadenceRuleRow, LastFiring, NewFiring};

#[derive(Default)]
pub struct InMemoryCadence {
    rules: Vec<CadenceRuleRow>,
    firings: RwLock<HashMap<String, NewFiring>>,
}

impl InMemoryCadence {
    pub fn new(rules: Vec<CadenceRuleRow>) -> Self {
        Self {
            rules,
            firings: RwLock::new(HashMap::new()),
        }
    }

    /// Test visibility into a recorded firing.
    pub async fn firing(&self, id: &str) -> Option<NewFiring> {
        self.firings.read().await.get(id).cloned()
    }
}

#[async_trait]
impl CadenceRepository for InMemoryCadence {
    async fn active_rules(&self) -> Result<Vec<CadenceRuleRow>, CadenceError> {
        let mut out = self.rules.clone();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn last_firing(&self, rule: &str) -> Result<Option<LastFiring>, CadenceError> {
        let guard = self.firings.read().await;
        Ok(guard
            .values()
            .filter(|f| f.rule_name == rule)
            .max_by_key(|f| f.fired_at)
            .map(|f| LastFiring {
                firing_id: f.firing_id.clone(),
                fired_at: f.fired_at,
                // Mirrors the Postgres adapter's `detail->>'rc'`:
                // record_outcome merges rc into detail, so an absent key is
                // "no outcome recorded yet", not a failure.
                rc: f
                    .detail
                    .get("rc")
                    .and_then(serde_json::Value::as_i64)
                    .map(|v| v as i32),
            }))
    }

    async fn claim_firing(&self, new: &NewFiring) -> Result<bool, CadenceError> {
        let mut guard = self.firings.write().await;
        if guard.contains_key(&new.firing_id) {
            // Mirrors ON CONFLICT (firing_id) DO NOTHING.
            return Ok(false);
        }
        guard.insert(new.firing_id.clone(), new.clone());
        Ok(true)
    }

    async fn record_outcome(
        &self,
        firing_id: &str,
        rc: i32,
        runtime_secs: u64,
    ) -> Result<(), CadenceError> {
        let mut guard = self.firings.write().await;
        if let Some(f) = guard.get_mut(firing_id) {
            // Mirrors `detail || $2` — merge, don't replace.
            let obj = f.detail.as_object_mut();
            match obj {
                Some(map) => {
                    map.insert("rc".into(), serde_json::json!(rc));
                    map.insert("runtime_secs".into(), serde_json::json!(runtime_secs));
                }
                None => {
                    f.detail = serde_json::json!({ "rc": rc, "runtime_secs": runtime_secs });
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn firing(id: &str, rule: &str) -> NewFiring {
        NewFiring {
            firing_id: id.into(),
            rule_name: rule.into(),
            verb: "board".into(),
            basis: "queue-depth".into(),
            fired_at: Utc.with_ymd_and_hms(2026, 8, 14, 12, 0, 0).unwrap(),
            detail: serde_json::json!({ "dock_depth": 8 }),
        }
    }

    #[tokio::test]
    async fn claim_is_exactly_once() {
        let repo = InMemoryCadence::default();
        let f = firing("cadence:board:2026-08-14T12:00Z", "board");
        assert!(repo.claim_firing(&f).await.unwrap(), "first claim wins");
        assert!(
            !repo.claim_firing(&f).await.unwrap(),
            "second claim of the same window must lose — this is what \
             keeps a crashed-mid-verb conductor from re-running it"
        );
    }

    #[tokio::test]
    async fn outcome_merges_into_detail_and_keeps_claim_context() {
        let repo = InMemoryCadence::default();
        let f = firing("cadence:board:2026-08-14T12:00Z", "board");
        repo.claim_firing(&f).await.unwrap();
        repo.record_outcome(&f.firing_id, 0, 42).await.unwrap();

        let got = repo.firing(&f.firing_id).await.unwrap();
        assert_eq!(got.detail["rc"], 0);
        assert_eq!(got.detail["runtime_secs"], 42);
        // The dock depth that TRIGGERED the firing survives the merge;
        // replacing detail would lose why the rule fired at all.
        assert_eq!(got.detail["dock_depth"], 8);
    }

    #[tokio::test]
    async fn last_firing_picks_the_newest_and_is_none_when_unfired() {
        let repo = InMemoryCadence::default();
        assert!(repo.last_firing("board").await.unwrap().is_none());

        let mut older = firing("cadence:board:2026-08-14T10:00Z", "board");
        older.fired_at = Utc.with_ymd_and_hms(2026, 8, 14, 10, 0, 0).unwrap();
        let newer = firing("cadence:board:2026-08-14T12:00Z", "board");
        repo.claim_firing(&older).await.unwrap();
        repo.claim_firing(&newer).await.unwrap();

        let last = repo.last_firing("board").await.unwrap().unwrap();
        assert_eq!(last.firing_id, newer.firing_id);
    }

    #[tokio::test]
    async fn last_firing_is_scoped_to_its_own_rule() {
        let repo = InMemoryCadence::default();
        repo.claim_firing(&firing("cadence:board:1", "board"))
            .await
            .unwrap();
        assert!(repo.last_firing("reconcile").await.unwrap().is_none());
    }
}
