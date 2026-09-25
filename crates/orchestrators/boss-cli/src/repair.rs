//! `boss repair step-plugin-version [--dry-run]` — the operator's end of
//! the one-time repair door (backlog 5a670a71; the contract is
//! `boss_jobs::plugin_version_repair`).
//!
//! WHY. Until car 0f48e5f9 a step's STEP_CREATED carried plugin version
//! 0 while its row was stamped with the active version, so a rebuild
//! from the log moved every such step to 0. Measured 2026-09-25: 144
//! pending steps on open packets had nothing later in the log to carry
//! the stored value. The fix for them is a RECORD, not a rewrite: one
//! correcting `jobs.step.updated` per step, built from the stored row by
//! the jobs API, which judges each step under its row lock and refuses
//! anything but that exact divergence.
//!
//! `--dry-run` is `GET /api/jobs/repairs/step-plugin-version` and writes
//! nothing; without it the verb POSTs, then reads the dry run BACK — the
//! POST's answer is a claim, and "nothing left to correct" read after it
//! is the fact. Both print every divergent step, refusals included: a
//! refused step is a finding about the log, and a digest would drop the
//! only copy of which one. Signed as the actor running the verb; the
//! door takes `publish` on `step_plugin`.

use anyhow::{Context, Result, bail};
use boss_jobs::plugin_version_repair::{Outcome, RepairReport};

use crate::steps::Wire;

const DOOR: &str = "/api/jobs/repairs/step-plugin-version";

#[derive(clap::Subcommand)]
pub enum Cmd {
    /// One-time repairs of the record, each through a reviewed jobs-API door.
    Repair {
        #[command(subcommand)]
        what: What,
    },
}

#[derive(clap::Subcommand)]
pub enum What {
    /// Append one correcting jobs.step.updated per step whose log says plugin version 0 under a stamped row.
    ///
    /// Corrects only a PENDING step on an OPEN packet whose last state
    /// event is a version-0 jobs.step.created and which agrees with its
    /// row in every other field; every other divergence is listed as
    /// refused, with its packet's status and partition, and left
    /// alone. Idempotent: a second run writes nothing. Read the dry run
    /// first.
    StepPluginVersion {
        /// List what would be written, and write nothing.
        #[arg(long)]
        dry_run: bool,
    },
}

pub async fn dispatch(cmd: Cmd) -> Result<()> {
    let Cmd::Repair {
        what: What::StepPluginVersion { dry_run },
    } = cmd;
    let wire = Wire::live()?;
    println!("{}", step_plugin_version(&wire, dry_run).await?);
    Ok(())
}

async fn report(wire: &Wire, method: reqwest::Method) -> Result<RepairReport> {
    let body = wire
        .call(method.clone(), DOOR, None)
        .await?
        .with_context(|| format!("{method} {DOOR} answered with no body"))?;
    serde_json::from_value(body).with_context(|| format!("{method} {DOOR}: not a repair report"))
}

/// Run the door (or its dry run) and say what the record now holds.
pub(crate) async fn step_plugin_version(wire: &Wire, dry_run: bool) -> Result<String> {
    if dry_run {
        return Ok(render(&report(wire, reqwest::Method::GET).await?));
    }
    let written = report(wire, reqwest::Method::POST).await?;
    let after = report(wire, reqwest::Method::GET).await?;
    read_back(&written, &after)?;
    Ok(format!(
        "{}\nread back: nothing left to correct ({} refused, listed above).",
        render(&written),
        after.count(&Outcome::Refused)
    ))
}

/// The write's read-back: a step the dry run would STILL correct means
/// the write did not land what it answered, and that is an error, not a
/// line in a summary.
pub(crate) fn read_back(written: &RepairReport, after: &RepairReport) -> Result<()> {
    let left: Vec<&str> = after
        .steps
        .iter()
        .filter(|s| s.outcome == Outcome::WouldCorrect)
        .map(|s| s.step_id.as_str())
        .collect();
    if !left.is_empty() {
        bail!(
            "the door answered {} corrected, and the read-back still finds {} to correct: {}",
            written.count(&Outcome::Corrected),
            left.len(),
            left.join(", ")
        );
    }
    Ok(())
}

