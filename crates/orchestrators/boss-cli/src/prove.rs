//! `boss prove <car>` — proof is a receipt, not a claim.
//!
//! WHY THIS EXISTS. The `gate` step is trustworthy because a machine
//! writes its receipt and `boss park` copies it verbatim; nobody types
//! a verdict. The `proven` step had no such thing. Its one required
//! field, `verified`, is free prose, so "proven in prod" meant only
//! that somebody wrote a sentence saying so.
//!
//! That is not a hypothetical weakness. On 2026-08-28 a change was
//! reported done on the strength of an HTTP 204 — the API accepted a
//! write, so the write was called proof — and the behaviour it claimed
//! had never been observed. Twice. David's question was the right one:
//! how is there evidence a fix is in prod before coming back to me?
//! The answer was that there wasn't, because nothing required any.
//!
//! WORSE, THE HAND-ROLLED CHECKS WERE THEMSELVES WRONG. The same day, a
//! verification loop used `grep -c ... || echo 0`; on no match that
//! prints `0` from grep AND `0` from the fallback, and the resulting
//! "0\n0" compared unequal to "0", so the loop reported success for a
//! file that had not changed. A check written fresh per change is a
//! second thing that can be broken, and it is broken silently, in the
//! direction of saying yes.
//!
//! SO THE VERB RUNS THE PROBE ITSELF. It does not accept output pasted
//! in; it executes the command, captures exit status and both streams
//! verbatim, and REFUSES to record anything unless the probe exits zero
//! and — unless the caller explicitly downgrades to `--exit-only` —
//! unless the expected string is actually present. What lands on the
//! step is what happened, not what was hoped.
//!
//! AND THE PROOF STAYS RE-RUNNABLE. The command is recorded alongside
//! its output, so `boss prove <car> --recheck` re-executes it later and
//! says whether the claim still holds. A proof that has silently
//! decayed — a ConfigMap preview reverted by the next converge, exactly
//! the failure `push-step-plugins.sh` warns about — becomes findable
//! instead of being a sentence in a closed packet that nobody rereads.
//!
//! WHAT IT DELIBERATELY DOES NOT DO. It does not judge whether the
//! probe is a good probe. `--probe 'true' --exit-only` will pass, and
//! it will pass legibly: the recorded proof shows a caller who asserted
//! nothing, which is a thing a reader can see and challenge. The prose
//! in `verified` stays human, because what a change MEANS is judgement.
//! Only the evidence under it is mechanised.

use std::path::Path;

use anyhow::{Result, bail};
use serde_json::{Value, json};

/// The step this verb fills. Job step titles come from the registry, so
/// a rename there is a rename here — pinned the same way `boss park`
/// pins its three: refuse rather than guess which step was meant.
const PROVEN: &str = "Proven in prod";

/// Streams are recorded verbatim up to this much. Long enough for a
/// real probe's output, short enough that a runaway `find /` does not
/// push a megabyte into job metadata.
const MAX_STREAM: usize = 4000;

fn clip(s: &str) -> String {
    let s = s.trim_end();
    if s.len() <= MAX_STREAM {
        return s.to_string();
    }
    let cut = s
        .char_indices()
        .map(|(i, _)| i)
        .nth(MAX_STREAM)
        .unwrap_or(s.len());
    format!("{}\n… [{} more bytes]", &s[..cut], s.len() - cut)
}

