//! The platform step-plugin bundle's publish —
//! `infra/platform/step-plugins/` into the step-plugin registry,
//! insert-if-missing by (kind, version).
//!
//! WHY (backlog 393d3234, consolidation H4, car 2 of 4; measured
//! 2026-09-18 on origin/main 6286857f). A step plugin is two artefacts:
//! a JS bundle under `infra/step-plugins/`, which the converge runner
//! mounts as the step-plugins ConfigMap on every deploy, and a ROW in
//! `step_plugins` naming it. The bundle always lived in the tree; the
//! row's only home was one of seven migrations (03-jobs.sql and six
//! `*-plugin.sql` files), so a fresh instance could not re-declare it
//! without replaying history and nothing drift-checked it against the
//! live row. The row now lives here, on the same seed path stations
//! took in car 1.
//!
//! WHY `infra/platform/step-plugins/` AND NOT BESIDE THE JS. Measured
//! against what already reads each directory: the quickstart image
//! copies `infra/platform` whole (Dockerfile:308) and nothing copies
//! `infra/step-plugins` into it — the JS reaches the cluster as a
//! ConfigMap the converge runner builds from `*.js`, never through the
//! image the seed runs in. A row beside its JS would have needed a new
//! Dockerfile COPY, a launcher flag, and an init.sh edit for the seed
//! to find it; a row here is found the way the station bundle is —
//! the sibling of `--seed-path` — and no launcher changed. The two
//! halves are held together by `infra/lint/step-plugin-bundle-exists.sh`
//! and `platform_step_plugins_bundle.rs`, which refuse a row whose JS
//! is absent.
//!
//! The decision table, the report and the refusal are
//! [`crate::bundle_seed`]'s; this module is the plugin half of the
//! port — three reads, one write — plus where the bundle is.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::bundle_seed::{BundleRegistry, BundleSeedError, Declared, seed_bundle};
use crate::registry::WorkflowStatus;
use crate::step_plugins::{StepPluginError, StepPluginRegistry, StepPluginSpec};

pub use crate::bundle_seed::{Refusal, SeedOutcome, SeedReport, SeedRow, differing_fields};

/// The in-tree platform step-plugin bundle, resolved from this crate —
/// a DIRECTORY, one `<kind>.toml` per plugin, beside the Workflow and
/// station bundles.
pub fn platform_step_plugins_path() -> &'static str {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../infra/platform/step-plugins"
    )
}

/// The JS a bundle row's `frontend_url` names, in the tree:
/// `infra/step-plugins/<frontend_url>`. The gateway serves the same
/// name from `/var/lib/boss/step-plugins/` once the converge runner
/// has mounted the directory.
pub fn step_plugin_js_path(frontend_url: &str) -> PathBuf {
    Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../infra/step-plugins"
    ))
    .join(frontend_url)
}

/// The step-plugin bundle that sits beside a Workflow bundle directory
/// — `<workflows dir>/../step-plugins` — which is how the seed binary
/// finds it from the one `--seed-path` every launcher already passes.
pub fn step_plugins_beside(workflows: &Path) -> PathBuf {
    workflows
        .parent()
        .map(|p| p.join("step-plugins"))
        .unwrap_or_else(|| PathBuf::from("step-plugins"))
}

impl Declared for StepPluginSpec {
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
impl<'a> BundleRegistry for dyn StepPluginRegistry + 'a {
    type Spec = StepPluginSpec;
    type Error = StepPluginError;
    const LABEL: &'static str = "platform-step-plugin-seed";
    const BUNDLE: &'static str = "infra/platform/step-plugins/<kind>.toml";

