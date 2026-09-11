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
//! A NUMERIC EXPECTATION IS COMPARED AS A WHOLE TOKEN. `--expect "1"`
//! once accepted a probe that printed `10`, because the comparison was
//! a plain substring test (421b3032): the claim was true and the proof
//! checked nothing, which is the one failure this verb exists to
//! prevent. Numbers now have to stand alone, and recording a bare
//! number warns and names the shape that cannot lie — assert inside the
//! probe and print a unique token.
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

/// Is this expectation a bare number?
///
/// `inf` and `nan` parse as floats and are not what anyone means by a
/// numeric expectation, so a digit is required as well.
pub(crate) fn is_bare_number(want: &str) -> bool {
    want.chars().any(|c| c.is_ascii_digit()) && want.parse::<f64>().is_ok()
}

/// Could this character be part of the same number token?
fn joins_a_token(c: char) -> bool {
    c.is_alphanumeric() || c == '.' || c == '_'
}

/// Did the probe print what was expected?
///
/// A token expectation is a substring test, which is what every good
/// probe relies on (`claim:ok` inside a longer line). A NUMERIC one is
/// a whole-token test, because a substring test accepted `10` as proof
/// of `1` (421b3032) — the claim was true and the proof checked
/// nothing. One definition, used by the record path and by `--recheck`.
pub(crate) fn observed(printed: &str, want: &str) -> bool {
    if !is_bare_number(want) {
        return printed.contains(want);
    }
    printed.match_indices(want).any(|(i, _)| {
        let before = printed[..i]
            .chars()
            .next_back()
            .is_none_or(|c| !joins_a_token(c));
        let after = printed[i + want.len()..]
            .chars()
            .next()
            .is_none_or(|c| !joins_a_token(c));
        before && after
    })
}

/// A bare number is a weak expectation even when it matches, so say so
/// at record time and name the shape that cannot lie. Returns the
/// warning text, or `None` when the expectation is already a token.
pub(crate) fn bare_number_warning(want: &str) -> Option<String> {
    is_bare_number(want).then(|| {
        format!(
            "boss prove: WEAK EXPECTATION — --expect {want:?} is a bare number, so the proof \
             rests on the probe's output being read correctly by a human later.\n  \
             It is compared as a whole token (a substring test would accept 10 as proof of 1), \
             but a number still says nothing about WHICH number was meant.\n  \
             The shape that cannot lie asserts inside the probe and prints a unique token:\n    \
             $ test $({want} …) -eq {want} && echo claim:ok    --expect 'claim:ok'"
        )
    })
}

/// Decide whether an outcome is evidence, or refuse and say why.
///
/// This is the whole gate, and it is deliberately only two rules: the
/// probe must have exited zero, and — unless the caller downgraded to
/// `exit_only` — the expected string must actually appear in what it
/// printed (see `observed` for what "appear" means). Both refusals
/// quote the streams, because a refusal that hides the output makes
/// the reader go hunting for it.
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
        if !observed(&o.stdout, want) && !observed(&o.stderr, want) {
            bail!(
                "the probe exited 0 but never printed {want:?}, so it did not \
                 observe what was claimed.{}\n\
                 \n  stdout: {}\n  stderr: {}\n\n\
                 An exit code alone is a weak assertion — `echo hi` exits 0 too. \
                 Either the change is not in prod, or the probe is looking in the \
                 wrong place.",
                if is_bare_number(want) {
                    format!(
                        "\n  {want:?} is a number, so it is compared as a whole token: a \
                         substring test would accept 10 as proof of 1, which is how a proof \
                         passes for the wrong reason (421b3032). If the number IS in the \
                         output as part of a longer one, assert it inside the probe instead: \
                         `test $(…) -eq {want} && echo claim:ok`."
                    )
                } else {
                    String::new()
                },
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

/// WHICH PROBES THIS DOOR WILL RUN (backlog 23b2dffa).
///
/// TWO DOORS RUN A CAR'S RECORDED PROBE AND ONLY ONE CHECKED IT. `boss
/// gate --park-probe` refuses a probe that names a tool the forge host
/// lacks, and one that reads the system of record unidentified. This
/// verb — the HAND door onto the same text, and the one an operator
/// reaches for while moving fast — applied neither, and the only trace
/// of the rule here was a comment describing the gate's refusal. One
/// rule, two enforcement sites, one of which is prose about the other,
/// is CLAUDE.md §9a in its behavioural form, so the predicates are now
/// `boss_jobs::probe` and both doors call them.
///
/// WHY IT REFUSES RATHER THAN WARNS. The operator IS present, and may
/// have a reason the check cannot see — which is an argument for a
/// warning until you notice `--recheck`: it re-runs this recorded probe
/// later, when nobody is watching, so a probe admitted by hand BECOMES
/// an unattended one. A refusal with a stated override keeps both — the
/// operator gets through, and the next reader learns it was overridden
/// and why.
///
/// AND WHY ONLY ONE OF THE TWO RULES REFUSES HERE. They are not the same
/// kind of rule. "Can this text run where it is going" is host-relative:
/// `host-absent-tools.txt` is a measurement of the FORGE, because a
/// `--park-probe` runs there, while this verb runs the probe HERE, in
/// front of the operator, one second later — and a pod-local `kubectl`
/// proof is the established shape for a claim only the cluster can show
/// (memory: proof probes from the pod). Refusing that would block
/// exactly the case where a human must prove what the forge cannot, so a
/// hand `--probe` is not judged against another machine's tools: the
/// question the manifest predicts, this verb measures. "Can its answer
/// be trusted" is host-independent, and refuses everywhere.
///
/// `--from-car` is the one path where both apply, because that text has
/// a second destination: the arrival rule runs it on the forge. There
/// the mismatch is NAMED and not refused — proving it by hand here is
/// the right move; what needs fixing is the probe the car recorded.
pub(crate) const OVERRIDE_FLAG: &str = "--probe-anyway";

pub(crate) use boss_jobs::probe::{UNIDENTIFIED_RULE, override_record};

/// What this door makes of a probe: a reason not to run it, something
/// the operator should know, or neither.
#[derive(Debug, Default)]
pub(crate) struct Admission {
    /// Why the probe must not run, unless the operator overrides it.
    pub refusal: Option<String>,
    /// Something worth saying that does not stop the probe.
    pub warning: Option<String>,
}

/// Judge a probe at this door. `from_car` says the text came from the
/// car's `proof_probe`, which means the forge will run it too.
pub(crate) fn admit(probe: &str, from_car: bool) -> Admission {
    let refusal = boss_jobs::probe::reads_the_sor_unidentified(probe).map(|client| {
        format!(
            "THE PROBE READS THE SYSTEM OF RECORD WITH `{client}` AND NO IDENTITY, so it \
             reads as operator:unidentified — and an unidentified reader is answered with a \
             NARROWER WORLD, silently. What this verb would record is a proof of nothing.\
             \n\n{evidence}\n\n\
             Read it as a named reader instead — `boss-api GET /api/...` where that door \
             exists, `{reader} /api/...` on the forge, or a curl that sends an identity \
             header.\n\n\
             If this read IS identified in a way a text check cannot see, say so and it \
             runs: {OVERRIDE_FLAG} '<reason>'. The reason is recorded in the proof, because \
             `--recheck` re-runs this probe later when nobody is watching.",
            evidence = boss_jobs::probe::UNIDENTIFIED_READ_EVIDENCE,
            reader = boss_jobs::probe::SOR_READER,
        )
    });
    let warning = if from_car {
        boss_jobs::probe::needs_absent_tool(probe).map(|tool| {
            format!(
                "boss prove: NOTE — this car's recorded probe invokes `{tool}`, which the \
                 forge host does not have (infra/forge/host-absent-tools.txt). It runs HERE, \
                 so proving by hand is fine and is the point; but the arrival rule could not \
                 have run it, and any later re-run on the forge will report `unrunnable` \
                 rather than a verdict (f9304366). Re-park the car with a probe the forge \
                 can run, or record it as --park-proof-event."
            )
        })
    } else {
        None
    };
    Admission { refusal, warning }
}

/// The override, resolved once: `None` when the flag was not given,
/// refusing a flag given with no reason — an override with no stated
/// reason is the silent yes the whole verb exists to end.
pub(crate) fn override_reason(given: Option<&str>) -> Result<Option<&str>> {
    match given.map(str::trim) {
        Some("") => bail!(
            "{OVERRIDE_FLAG} needs a reason: it is recorded in the proof, where the next \
             reader — or `--recheck`, unattended — finds out why a refused probe was run."
        ),
        other => Ok(other),
    }
}

/// The proof record. Serialised once, stored verbatim, re-read by
/// `--recheck` — so its field names are a contract, not a detail.
/// `overridden` is the one optional key: present only when the
/// operator ran a probe this door refused, absent otherwise, so its
/// presence means something to whoever reads the proof back. The forge
/// runner records no such key because it has no override door — which
/// is why `the_forge_runner_records_the_same_proof_shape` pins the
/// plain shape and this key is not in it.
pub(crate) fn proof_json(
    probe: &str,
    expect: Option<&str>,
    o: &Outcome,
    host: &str,
    at: &str,
    overridden: Option<&Value>,
) -> Value {
    let mut p = json!({
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
    });
    if let Some(o) = overridden {
        p["overridden"] = o.clone();
    }
    p
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

/// The matching core `find_car` and `boss job` share: a row matches by
/// exact `metadata.branch`, or by an id prefix of at least 8
/// characters. Pure and message-free so each verb can refuse in its
/// own vocabulary — `find_car`'s "no ship-a-change car" would be a lie
/// coming from `boss job get`, which sees every kind.
pub(crate) fn matching_jobs<'a>(rows: &'a [Value], given: &str) -> Vec<&'a Value> {
    rows.iter()
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
        .collect()
}

/// Which cars a lookup may legitimately resolve to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Eligible {
    /// Recording a proof WRITES to the car, so a car the pipeline has
    /// finished with is not a candidate: it cannot take a new proof.
    Live,
    /// `--recheck` re-runs a probe already recorded on the car and
    /// writes nothing, so a finished car is a legitimate target — which
    /// is the whole reason [`all_ship_a_change_cars`] pages over closed
    /// cars instead of filtering `status=open` at the API.
    AnyStatus,
}

