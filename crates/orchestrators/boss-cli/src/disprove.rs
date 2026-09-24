//! `boss prove <car> --disproved --probe '…' --verified-file <f>
//! --superseded-by <landed car | backlog item>` — a landed car whose
//! claim is MEASURED FALSE closes on the evidence, through its own
//! terminal (backlog 08664157).
//!
//! WHY. `boss prove` refuses a failing probe — "evidence AGAINST the
//! claim … nothing is recorded either way" — which is right for a proof
//! and left a disproof with no door. Car 6b23d135 claimed boss-dev is
//! not the first eviction under disk pressure; after it converged,
//! boss-dev was evicted for ephemeral-storage at 2026-09-24T01:27Z. The
//! car could only stand at `proven`, counted "ours" in the shed, and
//! the measurement was written on it by hand under a key nothing reads.
//! David, 2026-09-24: red must not linger.
//!
//! THE MIRROR OF A PROOF, NOT A WEAKER ONE. A proof requires the probe's
//! "holds" (exit 0 and the expectation printed); a disproof requires its
//! "judged false" — exit 1, the third code of the probe protocol
//! (infra/platform/documents/builder-rules.md rule 7) — and refuses
//! everything else by name: a pass (the claim holds on this run), a NOT
//! YET (75: early, not wrong), a probe that did not run (a tool
//! missing), and any other exit or a diagnosed crash (a mangled quote, a
//! numeric test on an empty string, a timeout), because none of those is
//! a verdict on the claim. The run is recorded VERBATIM as the proof
//! record is — probe, exit, streams, host, tree — under the same
//! admission rules, so a probe reading the system of record unidentified
//! is refused here as it is there: a narrower world can fail a claim as
//! falsely as it can pass one.
//!
//! AND IT NAMES THE REMEDY. A car that did not do what it said leaves
//! the work undone; `--superseded-by` points at the landed car that did
//! it or the backlog item that carries it, resolved and judged
//! (`boss_jobs::car_disprove::judge_remedy`) before anything is written.

use anyhow::{Context, Result, anyhow, bail};
use boss_jobs::car;
use boss_jobs::car_disprove::{self, DISPROVED_SLUG, Disproof};
use serde_json::Value;

use crate::prove::{self, Eligible, Outcome, Shell};

/// The one exit a probe says "judged false" with.
pub(crate) const FALSE_EXIT: i32 = 1;

/// PURE: is this run a disproof — the probe's own "judged false" — or
/// the refusal naming what it was instead.
pub(crate) fn disproof_verdict(
    probe: &str,
    o: &Outcome,
    expect: Option<&str>,
) -> Result<(), String> {
    match prove::verdict(probe, o, expect) {
        prove::Verdict::Proven => {
            return Err(format!(
                "the probe PASSED{} — the claim holds on this run, so there is nothing to \
                 disprove. If it passes for the wrong reason, the probe is what is wrong: \
                 write one that can see the failure.",
                expect
                    .map(|e| format!(" and printed {e:?}"))
                    .unwrap_or_default()
            ));
        }
        prove::Verdict::NotYet { said } => {
            return Err(format!(
                "the probe said NOT YET (exit {}): {said}. Early is not wrong — a claim \
                 that cannot be judged yet cannot be judged false either.",
                prove::NOT_YET_EXIT
            ));
        }
        prove::Verdict::Unrunnable { missing } => {
            return Err(format!(
                "the probe DID NOT RUN — this machine lacks {}. Nothing about the claim \
                 was judged, either way.",
                missing.join(", ")
            ));
        }
        // Read below: only exit 1 with nothing to diagnose is a verdict.
        prove::Verdict::NotProven(_) => {}
    }
    if let Some(d) = prove::failure_diagnosis(probe, o) {
        return Err(format!(
            "the probe exited {} but did not judge the claim:\n{d}",
            o.exit
        ));
    }
    if o.exit != FALSE_EXIT {
        return Err(format!(
            "the probe exited {}, and only exit {FALSE_EXIT} is a probe's \"judged false\" — \
             {}. Make the probe assert the claim and exit {FALSE_EXIT} when it does not \
             hold.\n\n  stdout: {}\n  stderr: {}",
            o.exit,
            match o.exit {
                0 => "exit 0 without the expectation is a probe that did not look, not a \
                      claim that failed"
                    .to_string(),
                124 => "124 is a timeout".to_string(),
                126 | 127 => format!("{} is a command that could not run", o.exit),
                2 => "2 is a shell or tool misuse".to_string(),
                n => format!("{n} names no verdict"),
            },
            shown(&o.stdout),
            shown(&o.stderr),
        ));
    }
    if let Some(want) = expect
        && (prove::observed(&o.stdout, want) || prove::observed(&o.stderr, want))
    {
        return Err(format!(
            "the probe exited {FALSE_EXIT} AND printed the claim's own token {want:?} — \
             it says both, so it says neither. Fix the probe."
        ));
    }
    Ok(())
}