fn word(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Corrected => "corrected",
        Outcome::WouldCorrect => "would-correct",
        Outcome::Agrees => "agrees",
        Outcome::Refused => "refused",
    }
}

/// Every divergent step, one line each, then the counts.
pub(crate) fn render(report: &RepairReport) -> String {
    let mut out = vec![if report.written {
        "boss repair step-plugin-version: WRITTEN".to_string()
    } else {
        "boss repair step-plugin-version: DRY RUN — nothing written".to_string()
    }];
    out.extend(report.steps.iter().map(|s| {
        let logged = match (&s.logged_kind, s.logged_version) {
            (Some(kind), Some(v)) => format!("logged v{v} ({kind})"),
            _ => "logged nothing".to_string(),
        };
        let line = format!(
            "  {:<13} step {}  job {} ({}, {})  {} {}  {} -> stored v{}",
            word(&s.outcome),
            s.step_id,
            s.job_id,
            s.packet_status,
            s.partition,
            s.kind,
            s.status,
            logged,
            s.stored_version
        );
        match &s.reason {
            Some(why) => format!("{line}\n                because {why}"),
            None => line,
        }
    }));
    let counts = [
        Outcome::Corrected,
        Outcome::WouldCorrect,
        Outcome::Refused,
        Outcome::Agrees,
    ]
    .iter()
    .map(|o| format!("{} {}", report.count(o), word(o)))
    .collect::<Vec<_>>()
    .join(", ");
    out.push(format!("{} divergent: {counts}", report.steps.len()));
    if !report.written && report.count(&Outcome::WouldCorrect) > 0 {
        out.push(
            "Run without --dry-run to append one correcting jobs.step.updated per would-correct step."
                .to_string(),
        );
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use boss_jobs::plugin_version_repair::StepRepair;

    fn step(id: &str, outcome: Outcome, reason: Option<&str>) -> StepRepair {
        StepRepair {
            step_id: id.into(),
            job_id: "job-1".into(),
            kind: "answer-question".into(),
            status: "pending".into(),
            logged_kind: reason
                .is_none()
                .then(|| boss_jobs::events::STEP_CREATED.to_string()),
            logged_version: reason.is_none().then_some(0),
            stored_version: 1,
            packet_status: "open".into(),
            partition: "real".into(),
            outcome,
            reason: reason.map(str::to_string),
        }
    }

    #[test]
    fn the_dry_run_lists_every_step_with_its_reason_and_the_counts() {
        let report = RepairReport {
            written: false,
            steps: vec![
                step("step-a", Outcome::WouldCorrect, None),
                step(
                    "step-b",
                    Outcome::Refused,
                    Some("the log holds no state event"),
                ),
            ],
        };
        let text = render(&report);
        assert!(text.contains("DRY RUN — nothing written"), "{text}");
        assert!(
            text.contains("would-correct step step-a")
                && text.contains("logged v0 (jobs.step.created) -> stored v1"),
            "{text}"
        );
        // Which packets a write would touch, read before it runs (the
        // review of car 55ae8de4).
        assert!(
            text.contains("job job-1 (open, real)"),
            "each line names its packet's status and partition: {text}"
        );
        assert!(
            text.contains("refused       step step-b")
                && text.contains("because the log holds no state event"),
            "{text}"
        );
        assert!(
            text.contains("2 divergent: 0 corrected, 1 would-correct, 1 refused, 0 agrees"),
            "{text}"
        );
        assert!(text.contains("Run without --dry-run"), "{text}");
    }

    #[test]
    fn a_write_whose_read_back_still_finds_a_step_to_correct_is_an_error() {
        let written = RepairReport {
            written: true,
            steps: vec![step("step-a", Outcome::Corrected, None)],
        };
        let clean = RepairReport {
            written: false,
            steps: vec![step("step-b", Outcome::Refused, Some("a title differs"))],
        };
        assert!(read_back(&written, &clean).is_ok());
        let stale = RepairReport {
            written: false,
            steps: vec![step("step-a", Outcome::WouldCorrect, None)],
        };
        let err = read_back(&written, &stale).unwrap_err().to_string();
        assert!(err.contains("still finds 1 to correct: step-a"), "{err}");
    }
}