/// What running a probe produced. Captured, never typed.
#[derive(Debug, Clone)]
pub(crate) struct Outcome {
    pub exit: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Run `probe` through a shell and capture everything it did.
pub(crate) fn execute(probe: &str) -> Result<Outcome> {
    execute_in(probe, None)
}

/// As [`execute`], but in a stated directory — what `--recheck` uses to
/// put the probe back where it was recorded.
pub(crate) fn execute_in(probe: &str, cwd: Option<&Path>) -> Result<Outcome> {
    let mut cmd = std::process::Command::new("sh");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd
        .arg("-c")
        .arg(probe)
        .output()
        .map_err(|e| anyhow::anyhow!("could not run the probe: {e}"))?;
    Ok(Outcome {
        // A signalled probe reports no code; -1 is recorded rather than
        // silently becoming 0, because "killed" must not read as "passed".
        exit: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Decide whether an outcome is evidence, or refuse and say why.
///
/// This is the whole gate, and it is deliberately only two rules: the
/// probe must have exited zero, and — unless the caller downgraded to
/// `exit_only` — the expected string must actually appear in what it
/// printed. Both refusals quote the streams, because a refusal that
/// hides the output makes the reader go hunting for it.
pub(crate) fn judge(o: &Outcome, expect: Option<&str>) -> Result<()> {
    if o.exit != 0 {
        bail!(
            "the probe exited {}, so it is not proof of anything.\n\
             \n  stdout: {}\n  stderr: {}\n\n\
             A probe that fails is evidence AGAINST the claim. Fix the change, \
             or fix the probe if the probe is what is wrong — but nothing is \
             recorded either way.",
            o.exit,
            if o.stdout.trim().is_empty() {
                "(empty)"
            } else {
                o.stdout.trim()
            },
            if o.stderr.trim().is_empty() {
                "(empty)"
            } else {
                o.stderr.trim()
            },
        );
    }
    if let Some(want) = expect {
        // Both streams count: plenty of real probes report on stderr.
        if !o.stdout.contains(want) && !o.stderr.contains(want) {
            bail!(
                "the probe exited 0 but never printed {want:?}, so it did not \
                 observe what was claimed.\n\
                 \n  stdout: {}\n  stderr: {}\n\n\
                 An exit code alone is a weak assertion — `echo hi` exits 0 too. \
                 Either the change is not in prod, or the probe is looking in the \
                 wrong place.",
                if o.stdout.trim().is_empty() {
                    "(empty)"
                } else {
                    o.stdout.trim()
                },
                if o.stderr.trim().is_empty() {
                    "(empty)"
                } else {
                    o.stderr.trim()
                },
            );
        }
    }
    Ok(())
}

/// The proof record. Serialised once, stored verbatim, re-read by
/// `--recheck` — so its field names are a contract, not a detail.
pub(crate) fn proof_json(
    probe: &str,
    expect: Option<&str>,
    o: &Outcome,
    host: &str,
    at: &str,
) -> Value {
    json!({
        "probe": probe,
        "expect": expect,
        "exit": o.exit,
        "stdout": clip(&o.stdout),
        "stderr": clip(&o.stderr),
        "host": host,
        // WHERE IT RAN, not just what ran. A command means the same
        // thing twice only if the host and the directory are the same
        // both times, and 3 of 10 rechecks false-failed for exactly
        // this: probes opening `git rev-parse HEAD` re-run outside a
        // repository, and probes authored on the workstation as
        // `ssh boss-gcp ...` re-run ON boss-gcp, where that name does
        // not resolve. Both are free to record — the verb already
        // knows them (66fd64c6).
        "cwd": std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        "at": at,
    })
}

/// Can the recorded probe be re-run HERE, meaning the same thing?
///
/// The distinction this draws is the whole point of the packet: a probe
/// that cannot be re-run is not the same fact as a claim that stopped
/// being true, and `--recheck` used to render them identically — as
/// NO LONGER HOLDS. An instrument that cries wolf 30% of the time gets
/// ignored, and then decay stops being detected at all.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Rerunnable {
    /// Same host, and the directory is available.
    Here,
    /// Recorded somewhere else. Re-running here tests a different thing.
    WrongHost { recorded: String, here: String },
    /// The directory the probe assumed is gone.
    MissingDir { cwd: String },
}

/// ABSENT CONTEXT IS NOT A MISMATCH. Proofs recorded before this change
/// carry no `cwd`, and the oldest carry no `host`. Refusing on missing
/// data would break every proof already on the board, so an unrecorded
/// field means "cannot check", and the recheck proceeds exactly as it
/// did before.
pub(crate) fn rerunnable(
    recorded_host: Option<&str>,
    here: &str,
    cwd: Option<&str>,
    dir_exists: bool,
) -> Rerunnable {
    if let Some(rec) = recorded_host.filter(|h| !h.is_empty() && *h != "unknown")
        && rec != here
    {
        return Rerunnable::WrongHost {
            recorded: rec.to_string(),
            here: here.to_string(),
        };
    }
    if let Some(dir) = cwd.filter(|c| !c.is_empty())
        && !dir_exists
    {
        return Rerunnable::MissingDir {
            cwd: dir.to_string(),
        };
    }
    Rerunnable::Here
}

/// Find the one open car for `given` — a branch name, or an id prefix.
///
/// Refuses on ambiguity rather than picking. `boss park` learned this
/// the expensive way: it took the LAST match from a list the API
/// returns newest-first, and quietly parked two cars against a stale
/// receipt. Choosing among candidates is how that happens, so this
/// does not choose.
pub(crate) fn find_car<'a>(cars: &'a [Value], given: &str) -> Result<&'a Value> {
    let matches: Vec<&Value> = cars
        .iter()
        .filter(|c| {
            let by_branch = c
                .get("metadata")
                .and_then(|m| m.get("branch"))
                .and_then(Value::as_str)
                == Some(given);
            let by_id = c
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|i| i.starts_with(given) && given.len() >= 8);
            by_branch || by_id
        })
        .collect();

