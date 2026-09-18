//! The platform station bundle's publish — `infra/platform/stations/`
//! into the station registry, insert-if-missing by (name, version).
//!
//! WHY (backlog 393d3234, consolidation H4, car 1 of 4; measured
//! 2026-09-18 on origin/main 1a09f660). Seven migrations were the only
//! home a platform station had. A migration is the wrong home for a
//! registry row: it runs once, so a fresh instance cannot re-declare
//! the row without replaying history (the playground's fresh database
//! inherited 31 example-tenant rule names that way on 2026-09-18); nothing
//! drift-checks it against the live row; and every edit is a contended
//! timestamped file. Dispatcher rules made this move on 2026-09-11
//! (`infra/dispatcher/rules/`) and the Workflow bundle before them
//! (`infra/platform/workflows/`, `boss-platform-workflow-seed`). This
//! is the same move for stations, on the same seed path.
//!
//! The decision table, the report and the refusal live in
//! [`crate::bundle_seed`] since car 2 (step plugins) needed them a
//! second time; this module is the station half of the port — the
//! three reads and the one write — plus where the bundle is.

use std::path::Path;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::bundle_seed::{BundleRegistry, BundleSeedError, Declared, seed_bundle};
use crate::registry::WorkflowStatus;
use crate::seed_loader::{SeedLoaderError, load_stations};
use crate::stations::{StationError, StationRegistry, StationSpec};

pub use crate::bundle_seed::{Refusal, SeedOutcome, SeedReport, SeedRow, differing_fields};

/// The in-tree platform station bundle, resolved from this crate — a
/// DIRECTORY, one `<name>.toml` per station, beside the Workflow
/// bundle `crate::registry::platform_bundle_path` names.
pub fn platform_stations_path() -> &'static str {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../infra/platform/stations"
    )
}

/// The station bundle that sits beside a Workflow bundle directory —
/// `<workflows dir>/../stations`. This is how the seed binary finds
/// it from the one `--seed-path` every launcher already passes
/// (bootstrap-db.sh, oss-quickstart init.sh), so no unit, Dockerfile
/// or script needed an edit to start publishing stations.
pub fn stations_beside(workflows: &Path) -> std::path::PathBuf {
    workflows
        .parent()
        .map(|p| p.join("stations"))
        .unwrap_or_else(|| std::path::PathBuf::from("stations"))
}

/// Load the bundle beside a Workflow bundle directory, or `None` when
/// there is none there — a caller decides what absence means (the
/// seed binary warns; a test asserts).
pub fn load_stations_beside(workflows: &Path) -> Option<Result<Vec<StationSpec>, SeedLoaderError>> {
    let dir = stations_beside(workflows);
    dir.is_dir().then(|| load_stations(&dir))
}

impl Declared for StationSpec {
    fn name(&self) -> &str {
        &self.name
    }
    fn version(&self) -> i32 {
        self.version
    }
    fn status(&self) -> WorkflowStatus {
        self.status
    }
}

#[async_trait]
impl<'a> BundleRegistry for dyn StationRegistry + 'a {
    type Spec = StationSpec;
    type Error = StationError;
    const LABEL: &'static str = "platform-station-seed";
    const BUNDLE: &'static str = "infra/platform/stations/<name>.toml";

