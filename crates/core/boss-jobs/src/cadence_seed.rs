//! The platform cadence bundle's publish — `infra/platform/cadence/`
//! into the cadence registry, insert-if-missing by (name, version).
//!
//! WHY (backlog 393d3234, consolidation H4, car 3 of 4; measured
//! 2026-09-18 on origin/main 07daca42). Nine migrations were the only
//! place a cadence rule was declared — six of them re-versioning ONE
//! rule, the boarding threshold, each a contended timestamped file,
//! and one of them (202609031515) a silent no-op because its
//! version-keyed retire missed the live row. A fresh instance could
//! reproduce the four active rules only by replaying that history, and
//! nothing compared the history's result to the live table. The rule
//! now lives here, on the seed path stations and step plugins took in
//! cars 1 and 2.
//!
//! THE BUNDLE IS THE DECLARED BASELINE, NOT A LOCK. `cadence_rules` is
//! live, editable protocol data by design (protocol-cadence.md: "we
//! want every protocol internalized so we can measure, experiment, and
//! update"); an operator who re-versions a rule live leaves the bundle
//! behind, and the seed's report says so — `behind the live lineage
//! (vN active), untouched` — as information, never a refusal. The one
//! refusal is the one every bundle has: a file edited at the SAME
//! (name, version) the live active row holds, which is drift the
//! bundle exists to remove. A rule the operator retired stays retired
//! (`Retired`, left alone): a boot never switches a schedule back on.
//!
//! The decision table, the report and the refusal are
//! [`crate::bundle_seed`]'s; this module is the cadence half of the
//! port — one read of the lineage, one write — plus where the bundle
//! is. The conductor's loop is untouched: it reads the LIVE active rows
//! through `/api/cadence/rules` as before.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::bundle_seed::{BundleRegistry, BundleSeedError, Declared, seed_bundle};
use crate::cadence::{CadenceError, CadenceRegistry, CadenceRuleSpec};
use crate::registry::WorkflowStatus;

pub use crate::bundle_seed::{Refusal, SeedOutcome, SeedReport, SeedRow, differing_fields};

/// The in-tree platform cadence bundle, resolved from this crate — a
/// DIRECTORY, one `<name>.toml` per rule, beside the Workflow, station
/// and step-plugin bundles.
pub fn platform_cadence_path() -> &'static str {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../infra/platform/cadence"
    )
}

/// The cadence bundle that sits beside a Workflow bundle directory —
/// `<workflows dir>/../cadence` — which is how the seed binary finds
/// it from the one `--seed-path` every launcher already passes.
pub fn cadence_beside(workflows: &Path) -> PathBuf {
    workflows
        .parent()
        .map(|p| p.join("cadence"))
        .unwrap_or_else(|| PathBuf::from("cadence"))
}

impl Declared for CadenceRuleSpec {
    fn name(&self) -> &str {
        CadenceRuleSpec::name(self)
    }
    fn version(&self) -> i32 {
        self.version
    }
    fn status(&self) -> WorkflowStatus {
        self.status
    }
}

#[async_trait]
impl<'a> BundleRegistry for dyn CadenceRegistry + 'a {
    type Spec = CadenceRuleSpec;
    type Error = CadenceError;
    const LABEL: &'static str = "platform-cadence-seed";
    const BUNDLE: &'static str = "infra/platform/cadence/<name>.toml";
    const VERSIONED: bool = true;

    async fn live_versions(&self, name: &str) -> Result<Vec<CadenceRuleSpec>, CadenceError> {
        CadenceRegistry::live_versions(self, name).await
    }