    async fn live_active(&self, kind: &str) -> Result<Option<StepPluginSpec>, StepPluginError> {
        match self.get_active(kind).await {
            Ok(row) => Ok(Some(row)),
            Err(StepPluginError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn live_version(
        &self,
        kind: &str,
        version: i32,
    ) -> Result<Option<StepPluginSpec>, StepPluginError> {
        match self.get_version(kind, version).await {
            Ok(row) => Ok(Some(row)),
            Err(StepPluginError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn publish_declared(
        &self,
        spec: StepPluginSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<StepPluginSpec, StepPluginError> {
        StepPluginRegistry::publish_declared(self, spec, actor, now).await
    }
}

pub type StepPluginSeedError = BundleSeedError<StepPluginError>;

/// Publish `specs` into `registry` by the decision table in
/// [`crate::bundle_seed`]. Classifies every row first and refuses
/// whole if any row contradicts its live twin; only then writes.
/// `dry_run` reports what would be written and writes nothing.
pub async fn seed_step_plugins(
    registry: &dyn StepPluginRegistry,
    specs: &[StepPluginSpec],
    actor: &boss_core::actor::ActorId,
    now: DateTime<Utc>,
    dry_run: bool,
) -> Result<SeedReport, StepPluginSeedError> {
    seed_bundle(registry, specs, actor, now, dry_run).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::step_plugins::InMemoryStepPlugins;
    use boss_core::actor::ActorId;

    fn actor() -> ActorId {
        ActorId::Automation("platform-workflow-seed".into())
    }

    fn now() -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::days(20_000)
    }

    fn spec(kind: &str, version: i32) -> StepPluginSpec {
        StepPluginSpec {
            kind: kind.into(),
            version,
            status: WorkflowStatus::Active,
            label: format!("Plugin {kind}"),
            description: None,
            category: "platform".into(),
            metadata_schema: serde_json::json!({"type": "object"}),
            frontend_url: format!("{kind}.js"),
            owning_team: "platform".into(),
            authoring_job_id: None,
            created_at: now(),
        }
    }

    async fn seeded(specs: &[StepPluginSpec]) -> InMemoryStepPlugins {
        let registry = InMemoryStepPlugins::new();
        seed_step_plugins(&registry, specs, &actor(), now(), false)
            .await
            .expect("seed into an empty registry");
        registry
    }

    #[tokio::test]
    async fn an_empty_registry_gets_every_row_at_its_declared_version() {
        let registry = InMemoryStepPlugins::new();
        let report = seed_step_plugins(
            &registry,
            &[spec("sign-off", 3), spec("checklist", 1)],
            &actor(),
            now(),
            false,
        )
        .await
        .expect("seed");
        assert_eq!(report.count(|o| *o == SeedOutcome::Inserted), 2);
        let sign_off = registry.get_active("sign-off").await.expect("active");
        assert_eq!(sign_off.version, 3, "the DECLARED version lands, not max+1");
        assert_eq!(
            registry
                .list_versions("sign-off")
                .await
                .expect("versions")
                .len(),
            1,
            "no synthetic history below the declared version"
        );
        assert_eq!(
            registry.recorded_events().len(),
            2,
            "one jobs.step_plugin.published per row written"
        );
        assert!(
            report.to_string().contains("platform-step-plugin-seed:"),
            "{report}"
        );
    }

    #[tokio::test]
    async fn a_row_edited_without_a_version_bump_is_refused_by_field() {
        let registry = seeded(&[spec("checklist", 1)]).await;
        let mut edited = spec("checklist", 1);
        edited.frontend_url = "elsewhere.js".into();
        let err = seed_step_plugins(&registry, &[edited], &actor(), now(), false)
            .await
            .expect_err("refused");
        let text = err.to_string();
        let StepPluginSeedError::Refused { rows: refusals, .. } = err else {
            panic!("expected a refusal");
        };
        assert_eq!(refusals[0].fields, vec!["frontend_url"]);
        assert!(
            text.contains("infra/platform/step-plugins/<kind>.toml"),
            "the refusal names the file to bump: {text}"
        );
        assert_eq!(
            registry
                .get_active("checklist")
                .await
                .expect("checklist")
                .frontend_url,
            "checklist.js",
            "a refused seed writes nothing"
        );
    }

    #[tokio::test]
    async fn a_version_bump_publishes_over_the_lower_live_row() {
        let registry = seeded(&[spec("checklist", 1)]).await;
        let report = seed_step_plugins(&registry, &[spec("checklist", 2)], &actor(), now(), false)
            .await
            .expect("seed");
        assert_eq!(
            report.rows[0].outcome,
            SeedOutcome::Published { superseded: 1 }
        );
        assert_eq!(
            registry
                .get_version("checklist", 1)
                .await
                .expect("v1 is history")
                .status,
            WorkflowStatus::Retired
        );
    }

    #[test]
    fn the_bundle_beside_a_workflows_dir_is_its_sibling() {
        assert_eq!(
            step_plugins_beside(Path::new("infra/platform/workflows")),
            PathBuf::from("infra/platform/step-plugins")
        );
    }
}
