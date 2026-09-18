//! One publish for every platform bundle — `infra/platform/<registry>/`
//! into its registry, insert-if-missing by (name, version).
//!
//! WHY ONE (backlog 393d3234, consolidation H4). Car 1 wrote this
//! decision table for stations in `station_seed.rs`; car 2 needed it
//! again for step plugins, and cars 3 and 4 (cadence rules, delivery
//! policy) will need it twice more. The table, the report, the refusal
//! and the classify-then-write discipline are the same for every
//! registry that has a name, a version and an active row — only the
//! one read and the one write differ, and those are the
//! [`BundleRegistry`] port. A fact that lived twice would have been
//! pinned by an equality test (CLAUDE.md §9a); a function that lives
//! once needs no pin.
//!
//! THE DECISION TABLE, per bundle row `(name, version)`:
//!
//! | live registry                                   | outcome                 |
//! |-------------------------------------------------|-------------------------|
//! | no row of this name                             | insert, active          |
//! | rows of this name, NONE active                  | retired — left alone    |
//! | active row at a LOWER version                   | publish; retire the old |
//! | row at (name, version) active and EQUAL         | present — untouched     |
//! | row at (name, version) active and DIFFERENT     | REFUSED, by field       |
//! | row at (name, version) exists but is not active | superseded — untouched  |
//! | no row at (name, version); active row is HIGHER | behind — untouched      |
//!
//! PRESENT MEANS ANY VERSION (backlog 8b2eaff2, 2026-09-18). The
//! second row is the one every registry got wrong, because "present"
//! was read as "an active row exists": the operator retired
//! `maintenance-deploy-confirm` at 12:20Z and the next boot's Workflow
//! seed published it again at 12:42Z as v3, active — the seed
//! reverting an operator's decision, on every boot, for as long as the
//! file was in the tree. A lineage with no active row is a kind
//! someone RETIRED, and re-activation is an explicit publish, never a
//! boot. So the first question is asked of the whole lineage, once,
//! here, for every registry that seeds from a bundle; the version
//! comparison below it is asked only of a lineage that has an active
//! row.
//!
//! THE WORKFLOW BUNDLE IS UNVERSIONED. A station or step-plugin file
//! declares its `version`; a Workflow file does not — the registry
//! assigns max+1 at `create_draft`, and the file is what a FRESH
//! database gets. So a registry says which table it reads with
//! [`BundleRegistry::VERSIONED`]: the Workflow seed stops at the first
//! two rows (absent → insert; retired → left alone; anything active →
//! present) and never compares, refuses or supersedes — "present means
//! present" is protocols-as-data Q1 as David answered it. Until this
//! car the Workflow seed's decision was an inline copy in the binary
//! (and a second inline copy in its test) — the §9a pair the packet
//! names, collapsed here.
//!
//! Insert-if-missing and nothing else, as the Workflow seed is: a row
//! an operator published lives on, and this never rewrites one. The
//! refusal is what makes editing safe — a bundle file changed without
//! a version bump would otherwise silently disagree with every
//! deployment that already has the row, which is the drift this move
//! exists to remove. A version bump is the edit path: the seed
//! publishes the new version and retires the live one, exactly as the
//! registry's own publish verb would.
//!
//! Every refusal is collected before anything is written, so a
//! refused seed writes nothing and names every offending row at once.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::registry::WorkflowStatus;

/// A bundle row: something with a name, a version, a status and
/// columns. `created_at` is not part of the declaration — it is when
/// the deployment was built, not what the row is — and is stripped
/// from the comparison here so no registry has to remember to.
pub trait Declared: Clone + Serialize + Send + Sync {
    fn name(&self) -> &str;
    fn version(&self) -> i32;
    fn status(&self) -> WorkflowStatus;
}

/// What a bundle seed needs of a registry: one read of a name's whole
/// lineage, and the one write that lands a row.
#[async_trait]
pub trait BundleRegistry: Send + Sync {
    type Spec: Declared;
    type Error: std::error::Error + Send + Sync + 'static;

    /// The seed's name in its report line — `platform-station-seed`.
    const LABEL: &'static str;
    /// Where a row is edited — `infra/platform/stations/<name>.toml`.
    const BUNDLE: &'static str;
    /// Whether a bundle row declares its own `version` (stations, step
    /// plugins) or the registry assigns one at publish (workflows).
    /// An unversioned bundle is decided by lineage alone — see the
    /// module doc.
    const VERSIONED: bool;

