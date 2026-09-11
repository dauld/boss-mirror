//! Boot-time viability check of ACTIVE Workflows.
//!
//! Every active Workflow must pass the viability lint
//! ([`crate::workflow_lint`]) — a spec whose graph can't reach an
//! outcome describes work no Job can finish, and a previously-valid
//! spec can go bad when an upstream StepType's enum domain changes.
//!
//! **What this pass does:** re-lint every active row and log each
//! failure at ERROR — kind, version, the problems, and how many open
//! Jobs are pinned to it. Then let the service start. It writes
//! nothing and it refuses nothing. Quarantine — retiring the row — is
//! a deliberate act an operator or a packet takes through the
//! registry; boot only says, loudly, that it is needed.
//!
//! **Why it does nothing more.** Two earlier contracts each took the
//! system of record down:
//!
//! - 2026-08-13: any unviable row made the API refuse to start. A
//!   `protocol-retro` row with no terminal published cleanly, lay
//!   latent, and took jobs, docs, the gateway and the human door down
//!   for ~11 minutes on a routine pod roll. Recovery needed direct SQL.
//! - 2026-09-07: the replacement auto-retired unpinned rows and
//!   refused to start over pinned ones. Boot found
//!   `incident-post-mortem` v1 unviable with one open Job pinned and
//!   refused — the pod crash-looped and the system of record was down.
//!   In the same pass it found `publish-to-github` v5 unviable with no
//!   Jobs pinned and retired it: a persisted write that silently
//!   disabled a live protocol, which nobody had asked for.
//!
//! Both are the same defect: a boot check that ACTS on a data
//! condition. A check may not take the service hostage and may not
//! take an action of its own. It reports; the report is the value.
//!
//! The publish gate ([`crate::workflow_lint::gate_active`], Layer 1)
//! makes a finding here rare BY CONSTRUCTION: no API path can set an
//! unviable row active any more. This pass exists for rows that
//! predate the gate, rows written by direct SQL, and specs that went
//! bad under a StepType change — not for anything publish can still
//! let through.

use crate::port::JobsRepository;
use crate::registry::WorkflowRegistry;
use crate::step_registry::StepRegistry;
use crate::workflow_lint::{WorkflowLintError, validate_workflow};

/// An ACTIVE Workflow row that fails the viability lint. Boot reports
/// it and does nothing to it.
#[derive(Debug, Clone)]
pub struct Unviable {
    pub kind: String,
    pub version: i32,
    /// Non-terminal Jobs pinned to this exact `(kind, version)` — the
    /// live work a retirement would strand.
    pub open_jobs: i64,
    pub problems: Vec<WorkflowLintError>,
}

/// What one boot check found.
#[derive(Debug, Clone, Default)]
pub struct ViabilityReport {
    /// Active Workflows examined.
    pub checked: usize,
    pub unviable: Vec<Unviable>,
}

/// Check every active Workflow against the viability lint. Each
/// failure is logged at ERROR with its kind, version, problems and
/// pinned open-Job count, and collected in the report. Nothing is
/// written and nothing is refused: the caller starts either way.
///
/// `Err` is reserved for the check itself failing (registry
/// unreachable, open-Job count failed). That is not a reason to
/// refuse to start either — the caller logs it and continues.
pub async fn check_active_workflows_viable<R: JobsRepository + ?Sized>(
    registry: &dyn WorkflowRegistry,
    jobs: &R,
) -> Result<ViabilityReport, String> {
    let active = registry
        .list_active(None)
        .await
        .map_err(|e| format!("could not list active Workflows: {e}"))?;

    let step_types = StepRegistry::v1();
    let mut report = ViabilityReport {
        checked: active.len(),
        ..Default::default()
    };

    for spec in &active {
        let problems = validate_workflow(spec, &step_types);
        if problems.is_empty() {
            continue;
        }
        // The problems are the operator's first clue — one line each.
        for p in &problems {
            tracing::error!("boot viability check: {p}");
        }

        let open_jobs = jobs
            .count_open_jobs_for_workflow(&spec.kind, spec.version)
            .await
            .map_err(|e| {
                format!(
                    "could not count open Jobs pinned to `{}` v{}: {e}",
                    spec.kind, spec.version
                )
            })?;

        tracing::error!(
            kind = %spec.kind,
            version = spec.version,
            open_jobs,
            problems = problems.len(),
            "unviable active Workflow — NOT retired at boot; quarantine is a deliberate act, \
             file it (publish a viable version, or retire the row and migrate its open Jobs)"
        );
        report.unviable.push(Unviable {
            kind: spec.kind.clone(),
            version: spec.version,
            open_jobs,
            problems,
        });
    }

    if report.unviable.is_empty() {
        tracing::info!(active = report.checked, "boot viability check passed");
    }
    Ok(report)
}