    async fn publish_declared(
        &self,
        spec: CadenceRuleSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<CadenceRuleSpec, CadenceError> {
        CadenceRegistry::publish_declared(self, spec, actor, now).await
    }
}

pub type CadenceSeedError = BundleSeedError<CadenceError>;

/// Publish `specs` into `registry` by the decision table in
/// [`crate::bundle_seed`]. Classifies every row first and refuses
/// whole if any row contradicts its live twin; only then writes.
/// `dry_run` reports what would be written and writes nothing.
pub async fn seed_cadence_rules(
    registry: &dyn CadenceRegistry,
    specs: &[CadenceRuleSpec],
    actor: &boss_core::actor::ActorId,
    now: DateTime<Utc>,
    dry_run: bool,
) -> Result<SeedReport, CadenceSeedError> {
    seed_bundle(registry, specs, actor, now, dry_run).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cadence::{CadenceRepository, CadenceRuleRow, InMemoryCadence};
    use boss_core::actor::ActorId;

    fn actor() -> ActorId {
        ActorId::Automation("platform-workflow-seed".into())
    }

    fn now() -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::days(20_000)
    }

    fn spec(name: &str, version: i32, every: i32) -> CadenceRuleSpec {
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
            created_at: now(),
        }
    }

    #[tokio::test]
    async fn an_empty_registry_gets_every_row_at_its_declared_version() {
        let registry = InMemoryCadence::default();
        let report = seed_cadence_rules(
            &registry,
            &[spec("train-reconcile", 1, 10), spec("board", 6, 1)],
            &actor(),
            now(),
            false,
        )
        .await
        .expect("seed");
        assert_eq!(report.count(|o| *o == SeedOutcome::Inserted), 2);
        let board = registry.live_versions("board").await.expect("lineage");
        assert_eq!(
            board.iter().map(|r| r.version).collect::<Vec<_>>(),
            vec![6],
            "the DECLARED version lands, with no synthetic history below it"
        );
        // What the seed published is what the conductor's read serves.
        let served = registry.active_rules().await.expect("active");
        assert_eq!(
            served.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(),
            vec!["board", "train-reconcile"]
        );
        assert!(
            report.to_string().contains("platform-cadence-seed:"),
            "{report}"
        );
    }

    #[tokio::test]
    async fn a_live_lineage_ahead_of_the_bundle_is_information_not_a_refusal() {
        // The operator re-versioned the rule live (v2, every 5) and the
        // bundle still says v1 — the case the packet's car-3 text names:
        // a protocol-drift line, not a refusal.
        let registry = InMemoryCadence::default();
        registry
            .publish_declared(spec("train-reconcile", 2, 5), &actor(), now())
            .await
            .expect("the operator's live edit");
        let report = seed_cadence_rules(
            &registry,
            &[spec("train-reconcile", 1, 10)],
            &actor(),
            now(),
            false,
        )
        .await
        .expect("a live lineage ahead of the bundle is not a failure");
        assert_eq!(
            report.rows[0].outcome,
            SeedOutcome::Behind { live_active: 2 }
        );
        assert_eq!(
            registry.active_rules().await.expect("active")[0].every_minutes,
            Some(5),
            "the operator's live row stands"
        );
    }

    #[tokio::test]
    async fn a_row_edited_without_a_version_bump_is_refused_by_field() {
        let registry = InMemoryCadence::default();
        seed_cadence_rules(
            &registry,
            &[spec("train-reconcile", 1, 10)],
            &actor(),
            now(),
            false,
        )
        .await
        .expect("seed");
        let err = seed_cadence_rules(
            &registry,
            &[spec("train-reconcile", 1, 5)],
            &actor(),
            now(),
            false,
        )
        .await
        .expect_err("refused");
        let text = err.to_string();
        let CadenceSeedError::Refused { rows: refusals, .. } = err else {
            panic!("expected a refusal");
        };
        assert_eq!(refusals[0].fields, vec!["every_minutes"]);
        assert!(
            text.contains("infra/platform/cadence/<name>.toml"),
            "the refusal names the file to bump: {text}"
        );
    }

    #[test]
    fn the_bundle_beside_a_workflows_dir_is_its_sibling() {
        assert_eq!(
            cadence_beside(Path::new("infra/platform/workflows")),
            PathBuf::from("infra/platform/cadence")
        );
    }
}
