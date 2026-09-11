//! Boot-time viability check of ACTIVE stations — the station
//! counterpart to [`crate::workflow_quarantine`], and the same contract.
//!
//! [`crate::station_lint::gate_active`] makes a finding here rare by
//! construction: no API path can set an unviable row active. This pass
//! exists for the paths that never touch publish — rows INSERTed by the
//! SQL seeds (`116-stations.sql` and friends), rows that predate the
//! gate, and rows edited directly in the database.
//!
//! **What this pass does:** re-lint every active row and log each
//! failure at ERROR — name, version, the problems. Then let the
//! service start. It writes nothing and it refuses nothing.
//! Quarantine — retiring the row — is a deliberate act an operator or
//! a packet takes through the registry; boot only says, loudly, that
//! it is needed.
//!
//! **Why it does nothing more.** Until 2026-09-08 this pass retired
//! each unviable row at boot, on the argument that station membership
//! is DERIVED (evaluated from the predicate at read time, no station
//! field on the packet) so nothing is pinned to a station version and
//! retiring strands nothing. That argument is true and beside the
//! point. The Workflow pass took the system of record down twice on
//! 2026-09-07 (outage packet 7752e636) — once by refusing to start,
//! once by retiring a live protocol nobody had asked to retire — and
//! the defect in both was the same one this file had: a boot check
//! that ACTS on a data condition. A persisted, non-idempotent write
//! from a boot path is a write that happens on every roll, credited
//! to an automation, that no packet filed. A check may not take an
//! action of its own. It reports; the report is the value.
//!
//! An `actor` row that never binds `@me` still shows every executor
//! the same list while claiming to be personal — a wrong queue is one
//! operators trust. That is exactly why the finding is at ERROR and
//! names the row: so the operator files the retirement, deliberately.

use crate::station_lint::{StationLintError, validate_station};
use crate::stations::StationRegistry;

/// An ACTIVE station row that fails the viability lint. Boot reports
/// it and does nothing to it.
#[derive(Debug, Clone)]
pub struct UnviableStation {
    pub name: String,
    pub version: i32,
    pub problems: Vec<StationLintError>,
}

/// What one boot check found.
#[derive(Debug, Clone, Default)]
pub struct StationViabilityReport {
    /// How many ACTIVE rows were examined.
    pub checked: usize,
    pub unviable: Vec<UnviableStation>,
}

/// Check every active station against the viability lint. Each
/// failure is logged at ERROR with its name, version and problems,
/// and collected in the report. Nothing is written and nothing is
/// refused: the caller starts either way.
///
/// `Err` is reserved for the check itself failing (registry
/// unreachable). That is not a reason to refuse to start either — the
/// caller logs it and continues.
pub async fn check_active_stations_viable(
    registry: &dyn StationRegistry,
) -> Result<StationViabilityReport, String> {
    let active = registry
        .list_active()
        .await
        .map_err(|e| format!("could not list active stations: {e}"))?;

    let mut report = StationViabilityReport {
        checked: active.len(),
        ..Default::default()
    };

    for spec in &active {
        let problems = validate_station(spec);
        if problems.is_empty() {
            continue;
        }
        // The problems are the operator's first clue — one line each.
        for p in &problems {
            tracing::error!("boot station check: {p}");
        }
        tracing::error!(
            name = %spec.name,
            version = spec.version,
            problems = problems.len(),
            "unviable active station — NOT retired at boot; quarantine is a deliberate act, \
             file it (publish a viable version, or retire the row)"
        );
        report.unviable.push(UnviableStation {
            name: spec.name.clone(),
            version: spec.version,
            problems,
        });
    }

    if report.unviable.is_empty() {
        tracing::info!(active = report.checked, "boot station check passed");
    }
    Ok(report)
}