    /// Every live row of this name, any status, any order — the whole
    /// lineage, so "present" is answered from all of it. Empty when
    /// the registry has never held the name.
    async fn live_versions(&self, name: &str) -> Result<Vec<Self::Spec>, Self::Error>;
    /// Retire any active row of the same name and insert `spec` active
    /// — at `spec.version()` for a versioned bundle, at the registry's
    /// next version otherwise — recording the registry's published
    /// event.
    async fn publish_declared(
        &self,
        spec: Self::Spec,
        actor: &boss_core::actor::ActorId,
        now: DateTime<Utc>,
    ) -> Result<Self::Spec, Self::Error>;
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
    /// The live active row IS this declaration, every column (a
    /// versioned bundle) — or the name has an active row at all (an
    /// unversioned one).
    Present,
    /// The name has live rows and NONE is active: an operator retired
    /// it (or a draft is still being authored — the same rule, since
    /// publishing over someone's draft is the same revert). `newest`
    /// is the highest live version. Untouched: re-activation is an
    /// explicit publish, never a boot.
    Retired { newest: i32 },
    /// The declared (name, version) exists live but is not the active
    /// row: the live lineage moved past it (an operator published a
    /// later version, `live_active`). Untouched.
    Superseded { live_active: i32 },
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
            Self::Retired { newest } => write!(
                f,
                "retired (left alone): no live version is active, newest is v{newest} \
                 — an operator's decision; re-activation is an explicit publish"
            ),
            Self::Superseded { live_active } => {
                write!(f, "superseded live by v{live_active}, untouched")
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

/// One row's line in the report. `version` is what the bundle
/// declared — `None` for an unversioned bundle, whose rows are
/// reported by name alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedRow {
    pub name: String,
    pub version: Option<i32>,
    pub outcome: SeedOutcome,
}

/// What a seed run did, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedReport {
    /// The seed's name, for the summary line.
    pub label: &'static str,
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
            let at = row.version.map(|v| format!("@v{v}")).unwrap_or_default();
            writeln!(f, "  {}{at}: {verb}{}", row.name, row.outcome)?;
        }
        let inserted = self.count(|o| matches!(o, SeedOutcome::Inserted));
        let published = self.count(|o| matches!(o, SeedOutcome::Published { .. }));
        let present = self.count(|o| matches!(o, SeedOutcome::Present));
        let retired = self.count(|o| matches!(o, SeedOutcome::Retired { .. }));
        let untouched = self.rows.len() - inserted - published - present - retired;
        write!(
            f,
            "{}: {inserted} inserted, {published} published, \
             {present} already present, {retired} retired (left alone), \
             {untouched} left to the live lineage{}",
            self.label,
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

/// The seed's failure: a refusal (nothing written) or the registry's
/// own error. Each carries the seed's own words — which seed, and
/// which file to bump `version` in — so the message reads the same
/// whether it is the station seed or the step-plugin seed that
/// refused; `E` is the registry's error type.
#[derive(Debug)]
pub enum BundleSeedError<E> {
    /// One or more bundle rows differ from the live ACTIVE row of the
    /// same (name, version). Nothing was written.
    Refused {
        seed: &'static str,
        bundle: &'static str,
        rows: Vec<Refusal>,
    },
    /// The registry failed on one row — reading its lineage, or
    /// publishing it (a Workflow the viability lint refuses lands
    /// here). Names the row: a boot that fails must say which kind.
    Registry {
        seed: &'static str,
        row: String,
        error: E,
    },
}

impl<E: std::error::Error> std::error::Error for BundleSeedError<E> {}

impl<E: std::fmt::Display> std::fmt::Display for BundleSeedError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { seed, bundle, rows } => f.write_str(&refusal_text(seed, bundle, rows)),
            Self::Registry { seed, row, error } => write!(f, "{seed}: {row}: {error}"),
        }
    }
}

/// The refusal in the registry's own words: which seed, and which file
/// to bump `version` in.
fn refusal_text(seed: &str, bundle: &str, rows: &[Refusal]) -> String {
    let mut out = format!(
        "{seed} REFUSED — a bundle row differs from the live active row of the same \
         (name, version), and this seed never rewrites a live row:\n"
    );
    for r in rows {
        out.push_str(&format!(
            "  {}@v{}: differs in {}\n",
            r.name,
            r.version,
            r.fields.join(", ")
        ));
    }
    out.push_str(&format!(
        "A version bump is the edit path: raise `version` in {bundle} and the seed \
         publishes the new version, retiring the live one. Nothing was written."
    ));
    out
}