/// The car's `status` as the jobs API spelled it, or `"?"` when the row
/// carries none. For DISPLAY only — whether a car is live is
/// `boss_jobs::car::is_open`'s single answer, never a second reading of
/// this string.
fn status_of(car: &Value) -> &str {
    car.get("status").and_then(Value::as_str).unwrap_or("?")
}

/// One `id8  status  title` line per candidate, so a refusal that lists
/// cars is answerable from the refusal.
///
/// The id is shortened by [`crate::train::id8`] rather than a second
/// `[..8]` of its own: the bare slice panicked on any row whose id the
/// reader could not find, which is the listing for a malformed row
/// crashing instead of printing it.
fn listed(rows: &[&Value]) -> String {
    rows.iter().fold(String::new(), |mut out, c| {
        out.push_str(&format!(
            "\n  {}  {}  {}",
            crate::train::id8(c.get("id").and_then(Value::as_str).unwrap_or("?")),
            status_of(c),
            c.get("title").and_then(Value::as_str).unwrap_or("?")
        ));
        out
    })
}

/// Find the one car for `given` — a branch name, or an id prefix.
///
/// Refuses on ambiguity rather than picking. `boss park` learned this
/// the expensive way: it took the LAST match from a list the API
/// returns newest-first, and quietly parked two cars against a stale
/// receipt. Choosing among candidates is how that happens, so this
/// does not choose.
///
/// It does DISCARD candidates that cannot take a proof at all — which
/// is not choosing between live cars, and is what backlog 0a58827d asked
/// for. Measured live 2026-09-10 05:02 UTC: `boss prove
/// feat/arrival-runs-the-probe-rerail` refused with "2 open cars match"
/// over one live car and one closed, abandoned twin ("gated, then
/// changed"). There was one candidate, so the refusal was wrong to fire,
/// and the word "open" sent the reader to investigate a dead car — two
/// queries spent learning what the message had already asserted falsely.
///
/// Whether a car is live is `boss_jobs::car::is_open`, the one definition
/// `boss park`, `boss rerail` and the auto-park handler already decide
/// on (CLAUDE.md §9a) — this verb does not carry a second reading of
/// `status`. What the CALLER's mode decides is what a finished car means
/// here:
///
/// - [`Eligible::Live`] — recording a proof is a WRITE, so a finished
///   car is discarded outright. With nothing left the refusal says the
///   matches are finished, rather than sending the reader after a typo.
/// - [`Eligible::AnyStatus`] — `--recheck` writes nothing, so a finished
///   car is a legitimate target; a live match is still PREFERRED when
///   the name has one, so a recheck resolves where it used to.
///
/// Either way every listed candidate is labelled with its status, so a
/// refusal that must name a finished car stays answerable without a
/// second query — the per-row label replaces the one collective "open"
/// / "closed" word, which could only describe a homogeneous set.
pub(crate) fn find_car<'a>(
    cars: &'a [Value],
    given: &str,
    eligible: Eligible,
) -> Result<&'a Value> {
    let matched = matching_jobs(cars, given);
    let live: Vec<&Value> = matched
        .iter()
        .copied()
        .filter(|c| boss_jobs::car::is_open(c))
        .collect();
    let candidates: Vec<&Value> = match eligible {
        Eligible::Live => live,
        Eligible::AnyStatus if !live.is_empty() => live,
        Eligible::AnyStatus => matched.clone(),
    };

    match candidates.len() {
        1 => Ok(candidates[0]),
        // Nothing live, but the name DID match — say so, rather than
        // sending the operator to hunt a typo that is not there.
        0 if !matched.is_empty() => bail!(
            "no car for {given:?} that can take a proof — every match is finished:{}\n\n\
             A closed or cancelled car is done, and recording a new proof on one would be \
             writing to a terminal. Use --recheck to re-run the probe already recorded on \
             it, or name the car that carries this work now.",
            listed(&matched)
        ),
        0 => bail!(
            "no ship-a-change car for {given:?}. Give the car's branch exactly \
             as it was parked, or at least 8 characters of its id."
        ),
        n => bail!(
            "{n} cars match {given:?} — say which:{}",
            listed(&candidates)
        ),
    }
}