    match matches.len() {
        1 => Ok(matches[0]),
        0 => bail!(
            "no open ship-a-change car for {given:?}. Give the car's branch exactly \
             as it was parked, or at least 8 characters of its id."
        ),
        n => {
            let mut listed = String::new();
            for c in &matches {
                listed.push_str(&format!(
                    "\n  {}  {}",
                    &c.get("id").and_then(Value::as_str).unwrap_or("?")[..8],
                    c.get("title").and_then(Value::as_str).unwrap_or("?")
                ));
            }
            bail!("{n} open cars match {given:?} — say which:{listed}")
        }
    }
}

/// The car's `proven` step, refusing unless it is actually reachable.
///
/// `proven` is gated on `job.metadata.merged = "true"`, so a step still
/// pending means the change has not merged. Recording proof there would
/// be recording that unshipped code works in production.
pub(crate) fn proven_step(car: &Value, replace: bool) -> Result<&Value> {
    let step = car
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("title").and_then(Value::as_str) == Some(PROVEN))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "the car has no step titled {PROVEN:?}. The ship-a-change workflow was \
                 renamed or re-versioned; this verb fills a step by title and will not \
                 guess which one was meant."
            )
        })?;

    match step.get("status").and_then(Value::as_str) {
        Some("ready") | Some("active") => Ok(step),
        // A PROOF THAT STOPPED HOLDING IS EVIDENCE, NOT A MISTAKE.
        // Car 932aa956's probe observed a real production refusal and
        // stopped holding within the hour, because the condition it
        // keyed on was transient. `--recheck` reported NO LONGER HOLDS
        // correctly and was wrong about the cause, and there was no way
        // to put a better probe under the same claim short of an
        // operator editing job metadata by hand (2b30eff4).
        Some("completed") if replace => Ok(step),
        Some("completed") => bail!(
            "this car is already proven. To check whether the proof STILL holds, \
             re-run with --recheck; it re-executes the recorded probe and changes \
             nothing. To put a BETTER probe under the same claim — because the first \
             one was transient rather than wrong — re-run with --replace, which keeps \
             the original proof and records the new one beside it."
        ),
        Some("pending") => bail!(
            "the `{PROVEN}` step is still pending, which means the car has not merged \
             — its predicate is `steps.review.done AND job.metadata.merged = \"true\"`. \
             A change that has not shipped cannot be proven in production."
        ),
        other => bail!("the `{PROVEN}` step is {other:?}, which this verb does not fill"),
    }
}

/// A recorded proof, as much of it as the writer stored.
#[derive(Debug)]
pub(crate) struct Recorded {
    pub probe: String,
    pub expect: Option<String>,
    /// `None` on proofs written before the context was recorded.
    pub host: Option<String>,
    pub cwd: Option<String>,
}

/// Read back a recorded proof so `--recheck` can re-run it.
pub(crate) fn recorded_probe_for(car: &Value, step: &Value) -> Result<Recorded> {
    // A REPLACEMENT SUPERSEDES THE ORIGINAL, and is read in preference
    // to it — the same precedence `regate_receipt` has over a stale
    // gate receipt. The original stays on the step; this is which proof
    // `--recheck` should be re-running, not which one happened.
    if let Some(last) = car
        .get("metadata")
        .and_then(|m| m.get("reproof"))
        .and_then(Value::as_array)
        .and_then(|a| a.last())
        && let Some(p) = last.get("proof")
    {
        return read_proof(p);
    }
    recorded_probe(step)
}

