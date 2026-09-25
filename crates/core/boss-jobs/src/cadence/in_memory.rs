//! In-memory adapter for `CadenceRepository` — the port-level test
//! double. Mirrors the Pg semantics that matter: the claim collapses
//! on a duplicate `firing_id`, and the outcome MERGES into `detail`
//! rather than replacing it.

use std::collections::HashMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use super::port::{CadenceError, CadenceRegistry, CadenceRepository};
use super::types::{CadenceRuleRow, CadenceRuleSpec, LastFiring, NewFiring};
use crate::registry::WorkflowStatus;

#[derive(Default)]
pub struct InMemoryCadence {
    /// The whole lineage, every version of every name — what
    /// `active_rules` filters and the registry half reads and writes,
    /// so a rule the seed publishes is a rule the conductor's read
    /// then serves, as in Postgres.
    rules: RwLock<Vec<CadenceRuleSpec>>,
    firings: RwLock<HashMap<String, NewFiring>>,
    /// What the Pg adapter records into `event_outbox` inside the
    /// row transaction, this adapter collects here — same events at
    /// the same write points (the `InMemoryStations` shape), so tests
    /// assert the event contract through the port without a database.
    recorded: std::sync::Mutex<Vec<boss_core::event::Event>>,
}

impl InMemoryCadence {
    /// Each row lands as version 1, active — the shape a fresh
    /// deployment's first migration gave every rule.
    pub fn new(rules: Vec<CadenceRuleRow>) -> Self {
        let lineage = rules
            .into_iter()
            .map(|row| CadenceRuleSpec {
                version: 1,
                status: WorkflowStatus::Active,
                row,
                created_at: DateTime::<Utc>::UNIX_EPOCH,
            })
            .collect();
        Self {
            rules: RwLock::new(lineage),
            firings: RwLock::new(HashMap::new()),
            recorded: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Test visibility into a recorded firing.
    pub async fn firing(&self, id: &str) -> Option<NewFiring> {
        self.firings.read().await.get(id).cloned()
    }

    /// Every event a registry write recorded, in write order — the
    /// in-memory stand-in for `SELECT ... FROM event_outbox`.
    pub fn recorded_events(&self) -> Vec<boss_core::event::Event> {
        self.recorded
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|p| p.into_inner().clone())
    }

    fn record(&self, event: boss_core::event::Event) {
        match self.recorded.lock() {
            Ok(mut g) => g.push(event),
            Err(p) => p.into_inner().push(event),
        }
    }
}

/// The version a publish must exceed: the newest the lineage holds,
/// any status, or 0 for a name never held (so versions start at 1).
/// ONE rule for both adapters' conflict arm; the Pg adapter asks the
/// database the same question with `MAX(version)`.
pub(super) fn newest_version(lineage: impl Iterator<Item = i32>) -> i32 {
    lineage.max().unwrap_or(0)
}

/// The conflict a publish at or below the newest version answers —
/// worded once, so the door's 409 reads the same over either adapter.
pub(super) fn not_above(name: &str, version: i32, newest: i32) -> CadenceError {
    CadenceError::Conflict(format!(
        "{name}@{version} is not above the newest version of its lineage (v{newest}); \
         a publish is a version bump — declare v{}",
        newest + 1
    ))
}

#[async_trait]
impl CadenceRepository for InMemoryCadence {
    async fn active_rules(&self) -> Result<Vec<CadenceRuleRow>, CadenceError> {
        let mut out: Vec<CadenceRuleRow> = self
            .rules
            .read()
            .await
            .iter()
            .filter(|r| r.status == WorkflowStatus::Active)
            .map(|r| r.row.clone())
            .collect();
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

#[async_trait]
impl CadenceRegistry for InMemoryCadence {
    async fn live_versions(&self, name: &str) -> Result<Vec<CadenceRuleSpec>, CadenceError> {
        let mut out: Vec<CadenceRuleSpec> = self
            .rules
            .read()
            .await
            .iter()
            .filter(|r| r.name() == name)
            .cloned()
            .collect();
        out.sort_by_key(|r| r.version);
        Ok(out)
    }

    async fn publish_declared(
        &self,
        mut spec: CadenceRuleSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<CadenceRuleSpec, CadenceError> {
        let mut rules = self.rules.write().await;
        let newest = newest_version(
            rules
                .iter()
                .filter(|r| r.name() == spec.name())
                .map(|r| r.version),
        );
        if spec.version <= newest {
            return Err(not_above(spec.name(), spec.version, newest));
        }
        // Mirrors the Pg adapter: retire by name, then insert.
        for r in rules.iter_mut() {
            if r.name() == spec.name() && r.status == WorkflowStatus::Active {
                r.status = WorkflowStatus::Retired;
            }
        }
        spec.status = WorkflowStatus::Active;
        spec.created_at = now;
        rules.push(spec.clone());
        self.record(crate::events::cadence_registry_event(
            crate::events::CADENCE_PUBLISHED,
            actor,
            &spec,
        ));
        Ok(spec)
    }

    async fn retire(
        &self,
        name: &str,
        actor: &boss_core::actor::ActorId,
        _now: DateTime<Utc>,
    ) -> Result<Option<CadenceRuleSpec>, CadenceError> {
        let mut rules = self.rules.write().await;
        let Some(row) = rules
            .iter_mut()
            .find(|r| r.name() == name && r.status == WorkflowStatus::Active)
        else {
            // Nothing active — nothing written, nothing recorded.
            return Ok(None);
        };
        row.status = WorkflowStatus::Retired;
        let retired = row.clone();
        self.record(crate::events::cadence_registry_event(
            crate::events::CADENCE_RETIRED,
            actor,
            &retired,
        ));
        Ok(Some(retired))
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

    // ------------------------------------------------------------------
    // The registry half's write contract (backlog 13d1fff3): every
    // write records its event, a retire returns the row it retired
    // and records nothing when nothing was active, and a publish must
    // be ABOVE the newest version of its lineage.
    // ------------------------------------------------------------------

    fn actor() -> boss_core::actor::ActorId {
        boss_core::actor::ActorId::Human("emp-david".into())
    }

    fn declared(name: &str, version: i32, every: i32) -> CadenceRuleSpec {
        CadenceRuleSpec {
            version,
            status: WorkflowStatus::Active,
            row: CadenceRuleRow {
                name: name.into(),
                verb: "reconcile".into(),
                basis: "wall".into(),
                every_minutes: Some(every),
                at_times: None,
                min_dock_depth: None,
                cooldown_minutes: None,
                cadence: None,
                anchor_date: None,
                business_calendar: None,
                regate_hold_minutes: None,
            },
            created_at: DateTime::<Utc>::UNIX_EPOCH,
        }
    }

    #[tokio::test]
    async fn a_retire_returns_the_row_it_retired_and_records_it() {
        let repo = InMemoryCadence::new(vec![declared("train-reconcile", 1, 10).row]);
        let now = Utc.with_ymd_and_hms(2026, 9, 18, 17, 0, 0).unwrap();
        let retired = repo
            .retire("train-reconcile", &actor(), now)
            .await
            .unwrap()
            .expect("the active row is retired and returned");
        assert_eq!(retired.status, WorkflowStatus::Retired);
        assert_eq!(retired.version, 1);
        assert!(
            repo.active_rules().await.unwrap().is_empty(),
            "the conductor's read no longer serves it"
        );
        let lineage = repo.live_versions("train-reconcile").await.unwrap();
        assert_eq!(lineage.len(), 1, "a retire keeps the row as history");
        assert_eq!(lineage[0].status, WorkflowStatus::Retired);
        let events = repo.recorded_events();
        assert_eq!(events.len(), 1, "exactly one jobs.cadence.retired");
        assert_eq!(events[0].kind, crate::events::CADENCE_RETIRED);
        assert_eq!(events[0].payload["name"], "train-reconcile");
        assert_eq!(events[0].payload["status"], "retired");
        assert_eq!(events[0].payload["_actor"], "emp-david");
    }

    #[tokio::test]
    async fn a_retire_of_nothing_active_is_none_and_records_nothing() {
        let repo = InMemoryCadence::default();
        let now = Utc.with_ymd_and_hms(2026, 9, 18, 17, 0, 0).unwrap();
        assert!(
            repo.retire("never-declared", &actor(), now)
                .await
                .unwrap()
                .is_none()
        );
        // Retired once, retiring again is the same answer.
        repo.publish_declared(declared("train-reconcile", 1, 10), &actor(), now)
            .await
            .unwrap();
        repo.retire("train-reconcile", &actor(), now)
            .await
            .unwrap()
            .expect("first retire");
        assert!(
            repo.retire("train-reconcile", &actor(), now)
                .await
                .unwrap()
                .is_none(),
            "an already-retired name has nothing active to retire"
        );
        let kinds: Vec<String> = repo.recorded_events().into_iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                crate::events::CADENCE_PUBLISHED,
                crate::events::CADENCE_RETIRED
            ],
            "the no-op retires recorded nothing"
        );
    }

    #[tokio::test]
    async fn a_publish_records_its_event_and_retires_the_prior_active_row() {
        let repo = InMemoryCadence::default();
        let now = Utc.with_ymd_and_hms(2026, 9, 18, 17, 0, 0).unwrap();
        repo.publish_declared(declared("train-reconcile", 1, 10), &actor(), now)
            .await
            .unwrap();
        let v2 = repo
            .publish_declared(declared("train-reconcile", 2, 5), &actor(), now)
            .await
            .unwrap();
        assert_eq!(v2.created_at, now, "stamped by the door's clock");
        let lineage = repo.live_versions("train-reconcile").await.unwrap();
        assert_eq!(
            lineage
                .iter()
                .map(|r| (r.version, r.status))
                .collect::<Vec<_>>(),
            vec![(1, WorkflowStatus::Retired), (2, WorkflowStatus::Active)]
        );
        let events = repo.recorded_events();
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .all(|e| e.kind == crate::events::CADENCE_PUBLISHED)
        );
        assert_eq!(events[1].payload["version"], 2);
        assert_eq!(events[1].payload["every_minutes"], 5);
    }

    #[tokio::test]
    async fn a_publish_not_above_the_newest_version_is_a_conflict() {
        let repo = InMemoryCadence::default();
        let now = Utc.with_ymd_and_hms(2026, 9, 18, 17, 0, 0).unwrap();
        repo.publish_declared(declared("train-reconcile", 3, 10), &actor(), now)
            .await
            .unwrap();
        // The same version (an overwrite) and a lower one (which would
        // retire v3 under an older declaration) are both refused.
        for v in [3, 2] {
            let err = repo
                .publish_declared(declared("train-reconcile", v, 5), &actor(), now)
                .await
                .expect_err("not above the newest");
            assert!(matches!(err, CadenceError::Conflict(_)), "{err}");
            assert!(err.to_string().contains('3'), "names the newest: {err}");
        }
        // A retired lineage still bounds the version: v3 retired, v2
        // is still below it.
        repo.retire("train-reconcile", &actor(), now).await.unwrap();
        let err = repo
            .publish_declared(declared("train-reconcile", 2, 5), &actor(), now)
            .await
            .expect_err("a retired lineage still bounds the version");
        assert!(matches!(err, CadenceError::Conflict(_)), "{err}");
        // Above it lands, re-activating the name.
        repo.publish_declared(declared("train-reconcile", 4, 5), &actor(), now)
            .await
            .expect("v4 is above v3");
        assert_eq!(repo.active_rules().await.unwrap().len(), 1);
        // A version below 1 on an empty lineage is refused the same way.
        let err = repo
            .publish_declared(declared("fresh", 0, 5), &actor(), now)
            .await
            .expect_err("versions start at 1");
        assert!(matches!(err, CadenceError::Conflict(_)), "{err}");
    }
}