/// Every `ship-a-change` car, paged on `total` so the target is
/// reachable no matter how many closed cars precede it.
///
/// The read used to be a single `?kind=ship-a-change&limit=200`. As
/// closed cars accumulate they fill that one page, so a legitimately
/// open, unproven car sorting past row 200 vanishes from [`find_car`] —
/// a false negative that grows with the pipeline's age. A `status=open`
/// filter would not fix it — and is why this read is the one car lookup
/// NOT built on `gate::all_open_cars`: `--recheck` re-runs the proof on
/// a CLOSED car, so the reader must read closed cars too and let
/// [`find_car`] decide which statuses the caller's mode admits
/// ([`Eligible`]). Paging on `total` keeps every car reachable, open or
/// closed.
async fn all_ship_a_change_cars(http: &reqwest::Client, base: &str) -> Result<Vec<Value>> {
    const PAGE: usize = 500;
    let mut cars: Vec<Value> = Vec::new();
    loop {
        let body = crate::gate::api_at(
            http,
            base,
            reqwest::Method::GET,
            &format!(
                "/api/jobs?kind=ship-a-change&limit={PAGE}&offset={}",
                cars.len()
            ),
            None,
        )
        .await?;
        let total = body
            .as_ref()
            .and_then(|v| v.get("total"))
            .and_then(Value::as_i64)
            .unwrap_or(0)
            .max(0) as usize;
        let got = {
            let page = crate::gate::rows(body);
            let n = page.len();
            cars.extend(page);
            n
        };
        // Stop when a page came back empty (offset past the data) or we
        // have accumulated the whole population. Either guard alone
        // terminates; both together survive a miscounted `total`.
        if got == 0 || cars.len() >= total {
            break;
        }
    }
    Ok(cars)
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
    /// The override this proof was admitted under, if the operator ran
    /// a probe the door refused. Read back so `--recheck` — the
    /// unattended re-run — says so instead of reporting a clean HOLDS
    /// on a probe somebody had to argue past.
    pub overridden: Option<Value>,
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

/// Is `value` one of the variants `field_type` declares, for the field
/// named `name` on this step?
///
/// Reads the step's OWN field spec — the one that arrived with the car
/// we already fetched, from the workflow version this packet is pinned
/// to. Deliberately not a constant in this file: the vocabulary already
/// lives in `infra/platform/workflows/` and in the registry
/// default, and a third copy here would be the drift CLAUDE.md 9a is
/// about. Validating against the packet's own spec also means a
/// workflow version that adds a variant needs no CLI release.
///
/// `Ok(())` when the field is absent or free-text: this verb is not the
/// place to invent a contract the protocol did not state.
pub(crate) fn check_enum_field(step: &Value, name: &str, value: &str) -> Result<()> {
    let Some(ft) = step
        .get("fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|f| f.get("name").and_then(Value::as_str) == Some(name))
        .and_then(|f| f.get("field_type"))
        .and_then(Value::as_str)
    else {
        return Ok(());
    };
    if !ft.contains('|') {
        return Ok(());
    }
    let allowed: Vec<&str> = ft.split('|').map(str::trim).collect();
    if allowed.contains(&value) {
        return Ok(());
    }
    bail!(
        "--{name} {value:?} is not one of {}. This is checked BEFORE the probe runs: \
         the probe would have executed against production, passed, and then been \
         thrown away when the API refused the record — which is how two proofs were \
         silently lost on 2026-09-02.",
        allowed.join(" | ")
    )
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
        overridden: v.get("overridden").cloned(),
    })
}

