//! In-memory adapter for `DeliveryPolicyRepository` — the port-level
//! test double. Mirrors the two Pg semantics that matter: `active_policy`
//! sees only the active row, and `policy_version` sees a version
//! whatever its status, because an in-flight train's pinned version may
//! have been retired underneath it.

use async_trait::async_trait;

use super::port::{DeliveryPolicyError, DeliveryPolicyRepository};
use super::types::DeliveryPolicyRow;

/// A stored row plus the status the registry holds it at.
#[derive(Debug, Clone)]
pub struct StoredPolicy {
    pub row: DeliveryPolicyRow,
    pub status: String,
}

#[derive(Default)]
pub struct InMemoryDeliveryPolicy {
    rows: Vec<StoredPolicy>,
}

impl InMemoryDeliveryPolicy {
    pub fn new(rows: Vec<StoredPolicy>) -> Self {
        Self { rows }
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
            .iter()
            .find(|s| s.row.name == name && s.status == "active")
            .map(|s| s.row.clone()))
    }

    async fn policy_version(
        &self,
        name: &str,
        version: i32,
    ) -> Result<Option<DeliveryPolicyRow>, DeliveryPolicyError> {
        Ok(self
            .rows
            .iter()
            .find(|s| s.row.name == name && s.row.version == version)
            .map(|s| s.row.clone()))
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
            consist_excluded_lints: serde_json::json!([]),
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
}