fn shown(s: &str) -> &str {
    if s.trim().is_empty() {
        "(empty)"
    } else {
        s.trim()
    }
}

/// PURE: the car `given` names by branch — the landed one first, else
/// any car with that branch (which [`car_disprove::judge_remedy`] then
/// refuses as not landed, by name).
pub(crate) fn remedy_by_branch<'a>(cars: &'a [Value], given: &str) -> Option<&'a Value> {
    car::landed_car_for(cars, given).or_else(|| {
        cars.iter()
            .find(|c| c.pointer("/metadata/branch").and_then(Value::as_str) == Some(given))
    })
}

/// The remedy packet `given` names: a car by branch, else any packet by
/// id (open first, then closed), read in full.
async fn resolve_remedy(http: &reqwest::Client, cars: &[Value], given: &str) -> Result<Value> {
    if let Some(c) = remedy_by_branch(cars, given) {
        return Ok(c.clone());
    }
    let id = crate::job::fetch_and_resolve(http, given)
        .await
        .with_context(|| {
            format!(
                "--superseded-by {given:?} is neither a car's branch nor a packet id — name \
                 the landed car that fixed it, or the backlog item that carries the fix"
            )
        })?;
    crate::car_retire::read(http, &id).await
}

/// `boss prove <car> --disproved`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run(
    car_ref: &str,
    probe: Option<String>,
    expect: Option<String>,
    from_car: bool,
    verified: Option<String>,
    superseded_by: &str,
    dry: bool,
    probe_anyway: Option<String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let overriding = prove::override_reason(probe_anyway.as_deref())?;
    let http = reqwest::Client::new();
    // The actor FIRST: a write nobody names is refused, and the refusal
    // costs a line rather than a probe against production (5083d6f5).
    if !dry {
        crate::identity::sign(&reqwest::Method::PATCH, "/api/jobs")?;
    }
    let base = crate::gate::resolve_jobs_base(None)?;
    let cars = crate::gate::all_cars_at(&http, &base).await?;
    let car = prove::find_car(&cars, car_ref, Eligible::Live)?.clone();
    let id = car
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("car has no id"))?
        .to_string();
    let short = &id[..8.min(id.len())];

    // Refuse before running anything: a car that cannot take the
    // terminal costs a line, not a probe.
    car_disprove::disprovable(&car)
        .map_err(|e| anyhow!("boss prove --disproved: REFUSED — {e}"))?;
    let remedy_job = resolve_remedy(&http, &cars, superseded_by).await?;
    let remedy = car_disprove::judge_remedy(&car, &remedy_job)
        .map_err(|e| anyhow!("boss prove --disproved: REFUSED — {e}"))?;
    let remedy_id = remedy_job
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("the remedy packet carries no id"))?
        .to_string();

    let (probe, expect, verified) = if from_car {
        let (p, e) = prove::car_probe(&car)?;
        println!("boss prove --disproved: {short} runs the probe it carried from park time");
        (p, Some(e), verified)
    } else {
        (
            probe.ok_or_else(|| {
                anyhow!(
                    "--probe is required: a disproof is a command that ran and said false, \
                     not a sentence (or --from-car, to run the car's own probe)"
                )
            })?,
            expect,
            verified,
        )
    };
    let verified = verified.ok_or_else(|| {
        anyhow!(
            "--verified (or --verified-file) is required: say what the failing probe means, \
             in prose, for a reader"
        )
    })?;

    // THE SAME ADMISSION RULES AS A PROOF (23b2dffa).
    let admission = prove::admit(&probe, from_car);
    for w in &admission.warnings {
        eprintln!("{w}");
    }
    let overridden = match (&admission.refusal, overriding) {
        (Some(r), None) => bail!("{r}"),
        (Some(r), Some(reason)) => {
            println!(
                "boss prove --disproved: running a probe this door refuses, on your stated \
                 reason — {reason}. It is recorded in the disproof as `overridden`."
            );
            Some(prove::override_record(r.rule, reason))
        }
        (None, _) => None,
    };

    let tree = crate::brief::repo_root().ok();
    let shell = Shell::here(None)
        .with_probe_reader(tree.as_deref(), &base)
        .with_car_instant(prove::car_merge_ref(&car));
    let obs = prove::probe_tree(&shell);
    println!("boss prove --disproved: {short}  $ {probe}");
    let o = prove::execute_with(&probe, &shell)?;
    disproof_verdict(&probe, &o, expect.as_deref()).map_err(|e| {
        anyhow!("boss prove --disproved: REFUSED — {e}\n\nNothing is recorded on {short}.")
    })?;
    let at = now.to_rfc3339();
    let record = prove::proof_json(
        &probe,
        expect.as_deref(),
        &o,
        &prove::host(),
        &at,
        &obs,
        overridden.as_ref(),
    );
    let shown = o.stdout.trim();
    println!(
        "boss prove --disproved: the probe exited {FALSE_EXIT} — the claim is judged FALSE\n  {}",
        if shown.is_empty() {
            "(no stdout)"
        } else {
            shown
        }
    );
    println!("  remedy: {remedy}");
    let disproof = Disproof {
        verified,
        disproof: serde_json::to_string(&record)?,
        superseded_by: remedy_id,
        remedy,
        completed_at: crate::gate::stamp(now),
    };

    // A car pinned to a version without the terminal is MOVED first —
    // through the conversion door, which previews, refuses an unsafe
    // move by step, and records the re-pin on the packet (7cf202a9).
    let mut current = car.clone();
    if car::find_step(&current, DISPROVED_SLUG, car_disprove::DISPROVED).is_none() {
        println!(
            "  car {short} is pinned to v{} with no `{DISPROVED_SLUG}` terminal — converting it",
            current["workflow_version"]
        );
        crate::job::convert(&id, None, dry).await?;
        if dry {
            println!(
                "boss prove --disproved: DRY — nothing written; after the conversion it \
                 closes through `{DISPROVED_SLUG}`"
            );
            return Ok(());
        }
        current = crate::car_retire::read(&http, &id).await?;
    }
    let writes = car_disprove::disprove_writes(&current, &disproof)
        .map_err(|e| anyhow!("boss prove --disproved: REFUSED — {e}"))?;
    if dry {
        println!(
            "boss prove --disproved: DRY — would record the failing run on \
             `{DISPROVED_SLUG}`, set {}, and complete `{DISPROVED_SLUG}`",
            writes.marker
        );
        return Ok(());
    }
    crate::car_retire::apply(
        &http,
        &id,
        &current,
        &writes,
        DISPROVED_SLUG,
        "boss prove --disproved",
    )
    .await?;
    if let Some(item) = current
        .pointer(&format!("/metadata/{}", car::BACKLOG_ITEM))
        .and_then(Value::as_str)
    {
        println!(
            "  note: item {} stays OPEN — the arrival rule closes an item on `merged` only, \
             and this car did not do what it said. Its remedy closes it.",
            &item[..8.min(item.len())]
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run_of(exit: i32, stdout: &str, stderr: &str) -> Outcome {
        Outcome {
            exit,
            stdout: stdout.into(),
            stderr: stderr.into(),
            missing_tools: Vec::new(),
        }
    }

    const PROBE: &str = "test \"$first\" = gate-pod && echo claim:ok || { echo \"evicted first: $first\"; exit 1; }";

    /// EXIT 1 IS A DISPROOF — the probe's own "judged false", the only
    /// run this door records. Car 6b23d135's shape: boss-dev named as the
    /// first eviction.
    #[test]
    fn exit_one_is_the_claim_judged_false() {
        let o = run_of(1, "evicted first: boss-dev-7889cd5f98-hnl2n", "");
        assert_eq!(disproof_verdict(PROBE, &o, Some("claim:ok")), Ok(()));
        assert_eq!(disproof_verdict(PROBE, &o, None), Ok(()));
    }

    /// A PASS, a NOT YET and a probe that did not run are each refused,
    /// each by what it was.
    #[test]
    fn a_pass_a_not_yet_and_an_unrun_probe_are_refused() {
        let pass = run_of(0, "claim:ok", "");
        let e = disproof_verdict(PROBE, &pass, Some("claim:ok")).unwrap_err();
        assert!(e.contains("PASSED"), "{e}");

        let not_yet = run_of(75, "", "not yet: no eviction on w-1 since convergence");
        let e = disproof_verdict(PROBE, &not_yet, Some("claim:ok")).unwrap_err();
        assert!(e.contains("NOT YET"), "{e}");
        assert!(e.contains("no eviction on w-1"), "{e}");

        let mut unrun = run_of(1, "", "kubectl: command not found");
        unrun.missing_tools = vec!["kubectl".into()];
        let e = disproof_verdict(PROBE, &unrun, Some("claim:ok")).unwrap_err();
        assert!(e.contains("DID NOT RUN"), "{e}");
        assert!(e.contains("kubectl"), "{e}");
    }

    /// Any exit but 1 names no verdict — a timeout, a missing command, a
    /// misuse, an exit 0 that did not look — and neither does a crash the
    /// diagnosis can name, whatever code it exited.
    #[test]
    fn any_other_exit_or_a_diagnosed_crash_is_not_a_verdict() {
        for (exit, why) in [
            (124, "timeout"),
            (127, "could not run"),
            (2, "misuse"),
            (0, "did not look"),
            (42, "names no verdict"),
        ] {
            let e =
                disproof_verdict(PROBE, &run_of(exit, "", "boom"), Some("claim:ok")).unwrap_err();
            assert!(e.contains(why), "exit {exit}: {e}");
        }
        // A numeric test that crashed on an empty operand exits 1 from
        // its `||` branch — the code points the wrong way.
        let crashed = run_of(1, "", "bash: line 1: [: : integer expression expected");
        let e = disproof_verdict("[ \"$n\" -gt 0 ] || exit 1", &crashed, None).unwrap_err();
        assert!(e.contains("did not judge the claim"), "{e}");
    }

    /// A probe that exits 1 and prints the claim's own token says both.
    #[test]
    fn exit_one_with_the_claims_token_is_refused() {
        let o = run_of(1, "claim:ok", "");
        let e = disproof_verdict(PROBE, &o, Some("claim:ok")).unwrap_err();
        assert!(e.contains("says both"), "{e}");
    }

    /// A remedy named by branch is the LANDED car first — a rebuilt
    /// branch may have an older, spent car beside it.
    #[test]
    fn a_remedy_by_branch_is_the_landed_car_first() {
        let cars = [
            json!({"id": "a-spent", "status": "closed",
                   "metadata": {"branch": "fix/scratch-floor", "outcome": "abandoned"}}),
            json!({"id": "b-landed", "status": "closed",
                   "metadata": {"branch": "fix/scratch-floor", "outcome": "merged",
                                "merged": "true"}}),
            json!({"id": "c-open", "status": "open", "metadata": {"branch": "fix/open"}}),
        ];
        assert_eq!(
            remedy_by_branch(&cars, "fix/scratch-floor").unwrap()["id"],
            "b-landed"
        );
        assert_eq!(remedy_by_branch(&cars, "fix/open").unwrap()["id"], "c-open");
        assert!(remedy_by_branch(&cars, "08664157").is_none());
    }
}