/// The probe a car recorded for itself at park time, read back as the
/// pair `boss prove` would have been given by hand.
///
/// WHERE IT RUNS. Two callers run this text on two different machines:
/// `--from-car` runs it wherever the operator is standing, and the
/// arrival rule runs it on the FORGE HOST as david, in the converged
/// checkout, with that host's tools. The forge has no kubeconfig, so a
/// probe can pass here and be unrunnable there — measured 2026-09-09
/// (f9304366), which is why `boss gate --park-probe` refuses a probe
/// naming a tool in infra/forge/host-absent-tools.txt.
///
/// THE CAR CARRIES ITS PROBE (28ac45ab): `boss gate --park-probe/
/// --park-expect` stamps the intent, the auto-park handler copies it
/// onto the car under `boss_jobs::car::PROOF_*`, and this is the read
/// side — `--from-car` here, and the arrival rule's runner. The keys
/// are one definition in core so the three cannot drift.
///
/// Refuses, naming the door, when the car recorded nothing — and says
/// so differently for an EVENT-BOUND car, whose builder decided a
/// machine cannot prove it: running "no probe" as a probe would be the
/// silent yes this verb exists to end.
pub(crate) fn car_probe(car: &Value) -> Result<(String, String)> {
    let md = |k: &str| {
        car.get("metadata")
            .and_then(|m| m.get(k))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    if let Some(event) = md(boss_jobs::car::PROOF_EVENT) {
        bail!(
            "this car is EVENT-BOUND, not probed: {event}\n\n\
             Its builder recorded the event that proves it instead of a command, so there \
             is nothing for --from-car to run. Prove it by hand when the event fires: \
             boss prove <car> --probe '<what you observed>' --expect '<string>' --verified '<prose>'."
        );
    }
    let Some(probe) = md(boss_jobs::car::PROOF_PROBE) else {
        bail!(
            "this car recorded no `{}`, so --from-car has nothing to run. The probe is \
             written at park time — `boss gate --park-probe '<cmd>' --park-expect '<string>'` \
             — by the builder who knows what the change does. Give one now with --probe \
             and --expect instead.",
            boss_jobs::car::PROOF_PROBE
        );
    };
    let Some(expect) = md(boss_jobs::car::PROOF_EXPECT) else {
        bail!(
            "this car recorded a `{}` but no `{}` — a probe asserting nothing is `echo hi`. \
             The gate verb refuses that pair, so the car was edited by hand; give --expect \
             here or fix the car's metadata.",
            boss_jobs::car::PROOF_PROBE,
            boss_jobs::car::PROOF_EXPECT
        );
    };
    Ok((probe, expect))
}

/// What a car-carried proof MEANS, when the operator did not say: the
/// car's own summary — the claim the change makes, which is exactly
/// what the probe checks in production. `verified` stays required in
/// the record; it just has a source the builder already wrote.
pub(crate) fn car_verified(car: &Value, given: Option<String>) -> Result<String> {
    if let Some(v) = given {
        return Ok(v);
    }
    car.get("metadata")
        .and_then(|m| m.get("summary"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "--verified is required: the car has no summary to fall back on, so say \
                 what the probe means, in prose, for a reader"
            )
        })
}

