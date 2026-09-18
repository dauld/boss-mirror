//! The platform delivery-policy bundle's publish —
//! `infra/platform/delivery-policy/` into the delivery-policy registry,
//! insert-if-missing by (name, version).
//!
//! WHY (backlog 393d3234, consolidation H4, car 4 of 4 — the last
//! registry; measured 2026-09-18 on origin/main 478231fb). Two
//! migrations were the only place the delivery policy was declared:
//! 202608242117 seeded v1 as train.rs's compiled constants carried
//! over verbatim, and 202609050500 retired it and published v2 with
//! the CI host floor at 40 — a policy edit that had to ride the
//! pipeline it was re-tuning, as a contended timestamped file. A fresh
//! instance could reproduce the active row only by replaying that
//! history, and nothing compared the history's result to the live row
//! except `boss-cli`'s pin against the compiled fallback. The row now
//! lives here, on the seed path stations, step plugins and cadence
//! rules took in cars 1–3.
//!
//! ONE ROW IS THE WHOLE POLICY. Unlike `cadence_rules`, this registry
//! has no live-edit verb: a change is a version bump in the bundle,
//! published at the next boot, and a train pins the version it departed
//! under (`metadata.delivery_policy_version`) so reconcile judges it by
//! those rules and not the new ones. The compiled fallback in
//! `boss-cli::delivery_policy` stays — the conductor runs outside the
//! cluster and must board when the registry is unreachable or empty —
//! and is held equal to this bundle by a test there.
//!
//! The decision table, the report and the refusal are
//! [`crate::bundle_seed`]'s; this module is the delivery-policy half of
//! the port — one read of the lineage, one write — plus where the
//! bundle is. The conductor's read is untouched: it reads the LIVE
//! active row over `/api/delivery/policy/<name>` as before.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::bundle_seed::{BundleRegistry, BundleSeedError, Declared, seed_bundle};
use crate::delivery::{DeliveryPolicyError, DeliveryPolicyRegistry, DeliveryPolicySpec};
use crate::registry::WorkflowStatus;

pub use crate::bundle_seed::{Refusal, SeedOutcome, SeedReport, SeedRow, differing_fields};

/// The in-tree platform delivery-policy bundle, resolved from this
/// crate — a DIRECTORY, one `<name>.toml` per policy, beside the
/// Workflow, station, step-plugin and cadence bundles.
pub fn platform_delivery_policy_path() -> &'static str {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../infra/platform/delivery-policy"
    )
}

/// The delivery-policy bundle that sits beside a Workflow bundle
/// directory — `<workflows dir>/../delivery-policy` — which is how the
/// seed binary finds it from the one `--seed-path` every launcher
/// already passes.
pub fn delivery_policy_beside(workflows: &Path) -> PathBuf {
    workflows
        .parent()
        .map(|p| p.join("delivery-policy"))
        .unwrap_or_else(|| PathBuf::from("delivery-policy"))
}

impl Declared for DeliveryPolicySpec {
    fn name(&self) -> &str {
        DeliveryPolicySpec::name(self)
    }
    fn version(&self) -> i32 {
        DeliveryPolicySpec::version(self)
    }
    fn status(&self) -> WorkflowStatus {
        self.status
    }
}

#[async_trait]
impl<'a> BundleRegistry for dyn DeliveryPolicyRegistry + 'a {
    type Spec = DeliveryPolicySpec;
    type Error = DeliveryPolicyError;
    const LABEL: &'static str = "platform-delivery-policy-seed";
    const BUNDLE: &'static str = "infra/platform/delivery-policy/<name>.toml";
    const VERSIONED: bool = true;

    async fn live_versions(
        &self,
        name: &str,
    ) -> Result<Vec<DeliveryPolicySpec>, DeliveryPolicyError> {
        DeliveryPolicyRegistry::live_versions(self, name).await
    }

