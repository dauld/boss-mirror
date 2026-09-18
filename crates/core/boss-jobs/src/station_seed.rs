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
//! THE DECISION TABLE, per bundle row `(name, version)`:
//!
//! | live registry                                   | outcome                 |
//! |-------------------------------------------------|-------------------------|
//! | no row of this name                             | insert, active          |
//! | active row at a LOWER version                   | publish; retire the old |
//! | row at (name, version) active and EQUAL         | present — untouched     |
//! | row at (name, version) active and DIFFERENT     | REFUSED, by field       |
//! | row at (name, version) exists but is not active | superseded — untouched  |
//! | no row at (name, version); active row is HIGHER | behind — untouched      |
//!
//! Insert-if-missing and nothing else, as the Workflow seed is: a row
//! an operator published lives on, and this never rewrites one. The
//! refusal is what makes editing safe — a bundle file changed without
//! a version bump would otherwise silently disagree with every
//! deployment that already has the row, which is the drift this car
//! exists to remove. A version bump is the edit path: the seed
//! publishes the new version and retires the live one, exactly as
//! `POST /api/stations/{name}/publish` would.
//!
//! Every refusal is collected before anything is written, so a
//! refused seed writes nothing and names every offending row at once.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{DateTime, Utc};

use crate::seed_loader::{SeedLoaderError, load_stations};
use crate::stations::{StationError, StationRegistry, StationSpec};

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

/// What the seed did — or, on a dry run, would do — with one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedOutcome {
    /// No row of this name existed; the declared version was inserted
    /// active.
    Inserted,
    /// A lower version was active; the declared version was published
    /// over it and the old one retired.
    Published { superseded: i32 },
    /// The live active row IS this declaration, every column.
    Present,
    /// The declared (name, version) exists live but is not the active
    /// row: the live lineage moved past it (an operator published a
    /// later version). Untouched.
    Superseded { live_active: Option<i32> },
    /// No row at the declared version and the live active row is a
    /// HIGHER version: the bundle is behind the deployment. Untouched,
    /// and worth a version bump in the tree.
    Behind { live_active: i32 },
}

impl std::fmt::Display for SeedOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Inserted => write!(f, "inserted"),
            Self::Published { superseded } => write!(f, "published, retiring v{superseded}"),
            Self::Present => write!(f, "already present, untouched"),
            Self::Superseded {
                live_active: Some(v),
            } => write!(f, "superseded live by v{v}, untouched"),
            Self::Superseded { live_active: None } => {
                write!(
                    f,
                    "exists live but is not active (no active row), untouched"
                )
            }
            Self::Behind { live_active } => {
                write!(
                    f,
                    "behind the live lineage (v{live_active} active), untouched"
                )
            }
        }
    }
}

/// One row's line in the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedRow {
    pub name: String,
    pub version: i32,
    pub outcome: SeedOutcome,
}

/// What a seed run did, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedReport {
    pub dry_run: bool,
    pub rows: Vec<SeedRow>,
}

impl SeedReport {
    /// How many rows had an outcome the predicate accepts.
    pub fn count(&self, pred: impl Fn(&SeedOutcome) -> bool) -> usize {
        self.rows.iter().filter(|r| pred(&r.outcome)).count()
    }
}

impl std::fmt::Display for SeedReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let would = if self.dry_run { "WOULD be " } else { "" };
        for row in &self.rows {
            let verb = match row.outcome {
                SeedOutcome::Inserted | SeedOutcome::Published { .. } => would,
                _ => "",
            };
            writeln!(f, "  {}@v{}: {verb}{}", row.name, row.version, row.outcome)?;
        }
        let inserted = self.count(|o| matches!(o, SeedOutcome::Inserted));
        let published = self.count(|o| matches!(o, SeedOutcome::Published { .. }));
        let present = self.count(|o| matches!(o, SeedOutcome::Present));
        let untouched = self.rows.len() - inserted - published - present;
        write!(
            f,
            "platform-station-seed: {inserted} inserted, {published} published, \
             {present} already present, {untouched} left to the live lineage{}",
            if self.dry_run { " (dry run)" } else { "" }
        )
    }
}