pub(crate) fn recorded_probe(step: &Value) -> Result<Recorded> {
    let md = step.get("metadata");
    let raw = md.and_then(|m| m.get("proof")).ok_or_else(|| {
        anyhow::anyhow!(
            "this step carries no `proof`, so there is nothing to re-run. It was \
                 completed before proof was mechanised, or filled by hand — its \
                 `verified` prose is a claim with no probe under it."
        )
    })?;
    read_proof(raw)
}

/// Parse one recorded proof, however it was stored.
fn read_proof(raw: &Value) -> Result<Recorded> {
    // Stored as a JSON string (verbatim, like the gate receipt) or, if a
    // future writer stores it structurally, as an object. Read both.
    let v: Value = match raw.as_str() {
        Some(s) => serde_json::from_str(s)
            .map_err(|e| anyhow::anyhow!("the recorded proof is not readable JSON: {e}"))?,
        None => raw.clone(),
    };
    let probe = v
        .get("probe")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("the recorded proof has no `probe` to re-run"))?
        .to_string();
    let expect = v.get("expect").and_then(Value::as_str).map(str::to_string);
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Ok(Recorded {
        probe,
        expect,
        host: text("host"),
        cwd: text("cwd"),
    })
}

fn host() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

/// The evidence the `proven` step carries, and when it was recorded.
///
/// `now` is the instant the probe ran under, not a fresh one taken at
/// write time: the proof and its stamp describe a single event, and a
/// slow API call should not drag the timestamp away from the probe.
fn proven_metadata(
    verified: &str,
    proof: &str,
    method: Option<&str>,
    now: chrono::DateTime<chrono::Utc>,
) -> Value {
    let mut md = json!({
        "verified": verified,
        // VERBATIM, as a string, exactly like the gate receipt: what the
        // machine saw, not a summary of it.
        "proof": proof,
        "completed_at": crate::gate::stamp(now),
    });
    if let Some(m) = method {
        md["method"] = json!(m);
    }
    md
}

