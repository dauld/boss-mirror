//! The platform Workflow bundle's publish — `infra/platform/workflows/`
//! into the Workflow registry, insert-if-missing by kind.
//!
//! WHY THIS MODULE (backlog 8b2eaff2, 2026-09-18). The Workflow seed
//! was the first bundle seed and the model the station and step-plugin
//! seeds followed — but its decision stayed an inline loop in
//! `bin/boss_platform_workflow_seed.rs`, with a second inline copy in
//! `tests/platform_seed_loader.rs`, while [`crate::bundle_seed`] grew
//! the table the other two share. The pair drifted the way CLAUDE.md
//! §9a says a fact that lives twice does: the binary read "present" as
//! `get_active(kind).is_ok()`, so the operator's 12:20Z retire of
//! `maintenance-deploy-confirm` was reverted by the 12:42Z boot, and
//! the test — asserting the same wrong reading — passed. The decision
//! now lives once, in `bundle_seed`'s table; this module is the
//! Workflow half of the port: one read of the lineage, the one write,
//! and the fact that the bundle is UNVERSIONED.
//!
//! INSERT-IF-MISSING, AND NOTHING ELSE — protocols-as-data Q1, as David
//! answered it: "the seed binary inserts what is missing and touches
//! nothing that exists ... Drift-healing goes away deliberately: it is
//! the feature that reverts operator edits." A Workflow file declares
//! no `version` (the registry assigns max+1 at `create_draft`; the file
//! is what a FRESH database gets), so this half reads only the first
//! two rows of the table: no row of the kind → insert; rows but none
//! active → retired, left alone; an active row → present, whatever it
//! says. Never a comparison, never a refusal, never a supersede — an
//! operator's edit to a seeded kind lives on, and so now does their
//! retire.
//!
//! Publishing goes through `create_draft` + `publish` rather than an
//! INSERT, so the viability lint runs on every row exactly as it would
//! for a workflow authored in the UI. A malformed bundle fails here,
//! loudly, on the deployment that is booting — not later, on the first
//! Job that tries to use it.

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::bundle_seed::{BundleRegistry, BundleSeedError, Declared, seed_bundle};
use crate::registry::{WorkflowError, WorkflowRegistry, WorkflowSpec, WorkflowStatus};

pub use crate::bundle_seed::{SeedOutcome, SeedReport, SeedRow};

impl Declared for WorkflowSpec {
    fn name(&self) -> &str {
        &self.kind
    }
    fn version(&self) -> i32 {
        self.version
    }
    fn status(&self) -> WorkflowStatus {
        self.status
    }
}

#[async_trait]
impl<'a> BundleRegistry for dyn WorkflowRegistry + 'a {
    type Spec = WorkflowSpec;
    type Error = WorkflowError;
    const LABEL: &'static str = "platform-workflow-seed";
    const BUNDLE: &'static str = "infra/platform/workflows/<kind>.toml";
    const VERSIONED: bool = false;

    async fn live_versions(&self, kind: &str) -> Result<Vec<WorkflowSpec>, WorkflowError> {
        self.list_versions(kind).await
    }

    /// Draft then publish, so the viability lint judges the row; the
    /// registry assigns the version. `publish` flips the latest draft
    /// of the kind — the one just drafted, since a kind reaches this
    /// write only when it has no row at all.
    async fn publish_declared(
        &self,
        spec: WorkflowSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<WorkflowSpec, WorkflowError> {
        let kind = spec.kind.clone();
        self.create_draft(spec, actor, now).await?;
        self.publish(&kind, actor, now).await
    }
}

pub type WorkflowSeedError = BundleSeedError<WorkflowError>;

/// Publish `specs` into `registry` by the first two rows of the
/// decision table in [`crate::bundle_seed`]: a kind with no row is
/// inserted; every other kind is left exactly as it is. `dry_run`
/// reports what would be written and writes nothing.
pub async fn seed_workflows(
    registry: &dyn WorkflowRegistry,
    specs: &[WorkflowSpec],
    actor: &boss_core::actor::ActorId,
    now: DateTime<Utc>,
    dry_run: bool,
) -> Result<SeedReport, WorkflowSeedError> {
    seed_bundle(registry, specs, actor, now, dry_run).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::{InMemoryWorkflows, StepSpec, Terminal};
    use boss_core::actor::ActorId;

    fn actor() -> ActorId {
        ActorId::Automation("platform-workflow-seed".into())
    }

    fn operator() -> ActorId {
        ActorId::Human("emp-david".into())
    }

    fn now() -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::days(20_000)
    }

