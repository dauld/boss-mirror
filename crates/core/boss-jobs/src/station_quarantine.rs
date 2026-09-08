//! Boot-time quarantine of unviable ACTIVE stations — the station
//! counterpart to [`crate::workflow_quarantine`].
//!
//! [`crate::station_lint::gate_active`] makes this pass rare by
//! construction: no API path can set an unviable row active. Quarantine
//! exists for the paths that never touch publish — rows INSERTed by the
//! SQL seeds (`116-stations.sql` and friends), rows that predate the
//! gate, and rows edited directly in the database.
//!
//! **Why retiring is safe here, and why this is simpler than the
//! Workflow pass.** A Workflow row cannot be auto-retired when open Jobs
//! are pinned to it — retiring would strand live work — which is why the
//! Workflow pass only reports. Stations have no such case *by construction*:
//! membership is DERIVED, evaluated from the predicate at read time
//! against open Jobs, with no station field on the packet
//! (`116-stations.sql`). Nothing is ever pinned to a station version, so
//! retiring one strands nothing. There is no refuse-to-start path in
//! this file at all.
//!
//! **Why retire rather than leave it live.** Every rule the lint
//! enforces describes a queue that is either permanently empty or actively
//! misleading — an `actor` row that never binds `@me` shows every
//! executor the same list while claiming to be personal. An absent
//! queue is a question an operator asks; a wrong queue is one they
//! trust. The marker event carries the problems, and `upstream` gives
//! them somewhere to look.

use chrono::{DateTime, Utc};

use crate::port::JobsRepository;
use crate::station_lint::{StationLintError, validate_station};
use crate::stations::{StationRegistry, StationSpec};

/// The actor every quarantine write is stamped with — a named platform
/// automation, so the log says who retired the row.
pub const QUARANTINE_ACTOR: &str = "station-quarantine";

/// A station row this pass retired.
#[derive(Debug, Clone)]
pub struct QuarantinedStation {
    pub name: String,
    pub version: i32,
    pub problems: Vec<StationLintError>,
}

/// What one pass did.
#[derive(Debug, Default)]
pub struct StationQuarantineReport {
    /// How many ACTIVE rows were examined.
    pub checked: usize,
    pub quarantined: Vec<QuarantinedStation>,
}

/// Run one quarantine pass over every active station.
///
/// Retires each unviable row and records a `jobs.station.quarantined`
/// marker for it. `Err` is reserved for the pass itself failing
/// (registry unreachable, retire write failed) — the caller cannot tell
/// whether the registry is clean, so it must decide what to do about
/// that. A row that is merely unviable is never an `Err`.
pub async fn quarantine_unviable_active_stations<R: JobsRepository + ?Sized>(
    registry: &dyn StationRegistry,
    jobs: &R,
    actor: &boss_core::actor::ActorId,
    now: DateTime<Utc>,
) -> Result<StationQuarantineReport, String> {
    let active = registry
        .list_active()
        .await
        .map_err(|e| format!("could not list active stations: {e}"))?;

    let mut report = StationQuarantineReport {
        checked: active.len(),
        ..Default::default()
    };

    for spec in &active {
        let problems = validate_station(spec);
        if problems.is_empty() {
            continue;
        }
        for p in &problems {
            tracing::error!("boot station check: {p}");
        }
        retire_and_mark(registry, jobs, spec, &problems, actor, now).await?;
        report.quarantined.push(QuarantinedStation {
            name: spec.name.clone(),
            version: spec.version,
            problems,
        });
    }

    if report.quarantined.is_empty() {
        tracing::info!(active = report.checked, "boot station check passed");
    }
    Ok(report)
}

/// Retire the row through the registry (recording
/// `jobs.station.retired` in the same transaction as the flip) and
/// record the loud `jobs.station.quarantined` marker.
async fn retire_and_mark<R: JobsRepository + ?Sized>(
    registry: &dyn StationRegistry,
    jobs: &R,
    spec: &StationSpec,
    problems: &[StationLintError],
    actor: &boss_core::actor::ActorId,
    now: DateTime<Utc>,
) -> Result<(), String> {
    registry
        .retire(&spec.name, actor, now)
        .await
        .map_err(|e| format!("could not quarantine station `{}`: {e}", spec.name))?;

    let event = crate::events::station_quarantined_event(actor, spec, problems);
    jobs.record_events(std::slice::from_ref(&event))
        .await
        .map_err(|e| {
            format!(
                "quarantined station `{}` but could not record the marker event: {e}",
                spec.name
            )
        })?;
    Ok(())
}