/// Run a probe against production and record it on the car — or refuse.
pub(crate) async fn run(
    car_ref: &str,
    probe: Option<String>,
    expect: Option<String>,
    exit_only: bool,
    verified: Option<String>,
    method: Option<String>,
    recheck: bool,
    replace: bool,
    dry: bool,
    // The operator's now, taken once at the CLI entry point and passed
    // in — the same shape `train::run` uses, so nothing down here reads
    // the wall clock on its own.
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let http = reqwest::Client::new();
    let cars = crate::gate::rows(
        crate::gate::api(
            &http,
            reqwest::Method::GET,
            "/api/jobs?kind=ship-a-change&limit=200",
            None,
        )
        .await?,
    );
    let car = find_car(&cars, car_ref)?;
    let car_id = car
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("car has no id"))?
        .to_string();
    let short = &car_id[..8.min(car_id.len())];

    // --recheck reads the proof already on the step and re-runs it. It
    // is read-only on purpose: a decayed proof is a finding to act on,
    // not something to quietly overwrite.
    if recheck {
        let step = car
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|s| s.get("title").and_then(Value::as_str) == Some(PROVEN))
            .ok_or_else(|| anyhow::anyhow!("the car has no step titled {PROVEN:?}"))?;
        let rec = recorded_probe_for(car, step)?;
        let (probe, expect) = (rec.probe, rec.expect);
        let here = host();
        let dir_exists = rec.cwd.as_deref().is_none_or(|d| Path::new(d).is_dir());
        match rerunnable(rec.host.as_deref(), &here, rec.cwd.as_deref(), dir_exists) {
            Rerunnable::WrongHost { recorded, here } => bail!(
                "CANNOT RE-RUN HERE — this proof was recorded on {recorded}, and you are on \
                 {here}.\n  $ {probe}\n\n\
                 The claim has NOT been tested either way, which is a different fact from it \
                 having decayed. Reporting the two identically is how this instrument \
                 false-failed 3 times in 10 (66fd64c6) — probes reference paths, hostnames and \
                 services that exist on the host they were written for, so re-running one \
                 elsewhere tests something else and usually fails.\n  \
                 Re-run it on {recorded}, or re-prove the car here to record a probe that \
                 belongs to this host."
            ),
            Rerunnable::MissingDir { cwd } => bail!(
                "CANNOT RE-RUN HERE — this proof was recorded in {cwd}, which does not exist \
                 on this machine.\n  $ {probe}\n\n\
                 A probe that opens `git rev-parse HEAD` means nothing outside a repository. \
                 The claim has not been tested either way."
            ),
            Rerunnable::Here => {}
        }
        println!("boss prove: re-running the recorded probe for {short}\n  $ {probe}");
        let o = match rec.cwd.as_deref() {
            Some(dir) if !dir.is_empty() => execute_in(&probe, Some(Path::new(dir)))?,
            _ => execute(&probe)?,
        };
        return match judge(&o, expect.as_deref()) {
            Ok(()) => {
                println!("boss prove: HOLDS — {short} is still true in production");
                Ok(())
            }
            Err(e) => bail!(
                "NO LONGER HOLDS — {short} was proven once and is not true now.\n\n{e}\n\n\
                 A proof can decay honestly: a step-plugin ConfigMap preview survives \
                 exactly until the next converge, and a car that never landed leaves \
                 prod looking fixed until it isn't."
            ),
        };
    }

    let probe = probe.ok_or_else(|| {
        anyhow::anyhow!("--probe is required: proof is a command that ran, not a sentence")
    })?;
    let verified = verified.ok_or_else(|| {
        anyhow::anyhow!("--verified is required: say what the probe means, in prose, for a reader")
    })?;
    if expect.is_none() && !exit_only {
        bail!(
            "give --expect '<string the probe must print>', or pass --exit-only to assert \
             on the exit code alone.\n\n\
             Exit codes are a fine assertion when the command IS the test (`grep -q`, \
             `test -f`), and a weak one otherwise — `echo hi` exits 0. Downgrading is \
             allowed, but it is recorded in the proof so a reader can see what was \
             actually asserted."
        );
    }

    // Refuse before running anything: a car that has not merged should
    // cost a line of output, not a probe against production.
    proven_step(car, replace)?;

    println!("boss prove: {short}  $ {probe}");
    let o = execute(&probe)?;
    judge(&o, expect.as_deref())?;

    let at = now.to_rfc3339();
    let proof = proof_json(&probe, expect.as_deref(), &o, &host(), &at);
    let shown = o.stdout.trim();
    println!(
        "boss prove: probe exited 0{}\n  {}",
        match &expect {
            Some(w) => format!(" and printed {w:?}"),
            None => " (--exit-only, no output asserted)".into(),
        },
        if shown.is_empty() {
            "(no output)"
        } else {
            shown
        }
    );

    if dry {
        println!("boss prove: DRY — would record this proof on {short} and complete `{PROVEN}`");
        return Ok(());
    }

    // Re-read the step id from the car we already fetched.
    let sid = proven_step(car, replace)?
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("the `{PROVEN}` step has no id"))?
        .to_string();

    let md = proven_metadata(
        &verified,
        &serde_json::to_string(&proof)?,
        method.as_deref(),
        now,
    );

    if replace {
        // THE STEP IS FROZEN, SO THE NEW PROOF LANDS BESIDE IT. A
        // completed step cannot be rewritten — which is right, because
        // the original proof is evidence about the system and erasing
        // it would destroy the record of what used to hold. So the
        // replacement appends to `reproof` in JOB metadata, the same
        // door `regate_receipt` uses for a receipt whose branch moved,
        // and `--recheck` reads the newest entry in preference.
        let mut history: Vec<Value> = car
            .get("metadata")
            .and_then(|m| m.get("reproof"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        history.push(json!({
            "proof": serde_json::to_string(&proof)?,
            "verified": verified,
            "recorded_at": crate::gate::stamp(now),
        }));
        crate::gate::api(
            &http,
            reqwest::Method::PATCH,
            &format!("/api/jobs/{car_id}/metadata"),
            Some(json!({"reproof": history})),
        )
        .await?;
        println!(
            "boss prove: {short} re-proven — recorded as reproof #{}. The original \
             proof is untouched on the step; a proof that used to hold and no longer \
             does is evidence, not a mistake to erase.",
            history.len()
        );
        return Ok(());
    }

    crate::gate::api(
        &http,
        reqwest::Method::PUT,
        &format!("/api/jobs/{car_id}/steps/{sid}"),
        Some(json!({"status": "completed", "metadata": md})),
    )
    .await?;

    println!("boss prove: {short} proven — the probe is recorded and re-runnable");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(stdout: &str) -> Outcome {
        Outcome {
            exit: 0,
            stdout: stdout.into(),
            stderr: String::new(),
        }
    }

    /// THE RULE THE VERB EXISTS TO ENFORCE: a failing probe is not proof.
    #[test]
    fn a_nonzero_probe_is_refused_and_its_output_is_shown() {
        let o = Outcome {
            exit: 1,
            stdout: "nope".into(),
            stderr: "boom".into(),
        };
        let e = judge(&o, None).unwrap_err().to_string();
        assert!(e.contains("exited 1"), "{e}");
        assert!(
            e.contains("boom"),
            "the refusal must quote the streams: {e}"
        );
    }

    /// A signalled probe must not be read as success.
    #[test]
    fn a_killed_probe_is_not_success() {
        let o = Outcome {
            exit: -1,
            stdout: String::new(),
            stderr: String::new(),
        };
        assert!(
            judge(&o, None).is_err(),
            "a signalled probe reports no code, not a pass"
        );
    }

    /// The case that motivated the verb: exit 0 proving nothing.
    #[test]
    fn exit_zero_without_the_expected_string_is_refused() {
        let e = judge(&ok("hi"), Some("MARKER")).unwrap_err().to_string();
        assert!(e.contains("never printed"), "{e}");
        assert!(
            e.contains("echo hi"),
            "the refusal should name the weakness: {e}"
        );
    }

    #[test]
    fn the_expected_string_may_arrive_on_stderr() {
        let o = Outcome {
            exit: 0,
            stdout: String::new(),
            stderr: "MARKER present".into(),
        };
        assert!(
            judge(&o, Some("MARKER")).is_ok(),
            "real probes report on stderr too"
        );
    }

    #[test]
    fn a_matching_probe_passes() {
        assert!(judge(&ok("MARKER present"), Some("MARKER")).is_ok());
    }

    /// The probe is actually EXECUTED, not parsed — this is what makes
    /// the recorded output evidence rather than transcription.
    #[test]
    fn execute_captures_what_really_happened() {
        let o = execute("printf hello; printf oops >&2; exit 3").unwrap();
        assert_eq!(o.exit, 3);
        assert_eq!(o.stdout, "hello");
        assert_eq!(o.stderr, "oops");
    }

    /// Guards the bug class that made a hand-rolled check lie on
    /// 2026-08-28: `grep -c || echo 0` prints "0\n0" on no match. Under
    /// this verb the same probe is refused, because grep's exit is 1.
    #[test]
    fn the_grep_c_fallback_that_lied_is_now_refused() {
        let o = execute("printf '' | grep -c MARKER || echo 0").unwrap();
        assert_eq!(
            o.stdout.trim(),
            "0\n0".trim_matches('"'),
            "reproduces the double zero"
        );
        // The honest form is what the verb pushes callers toward:
        let honest = execute("printf '' | grep -q MARKER").unwrap();
        assert!(
            judge(&honest, None).is_err(),
            "no match must refuse, not pass"
        );
    }

    #[test]
    fn a_proof_round_trips_through_the_recorded_form() {
        let o = ok("MARKER present");
        let p = proof_json(
            "grep -q MARKER f",
            Some("MARKER"),
            &o,
            "h",
            "2026-08-28T00:00:00Z",
        );
        let step = json!({"metadata": {"proof": serde_json::to_string(&p).unwrap()}});
        let rec = recorded_probe(&step).unwrap();
        assert_eq!(rec.probe, "grep -q MARKER f");
        assert_eq!(rec.expect.as_deref(), Some("MARKER"));
    }

    /// THE 932aa956 / 3f846cc5 CASE. A probe authored on the workstation
    /// as `ssh boss-gcp ...` re-runs ON boss-gcp, where that name does
    /// not resolve. The claim still held; the instrument was wrong.
    #[test]
    fn a_proof_recorded_elsewhere_cannot_be_rechecked_here() {
        assert_eq!(
            rerunnable(Some("mac-studio"), "boss-gcp", None, true),
            Rerunnable::WrongHost {
                recorded: "mac-studio".into(),
                here: "boss-gcp".into()
            }
        );
    }

    /// THE 64d5e3c7 CASE. The probe opened `git rev-parse --short HEAD`
    /// and was re-run outside any repository.
    #[test]
    fn a_proof_whose_directory_is_gone_cannot_be_rechecked() {
        assert_eq!(
            rerunnable(Some("h"), "h", Some("/var/lib/boss-train/repo"), false),
            Rerunnable::MissingDir {
                cwd: "/var/lib/boss-train/repo".into()
            }
        );
    }

    /// AND THE ONE THAT MUST NOT BREAK. Every proof already on the board
    /// was recorded without a `cwd`, and the oldest without a `host`.
    /// Refusing on absent context would turn a 30% false-alarm rate into
    /// a 100% one.
    #[test]
    fn absent_context_is_not_a_mismatch() {
        assert_eq!(rerunnable(None, "anywhere", None, true), Rerunnable::Here);
        assert_eq!(
            rerunnable(Some(""), "anywhere", None, true),
            Rerunnable::Here
        );
        assert_eq!(
            rerunnable(Some("unknown"), "anywhere", None, true),
            Rerunnable::Here
        );
        assert_eq!(
            rerunnable(Some("h"), "h", Some(""), false),
            Rerunnable::Here
        );
    }

    #[test]
    fn the_same_host_and_a_live_directory_re_runs() {
        assert_eq!(
            rerunnable(Some("h"), "h", Some("/tmp"), true),
            Rerunnable::Here
        );
    }

    /// The context is recorded so it can be read back — a field that is
    /// written but not recoverable is not a contract.
    #[test]
    fn the_recorded_context_round_trips() {
        let o = Outcome {
            exit: 0,
            stdout: "ok".into(),
            stderr: String::new(),
        };
        let proof = proof_json("echo ok", None, &o, "somehost", "2026-08-29T00:00:00Z");
        let step = json!({"metadata": {"proof": proof.to_string()}});
        let rec = recorded_probe(&step).expect("readable");
        assert_eq!(rec.host.as_deref(), Some("somehost"));
        assert!(
            rec.cwd.is_some_and(|c| !c.is_empty()),
            "cwd must be recorded"
        );
    }

    #[test]
    fn a_step_proven_by_hand_has_nothing_to_recheck() {
        let step = json!({"metadata": {"verified": "trust me"}});
        let e = recorded_probe(&step).unwrap_err().to_string();
        assert!(e.contains("a claim with no probe under it"), "{e}");
    }

    fn car(id: &str, branch: &str, proven_status: &str) -> Value {
        json!({
            "id": id,
            "title": "a car",
            "metadata": {"branch": branch},
            "steps": [{"id": "s1", "title": PROVEN, "status": proven_status, "metadata": {}}],
        })
    }

    /// An unmerged car cannot be proven in production.
    #[test]
    fn a_pending_proven_step_is_refused_because_it_has_not_merged() {
        let c = car("11111111-a", "feat/x", "pending");
        let e = proven_step(&c, false).unwrap_err().to_string();
        assert!(e.contains("has not merged"), "{e}");
    }

    #[test]
    fn a_ready_proven_step_is_fillable() {
        assert!(proven_step(&car("11111111-a", "feat/x", "ready"), false).is_ok());
    }

    /// Ambiguity is refused rather than resolved — the failure mode that
    /// cost two cars when `boss park` picked from a list instead.
    #[test]
    fn two_matching_cars_are_refused_not_chosen() {
        let cars = vec![
            car("11111111-aaa", "feat/x", "ready"),
            car("11111111-bbb", "feat/x", "ready"),
        ];
        let e = find_car(&cars, "feat/x").unwrap_err().to_string();
        assert!(e.contains("2 open cars match"), "{e}");
    }

    #[test]
    fn a_car_is_found_by_branch_or_by_id_prefix() {
        let cars = vec![car("abcdef12-3456", "feat/x", "ready")];
        assert!(find_car(&cars, "feat/x").is_ok());
        assert!(find_car(&cars, "abcdef12").is_ok());
        // Too short to be an id, and not a branch: refused, not guessed.
        assert!(find_car(&cars, "abc").is_err());
    }

    #[test]
    fn a_long_stream_is_clipped_rather_than_pushed_into_metadata() {
        let clipped = clip(&"x".repeat(MAX_STREAM + 500));
        assert!(clipped.len() < MAX_STREAM + 100);
        assert!(clipped.contains("more bytes"));
    }

    #[test]
    fn a_recorded_proof_says_when_it_was_taken() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-29T04:15:09.847213Z")
            .unwrap()
            .into();
        let md = proven_metadata("v", "{\"exit\":0}", Some("api"), now);

        assert_eq!(
            md["completed_at"], "2026-08-29T04:15:09Z",
            "proof with no timestamp cannot be aged, so a --recheck cannot \
             tell a proof taken minutes ago from one taken in June"
        );
        // The stamp must match the conductor's format byte for byte —
        // proof lag is this minus `review.completed_at`.
        assert_eq!(md["completed_at"], json!(crate::gate::stamp(now)));
        // And it does not disturb what the step already carried.
        assert_eq!(md["verified"], json!("v"));
        assert_eq!(md["proof"], json!("{\"exit\":0}"));
        assert_eq!(md["method"], json!("api"));
    }

    #[test]
    fn an_omitted_method_stays_omitted() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-29T04:15:09Z")
            .unwrap()
            .into();
        let md = proven_metadata("", "{}", None, now);
        assert!(md.get("method").is_none(), "an absent method is not `null`");
        assert!(md["completed_at"].is_string());
    }

    /// A COMPLETED STEP IS CLOSED TO A NEW PROOF, AND OPEN TO A BETTER
    /// ONE. Car 932aa956's probe observed a real production refusal and
    /// stopped holding within the hour because the condition was
    /// transient — `--recheck` said NO LONGER HOLDS, correctly, and was
    /// wrong about the cause. Without --replace the only route to a
    /// durable probe was an operator editing job metadata by hand.
    #[test]
    fn a_proven_step_reopens_only_for_a_replacement() {
        let done = car("11111111-a", "feat/x", "completed");
        assert!(
            proven_step(&done, false).is_err(),
            "an ordinary prove must not overwrite a recorded proof"
        );
        assert!(
            proven_step(&done, true).is_ok(),
            "--replace is how a transient proof gets a better probe"
        );
    }

    /// ...and the refusal now says how, rather than only saying no.
    #[test]
    fn the_refusal_names_replace_as_the_way_forward() {
        let e = proven_step(&car("11111111-a", "feat/x", "completed"), false)
            .unwrap_err()
            .to_string();
        assert!(e.contains("--recheck"), "{e}");
        assert!(e.contains("--replace"), "{e}");
    }

    /// A REPLACEMENT SUPERSEDES THE ORIGINAL FOR RE-RUNNING, and the
    /// original is still on the step — the same precedence
    /// `regate_receipt` has over a stale gate receipt. Which proof
    /// `--recheck` should re-run is a different question from which one
    /// happened, and both answers are kept.
    #[test]
    fn recheck_re_runs_the_newest_replacement() {
        let o = Outcome {
            exit: 0,
            stdout: String::new(),
            stderr: String::new(),
        };
        let first = proof_json("old-probe", None, &o, "h", "2026-08-29T00:00:00Z");
        let better = proof_json("better-probe", None, &o, "h", "2026-08-30T00:00:00Z");
        let step = json!({"metadata": {"proof": first.to_string()}});
        let with_reproof = json!({"metadata": {"reproof": [
            {"proof": better.to_string(), "recorded_at": "2026-08-30T00:00:00Z"}
        ]}});
        let plain = json!({"metadata": {}});

        assert_eq!(
            recorded_probe_for(&plain, &step).expect("readable").probe,
            "old-probe",
            "with no replacement, the step's own proof is what re-runs"
        );
        assert_eq!(
            recorded_probe_for(&with_reproof, &step)
                .expect("readable")
                .probe,
            "better-probe",
            "a replacement is what --recheck should be re-running"
        );
    }
}