    async fn publish_declared(
        &self,
        spec: DeliveryPolicySpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<DeliveryPolicySpec, DeliveryPolicyError> {
        DeliveryPolicyRegistry::publish_declared(self, spec, actor, now).await
    }
}

pub type DeliveryPolicySeedError = BundleSeedError<DeliveryPolicyError>;

/// Publish `specs` into `registry` by the decision table in
/// [`crate::bundle_seed`]. Classifies every row first and refuses
/// whole if any row contradicts its live twin; only then writes.
/// `dry_run` reports what would be written and writes nothing.
pub async fn seed_delivery_policies(
    registry: &dyn DeliveryPolicyRegistry,
    specs: &[DeliveryPolicySpec],
    actor: &boss_core::actor::ActorId,
    now: DateTime<Utc>,
    dry_run: bool,
) -> Result<SeedReport, DeliveryPolicySeedError> {
    seed_bundle(registry, specs, actor, now, dry_run).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delivery::{DeliveryPolicyRepository, DeliveryPolicyRow, InMemoryDeliveryPolicy};
    use boss_core::actor::ActorId;

    fn actor() -> ActorId {
        ActorId::Automation("platform-workflow-seed".into())
    }

    fn now() -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::days(20_000)
    }

    fn spec(name: &str, version: i32, gate_max_concurrent: i32) -> DeliveryPolicySpec {
        DeliveryPolicySpec {
            status: WorkflowStatus::Active,
            row: DeliveryPolicyRow {
                name: name.into(),
                version,
                max_red_trains: 2,
                stall_hours: 6,
                consist_budget_secs: 60,
                consist_output_budget: 1200,
                consist_files_named: 6,
                skip_reason_file_budget: 96,
                blip_cause_budget: 80,
                ci_host_floor_gb: 40,
                gate_max_concurrent,
            },
            created_at: now(),
        }
    }

    #[tokio::test]
    async fn an_empty_registry_gets_the_row_at_its_declared_version() {
        let registry = InMemoryDeliveryPolicy::default();
        let report = seed_delivery_policies(
            &registry,
            &[spec("train-conductor", 2, 3)],
            &actor(),
            now(),
            false,
        )
        .await
        .expect("seed");
        assert_eq!(report.count(|o| *o == SeedOutcome::Inserted), 1);
        let lineage = registry
            .live_versions("train-conductor")
            .await
            .expect("lineage");
        assert_eq!(
            lineage.iter().map(|r| r.version()).collect::<Vec<_>>(),
            vec![2],
            "the DECLARED version lands, with no synthetic history below it"
        );
        // What the seed published is what the conductor's read serves.
        let served = registry
            .active_policy("train-conductor")
            .await
            .expect("active")
            .expect("served");
        assert_eq!((served.version, served.gate_max_concurrent), (2, 3));
        assert!(
            report
                .to_string()
                .contains("platform-delivery-policy-seed:"),
            "{report}"
        );
    }

    #[tokio::test]
    async fn a_row_edited_without_a_version_bump_is_refused_by_field() {
        let registry = InMemoryDeliveryPolicy::default();
        seed_delivery_policies(
            &registry,
            &[spec("train-conductor", 2, 3)],
            &actor(),
            now(),
            false,
        )
        .await
        .expect("seed");
        let err = seed_delivery_policies(
            &registry,
            &[spec("train-conductor", 2, 4)],
            &actor(),
            now(),
            false,
        )
        .await
        .expect_err("refused");
        let text = err.to_string();
        let DeliveryPolicySeedError::Refused { rows: refusals, .. } = err else {
            panic!("expected a refusal");
        };
        assert_eq!(refusals[0].fields, vec!["gate_max_concurrent"]);
        assert!(
            text.contains("infra/platform/delivery-policy/<name>.toml"),
            "the refusal names the file to bump: {text}"
        );
        assert_eq!(
            registry
                .active_policy("train-conductor")
                .await
                .expect("active")
                .expect("served")
                .gate_max_concurrent,
            3,
            "a refused seed writes nothing"
        );
    }

    #[test]
    fn the_bundle_beside_a_workflows_dir_is_its_sibling() {
        assert_eq!(
            delivery_policy_beside(Path::new("infra/platform/workflows")),
            PathBuf::from("infra/platform/delivery-policy")
        );
    }
}
