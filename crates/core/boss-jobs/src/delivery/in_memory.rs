//! In-memory adapter for `DeliveryPolicyRepository` — the port-level
//! test double. Mirrors the two Pg semantics that matter: `active_policy`
//! sees only the active row, and `policy_version` sees a version
//! whatever its status, because an in-flight train's pinned version may
//! have been retired underneath it. Since the bundle seed (backlog
//! 393d3234, car 4) it is also the `DeliveryPolicyRegistry` double, so
//! a row the seed publishes is a row the conductor's read then serves,
//! as in Postgres.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;

use super::port::{DeliveryPolicyError, DeliveryPolicyRegistry, DeliveryPolicyRepository};
use super::types::{DeliveryPolicyRow, DeliveryPolicySpec};
use crate::registry::WorkflowStatus;

/// A stored row plus the status the registry holds it at.
#[derive(Debug, Clone)]
pub struct StoredPolicy {
    pub row: DeliveryPolicyRow,
    pub status: String,
}

#[derive(Default)]
pub struct InMemoryDeliveryPolicy {
    /// The whole lineage, every version of every name.
    rows: RwLock<Vec<DeliveryPolicySpec>>,
}

impl InMemoryDeliveryPolicy {
    pub fn new(rows: Vec<StoredPolicy>) -> Self {
        let lineage = rows
            .into_iter()
            .map(|s| DeliveryPolicySpec {
                // A status string the registry could not hold is a
                // test's typo; the CHECK constraint refuses it in Pg.
                status: s.status.parse().unwrap_or(WorkflowStatus::Retired),
                row: s.row,
                created_at: DateTime::<Utc>::UNIX_EPOCH,
            })
            .collect();
        Self {
            rows: RwLock::new(lineage),
        }
    }
}

#[async_trait]
impl DeliveryPolicyRepository for InMemoryDeliveryPolicy {
    async fn active_policy(
        &self,
        name: &str,
    ) -> Result<Option<DeliveryPolicyRow>, DeliveryPolicyError> {
        Ok(self
            .rows
            .read()
            .await
            .iter()
            .find(|s| s.name() == name && s.status == WorkflowStatus::Active)
            .map(|s| s.row.clone()))
    }

    async fn policy_version(
        &self,
        name: &str,
        version: i32,
    ) -> Result<Option<DeliveryPolicyRow>, DeliveryPolicyError> {
        Ok(self
            .rows
            .read()
            .await
            .iter()
            .find(|s| s.name() == name && s.version() == version)
            .map(|s| s.row.clone()))
    }
}

#[async_trait]
impl DeliveryPolicyRegistry for InMemoryDeliveryPolicy {
    async fn live_versions(
        &self,
        name: &str,
    ) -> Result<Vec<DeliveryPolicySpec>, DeliveryPolicyError> {
        let mut out: Vec<DeliveryPolicySpec> = self
            .rows
            .read()
            .await
            .iter()
            .filter(|r| r.name() == name)
            .cloned()
            .collect();
        out.sort_by_key(|r| r.version());
        Ok(out)
    }

    async fn publish_declared(
        &self,
        mut spec: DeliveryPolicySpec,
        _actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<DeliveryPolicySpec, DeliveryPolicyError> {
        let mut rows = self.rows.write().await;
        if rows
            .iter()
            .any(|r| r.name() == spec.name() && r.version() == spec.version())
        {
            return Err(DeliveryPolicyError::Conflict(format!(
                "row already exists: {}@{}",
                spec.name(),
                spec.version()
            )));
        }
        // Mirrors the Pg adapter: retire by name, then insert.
        for r in rows.iter_mut() {
            if r.name() == spec.name() && r.status == WorkflowStatus::Active {
                r.status = WorkflowStatus::Retired;
            }
        }
        spec.status = WorkflowStatus::Active;
        spec.created_at = now;
        rows.push(spec.clone());
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(version: i32) -> DeliveryPolicyRow {
        DeliveryPolicyRow {
            name: "train-conductor".into(),
            version,
            max_red_trains: 2,
            stall_hours: 6,
            consist_budget_secs: 60,
            consist_output_budget: 1200,
            consist_files_named: 6,
            skip_reason_file_budget: 96,
            blip_cause_budget: 80,
            ci_host_floor_gb: 40,
            gate_max_concurrent: 3,
        }
    }

    fn stored(version: i32, status: &str) -> StoredPolicy {
        StoredPolicy {
            row: row(version),
            status: status.into(),
        }
    }

    #[tokio::test]
    async fn active_policy_reads_only_the_active_row() {
        let repo = InMemoryDeliveryPolicy::new(vec![stored(1, "retired"), stored(2, "active")]);
        let got = repo
            .active_policy("train-conductor")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            got.version, 2,
            "a retired version is not the policy in force"
        );
    }

    #[tokio::test]
    async fn an_empty_registry_is_none_not_an_error() {
        let repo = InMemoryDeliveryPolicy::default();
        assert!(
            repo.active_policy("train-conductor")
                .await
                .unwrap()
                .is_none(),
            "no policy is a normal answer — the conductor falls back to its \
             compiled values rather than refusing to run"
        );
    }

    #[tokio::test]
    async fn a_pinned_version_is_readable_after_it_is_retired() {
        // The case pinning exists for: an edit lands mid-flight, so the
        // version the train departed under is no longer active. It must
        // still be readable, or the pin buys nothing.
        let repo = InMemoryDeliveryPolicy::new(vec![stored(1, "retired"), stored(2, "active")]);
        let got = repo
            .policy_version("train-conductor", 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got.version, 1);
    }

    #[tokio::test]
    async fn an_unknown_version_is_none() {
        let repo = InMemoryDeliveryPolicy::new(vec![stored(1, "active")]);
        assert!(
            repo.policy_version("train-conductor", 9)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_published_version_retires_the_active_one_and_is_then_served() {
        let repo = InMemoryDeliveryPolicy::new(vec![stored(1, "active")]);
        let actor = boss_core::actor::ActorId::Automation("platform-workflow-seed".into());
        let spec = DeliveryPolicySpec {
            status: WorkflowStatus::Active,
            row: row(2),
            created_at: DateTime::<Utc>::UNIX_EPOCH,
        };
        repo.publish_declared(spec, &actor, DateTime::<Utc>::UNIX_EPOCH)
            .await
            .expect("a new version publishes");
        let lineage = repo.live_versions("train-conductor").await.unwrap();
        assert_eq!(
            lineage
                .iter()
                .map(|r| (r.version(), r.status))
                .collect::<Vec<_>>(),
            vec![(1, WorkflowStatus::Retired), (2, WorkflowStatus::Active)]
        );
        assert_eq!(
            repo.active_policy("train-conductor")
                .await
                .unwrap()
                .unwrap()
                .version,
            2,
            "what the seed published is what the conductor's read serves"
        );
        let again = DeliveryPolicySpec {
            status: WorkflowStatus::Active,
            row: row(2),
            created_at: DateTime::<Utc>::UNIX_EPOCH,
        };
        assert!(
            matches!(
                repo.publish_declared(again, &actor, DateTime::<Utc>::UNIX_EPOCH)
                    .await,
                Err(DeliveryPolicyError::Conflict(_))
            ),
            "a row already at (name, version) is a conflict, not an overwrite"
        );
    }
}