/// A bundle row the live registry contradicts at the same (name,
/// version): the columns that differ, in column order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub name: String,
    pub version: i32,
    pub fields: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum StationSeedError {
    /// One or more bundle rows differ from the live ACTIVE row of the
    /// same (name, version). Nothing was written.
    #[error("{}", refusal_text(.0))]
    Refused(Vec<Refusal>),
    #[error("station registry: {0}")]
    Registry(#[from] StationError),
}

fn refusal_text(refusals: &[Refusal]) -> String {
    let mut out = String::from(
        "platform-station-seed REFUSED — a bundle row differs from the live active row \
         of the same (name, version), and this seed never rewrites a live row:\n",
    );
    for r in refusals {
        out.push_str(&format!(
            "  {}@v{}: differs in {}\n",
            r.name,
            r.version,
            r.fields.join(", ")
        ));
    }
    out.push_str(
        "A version bump is the edit path: raise `version` in infra/platform/stations/<name>.toml \
         and the seed publishes the new version, retiring the live one. Nothing was written.",
    );
    out
}

/// A row as a DECLARATION: every column but `created_at`, which is
/// when the deployment was built rather than what the station is.
fn declaration(spec: &StationSpec) -> BTreeMap<String, serde_json::Value> {
    let mut map: BTreeMap<String, serde_json::Value> = serde_json::to_value(spec)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    map.remove("created_at");
    map
}

/// The columns on which two rows disagree as declarations, sorted.
pub fn differing_fields(a: &StationSpec, b: &StationSpec) -> Vec<String> {
    let (da, db) = (declaration(a), declaration(b));
    da.keys()
        .chain(db.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|k| da.get(*k) != db.get(*k))
        .cloned()
        .collect()
}

/// Classify one bundle row against the live registry. Reads only.
async fn classify(
    registry: &dyn StationRegistry,
    spec: &StationSpec,
) -> Result<Result<SeedOutcome, Refusal>, StationSeedError> {
    let active = match registry.get_active(&spec.name).await {
        Ok(row) => Some(row),
        Err(StationError::NotFound(_)) => None,
        Err(e) => return Err(e.into()),
    };
    match registry.get_version(&spec.name, spec.version).await {
        Ok(live) => {
            if live.status != crate::registry::WorkflowStatus::Active {
                return Ok(Ok(SeedOutcome::Superseded {
                    live_active: active.map(|a| a.version),
                }));
            }
            let fields = differing_fields(&live, spec);
            if fields.is_empty() {
                Ok(Ok(SeedOutcome::Present))
            } else {
                Ok(Err(Refusal {
                    name: spec.name.clone(),
                    version: spec.version,
                    fields,
                }))
            }
        }
        Err(StationError::NotFound(_)) => Ok(Ok(match active {
            None => SeedOutcome::Inserted,
            Some(a) if a.version < spec.version => SeedOutcome::Published {
                superseded: a.version,
            },
            Some(a) => SeedOutcome::Behind {
                live_active: a.version,
            },
        })),
        Err(e) => Err(e.into()),
    }
}

/// Publish `specs` into `registry` by the decision table in the
/// module doc. Classifies every row first and refuses whole if any
/// row contradicts its live twin; only then writes. `dry_run` reports
/// what would be written and writes nothing.
pub async fn seed_stations(
    registry: &dyn StationRegistry,
    specs: &[StationSpec],
    actor: &boss_core::actor::ActorId,
    now: DateTime<Utc>,
    dry_run: bool,
) -> Result<SeedReport, StationSeedError> {
    let mut rows = Vec::with_capacity(specs.len());
    let mut refusals = Vec::new();
    for spec in specs {
        match classify(registry, spec).await? {
            Ok(outcome) => rows.push(SeedRow {
                name: spec.name.clone(),
                version: spec.version,
                outcome,
            }),
            Err(refusal) => refusals.push(refusal),
        }
    }
    if !refusals.is_empty() {
        return Err(StationSeedError::Refused(refusals));
    }
    if !dry_run {
        for (row, spec) in rows.iter().zip(specs) {
            if matches!(
                row.outcome,
                SeedOutcome::Inserted | SeedOutcome::Published { .. }
            ) {
                registry.publish_declared(spec.clone(), actor, now).await?;
            }
        }
    }
    Ok(SeedReport { dry_run, rows })
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
        let StationSeedError::Refused(refusals) = err else {
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