    async fn live_active(&self, name: &str) -> Result<Option<StationSpec>, StationError> {
        match self.get_active(name).await {
            Ok(row) => Ok(Some(row)),
            Err(StationError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn live_version(
        &self,
        name: &str,
        version: i32,
    ) -> Result<Option<StationSpec>, StationError> {
        match self.get_version(name, version).await {
            Ok(row) => Ok(Some(row)),
            Err(StationError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn publish_declared(
        &self,
        spec: StationSpec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<StationSpec, StationError> {
        StationRegistry::publish_declared(self, spec, actor, now).await
    }
}

pub type StationSeedError = BundleSeedError<StationError>;

/// Publish `specs` into `registry` by the decision table in
/// [`crate::bundle_seed`]. Classifies every row first and refuses
/// whole if any row contradicts its live twin; only then writes.
/// `dry_run` reports what would be written and writes nothing.
pub async fn seed_stations(
    registry: &dyn StationRegistry,
    specs: &[StationSpec],
    actor: &boss_core::actor::ActorId,
    now: DateTime<Utc>,
    dry_run: bool,
) -> Result<SeedReport, StationSeedError> {
    seed_bundle(registry, specs, actor, now, dry_run).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::station_queue::StationPredicate;
    use crate::stations::{InMemoryStations, StationKind};
    use boss_core::actor::ActorId;

    fn actor() -> ActorId {
        ActorId::Automation("platform-workflow-seed".into())
    }

    fn now() -> DateTime<Utc> {
        DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::days(20_000)
    }

    fn spec(name: &str, version: i32) -> StationSpec {
        let mut s = StationSpec::draft(
            name,
            format!("Station {name}"),
            StationKind::Batch,
            StationPredicate {
                kind: Some("ship-a-change".into()),
                ..Default::default()
            },
            now(),
        );
        s.version = version;
        s.status = crate::registry::WorkflowStatus::Active;
        s
    }

    async fn seeded(specs: &[StationSpec]) -> InMemoryStations {
        let registry = InMemoryStations::new();
        seed_stations(&registry, specs, &actor(), now(), false)
            .await
            .expect("seed into an empty registry");
        registry
    }

    #[tokio::test]
    async fn an_empty_registry_gets_every_row_at_its_declared_version() {
        let registry = InMemoryStations::new();
        let report = seed_stations(
            &registry,
            &[spec("dock", 3), spec("repair", 1)],
            &actor(),
            now(),
            false,
        )
        .await
        .expect("seed");
        assert_eq!(report.count(|o| *o == SeedOutcome::Inserted), 2);
        let dock = registry.get_active("dock").await.expect("dock active");
        assert_eq!(dock.version, 3, "the DECLARED version lands, not max+1");
        assert_eq!(
            registry
                .list_versions("dock")
                .await
                .expect("versions")
                .len(),
            1,
            "no synthetic history below the declared version"
        );
        assert_eq!(
            registry.recorded_events().len(),
            2,
            "one jobs.station.published per row written"
        );
    }

    #[tokio::test]
    async fn a_second_run_is_a_no_op_that_records_nothing() {
        let registry = seeded(&[spec("dock", 3)]).await;
        let before = registry.recorded_events().len();
        let report = seed_stations(&registry, &[spec("dock", 3)], &actor(), now(), false)
            .await
            .expect("seed");
        assert_eq!(report.rows[0].outcome, SeedOutcome::Present);
        assert_eq!(registry.recorded_events().len(), before);
    }

    #[tokio::test]
    async fn a_row_edited_without_a_version_bump_is_refused_by_field() {
        let registry = seeded(&[spec("dock", 3), spec("repair", 1)]).await;
        let mut edited = spec("dock", 3);
        edited.wip_limit = Some(24);
        edited.title = "Renamed".into();
        let mut also = spec("repair", 1);
        also.discipline = vec![crate::station_queue::DisciplineKey::Age];
        let err = seed_stations(&registry, &[edited, also], &actor(), now(), false)
            .await
            .expect_err("refused");
        let StationSeedError::Refused { rows: refusals, .. } = err else {
            panic!("expected a refusal");
        };
        assert_eq!(refusals.len(), 2, "every offending row is named at once");
        assert_eq!(refusals[0].name, "dock");
        assert_eq!(refusals[0].fields, vec!["title", "wip_limit"]);
        assert_eq!(refusals[1].fields, vec!["discipline"]);
        assert_eq!(
            registry.get_active("dock").await.expect("dock").title,
            "Station dock",
            "a refused seed writes nothing"
        );
    }

    #[tokio::test]
    async fn a_version_bump_publishes_over_the_lower_live_row() {
        let registry = seeded(&[spec("dock", 3)]).await;
        let mut bumped = spec("dock", 4);
        bumped.wip_limit = Some(24);
        let report = seed_stations(&registry, &[bumped], &actor(), now(), false)
            .await
            .expect("seed");
        assert_eq!(
            report.rows[0].outcome,
            SeedOutcome::Published { superseded: 3 }
        );
        assert_eq!(registry.get_active("dock").await.expect("dock").version, 4);
        assert_eq!(
            registry
                .get_version("dock", 3)
                .await
                .expect("v3 is history")
                .status,
            crate::registry::WorkflowStatus::Retired
        );
    }

    #[tokio::test]
    async fn a_live_lineage_ahead_of_the_bundle_is_left_alone() {
        // An operator published v4 through the API after the bundle
        // seeded v3: v3 is retired live, v4 active.
        let registry = seeded(&[spec("dock", 3)]).await;
        let mut v4 = spec("dock", 4);
        v4.title = "Operator's dock".into();
        registry
            .publish_declared(v4, &actor(), now())
            .await
            .expect("operator publish");
        // The bundle still says v3 — and the operator's v4 is
        // NOT what v3 said. Neither a refusal nor a rewrite.
        let mut bundle_v3 = spec("dock", 3);
        bundle_v3.title = "Bundle says something else".into();
        let report = seed_stations(&registry, &[bundle_v3], &actor(), now(), false)
            .await
            .expect("an operator's later version is not the bundle's problem");
        assert_eq!(
            report.rows[0].outcome,
            SeedOutcome::Superseded {
                live_active: Some(4)
            }
        );
        assert_eq!(
            registry.get_active("dock").await.expect("dock").title,
            "Operator's dock"
        );
    }

    #[tokio::test]
    async fn a_bundle_behind_a_live_version_it_never_declared_is_left_alone() {
        let registry = seeded(&[spec("dock", 5)]).await;
        let report = seed_stations(&registry, &[spec("dock", 3)], &actor(), now(), false)
            .await
            .expect("seed");
        assert_eq!(
            report.rows[0].outcome,
            SeedOutcome::Behind { live_active: 5 }
        );
        assert_eq!(registry.get_active("dock").await.expect("dock").version, 5);
    }

    #[tokio::test]
    async fn a_dry_run_reports_and_writes_nothing() {
        let registry = InMemoryStations::new();
        let report = seed_stations(&registry, &[spec("dock", 3)], &actor(), now(), true)
            .await
            .expect("dry run");
        assert_eq!(report.rows[0].outcome, SeedOutcome::Inserted);
        assert!(report.to_string().contains("WOULD be inserted"), "{report}");
        assert!(registry.list_active().await.expect("list").is_empty());
    }

    #[test]
    fn created_at_is_not_part_of_the_declaration() {
        let a = spec("dock", 3);
        let mut b = spec("dock", 3);
        b.created_at = now() + chrono::Duration::hours(1);
        assert!(differing_fields(&a, &b).is_empty());
    }

    #[test]
    fn the_bundle_beside_a_workflows_dir_is_its_sibling() {
        assert_eq!(
            stations_beside(Path::new("infra/platform/workflows")),
            std::path::PathBuf::from("infra/platform/stations")
        );
    }
}