/// The host a verb is running on. Shared with `boss car open`, which
/// records it on the car at build start — the same question, one
/// definition (CLAUDE.md §9a).
pub(crate) fn host() -> String {
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
    from_car: bool,
    // The stated override for a probe this door refuses — see [`admit`].
    probe_anyway: Option<String>,
    // The operator's now, taken once at the CLI entry point and passed
    // in — the same shape `train::run` uses, so nothing down here reads
    // the wall clock on its own.
    now: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let overriding = override_reason(probe_anyway.as_deref())?;
    let http = reqwest::Client::new();
    let cars = all_ship_a_change_cars(&http, &crate::gate::resolve_jobs_base(None)?).await?;
    // `--recheck` RE-RUNS a recorded probe and writes nothing, so a
    // finished car is a legitimate target for it and for nothing else.
    // Every other path records a proof, which a closed or cancelled car
    // cannot take — so those are not candidates, and naming them as
    // "open" is what sent an operator after a dead twin (0a58827d).
    let eligible = if recheck {
        Eligible::AnyStatus
    } else {
        Eligible::Live
    };
    let car = find_car(&cars, car_ref, eligible)?;
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
        // THE RULES APPLY TO A RE-RUN TOO, and this is the path that
        // most needs them: `--recheck` is the unattended re-run, so a
        // recorded probe that reads the system of record unidentified
        // would report a confident HOLDS against a narrowed world. The
        // refusal says so instead, and names `--replace` — which exists
        // to put a better probe under a claim already proven.
        if let Some(r) = admit(&probe, true).refusal {
            match overriding {
                None => bail!(
                    "CANNOT RE-CHECK THIS PROBE — {r}\n\n\
                     A recorded proof is not evidence of itself: re-prove the car with \
                     `--replace` and a probe that reads as a named reader, which leaves the \
                     original on the step where a reader can see what used to be claimed."
                ),
                Some(reason) => println!(
                    "boss prove: re-checking a probe this door refuses, on your stated \
                     reason — {reason}"
                ),
            }
        }
        if let Some(o) = &rec.overridden {
            println!(
                "boss prove: this proof was recorded OVER A REFUSAL — {}",
                serde_json::to_string(o).unwrap_or_else(|_| "(unreadable)".into())
            );
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

    // --from-car: the probe the builder recorded at park time, and the
    // car's summary as the default prose. The clap-level conflicts
    // keep --probe/--expect/--exit-only off this path.
    let (probe, expect, verified) = if from_car {
        let (p, e) = car_probe(car)?;
        println!("boss prove: {short} runs the probe it carried from park time");
        (Some(p), Some(e), Some(car_verified(car, verified)?))
    } else {
        (probe, expect, verified)
    };
    let probe = probe.ok_or_else(|| {
        anyhow::anyhow!(
            "--probe is required: proof is a command that ran, not a sentence \
             (or --from-car, to run the probe the car recorded at park time)"
        )
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
    let target = proven_step(car, replace)?;
    // ...and a --method the protocol does not accept is the same class
    // of refusal, for the same reason. It used to be caught only by the
    // API when the finished proof was recorded, so the probe ran, the
    // evidence was good, and it was discarded on a 400.
    if let Some(m) = method.as_deref() {
        check_enum_field(target, "method", m)?;
    }

    // A bare number is legal but weak, so say so BEFORE the probe runs
    // — the reader is looking at the output right then, which is the
    // only moment the better shape is cheap to adopt.
    if let Some(w) = expect.as_deref().and_then(bare_number_warning) {
        eprintln!("{w}");
    }

    // THE SAME RULES THE GATE APPLIES, AT THIS DOOR (23b2dffa), before
    // the probe runs and before anything is recorded.
    let admission = admit(&probe, from_car);
    if let Some(w) = &admission.warning {
        eprintln!("{w}");
    }
    let overridden = match (&admission.refusal, overriding) {
        (Some(r), None) => bail!("{r}"),
        (Some(_), Some(reason)) => {
            println!(
                "boss prove: running a probe this door refuses, on your stated reason — \
                 {reason}\n  It is recorded in the proof as `overridden`, so a later reader \
                 (and `--recheck`) sees the claim was argued past rather than clean."
            );
            Some(override_record(UNIDENTIFIED_RULE, reason))
        }
        (None, Some(_)) => {
            eprintln!(
                "boss prove: {OVERRIDE_FLAG} was given but nothing refused this probe — \
                 nothing is recorded as overridden."
            );
            None
        }
        (None, None) => None,
    };

    println!("boss prove: {short}  $ {probe}");
    let o = execute(&probe)?;
    judge(&o, expect.as_deref())?;

    let at = now.to_rfc3339();
    let proof = proof_json(
        &probe,
        expect.as_deref(),
        &o,
        &host(),
        &at,
        overridden.as_ref(),
    );
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

    /// THE BUG (421b3032): `--expect "1"` accepted a probe that printed
    /// `10`, because the comparison was a substring test. The claim was
    /// true; the proof did not check what was meant. A numeric
    /// expectation must appear as a WHOLE token or it is not evidence.
    #[test]
    fn a_number_does_not_match_a_longer_number() {
        for printed in ["10", "21", "100", "0.15", "v1.2.3", "step_1a"] {
            let e = judge(&ok(printed), Some("1")).unwrap_err().to_string();
            assert!(
                e.contains("never printed"),
                "expect \"1\" must not match {printed:?}: {e}"
            );
            assert!(
                e.contains("whole"),
                "the refusal must say the number is compared as a whole token: {e}"
            );
        }
    }

    /// ...and it still matches when it really is the number printed.
    #[test]
    fn a_number_matches_when_it_stands_alone() {
        for printed in ["1", "1 rule", "count=1", "rules: 1", "1\n", "(1)"] {
            assert!(
                judge(&ok(printed), Some("1")).is_ok(),
                "expect \"1\" must match {printed:?}"
            );
        }
    }

    /// Only NUMERIC expectations tighten. A token expectation is still
    /// a substring test, which is what every good probe relies on.
    #[test]
    fn a_non_numeric_expectation_is_still_a_substring() {
        assert!(judge(&ok("prefix-claim:ok-suffix"), Some("claim:ok")).is_ok());
        assert!(judge(&ok("unprovenX"), Some("unproven")).is_ok());
    }

    /// The stronger rider: a bare number is a weak expectation even
    /// when it matches, so recording one warns and names the shape
    /// that cannot lie.
    #[test]
    fn a_bare_number_expectation_warns_at_record_time() {
        let w = bare_number_warning("1").expect("a bare number must warn");
        assert!(w.contains("WEAK"), "the warning must be loud: {w}");
        assert!(
            w.contains("-eq 1 && echo claim:ok"),
            "the warning must name the shape that cannot lie: {w}"
        );
        assert!(bare_number_warning("0.15").is_some());
        assert!(
            bare_number_warning("claim:ok").is_none(),
            "a token expectation is the good shape and must not warn"
        );
        assert!(bare_number_warning("1 rule").is_none());
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

    /// THE CAR CARRIES ITS PROBE (28ac45ab). `--from-car` reads the
    /// pair the auto-park handler copied from the gate's park intent,
    /// under the one set of keys core defines.
    #[test]
    fn from_car_reads_the_probe_the_car_recorded() {
        let car = serde_json::json!({
            "id": "c1", "metadata": {
                "summary": "The yard shows the probe count. And why.",
                "proof_probe": "curl -s http://sor/api/yard | grep -c unproven",
                "proof_expect": "unproven"
            }
        });
        let (p, e) = car_probe(&car).unwrap();
        assert_eq!(p, "curl -s http://sor/api/yard | grep -c unproven");
        assert_eq!(e, "unproven");
        // The prose defaults to the car's own claim, and an explicit
        // --verified still wins.
        assert_eq!(
            car_verified(&car, None).unwrap(),
            "The yard shows the probe count. And why."
        );
        assert_eq!(car_verified(&car, Some("seen".into())).unwrap(), "seen");
    }

    /// No probe recorded → a refusal that names the park-time door; an
    /// event-bound car → a refusal that quotes the event, so nobody
    /// runs "nothing" and records a yes.
    #[test]
    fn from_car_refuses_an_unprobed_or_event_bound_car() {
        let bare = serde_json::json!({"id": "c1", "metadata": {"summary": "x"}});
        let e = car_probe(&bare).unwrap_err().to_string();
        assert!(e.contains("--park-probe"), "{e}");

        let event = serde_json::json!({"id": "c1", "metadata": {
            "proof_event": "event-bound — the next yard-button cancel"
        }});
        let e = car_probe(&event).unwrap_err().to_string();
        assert!(e.contains("EVENT-BOUND"), "{e}");
        assert!(e.contains("yard-button cancel"), "quotes the event: {e}");

        // A probe with no expectation is refused here too — the gate
        // refuses the pair, so this is a hand-edited car.
        let half = serde_json::json!({"id": "c1", "metadata": {"proof_probe": "true"}});
        let e = car_probe(&half).unwrap_err().to_string();
        assert!(e.contains("echo hi"), "{e}");

        // And with no summary there is no default prose to fall back on.
        let e = car_verified(&half, None).unwrap_err().to_string();
        assert!(e.contains("--verified is required"), "{e}");
    }

    /// A FACT THAT LIVES TWICE GETS AN EQUALITY TEST (CLAUDE.md 9a).
    /// The arrival path judges probes in sh, not in Rust, so the
    /// whole-token rule has to hold there too — otherwise the machine
    /// half still accepts 10 as proof of 1. This lifts the script's
    /// own matcher out and runs both sides against one table.
    #[test]
    fn the_forge_runner_judges_an_expectation_the_same_way() {
        const SH: &str = include_str!("../../../../infra/forge/run-car-probe.sh");
        const START: &str = "# --- expectation-match";
        const END: &str = "# --- end expectation-match";
        let a = SH
            .find(START)
            .expect("run-car-probe.sh lost its matcher block");
        let b = SH
            .find(END)
            .expect("run-car-probe.sh lost its matcher end marker");
        let matcher = &SH[a..b];

        for (printed, expect, want) in [
            ("10", "1", false),
            ("100", "1", false),
            ("0.15", "1", false),
            ("v1.2.3", "1", false),
            ("1", "1", true),
            ("1 rule", "1", true),
            ("count=1", "1", true),
            ("rules: 1", "1", true),
            ("0 errors", "0", true),
            ("10 errors", "0", false),
            ("prefix-claim:ok-suffix", "claim:ok", true),
            ("nothing here", "claim:ok", false),
        ] {
            assert_eq!(
                observed(printed, expect),
                want,
                "prove.rs: {expect:?} against {printed:?}"
            );
            let script = format!(
                "{matcher}\nf=$(mktemp)\nprintf '%s\\n' \"$1\" > \"$f\"\n\
                 if printed_expectation \"$2\" \"$f\"; then echo MATCH; else echo NOMATCH; fi\n\
                 rm -f \"$f\"\n"
            );
            let out = std::process::Command::new("bash")
                .arg("-c")
                .arg(&script)
                .arg("pin")
                .arg(printed)
                .arg(expect)
                .output()
                .expect("bash runs the lifted matcher");
            let said = String::from_utf8_lossy(&out.stdout);
            assert_eq!(
                said.trim() == "MATCH",
                want,
                "run-car-probe.sh disagrees with prove.rs on {expect:?} against {printed:?} \
                 (it said {said:?}) — the two comparisons must stay one rule"
            );
        }
    }

    /// THE PROOF RECORD LIVES TWICE — here and in the forge's
    /// run-car-probe.sh, which records the same shape in sh so the
    /// arrival rule can prove a car on a host with no `boss` binary
    /// (28ac45ab). Pinned (CLAUDE.md 9a): every key `proof_json` and
    /// `proven_metadata` write must appear in the script's jq record,
    /// and its stream cap must equal MAX_STREAM — or `--recheck` reads
    /// a machine-written proof it cannot re-run.
    #[test]
    fn the_forge_runner_records_the_same_proof_shape() {
        const SH: &str = include_str!("../../../../infra/forge/run-car-probe.sh");
        let p = proof_json("true", Some("x"), &ok("x"), "h", "now", None);
        for k in p.as_object().unwrap().keys() {
            assert!(
                SH.contains(&format!("{k}:${k}")),
                "run-car-probe.sh does not record proof key `{k}`"
            );
        }
        let at: chrono::DateTime<chrono::Utc> =
            chrono::DateTime::parse_from_rfc3339("2026-09-08T18:00:00Z")
                .unwrap()
                .into();
        let md = proven_metadata("v", "{}", None, at);
        for k in md.as_object().unwrap().keys() {
            assert!(
                SH.contains(&format!("{k}:$")),
                "run-car-probe.sh does not write proven key `{k}`"
            );
        }
        // `why`, `unrunnable` and `missing_tools` are the diagnosis
        // half (f9304366): a probe authored on the pod and run on the
        // forge can be correct and unrunnable, and exit-plus-empty-
        // streams reads exactly like a false claim. The script must
        // record which of the two it saw.
        for k in ["at", "exit", "output", "why", "unrunnable", "missing_tools"] {
            assert!(
                SH.contains(&format!("{k}:${k}")),
                "run-car-probe.sh's proof_attempt lacks `{k}`"
            );
        }
        assert!(
            SH.contains("command_not_found_handle"),
            "run-car-probe.sh must give the probe's shell a channel its own \
             redirections cannot swallow, or a missing tool records as silence"
        );
        assert!(
            SH.contains(&format!("MAX_STREAM={MAX_STREAM}")),
            "the two stream caps differ"
        );
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
            None,
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
        let proof = proof_json(
            "echo ok",
            None,
            &o,
            "somehost",
            "2026-08-29T00:00:00Z",
            None,
        );
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

    /// A car at a named status — the shape the jobs API returns, which
    /// `car()` predates.
    fn car_at(id: &str, branch: &str, status: &str) -> Value {
        let mut c = car(id, branch, "ready");
        c["status"] = json!(status);
        c
    }

    /// Ambiguity is refused rather than resolved — the failure mode that
    /// cost two cars when `boss park` picked from a list instead.
    #[test]
    fn two_matching_cars_are_refused_not_chosen() {
        let cars = vec![
            car("11111111-aaa", "feat/x", "ready"),
            car("11111111-bbb", "feat/x", "ready"),
        ];
        let e = find_car(&cars, "feat/x", Eligible::Live)
            .unwrap_err()
            .to_string();
        assert!(e.contains("2 cars match"), "{e}");
    }

    /// THE DEFECT (backlog 0a58827d). `boss prove` refused with "2 open
    /// cars match" when only one was open: the other had been abandoned
    /// ("gated, then changed"). A closed car cannot take a new proof, so
    /// it was never a candidate — and with the dead twin gone the case
    /// resolves to one car and needs no refusal at all. The refusal sent
    /// an operator to investigate a dead car.
    #[test]
    fn a_closed_twin_is_not_a_candidate_for_a_new_proof() {
        let cars = vec![
            car_at("ef4eff3c-aaa", "feat/arrival-runs-the-probe-rerail", "open"),
            car_at(
                "538775dd-bbb",
                "feat/arrival-runs-the-probe-rerail",
                "closed",
            ),
        ];
        let got = find_car(&cars, "feat/arrival-runs-the-probe-rerail", Eligible::Live)
            .expect("one live car is not ambiguous");
        assert_eq!(got.get("id").and_then(Value::as_str), Some("ef4eff3c-aaa"));
    }

    /// A cancelled car is as finished as a closed one — answered by
    /// `boss_jobs::car::is_open`, so this verb carries no second reading
    /// of `status` to drift from it.
    #[test]
    fn a_cancelled_twin_is_not_a_candidate_either() {
        let cars = vec![
            car_at("ef4eff3c-aaa", "feat/x", "open"),
            car_at("538775dd-bbb", "feat/x", "cancelled"),
        ];
        assert!(find_car(&cars, "feat/x", Eligible::Live).is_ok());
    }

    /// And when the ONLY match is finished, the refusal says so. The
    /// bare "no open car — check your spelling" would send the operator
    /// hunting a typo that is not there.
    #[test]
    fn the_only_match_being_closed_is_said_out_loud() {
        let cars = vec![car_at("538775dd-bbb", "feat/x", "closed")];
        let e = find_car(&cars, "feat/x", Eligible::Live)
            .unwrap_err()
            .to_string();
        assert!(e.contains("538775dd"), "names the car: {e}");
        assert!(e.contains("closed"), "names its status: {e}");
        assert!(
            e.contains("--recheck"),
            "names what a closed car can do: {e}"
        );
    }

    /// `--recheck` re-runs a probe already recorded on the car, which is
    /// a read — so a closed car IS its candidate, and that is why the
    /// reader pages over closed cars at all.
    #[test]
    fn a_recheck_can_still_name_a_closed_car() {
        let cars = vec![car_at("538775dd-bbb", "feat/x", "closed")];
        assert!(find_car(&cars, "feat/x", Eligible::AnyStatus).is_ok());
    }

    /// A listing is not homogeneous, so one collective word cannot
    /// describe it: two finished matches can be finished DIFFERENTLY.
    /// Each row carries its own status, which is what makes "say which"
    /// answerable without a second query — and is why the refusal no
    /// longer prefixes the count with a single "open" / "closed".
    #[test]
    fn a_refusal_labels_each_candidates_status() {
        let cars = vec![
            car_at("ef4eff3c-aaa", "feat/x", "cancelled"),
            car_at("538775dd-bbb", "feat/x", "closed"),
        ];
        let e = find_car(&cars, "feat/x", Eligible::Live)
            .unwrap_err()
            .to_string();
        assert!(
            !e.contains("cars match"),
            "nothing is a candidate, so nothing is ambiguous: {e}"
        );
        assert!(e.contains("ef4eff3c  cancelled"), "{e}");
        assert!(e.contains("538775dd  closed"), "{e}");
    }

    /// A row whose status is absent is NOT assumed finished. Dropping a
    /// candidate the code cannot see the state of would be guessing in
    /// the direction that loses cars.
    #[test]
    fn a_car_with_no_status_stays_a_candidate() {
        let cars = vec![car("abcdef12-3456", "feat/x", "ready")];
        assert!(find_car(&cars, "feat/x", Eligible::Live).is_ok());
    }

    /// The live match is PREFERRED for a recheck too — the behaviour
    /// that landed before this change and must not regress. A recheck
    /// admits a finished car, but when the name also matches a live one
    /// that is the one meant, so the abandoned twin does not turn a
    /// resolvable recheck into an ambiguity.
    #[test]
    fn a_recheck_prefers_the_live_car_over_an_abandoned_twin() {
        let mut abandoned = car("22222222-bbb", "feat/x", "ready");
        abandoned["status"] = json!("closed");
        abandoned["metadata"]["skip_reason"] =
            json!("gate receipt is for fb973bd0 but the branch boards 0ec4521f");
        let cars = vec![car("11111111-aaa", "feat/x", "ready"), abandoned];
        let found = find_car(&cars, "feat/x", Eligible::AnyStatus)
            .expect("the one live car is the candidate");
        assert_eq!(
            found.get("id").and_then(Value::as_str),
            Some("11111111-aaa")
        );
    }

    /// And the refusal is not regressed: two cars that are genuinely
    /// open still refuse, and still name both so "say which" is
    /// answerable.
    #[test]
    fn two_genuinely_open_cars_still_refuse_and_list_both() {
        let cars = vec![
            car_at("11111111-aaa", "feat/x", "open"),
            car_at("22222222-bbb", "feat/x", "open"),
        ];
        let e = find_car(&cars, "feat/x", Eligible::Live)
            .unwrap_err()
            .to_string();
        assert!(e.contains("2 cars match"), "{e}");
        assert!(e.contains("11111111  open"), "{e}");
        assert!(e.contains("22222222  open"), "{e}");
    }

    /// And when the ambiguity is among finished cars only — which only a
    /// recheck can reach — no row is called open.
    #[test]
    fn an_ambiguity_among_closed_cars_is_not_called_open() {
        let cars = vec![
            car_at("11111111-aaa", "feat/x", "closed"),
            car_at("22222222-bbb", "feat/x", "closed"),
        ];
        let e = find_car(&cars, "feat/x", Eligible::AnyStatus)
            .unwrap_err()
            .to_string();
        assert!(e.contains("2 cars match"), "{e}");
        assert!(e.contains("11111111  closed"), "{e}");
        assert!(!e.contains("open"), "{e}");
    }

    #[test]
    fn a_car_is_found_by_branch_or_by_id_prefix() {
        let cars = vec![car("abcdef12-3456", "feat/x", "ready")];
        assert!(find_car(&cars, "feat/x", Eligible::Live).is_ok());
        assert!(find_car(&cars, "abcdef12", Eligible::Live).is_ok());
        // Too short to be an id, and not a branch: refused, not guessed.
        assert!(find_car(&cars, "abc", Eligible::Live).is_err());
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

    /// The refusal happens BEFORE the probe runs, and it reads the
    /// step's own declared vocabulary rather than a copy in this file
    /// (9a: it already lives in workflows.toml and the registry
    /// default). 2026-09-02: two proof probes ran green against
    /// production and were thrown away when the API refused a
    /// free-prose --method, because nothing checked it locally.
    #[test]
    fn a_bad_method_is_refused_against_the_steps_own_vocabulary() {
        let step = json!({"fields": [
            {"name": "method", "field_type": "browser|api|log", "required": false}
        ]});
        assert!(check_enum_field(&step, "method", "api").is_ok());
        let e = check_enum_field(&step, "method", "ran it live")
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("browser | api | log"),
            "names the vocabulary: {e}"
        );
        assert!(
            e.contains("BEFORE the probe runs"),
            "says why it refuses early: {e}"
        );
    }

    /// A field the step does not declare, or one that is free text, is
    /// not this verb's business to constrain — it must not invent a
    /// contract the protocol never stated.
    #[test]
    fn an_undeclared_or_free_text_field_is_not_constrained() {
        let free = json!({"fields": [{"name": "note", "field_type": "string"}]});
        assert!(check_enum_field(&free, "note", "anything at all").is_ok());
        assert!(check_enum_field(&json!({}), "method", "whatever").is_ok());
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
        let first = proof_json("old-probe", None, &o, "h", "2026-08-29T00:00:00Z", None);
        let better = proof_json("better-probe", None, &o, "h", "2026-08-30T00:00:00Z", None);
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

    /// THE FALSE NEGATIVE THAT GREW WITH THE PIPELINE'S AGE.
    ///
    /// The car read was one `?kind=ship-a-change&limit=200`. Once more
    /// than 200 closed cars had accumulated, a legitimately open,
    /// unproven car sorting after them fell off that single page, and
    /// `find_car` reported "no open ship-a-change car" for a car that
    /// plainly existed. Paging on `total` must reach it — and must keep
    /// reading closed cars too, because `--recheck` proves a CLOSED one,
    /// which is why the fix is not a `status=open` filter.
    ///
    /// The stub honours `limit`/`offset` and reports the true `total`,
    /// so the reader is exercised across page boundaries with no
    /// `BOSS_JOBS_URL` anywhere in the environment.
    #[tokio::test]
    async fn a_car_past_the_first_page_is_still_reachable() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        // Enough closed cars to overflow more than one page, then one
        // OPEN car dead last — only a full paging read finds it.
        let needle = "fix/needle-in-the-haystack";
        let mut all: Vec<Value> = (0..1100)
            .map(|i| {
                json!({
                    "id": format!("{i:08}-0000-0000-0000-0000000000cc"),
                    "status": "closed",
                    "title": format!("closed car {i}"),
                    "metadata": {"branch": format!("fix/closed-{i}")},
                })
            })
            .collect();
        all.push(json!({
            "id": "ffffffff-0000-0000-0000-0000000000ff",
            "status": "open",
            "title": "the open one",
            "metadata": {"branch": needle},
        }));
        let total = all.len();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let served = all.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    break;
                };
                let mut buf = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    match sock.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&chunk[..n]),
                    }
                    if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&buf).into_owned();
                let target = head.split_whitespace().nth(1).unwrap_or("/").to_string();
                let param = |k: &str| -> Option<usize> {
                    target
                        .split(['?', '&'])
                        .find_map(|p| p.strip_prefix(&format!("{k}=")))
                        .and_then(|v| v.parse().ok())
                };
                let limit = param("limit").unwrap_or(100).max(1);
                let offset = param("offset").unwrap_or(0);
                let page: Vec<Value> = served.iter().skip(offset).take(limit).cloned().collect();
                let body = json!({"data": page, "total": served.len()}).to_string();
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            }
        });

        let base = format!("http://{addr}");
        let http = reqwest::Client::new();
        let cars = all_ship_a_change_cars(&http, &base)
            .await
            .expect("paging read succeeds");
        assert_eq!(
            cars.len(),
            total,
            "every page must be read, not just the first {}",
            cars.len()
        );
        let car = find_car(&cars, needle, Eligible::Live)
            .expect("the open car past page 1 must be found");
        assert_eq!(
            car.get("metadata")
                .and_then(|m| m.get("branch"))
                .and_then(Value::as_str),
            Some(needle)
        );
    }

    // ------------------------------------------------------------------
    // ONE RULE SET, BOTH DOORS (backlog 23b2dffa). `boss gate
    // --park-probe` refuses a probe naming a tool the forge lacks, and
    // one reading the system of record unidentified. This verb — the
    // HAND door onto the same recorded probe — applied neither, and
    // said so only in a comment describing the gate's refusal.
    // ------------------------------------------------------------------

    /// THE RULE THAT HOLDS ON EVERY HOST. An unidentified read of the
    /// system of record is answered with a NARROWER WORLD, silently, so
    /// an absence assertion passes falsely — measured twice in one hour
    /// on 2026-09-10 (61085a9e), once on a probe that would have
    /// recorded a green proof of nothing. That is true wherever the
    /// probe runs, so the hand verb refuses it too, and names the way
    /// out rather than leaving the operator to argue with a wall.
    #[test]
    fn a_probe_that_reads_the_sor_unidentified_is_refused_by_the_hand_verb() {
        for probe in [
            "curl -fsS $BOSS_JOBS_URL/api/jobs?kind=pr-train&status=open | grep -q b9302f90",
            "curl -sf http://boss-jobs-internal.boss.svc.cluster.local:7900/api/yard/status | grep -q trains",
            "wget -qO- http://10.20.0.34:7900/api/stations/loading-dock/queue | grep -q x",
        ] {
            let a = admit(probe, false);
            let r = a.refusal.unwrap_or_else(|| panic!("admitted: {probe}"));
            assert!(r.contains("unidentified"), "{r}");
            assert!(
                r.contains(OVERRIDE_FLAG),
                "the refusal must name its override: {r}"
            );
        }
    }

    /// The same refusal on the path that runs the probe the CAR
    /// recorded: a car's `proof_probe` can be written by hand with a
    /// metadata PATCH, so it never had to meet a gate.
    #[test]
    fn a_car_carried_probe_that_reads_the_sor_unidentified_is_refused_too() {
        let a = admit(
            "curl -fsS $BOSS_JOBS_URL/api/yard/status | grep -q dock_depth",
            true,
        );
        assert!(
            a.refusal.is_some(),
            "a car-carried probe gets the same rule"
        );
    }

    /// AND THE RULE THAT DOES NOT. `host-absent-tools.txt` says what the
    /// FORGE lacks, because a `--park-probe` runs there. This verb runs
    /// the probe HERE, in front of the operator, one second later — and
    /// a pod-local `kubectl` proof is the established shape for a claim
    /// only the cluster can show, so refusing it would block exactly the
    /// case where a human has to prove what the forge cannot.
    #[test]
    fn the_hand_verb_does_not_judge_a_hand_probe_against_the_forges_tools() {
        let a = admit(
            "kubectl -n boss get deploy boss-jobs -o json | grep -q boss-ci",
            false,
        );
        assert!(a.refusal.is_none(), "{:?}", a.refusal);
        assert!(a.warning.is_none(), "{:?}", a.warning);
    }

    /// But a probe that came from the CAR has a second destination — the
    /// arrival rule runs that same text on the forge — so the mismatch
    /// is named. Named, not refused: proving it by hand here is the
    /// right move, and the car's recorded probe is what needs fixing.
    #[test]
    fn a_car_carried_probe_the_forge_cannot_run_is_named_but_not_refused() {
        let a = admit(
            "kubectl -n boss get deploy boss-jobs -o json | grep -q boss-ci",
            true,
        );
        assert!(a.refusal.is_none(), "{:?}", a.refusal);
        let w = a.warning.expect("the forge cannot run this car's probe");
        assert!(w.contains("kubectl"), "{w}");
        assert!(w.contains("forge"), "{w}");
    }

    /// A named read is not this rule's business, and a MENTION is not a
    /// read — the shared command-position scan is what keeps both legal.
    #[test]
    fn named_reads_and_mere_mentions_are_admitted() {
        for probe in [
            "boss-sor-read /api/yard/status | grep -q dock_depth",
            "boss-api GET /api/jobs?kind=pr-train | grep -q arrived",
            "grep -c BOSS_JOBS_URL infra/forge/run-car-probe.sh",
            "test -n \"$BOSS_JOBS_URL\" && echo claim-ok",
        ] {
            let a = admit(probe, true);
            assert!(a.refusal.is_none(), "{probe}: {:?}", a.refusal);
        }
    }

    /// AN OVERRIDE NOBODY CAN SEE IS THE SAME DEFECT AGAIN. The escape
    /// hatch is recorded IN the proof — the one record `--recheck` and
    /// every later reader already open — and absent entirely when it was
    /// not used, so its presence means something.
    #[test]
    fn an_overridden_refusal_is_recorded_in_the_proof() {
        let ov = override_record(
            UNIDENTIFIED_RULE,
            "the identity header comes from my shell profile",
        );
        let p = proof_json(
            "curl $BOSS_JOBS_URL/api/x",
            Some("x"),
            &ok("x"),
            "h",
            "now",
            Some(&ov),
        );
        assert_eq!(p["overridden"]["rule"], UNIDENTIFIED_RULE);
        assert_eq!(
            p["overridden"]["reason"],
            "the identity header comes from my shell profile"
        );
        let plain = proof_json("true", Some("x"), &ok("x"), "h", "now", None);
        assert!(
            plain.get("overridden").is_none(),
            "no override, no key — a reader must not read one to learn nothing"
        );
    }
}