/// A row as a DECLARATION: every column but `created_at`.
fn declaration<S: Declared>(spec: &S) -> BTreeMap<String, serde_json::Value> {
    let mut map: BTreeMap<String, serde_json::Value> = serde_json::to_value(spec)
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    map.remove("created_at");
    map
}

/// The columns on which two rows disagree as declarations, sorted.
pub fn differing_fields<S: Declared>(a: &S, b: &S) -> Vec<String> {
    let (da, db) = (declaration(a), declaration(b));
    da.keys()
        .chain(db.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|k| da.get(*k) != db.get(*k))
        .cloned()
        .collect()
}

/// What a name's live lineage says before any version is compared:
/// nothing, rows nobody has active, or the active row.
enum Lineage<'a, S> {
    Absent,
    Retired { newest: i32 },
    Active(&'a S),
}

fn lineage<S: Declared>(live: &[S]) -> Lineage<'_, S> {
    match live.iter().find(|r| r.status() == WorkflowStatus::Active) {
        Some(active) => Lineage::Active(active),
        None => match live.iter().map(Declared::version).max() {
            Some(newest) => Lineage::Retired { newest },
            None => Lineage::Absent,
        },
    }
}

/// The decision table as a pure function of the live lineage and the
/// bundle row. `versioned` selects the table: the whole of it when the
/// bundle declares the row's version, its first two rows when the
/// registry assigns one.
pub fn decide<S: Declared>(live: &[S], spec: &S, versioned: bool) -> Result<SeedOutcome, Refusal> {
    let active = match lineage(live) {
        Lineage::Absent => return Ok(SeedOutcome::Inserted),
        Lineage::Retired { newest } => return Ok(SeedOutcome::Retired { newest }),
        Lineage::Active(_) if !versioned => return Ok(SeedOutcome::Present),
        Lineage::Active(active) => active,
    };
    match live.iter().find(|r| r.version() == spec.version()) {
        Some(live) if live.status() != WorkflowStatus::Active => Ok(SeedOutcome::Superseded {
            live_active: active.version(),
        }),
        Some(live) => {
            let fields = differing_fields(live, spec);
            if fields.is_empty() {
                Ok(SeedOutcome::Present)
            } else {
                Err(Refusal {
                    name: spec.name().to_string(),
                    version: spec.version(),
                    fields,
                })
            }
        }
        None if active.version() < spec.version() => Ok(SeedOutcome::Published {
            superseded: active.version(),
        }),
        None => Ok(SeedOutcome::Behind {
            live_active: active.version(),
        }),
    }
}

/// Classify one bundle row against the live registry. Reads only.
async fn classify<R: BundleRegistry + ?Sized>(
    registry: &R,
    spec: &R::Spec,
) -> Result<Result<SeedOutcome, Refusal>, R::Error> {
    let live = registry.live_versions(spec.name()).await?;
    Ok(decide(&live, spec, R::VERSIONED))
}

/// Publish `specs` into `registry` by the decision table in the
/// module doc. Classifies every row first and refuses whole if any
/// row contradicts its live twin; only then writes. `dry_run` reports
/// what would be written and writes nothing.
pub async fn seed_bundle<R: BundleRegistry + ?Sized>(
    registry: &R,
    specs: &[R::Spec],
    actor: &boss_core::actor::ActorId,
    now: DateTime<Utc>,
    dry_run: bool,
) -> Result<SeedReport, BundleSeedError<R::Error>> {
    let registry_error = |spec: &R::Spec, error| BundleSeedError::Registry {
        seed: R::LABEL,
        row: spec.name().to_string(),
        error,
    };
    let mut rows = Vec::with_capacity(specs.len());
    let mut refusals = Vec::new();
    for spec in specs {
        match classify(registry, spec)
            .await
            .map_err(|e| registry_error(spec, e))?
        {
            Ok(outcome) => rows.push(SeedRow {
                name: spec.name().to_string(),
                version: R::VERSIONED.then(|| spec.version()),
                outcome,
            }),
            Err(refusal) => refusals.push(refusal),
        }
    }
    if !refusals.is_empty() {
        return Err(BundleSeedError::Refused {
            seed: R::LABEL,
            bundle: R::BUNDLE,
            rows: refusals,
        });
    }
    if !dry_run {
        for (row, spec) in rows.iter().zip(specs) {
            if matches!(
                row.outcome,
                SeedOutcome::Inserted | SeedOutcome::Published { .. }
            ) {
                registry
                    .publish_declared(spec.clone(), actor, now)
                    .await
                    .map_err(|e| registry_error(spec, e))?;
            }
        }
    }
    Ok(SeedReport {
        label: R::LABEL,
        dry_run,
        rows,
    })
}