    /// A viable two-step protocol, as the registry's own tests build one.
    fn spec(kind: &str) -> WorkflowSpec {
        WorkflowSpec::platform_seed(
            kind,
            format!("Protocol {kind}"),
            "platform",
            vec!["asset".into()],
            vec![
                StepSpec {
                    title: "start".into(),
                    kind: "task".into(),
                    ready_when: "true".into(),
                    ..Default::default()
                },
                StepSpec {
                    title: "finish".into(),
                    kind: "task".into(),
                    ready_when: "steps.start.done".into(),
                    terminal: Some(Terminal {
                        outcome: "done".into(),
                    }),
                    ..Default::default()
                },
            ],
        )
    }

    #[tokio::test]
    async fn an_empty_registry_gets_every_kind_at_v1() {
        let registry = InMemoryWorkflows::new();
        let report = seed_workflows(
            &registry,
            &[spec("alpha"), spec("beta")],
            &actor(),
            now(),
            false,
        )
        .await
        .expect("seed");
        assert_eq!(report.count(|o| *o == SeedOutcome::Inserted), 2);
        assert_eq!(
            report.rows[0].version, None,
            "an unversioned bundle reports by kind alone"
        );
        let alpha = registry.get_active("alpha").await.expect("alpha active");
        assert_eq!(alpha.version, 1, "a fresh insert is v1");
        assert!(report.to_string().contains("  alpha: inserted"), "{report}");
    }

    #[tokio::test]
    async fn an_active_kind_is_present_whatever_it_says() {
        let registry = InMemoryWorkflows::new();
        seed_workflows(&registry, &[spec("alpha")], &actor(), now(), false)
            .await
            .expect("seed");
        // The operator edits the kind the way the UI does.
        let mut edited = registry.get_active("alpha").await.expect("seeded");
        edited.label = "Operator's own label".into();
        edited.status = WorkflowStatus::Draft;
        registry
            .create_draft(edited, &operator(), now())
            .await
            .expect("draft");
        registry
            .publish("alpha", &operator(), now())
            .await
            .expect("publish");

        let report = seed_workflows(&registry, &[spec("alpha")], &actor(), now(), false)
            .await
            .expect("seed");
        assert_eq!(report.rows[0].outcome, SeedOutcome::Present);
        let live = registry.get_active("alpha").await.expect("still active");
        assert_eq!(live.version, 2);
        assert_eq!(
            live.label, "Operator's own label",
            "the seed never compares, refuses or rewrites a live Workflow"
        );
    }

    /// The 12:42Z revert (backlog 8b2eaff2): a retired kind whose file
    /// is still in the bundle is left retired.
    #[tokio::test]
    async fn a_retired_kind_is_left_retired() {
        let registry = InMemoryWorkflows::new();
        seed_workflows(&registry, &[spec("alpha")], &actor(), now(), false)
            .await
            .expect("seed");
        registry
            .retire("alpha", &operator(), now())
            .await
            .expect("the operator's retire");

        let report = seed_workflows(&registry, &[spec("alpha")], &actor(), now(), false)
            .await
            .expect("a retired kind is not a failure");
        assert_eq!(report.rows[0].outcome, SeedOutcome::Retired { newest: 1 });
        assert!(
            report.to_string().contains("  alpha: retired (left alone)"),
            "{report}"
        );
        assert!(
            matches!(
                registry.get_active("alpha").await,
                Err(WorkflowError::NotFound(_))
            ),
            "no active row after the boot"
        );
        assert_eq!(
            registry
                .list_versions("alpha")
                .await
                .expect("versions")
                .len(),
            1,
            "nothing was published over the retire"
        );
    }

    #[tokio::test]
    async fn a_dry_run_reports_and_writes_nothing() {
        let registry = InMemoryWorkflows::new();
        let report = seed_workflows(&registry, &[spec("alpha")], &actor(), now(), true)
            .await
            .expect("dry run");
        assert_eq!(report.rows[0].outcome, SeedOutcome::Inserted);
        assert!(report.to_string().contains("WOULD be inserted"), "{report}");
        assert!(registry.list_active(None).await.expect("list").is_empty());
    }
}
