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

/// WHAT FAILED, NOT WHAT MIGHT HAVE — the verdict half of 4fccc595.
///
/// `judge` says the probe exited nonzero and quotes the streams. That
/// was not enough twice over on car a0ab90a5: the recorded verdict
/// offered two possibilities ("the claim is not holding, or the probe is
/// wrong"), and the evidence under it was an empty string and a
/// rewritten exit code. A reader had to re-derive what the system
/// already held, which is the defect class, not the incident.
///
/// So this names ONE thing when the record supports naming one, and
/// nothing when it does not. Order matters: the self-contradictory shape
/// is checked FIRST because a probe carrying it never had a verdict to
/// give — its nonzero exit says nothing about production either way, and
/// the measured probe carried both findings at once. A crashed numeric
/// test comes next, for the same reason and one more: its exit code is
/// whatever the `||` branch behind it chose — 75 on both measured cars
/// — so the code is not just uninformative, it points the wrong way.
pub(crate) fn failure_diagnosis(probe: &str, o: &Outcome) -> Option<String> {
    if o.exit == 0 {
        return None;
    }
    if boss_jobs::probe::asserts_its_own_negation(probe) {
        let rewritten = match boss_jobs::probe::rewrites_its_exit_status(probe) {
            Some(n) => format!(
                "\n  AND THE NUMBER THAT WOULD PROVE IT IS GONE: a bare `|| exit {n}` replaced \
                 jq's own status with {n}, so 4 (produced nothing — the claim HELD) and 5 \
                 (called `error` — the claim FAILED) read identically. Keep it: \
                 `|| {{ echo \"<what failed> (exit $?)\"; exit 1; }}`."
            ),
            None => String::new(),
        };
        return Some(format!(
            "THE PROBE IS SELF-CONTRADICTORY, so this exit code is not a verdict on the \
             claim. It runs `jq -e` over a filter whose success branch is `empty`.\
             \n  {evidence}{rewritten}\n  \
             Nothing here says whether the change is in production. Fix the probe and \
             re-run — and if the car is parked, re-park it so the arrival rule does not \
             run this text again.",
            evidence = boss_jobs::probe::SELF_CONTRADICTORY_EVIDENCE,
        ));
    }
    if let Some(line) = crashed_comparing_a_non_number(&o.stderr) {
        return Some(format!(
            "THE PROBE CRASHED comparing a non-number (jq printed null?), so exit {exit} is \
             not a verdict on the claim — it is whichever `||` branch caught the crash. \
             bash's `[` said:\n  {line}\n  \
             Guard the value before the numeric test — `// empty` in the jq filter, or \
             `case \"$n\" in ''|*[!0-9]*) echo \"not yet: no number\"; exit 75;; esac` — so a \
             missing value says not-yet BY NAME instead of crashing into the not-yet branch. \
             Fix the probe and re-park; a recheck re-runs a crash forever (68081368).",
            exit = o.exit,
        ));
    }
    if o.stdout.trim().is_empty() && o.stderr.trim().is_empty() {
        let rewritten = match boss_jobs::probe::rewrites_its_exit_status(probe) {
            Some(n) => format!(
                " A bare `|| exit {n}` in the probe is the usual cause: it replaces the \
                 failing command's status with {n} and prints nothing, so the one number \
                 that named the cause is gone."
            ),
            None => String::new(),
        };
        return Some(format!(
            "THE FAILURE CANNOT BE READ: the probe exited {exit} and printed NOTHING on \
             either stream, so this is not a verdict on the claim — it is a missing \
             record.{rewritten}\n  \
             Make each outcome speak: print a named token on success, and on failure echo \
             what failed with the status that caused it before exiting. A probe whose \
             failure is silent costs its next reader the whole diagnosis (4fccc595: 18 \
             hours, on a probe that could not pass).",
            exit = o.exit,
        ));
    }
    None
}

/// WHAT BASH SAYS WHEN `[` OR `((` IS HANDED A NON-NUMBER (backlog
/// 68081368). Cars dd1d872d and 2e4d3bce each recorded the not-yet
/// sentence over a stderr of `bash: line 10: [: null: integer expression
/// expected`: jq printed null, `[ "$n" -ge 1 ]` exited 2, and the `||`
/// branch meant for "no such packet yet" exited 75. Nothing was judged.
///
/// The forge runner (infra/forge/run-car-probe.sh) composes its own
/// verdict in shell, on a host with no `boss` binary, so it carries this
/// list as a grep pattern rather than reading it here. Two copies, one
/// equality test: `the_forge_runner_names_the_same_crashes` runs the
/// script's verdict block over every entry, so an entry added on one
/// side and not the other fails by name (CLAUDE.md §9a).
pub(crate) const NUMERIC_CRASH_MARKERS: [&str; 3] = [
    "integer expression expected",
    "unary operator expected",
    "syntax error: invalid arithmetic operator",
];

/// The first stderr line that carries one of [`NUMERIC_CRASH_MARKERS`],
/// trimmed — the line the verdict quotes. `None` when the probe crashed
/// on nothing of the kind, so the other diagnoses get their turn.
pub(crate) fn crashed_comparing_a_non_number(stderr: &str) -> Option<&str> {
    stderr
        .lines()
        .map(str::trim)
        .find(|line| NUMERIC_CRASH_MARKERS.iter().any(|m| line.contains(m)))
}

/// [`judge`], with the diagnosis attached when the record supports one.
/// Every path that runs a probe goes through here rather than `judge`,
/// so a refusal the operator reads and a refusal `--recheck` records
/// cannot say different things.
pub(crate) fn judge_probe(probe: &str, o: &Outcome, expect: Option<&str>) -> Result<()> {
    judge(o, expect).map_err(|e| match failure_diagnosis(probe, o) {
        Some(d) => anyhow::anyhow!("{e}\n\n{d}"),
        None => e,
    })
}

/// THE EXIT CODE THAT MEANS "NOT YET" — the probe's, and now this
/// verb's. 75 is EX_TEMPFAIL, the code a recorded probe exits with
/// when the world is not ready to judge the claim ("not yet: no
/// disk-report request carrying for_sweep yet"). The forge runner
/// (infra/forge/run-car-probe.sh) exits 75 itself in that case so the
/// ops-request carries the verdict; this verb exits the same number
/// for the same reason, so a script wrapping either door tells the
/// three answers apart the same way.
pub(crate) const NOT_YET_EXIT: i32 = 75;

/// THE THREE-WAY READING (backlog 726562de). Two doors run a car's
/// probe, and until 2026-09-14 they read exit 75 in opposite
/// directions. The forge runner learned the not-yet protocol on
/// 2026-09-13 (feat/a-probe-can-say-not-yet): a clean exit 75 is NOT
/// YET — the probe ran, found nothing to judge yet, and said so — so it
/// stamps `proof_attempt{not_yet:true}`, leaves `proven` ready and lets
/// the daily recheck run it again. This door was not taught: `judge`
/// has two rules, and exit 75 broke the first, so `boss prove <car>
/// --from-car` on the SAME run printed "A probe that fails is evidence
/// AGAINST the claim". One record, two verdicts, depending on which
/// door happened to run it — found by the pin that could not make the
/// wording equal because one side had no wording at all.
///
/// So the verdict is an enum with three arms rather than a `Result`
/// with two, and every path that runs a probe reads it here. Not-yet
/// is what a probe SAYS, not merely a number it exits with: it takes
/// exit 75 AND no [`failure_diagnosis`] — the same order the forge's
/// verdict block uses, where a crashed numeric test (68081368) and a
/// silent exit (4fccc595) are checked before the code is read, because
/// in both the 75 is whichever `||` branch caught a probe that never
/// judged anything.
#[derive(Debug)]
pub(crate) enum Verdict {
    /// Exit 0 and the expectation printed — [`judge`]'s two rules.
    Proven,
    /// Exit 75 with something to say and nothing to diagnose. `said` is
    /// the probe's own not-yet line ([`what_it_said`]).
    NotYet { said: String },
    /// Everything else: [`judge_probe`]'s refusal, diagnosis attached.
    NotProven(anyhow::Error),
}

/// Read one run three ways. See [`Verdict`].
pub(crate) fn verdict(probe: &str, o: &Outcome, expect: Option<&str>) -> Verdict {
    if o.exit == NOT_YET_EXIT
        && failure_diagnosis(probe, o).is_none()
        && let Some(said) = what_it_said(o)
    {
        return Verdict::NotYet { said };
    }
    match judge_probe(probe, o, expect) {
        Ok(()) => Verdict::Proven,
        Err(e) => Verdict::NotProven(e),
    }
}

/// WHAT THE PROBE SAID, in one line: the first non-empty line of
/// stderr, else of stdout, trimmed and cut as the forge cuts it (300
/// characters). stderr first because that is where a tool puts its
/// diagnosis — jq's `error(…)`, curl's message, bash's command-not-
/// found. Mirrors run-car-probe.sh's `said`, so the reason clause both
/// doors quote is the same line.
pub(crate) fn what_it_said(o: &Outcome) -> Option<String> {
    o.stderr
        .lines()
        .chain(o.stdout.lines())
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(300).collect())
}

/// THE NOT-YET SENTENCE — `proof_attempt.why` when the probe said not
/// yet, and what the operator reads. Pinned word-for-word against the
/// forge's verdict block by
/// `the_forge_runner_gives_the_same_verdict_for_every_outcome`: the
/// lead word, the reason clause, and the disclaimer that this is not a
/// verdict against the change are load-bearing text a reader of the
/// car acts on, and must not depend on which door ran the probe.
pub(crate) fn not_yet_why(host: &str, said: &str) -> String {
    format!(
        "NOT YET: the probe ran on {host} and said the claim cannot be judged until \
         something happens — {said}. Not a verdict against the change; \
         recheck-failing-probes-daily runs it again."
    )
}

/// THE ATTEMPT RECORD — a run that settled nothing, written on the CAR
/// (`metadata.proof_attempt`, via the merge-PATCH), never on the step,
/// which stays open. The shape is the forge runner's, key for key:
/// `the_hand_door_records_the_forges_attempt_shape` pins the two
/// records equal, because `--recheck`, `boss orient` and the yard's
/// shed read one shape and must not learn which door wrote it.
/// `unrunnable` and `missing_tools` are the forge's fd-9 finding; this
/// door has no such channel, so it records what the forge records when
/// the channel is empty — a probe that RAN. `expect` is `null` under
/// `--exit-only`, as `proof_json` records it.
pub(crate) fn attempt_json(
    probe: &str,
    expect: Option<&str>,
    o: &Outcome,
    host: &str,
    at: &str,
    why: &str,
) -> Value {
    json!({
        "at": at,
        "exit": o.exit,
        "stdout": clip(&o.stdout),
        "stderr": clip(&o.stderr),
        "host": host,
        "probe": probe,
        "expect": expect,
        "why": why,
        "unrunnable": false,
        // The flag is the sentence's: a record whose `not_yet` and `why`
        // disagree cannot be written from here.
        "not_yet": why.starts_with("NOT YET"),
        "missing_tools": [],
    })
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
///
/// AND WHY THE TWO SHAPE CHECKS ONLY WARN (backlog 4fccc595). They are
/// host-independent like the unidentified read, so they are checked on
/// every probe at every door a person is standing at — but they are said,
/// not refused, and the reason is the DIRECTION OF THE LIE. An
/// unidentified read fails OPEN: the probe passes, the absence assertion
/// is green against a world it was never allowed to see, and a car closes
/// on a proof of nothing. A `jq -e` whose success branch is `empty`, and
/// a bare `|| exit n`, fail CLOSED: `judge` records nothing on a nonzero
/// exit, so the worst they can do is strand a car and misdescribe why —
/// which is exactly what they did for 18 hours, and which a line of text
/// in front of the operator fixes. Both detectors are also deliberately
/// coarse text scans (`boss_jobs::probe` says where they are wrong), and
/// a refusal cannot be spent that cheaply: a false refusal blocks a
/// correct probe, while a false warning costs a line someone ignores.
///
/// The SAME two warnings are said by `boss gate --park-probe`
/// (`ParkIntent::probe_warnings`), because that is the door where the
/// measured probe was admitted and the one moment the shape is cheap to
/// fix. Neither is said at the two UNATTENDED doors — the arrival rule
/// and the forge runner — because nothing there reads a warning, and a
/// check nobody reads is a check that is not running (CLAUDE.md
/// §Diagnosis). What those two got instead is a VERDICT that names one
/// thing: [`failure_diagnosis`] here, and run-car-probe.sh's `why`.
pub(crate) const OVERRIDE_FLAG: &str = "--probe-anyway";

pub(crate) use boss_jobs::probe::{UNIDENTIFIED_RULE, override_record};

/// What this door makes of a probe: a reason not to run it, things the
/// operator should know, or neither.
#[derive(Debug, Default)]
pub(crate) struct Admission {
    /// Why the probe must not run, unless the operator overrides it.
    pub refusal: Option<String>,
    /// Things worth saying that do not stop the probe. A list because
    /// they are independent findings and a probe can trip more than one
    /// — the measured 18-hour probe (4fccc595) tripped two, and showing
    /// the operator only the first would have hidden the other.
    pub warnings: Vec<String>,
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
             exists, `{reader} /api/...` on the forge, a curl that sends an identity \
             header, or a gateway session (POST /api/auth/guest, then curl -b). The DOOR \
             has to do the read; `jq` or `python3` fed its stdout is a parser and is not \
             refused (5dc5159d).\n\n\
             If this read IS identified in a way a text check cannot see, say so and it \
             runs: {OVERRIDE_FLAG} '<reason>'. The reason is recorded in the proof, because \
             `--recheck` re-runs this probe later when nobody is watching.",
            evidence = boss_jobs::probe::UNIDENTIFIED_READ_EVIDENCE,
            reader = boss_jobs::probe::SOR_READER,
        )
    });
    let mut warnings: Vec<String> = Vec::new();
    if from_car && let Some(tool) = boss_jobs::probe::needs_absent_tool(probe) {
        warnings.push(format!(
            "boss prove: NOTE — this car's recorded probe invokes `{tool}`, which the \
             forge host does not have (infra/forge/host-absent-tools.txt). It runs HERE, \
             so proving by hand is fine and is the point; but the arrival rule could not \
             have run it, and any later re-run on the forge will report `unrunnable` \
             rather than a verdict (f9304366). Re-park the car with a probe the forge \
             can run, or record it as --park-proof-event."
        ));
    }
    warnings.extend(shape_warnings(probe).map(|w| format!("boss prove: {w}")));
    Admission { refusal, warnings }
}

/// THE THREE SHAPE WARNINGS, in the wording every door can use (the
/// prefix is the door's). All read the probe TEXT, like the two rules
/// above them, and all are host-independent — so they are said at every
/// door a human is standing at, `--from-car` or not. The third
/// (0df3af1c) joined the first two for the same reason they exist: `[`
/// exits 2 on a non-integer, so the shape fails CLOSED, and the one
/// place its stderr has a reader is the terminal it is typed at.
pub(crate) fn shape_warnings(probe: &str) -> impl Iterator<Item = String> {
    let inverted = boss_jobs::probe::asserts_its_own_negation(probe).then(|| {
        format!(
            "THIS PROBE MAY ASSERT ITS OWN NEGATION — it runs `jq -e` over a filter whose \
             success branch is `empty`. `jq -e` exits 4 when the filter produces no output, \
             and `empty` is no output, so the claim HOLDING is what makes jq exit nonzero.\
             \n  {evidence}\n  \
             This is a warning, not a refusal: the shape fails CLOSED, so it can strand a \
             car but never record a proof of nothing. Check it if the filter is doing \
             something else.",
            evidence = boss_jobs::probe::SELF_CONTRADICTORY_EVIDENCE,
        )
    });
    let rewritten = boss_jobs::probe::rewrites_its_exit_status(probe).map(|n| {
        format!(
            "THIS PROBE DISCARDS THE STATUS THAT WOULD EXPLAIN ITS FAILURE — a bare \
             `|| exit {n}` replaces the failing command's exit code with {n} and prints \
             nothing, so jq's 4 (the filter produced nothing) and 5 (the filter called \
             `error`), or curl's 7 (could not connect) and 22 (the server said no), all \
             arrive as {n}. That is the reduction CLAUDE.md §Diagnosis names: it suppresses \
             OUTPUT, not work, and it is paid for by whoever is next in front of the \
             failure.\n  \
             Keep the evidence first: `|| {{ echo \"<what failed> (exit $?)\"; exit 1; }}`."
        )
    });
    let unguarded = boss_jobs::probe::compares_an_unguarded_number(probe).map(|var| {
        format!(
            "THIS PROBE COMPARES `${var}` AS A NUMBER WITHOUT CHECKING THAT IT IS ONE. When \
             the query matches nothing, `jq -r` prints the literal `null` — four characters, \
             not an empty string, so a `${{{var}:-9999}}` default does not fill it — and `[` \
             answers `integer expression expected` on stderr, exits 2, and takes the else \
             branch. Measured 2026-09-14 (0df3af1c): three cars sat UNPROVEN on exactly that, \
             each behind a stderr nobody reads, until an operator rewrote the probe by hand.\
             \n  \
             Guard on either side of the pipe, so a missing number says NOT YET instead:\n    \
             in jq:        `… | first | .field // empty`   (or `first | select(. != null)`)\n    \
             in the shell: `case \"${var}\" in ''|*[!0-9]*) echo 'not yet: <what has not happened>'; exit 75;; esac`\n  \
             This is a warning, not a refusal: `[` fails closed on the string, so the shape \
             strands a car but never records a proof of nothing."
        )
    });
    inverted.into_iter().chain(rewritten).chain(unguarded)
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
        // --replace WITH NOTHING TO REPLACE (backlog 251dba77, measured
        // 2026-09-11). The flag's write path appends to `reproof` in JOB
        // metadata and never touches the step, because a COMPLETED step
        // is frozen — so given an open step it recorded a second-class
        // copy of a proof nobody had yet, printed "re-proven — recorded
        // as reproof #1. The original proof is untouched on the step"
        // (two assertions, both false), and left the step `ready`. The
        // car did not advance and nothing said so.
        Some(open @ ("ready" | "active")) if replace && recorded_proof_on(step).is_none() => bail!(
            "--replace has nothing to replace: the `{PROVEN}` step is {open} and carries no \
             recorded proof, so there is no first proof to keep beside a second one.\n\n\
             Run the same command WITHOUT --replace. That is the path that records a FIRST \
             proof: it writes the proof on the step and completes it, which is what makes \
             the car advance.\n\n\
             --replace is for a step already COMPLETED, whose proof is frozen and must not \
             be erased — a proof that used to hold and no longer does is evidence, not a \
             mistake (2b30eff4)."
        ),
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

/// The proof already recorded on this step, if there is one.
///
/// The step's own `proof` field — required by the workflow when the step
/// completes, so a completed step always has one — read here to answer
/// the only question `--replace` needs: is there anything to replace?
pub(crate) fn recorded_proof_on(step: &Value) -> Option<&str> {
    step.get("metadata")
        .and_then(|m| m.get("proof"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
}

/// Which record a re-runnable probe was read from. `--recheck` reports
/// differently on each: a PROOF that fails now has decayed ("NO LONGER
/// HOLDS"), while an ATTEMPT was never a proof, so the same failure is
/// "still not proven" and the same success is "now provable" — never
/// "still HOLDS" about a claim nobody ever proved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// The step's `proof`, or the newest `reproof` entry.
    Proof,
    /// The car's `proof_attempt`: a run that settled nothing, with
    /// `not_yet` as the writer stamped it.
    Attempt { not_yet: bool },
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
    pub source: Source,
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
    if recorded_proof_on(step).is_some() {
        return recorded_probe(step);
    }
    // A NOT-YET CAR HAS NO PROOF TO RE-RUN, AND ONE PROBE THAT RAN
    // (726562de). Its `proven` step is still ready, so the step reader
    // below would refuse with "nothing to re-run" — but "not yet" is a
    // request to run it again, and the car holds exactly what ran in
    // `proof_attempt` (the forge's record or this door's, one shape).
    // Read in preference to nothing, never in preference to a proof.
    if let Some(attempt) = car.get("metadata").and_then(|m| m.get("proof_attempt"))
        && attempt.get("probe").and_then(Value::as_str).is_some()
    {
        let mut rec = read_proof(attempt)?;
        rec.source = Source::Attempt {
            not_yet: attempt.get("not_yet").and_then(Value::as_bool) == Some(true),
        };
        return Ok(rec);
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
        source: Source::Proof,
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
        if let Source::Attempt { not_yet } = rec.source {
            println!(
                "boss prove: {short} is not proven — re-running the probe of its last \
                 attempt, which said {}",
                if not_yet { "NOT YET" } else { "NOT PROVEN" }
            );
        }
        println!("boss prove: re-running the recorded probe for {short}\n  $ {probe}");
        let o = match rec.cwd.as_deref() {
            Some(dir) if !dir.is_empty() => execute_in(&probe, Some(Path::new(dir)))?,
            _ => execute(&probe)?,
        };
        // THREE READINGS, and which record they are read against
        // decides the sentence: a PROOF that fails now has decayed; an
        // ATTEMPT was never a proof. --recheck writes nothing on any of
        // them — `proven` stays exactly as it was (ready, for a not-yet
        // car), and a not-yet exits 75 so a script can tell.
        return match (verdict(&probe, &o, expect.as_deref()), rec.source) {
            (Verdict::Proven, Source::Proof) => {
                println!("boss prove: HOLDS — {short} is still true in production");
                Ok(())
            }
            (Verdict::Proven, Source::Attempt { .. }) => {
                println!(
                    "boss prove: NOW PROVABLE — {short}'s last attempt did not settle the \
                     claim and the same probe passes now. Nothing is recorded by --recheck; \
                     run boss prove {short} --from-car (or --probe/--expect) to record it \
                     and complete `{PROVEN}`."
                );
                Ok(())
            }
            (Verdict::NotYet { said }, _) => {
                println!("boss prove: {}", not_yet_why(&here, &said));
                println!("boss prove: --recheck records nothing; `{PROVEN}` stays as it is");
                std::process::exit(NOT_YET_EXIT)
            }
            (Verdict::NotProven(e), Source::Proof) => bail!(
                "NO LONGER HOLDS — {short} was proven once and is not true now.\n\n{e}\n\n\
                 A proof can decay honestly: a step-plugin ConfigMap preview survives \
                 exactly until the next converge, and a car that never landed leaves \
                 prod looking fixed until it isn't."
            ),
            (Verdict::NotProven(e), Source::Attempt { .. }) => bail!(
                "STILL NOT PROVEN — {short}'s recorded probe does not pass now either.\n\n{e}"
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
    for w in &admission.warnings {
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
    let at = now.to_rfc3339();
    match verdict(&probe, &o, expect.as_deref()) {
        Verdict::Proven => {}
        Verdict::NotProven(e) => return Err(e),
        // NOT YET (726562de): the probe ran and said the claim cannot be
        // judged until something happens. Not a verdict against the
        // change, so nothing is refused — and not a proof, so nothing
        // completes. What lands is the forge's record, from this door:
        // `proof_attempt{not_yet:true}` on the car, `proven` untouched,
        // and the daily recheck (recheck-failing-probes-daily picks up
        // any car carrying an attempt) runs it again. Exit 75, as the
        // probe did and as the forge does.
        Verdict::NotYet { said } => {
            let here = host();
            let why = not_yet_why(&here, &said);
            println!("boss prove: {why}");
            let step_status = target.get("status").and_then(Value::as_str).unwrap_or("?");
            if dry {
                println!(
                    "boss prove: DRY — would record this as proof_attempt on {short}; \
                     `{PROVEN}` stays {step_status}"
                );
                std::process::exit(NOT_YET_EXIT);
            }
            let attempt = attempt_json(&probe, expect.as_deref(), &o, &here, &at, &why);
            crate::gate::api(
                &http,
                reqwest::Method::PATCH,
                &format!("/api/jobs/{car_id}/metadata"),
                Some(json!({"proof_attempt": attempt})),
            )
            .await?;
            println!(
                "boss prove: proof_attempt recorded on {short}; `{PROVEN}` stays {step_status}"
            );
            std::process::exit(NOT_YET_EXIT);
        }
    }

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
        //
        // Read out of the ATTEMPT's own jq record, not the whole file:
        // `stdout:$stdout` appears in the proof record too, so a
        // file-wide `contains` would pass for an attempt that dropped
        // both streams — the pin would be green about the very thing
        // 4fccc595 lost.
        let attempt = SH
            .split_once("attempt=$(jq -cn")
            .expect("run-car-probe.sh builds a proof_attempt record")
            .1
            .split_once("proof_attempt")
            .expect("…and PATCHes it onto the car")
            .0;
        for k in [
            "at",
            "exit",
            "stdout",
            "stderr",
            "why",
            "unrunnable",
            "missing_tools",
        ] {
            assert!(
                attempt.contains(&format!("{k}:${k}")),
                "run-car-probe.sh's proof_attempt lacks `{k}`:\n{attempt}"
            );
        }
        // AND NOT ONE MERGED FIELD. The attempt recorded `output` —
        // stdout and stderr printf'd together — and the case that cost
        // 18 hours read `output: ""`, which cannot tell a reader whether
        // both streams were empty or the record dropped them (4fccc595).
        assert!(
            !attempt.contains("output:$output"),
            "the two streams are recorded separately, as the proof record does:\n{attempt}"
        );
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
        assert!(a.warnings.is_empty(), "{:?}", a.warnings);
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
        let w = a
            .warnings
            .first()
            .expect("the forge cannot run this car's probe");
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

    // -----------------------------------------------------------------
    // THE PROBE THAT COULD NOT PASS (backlog 4fccc595)
    // -----------------------------------------------------------------

    /// jq is a declared required tool of the CI image
    /// (infra/forge/boss-ci/required-tools.txt), and this section is
    /// about jq's own exit codes — so its absence is a failure, never a
    /// skip. A suite that passes by skipping is the check-nobody-reads
    /// defect with extra steps.
    fn require_jq() {
        let ok = std::process::Command::new("sh")
            .arg("-c")
            .arg("command -v jq >/dev/null")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(
            ok,
            "jq is missing and this test would otherwise pass by skipping — \
             jq is declared in infra/forge/boss-ci/required-tools.txt"
        );
    }

    /// The shape off car a0ab90a5, reduced to one claim so the test needs
    /// no network: `if <claim> then empty else error(…) end` under
    /// `jq -e`. The claim HOLDS in this fixture.
    const INVERTED_FILTER: &str = "printf '%s' '{\"category\":\"platform\"}' | \
         jq -e 'if (.category==\"platform\") then empty else error(\"category=\\(.category)\") end'";

    /// THE ANCHOR (4fccc595). Eighteen hours were spent on a probe that
    /// reported failure precisely when its claim held, and the record
    /// said "not holding, or the probe is wrong" over an empty output and
    /// a rewritten exit code. Three things must now be true of that
    /// exact shape: the door NAMES it, the recorded attempt carries jq's
    /// OWN exit status, and the verdict names ONE thing.
    #[test]
    fn the_inverted_jq_e_probe_is_named_and_its_real_exit_is_recorded() {
        require_jq();

        // 1. The door says so, before anything runs — and warns rather
        //    than refuses, because this shape cannot record a false
        //    proof (see `admit`).
        let a = admit(INVERTED_FILTER, false);
        assert!(a.refusal.is_none(), "{:?}", a.refusal);
        let w = a
            .warnings
            .iter()
            .find(|w| w.contains("jq -e"))
            .unwrap_or_else(|| panic!("the inverted shape is not named: {:?}", a.warnings));
        assert!(w.contains("empty"), "{w}");
        assert!(w.contains("4"), "the warning must name jq's exit 4: {w}");

        // 2. THE CLAIM HOLDS AND THE PROBE FAILS, with jq's own 4 —
        //    the number that identifies the defect. Recorded verbatim.
        let o = execute(INVERTED_FILTER).unwrap();
        assert_eq!(
            o.exit, 4,
            "jq -e exits 4 when its filter produces no output; `empty` produces none"
        );
        let p = proof_json(INVERTED_FILTER, Some("claim:ok"), &o, "h", "now", None);
        assert_eq!(p["exit"], 4, "the proof record carries the REAL status");
        assert!(
            p.get("stderr").is_some(),
            "stderr is recorded as its own field"
        );

        // 3. And the verdict names the self-contradiction instead of
        //    offering the reader two possibilities.
        let e = judge_probe(INVERTED_FILTER, &o, Some("claim:ok"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("exited 4"), "{e}");
        assert!(
            e.contains("SELF-CONTRADICTORY"),
            "the verdict must name what failed: {e}"
        );
        assert!(
            e.contains("4fccc595"),
            "and carry the measured evidence: {e}"
        );
    }

    /// THE OTHER HALF OF THE SAME PROBE: jq's stderr, which exists the
    /// moment the claim actually fails. `error(…)` prints to stderr and
    /// exits 5 — a DIFFERENT number from the 4 above, which is the whole
    /// reason a rewritten exit code costs a diagnosis.
    #[test]
    fn jq_names_the_failing_row_on_stderr_and_exits_five() {
        require_jq();
        let failing = "printf '%s' '{\"category\":\"a sentence\"}' | \
             jq -e 'if (.category==\"platform\") then empty else \
             error(\"maintenance-backup: category=\\(.category)\") end'";
        let o = execute(failing).unwrap();
        assert_eq!(o.exit, 5, "jq exits 5 when the filter calls error()");
        assert!(
            o.stderr.contains("maintenance-backup: category=a sentence"),
            "jq's own diagnosis, captured: {:?}",
            o.stderr
        );
        let p = proof_json(failing, Some("claim:ok"), &o, "h", "now", None);
        assert!(
            p["stderr"].as_str().unwrap().contains("maintenance-backup"),
            "and recorded: {p}"
        );
    }

    /// `|| exit 1` IS WHY THE NUMBER WAS GONE. The same filter with the
    /// trailing bare exit records a 1, and 4 and 5 — two different
    /// causes — become the same digit. The diagnosis says so, naming the
    /// reduction rather than leaving a reader to find it.
    #[test]
    fn a_bare_exit_rewrite_is_named_as_the_reason_the_status_is_gone() {
        require_jq();
        let rewritten = format!("{INVERTED_FILTER} || exit 1");
        let o = execute(&rewritten).unwrap();
        assert_eq!(o.exit, 1, "jq's 4 is gone — this is the measured record");
        let d = failure_diagnosis(&rewritten, &o).expect("a failure this shape is diagnosable");
        assert!(d.contains("SELF-CONTRADICTORY"), "{d}");
        assert!(
            d.contains("|| exit 1"),
            "the reduction that erased jq's status must be named: {d}"
        );
        let a = admit(&rewritten, false);
        assert!(
            a.warnings.iter().any(|w| w.contains("|| exit")),
            "the door names it too: {:?}",
            a.warnings
        );
    }

    /// A FAILURE THAT CANNOT BE READ is its own finding. Exit code, both
    /// streams empty, nothing to go on — the verdict must say THAT,
    /// rather than asserting which of several causes it was.
    #[test]
    fn a_failure_with_no_output_at_all_is_named_as_unreadable() {
        let o = Outcome {
            exit: 1,
            stdout: String::new(),
            stderr: String::new(),
        };
        let d = failure_diagnosis("some-command --quiet", &o).expect("silence is diagnosable");
        assert!(
            d.contains("printed NOTHING"),
            "the reader must be told the record is empty, not left to infer it: {d}"
        );
        assert!(
            d.contains("not a verdict on the claim"),
            "an unreadable failure is not evidence against the claim: {d}"
        );
    }

    /// And an honest failure — a probe that ran, disagreed, and SAID so —
    /// gets no invented diagnosis. A verdict that speculates on every
    /// failure is noise, and noise is what gets ignored.
    #[test]
    fn a_probe_that_explains_its_own_failure_is_left_alone() {
        let o = Outcome {
            exit: 1,
            stdout: "CLAIM FAILS for maintenance-backup (jq exit 5)".into(),
            stderr: String::new(),
        };
        assert!(
            failure_diagnosis(
                "boss-sor-read /api/x | grep -q y || { echo x; exit 1; }",
                &o
            )
            .is_none(),
            "the probe already named what failed"
        );
        // Nor on a pass: `judge_probe` adds nothing when there is
        // nothing to explain.
        assert!(judge_probe("true", &ok("claim:ok"), Some("claim:ok")).is_ok());
    }

    // -----------------------------------------------------------------
    // A PROBE THAT CRASHED IS NOT A PROBE THAT SAID NOT-YET (68081368)
    // -----------------------------------------------------------------

    /// THE MEASURED RECORD, twice (cars dd1d872d and 2e4d3bce,
    /// 2026-09-14): a probe read a count with jq, jq printed `null`,
    /// bash's `[` refused to compare it and exited 2, and the `||`
    /// branch behind it said "not yet" and exited 75. The verdict
    /// recorded "said the claim cannot be judged until something
    /// happens" — the not-yet sentence, over stderr that held the crash.
    /// The reader had to open stderr to learn the probe had never
    /// judged anything.
    const CRASHED_STDERR: &str = "bash: line 10: [: null: integer expression expected\n";
    const NULL_COUNT_PROBE: &str = "n=$(boss-sor-read /api/jobs?kind=x | jq -r .total); \
         if [ \"$n\" -ge 1 ]; then echo x:ok; else echo 'not yet: none'; exit 75; fi";

    fn crashed(exit: i32, stderr: &str) -> Outcome {
        Outcome {
            exit,
            stdout: "not yet: none\n".into(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn a_probe_that_crashed_comparing_a_non_number_has_the_crash_named() {
        let o = crashed(75, CRASHED_STDERR);
        let d = failure_diagnosis(NULL_COUNT_PROBE, &o).expect("a crash is diagnosable");
        assert!(
            d.contains("THE PROBE CRASHED"),
            "the crash comes first: {d}"
        );
        assert!(
            d.contains("[: null: integer expression expected"),
            "the stderr line is quoted, not pointed at: {d}"
        );
        assert!(
            d.contains("// empty") && d.contains("*[!0-9]*"),
            "and the guard to add is named: {d}"
        );
        assert!(
            !d.contains("cannot be judged"),
            "a crash is not the not-yet sentence: {d}"
        );
        // The door's refusal carries it too — `judge_probe` is the one
        // path every probe run goes through.
        let e = judge_probe(NULL_COUNT_PROBE, &o, Some("x:ok"))
            .unwrap_err()
            .to_string();
        assert!(e.contains("THE PROBE CRASHED"), "{e}");
    }

    /// Every message bash's `[` and `((` print for a non-number is a
    /// crash, not only the one measured. A fourth one is a line in
    /// `NUMERIC_CRASH_MARKERS`, and this test walks the list so the
    /// list is what gets tested — at every exit code a `||` branch
    /// could turn the crash into.
    #[test]
    fn every_numeric_crash_marker_is_named_whatever_the_exit_code() {
        for marker in NUMERIC_CRASH_MARKERS {
            let stderr = format!("bash: line 3: [: : {marker}\n");
            for exit in [1, 2, 75] {
                let d = failure_diagnosis(NULL_COUNT_PROBE, &crashed(exit, &stderr))
                    .unwrap_or_else(|| panic!("{marker:?} at exit {exit} is a crash"));
                assert!(d.contains("THE PROBE CRASHED"), "{marker}: {d}");
                assert!(d.contains(marker), "the line is quoted: {d}");
            }
        }
    }

    /// A CLEAN NOT-YET IS LEFT ALONE BY THE DIAGNOSER. Exit 75, a "not
    /// yet:" line, an empty stderr — the probe said what it meant, and
    /// `failure_diagnosis` invents nothing for it; `verdict` reads it as
    /// NOT YET precisely BECAUSE there is no diagnosis (726562de), the
    /// same order the forge's verdict block uses. Nor does an ordinary
    /// failure — a curl refusal on stderr — read as a crash.
    #[test]
    fn a_clean_exit_75_gets_no_crash_diagnosis() {
        let o = Outcome {
            exit: 75,
            stdout: "not yet: no disk-report request carrying for_sweep yet\n".into(),
            stderr: String::new(),
        };
        assert!(failure_diagnosis(NULL_COUNT_PROBE, &o).is_none());
        let o = Outcome {
            exit: 1,
            stdout: String::new(),
            stderr: "curl: (7) Failed to connect\n".into(),
        };
        assert!(failure_diagnosis(NULL_COUNT_PROBE, &o).is_none());
    }

    /// A FACT THAT LIVES TWICE GETS AN EQUALITY TEST (CLAUDE.md 9a).
    /// The forge runner composes its verdict in shell, on a host with
    /// no `boss` binary, so the marker list cannot be ONE definition —
    /// the shell's copy is a grep pattern. This lifts the runner's
    /// verdict block (between the same markers boss-testing's
    /// run_car_probe_sh.rs lifts) and runs it over every entry in
    /// `NUMERIC_CRASH_MARKERS`: a marker added here and not there fails
    /// by name, with both verdicts in the message.
    #[test]
    fn the_forge_runner_names_the_same_crashes() {
        for marker in NUMERIC_CRASH_MARKERS {
            let stderr = format!("bash: line 10: [: null: {marker}\n");
            let run = ForgeRun {
                rc: 75,
                stdout: "not yet: none\n",
                stderr: &stderr,
                missing_tools: "",
                expect: "x:ok",
            };
            let (ok, why) = forge_verdict("prove-forge-crash-verdict", &run);
            assert!(!ok, "a crash is not proof: {why}");
            let ours = failure_diagnosis(NULL_COUNT_PROBE, &crashed(75, &stderr)).unwrap();
            for phrase in ["THE PROBE CRASHED", marker, "// empty", "*[!0-9]*"] {
                assert!(
                    why.contains(phrase),
                    "run-car-probe.sh does not name {marker:?} as a crash the way \
                     prove.rs does (missing {phrase:?}):\n  sh: {why}\n  rs: {ours}"
                );
            }
            assert!(
                !why.starts_with("NOT YET"),
                "a crash must not read as not-yet on the forge either: {why}"
            );
        }
    }

    // -----------------------------------------------------------------
    // ONE VERDICT, TWO AUTHORS, PINNED EQUAL (backlog a44e16aa)
    // -----------------------------------------------------------------

    /// What the forge runner has in scope when it composes `why`: the
    /// probe's exit and two streams, the tools fd 9 caught it failing
    /// to find (the runner's `missing_list`, empty when it ran), and the
    /// expectation. The same run, read as `boss prove` reads it, is
    /// [`ForgeRun::outcome`] — so both authors judge ONE record.
    struct ForgeRun<'a> {
        rc: i32,
        stdout: &'a str,
        stderr: &'a str,
        missing_tools: &'a str,
        expect: &'a str,
    }

    impl ForgeRun<'_> {
        fn outcome(&self) -> Outcome {
            Outcome {
                exit: self.rc,
                stdout: self.stdout.into(),
                stderr: self.stderr.into(),
            }
        }
    }

    /// The text of run-car-probe.sh from one marker up to the next, so
    /// a test RUNS the script's shell rather than restating it. The
    /// opening marker rides along: every marker is a whole comment
    /// line, or the head of one.
    fn forge_block(begin: &str, end: &str) -> &'static str {
        const SH: &str = include_str!("../../../../infra/forge/run-car-probe.sh");
        let a = SH
            .find(begin)
            .unwrap_or_else(|| panic!("run-car-probe.sh lost its {begin} marker"));
        let b = SH[a..]
            .find(end)
            .unwrap_or_else(|| panic!("run-car-probe.sh lost its {end} marker"));
        &SH[a..a + b]
    }

    /// The forge runner's judgement of one run: `ok` from its judge
    /// block (the two rules, over its own matcher) and `why` from its
    /// verdict block — the sentence `proof_attempt.why` would carry.
    /// Supplies exactly the variables the script has in scope there.
    fn forge_verdict(case: &str, run: &ForgeRun<'_>) -> (bool, String) {
        let dir = boss_testing::scratch::scratch_dir(case);
        std::fs::write(dir.join("out"), run.stdout).unwrap();
        std::fs::write(dir.join("errs"), run.stderr).unwrap();
        let unrunnable = if run.missing_tools.is_empty() {
            "false"
        } else {
            "true"
        };
        let script = format!(
            "set -uo pipefail\nworkdir={dir}\nrc={rc}\nunrunnable={unrunnable}\n\
             missing_list={missing:?}\nhost='david-asus-minipc'\nPROBE_USER=david\n\
             PROBE_DIR=/home/david/boss\nexpect={expect:?}\n{matcher}\n{judge}\n{verdict}\n\
             printf '%s\\n%s' \"$ok\" \"$why\"\n",
            dir = dir.display(),
            rc = run.rc,
            missing = run.missing_tools,
            expect = run.expect,
            matcher = forge_block("# --- expectation-match", "# --- end expectation-match"),
            judge = forge_block("# PROBE-JUDGE-BEGIN", "# PROBE-JUDGE-END"),
            verdict = forge_block("# PROBE-VERDICT-BEGIN", "# PROBE-VERDICT-END"),
        );
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("bash runs the lifted blocks");
        assert!(
            out.status.success(),
            "the lifted blocks must run: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let printed = String::from_utf8_lossy(&out.stdout).to_string();
        let (ok, why) = printed.split_once('\n').expect("ok, then why");
        (ok == "1", why.to_string())
    }

    /// A probe as the forge's shell runs it: the runner's PROBE-PRELUDE
    /// ahead of the text, fd 9 pointed at a not-found channel. Returns
    /// the outcome and the channel's contents joined the way the runner
    /// joins `missing_list` — the one input `boss prove` never has.
    fn run_as_the_forge_would(case: &str, probe: &str) -> (Outcome, String) {
        let dir = boss_testing::scratch::scratch_dir(case);
        let notfound = dir.join("notfound");
        std::fs::write(&notfound, "").unwrap();
        let prelude = forge_block("# PROBE-PRELUDE-BEGIN", "# PROBE-PRELUDE-END");
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!("{prelude}\n{probe}"))
            .env("BOSS_PROBE_NOTFOUND", &notfound)
            .output()
            .expect("bash runs the probe");
        let caught = std::fs::read_to_string(&notfound).unwrap();
        let mut missing: Vec<&str> = caught.lines().filter(|l| !l.is_empty()).collect();
        missing.sort_unstable();
        missing.dedup();
        (
            Outcome {
                exit: out.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            },
            missing.join(", "),
        )
    }

    /// The ALL-CAPS verdict words this door can lead with — the three
    /// `failure_diagnosis` names, and NOT YET, which `verdict` gives a
    /// clean exit 75 (726562de). A run this door reads one way must not
    /// be led with another of these by the shell — a verdict one door
    /// invents and the other does not is the two-sentence defect in a
    /// new coat, and NOT YET against NOT PROVEN was the measured one.
    const VERDICT_WORDS: [&str; 4] = [
        "THE PROBE CRASHED",
        "THE FAILURE CANNOT BE READ",
        "THE PROBE IS SELF-CONTRADICTORY",
        "NOT YET",
    ];

    /// One outcome, judged by both authors, and the phrases the two
    /// verdicts must share: the ALL-CAPS word when there is one, and the
    /// reason clause a reader acts on.
    struct SameVerdict<'a> {
        case: &'a str,
        probe: &'a str,
        run: ForgeRun<'a>,
        /// The verdict word this door leads with — `failure_diagnosis`'s
        /// when it diagnoses, NOT YET when `verdict` says so — or `None`
        /// when it (rightly) adds nothing to `judge`'s refusal; then the
        /// shell adds nothing of the kind either.
        diagnosis: Option<&'a str>,
        /// Load-bearing text both verdicts carry, verbatim.
        agree: &'a [&'a str],
    }

    /// A PROBE VERDICT HAS TWO AUTHORS (backlog a44e16aa): the forge
    /// runner's PROBE-VERDICT shell, on a host with no `boss` binary,
    /// and `judge_probe` here. `the_forge_runner_names_the_same_crashes`
    /// pinned the newest branch equal and left the older ones living
    /// twice — CANNOT BE READ, not-yet, plain red, the wrong string —
    /// where a wording change on one side silently diverged the other,
    /// and the reader of a car saw different sentences for one verdict
    /// depending on which door ran the probe.
    ///
    /// So every branch is run through both, on ONE record each, and the
    /// load-bearing phrases are asserted shared. The Rust wording is the
    /// reference (it has the unit tests); where the shell disagreed it
    /// was changed to match. One branch is deliberately NOT equalised
    /// and is pinned as such instead: unrunnable, because its input (fd
    /// 9's not-found channel) exists only on the forge — this door sees
    /// bash's `command not found` on stderr and nothing more, so both
    /// name the tool and neither diagnoses.
    ///
    /// NOT-YET WAS THE OTHER EXCEPTION, and the pin documented it as an
    /// asymmetry it could not resolve as wording: this door had no
    /// branch for exit 75 at all, so the forge said NOT YET and the hand
    /// door said "evidence AGAINST" about one run (726562de). Now both
    /// have the branch, and it is pinned like the rest — `verdict` reads
    /// the run as NOT YET, and `not_yet_why` is the sentence the forge's
    /// verdict block composes, lead word and reason clause alike.
    #[test]
    fn the_forge_runner_gives_the_same_verdict_for_every_outcome() {
        let (missing_tool, missing_list) = run_as_the_forge_would(
            "prove-forge-verdict-missing-tool",
            "kubectl-no-such-tool get pods -A && echo pods:ok",
        );
        assert_eq!(missing_list, "kubectl-no-such-tool", "fd 9 caught the tool");
        let cases = [
            SameVerdict {
                case: "not-yet",
                probe: "n=$(boss-sor-read /api/x | jq -r '.total // empty'); case \"$n\" in \
                        ''|*[!0-9]*) echo 'not yet: no disk-report request carrying for_sweep \
                        yet'; exit 75;; esac; echo sweep-measured:ok",
                run: ForgeRun {
                    rc: 75,
                    stdout: "not yet: no disk-report request carrying for_sweep yet\n",
                    stderr: "",
                    missing_tools: "",
                    expect: "sweep-measured:ok",
                },
                diagnosis: Some("NOT YET"),
                agree: &[
                    "NOT YET",
                    "cannot be judged until something happens",
                    "not yet: no disk-report request carrying for_sweep yet",
                    "Not a verdict against the change",
                    "recheck-failing-probes-daily runs it again",
                ],
            },
            SameVerdict {
                case: "unrunnable",
                probe: "kubectl-no-such-tool get pods -A && echo pods:ok",
                run: ForgeRun {
                    rc: missing_tool.exit,
                    stdout: &missing_tool.stdout,
                    stderr: &missing_tool.stderr,
                    missing_tools: &missing_list,
                    expect: "pods:ok",
                },
                diagnosis: None,
                agree: &["kubectl-no-such-tool", "not found"],
            },
            SameVerdict {
                case: "cannot-be-read",
                probe: "boss-sor-read /api/x | jq -e '.total == 0' >/dev/null || exit 1",
                run: ForgeRun {
                    rc: 1,
                    stdout: "",
                    stderr: "",
                    missing_tools: "",
                    expect: "x:ok",
                },
                diagnosis: Some("THE FAILURE CANNOT BE READ"),
                agree: &[
                    "THE FAILURE CANNOT BE READ",
                    "exited 1",
                    "printed NOTHING on either stream",
                    "not a verdict on the claim",
                    "missing record",
                    "bare",
                    "|| exit",
                ],
            },
            SameVerdict {
                case: "plain-red",
                probe: "boss-sor-read /api/x | jq -e '.total == 0' >/dev/null || { echo \
                        \"CLAIM FAILS for maintenance-backup (jq exit $?)\"; exit 1; }",
                run: ForgeRun {
                    rc: 1,
                    stdout: "CLAIM FAILS for maintenance-backup (jq exit 5)\n",
                    stderr: "",
                    missing_tools: "",
                    expect: "x:ok",
                },
                diagnosis: None,
                agree: &[
                    "exited 1",
                    "not proof of anything",
                    "CLAIM FAILS for maintenance-backup (jq exit 5)",
                ],
            },
            SameVerdict {
                case: "wrong-string",
                probe: "echo dock_depth=$(boss-sor-read /api/yard/status | jq .dock_depth)",
                run: ForgeRun {
                    rc: 0,
                    stdout: "dock_depth=0\n",
                    stderr: "",
                    missing_tools: "",
                    expect: "dock_depth=1",
                },
                diagnosis: None,
                agree: &[
                    "exited 0",
                    "never printed",
                    "dock_depth=1",
                    "dock_depth=0",
                    "weak assertion",
                    "echo hi",
                    "exits 0 too",
                ],
            },
            SameVerdict {
                case: "zero-and-silent",
                probe: "boss-sor-read /api/yard/status | jq -e '.dock_depth == 1' >/dev/null",
                run: ForgeRun {
                    rc: 0,
                    stdout: "",
                    stderr: "",
                    missing_tools: "",
                    expect: "dock:ok",
                },
                diagnosis: None,
                agree: &[
                    "exited 0",
                    "never printed",
                    "dock:ok",
                    "weak assertion",
                    "echo hi",
                    "exits 0 too",
                ],
            },
        ];
        for c in &cases {
            let o = c.run.outcome();
            let (ok, why) = forge_verdict(&format!("prove-forge-verdict-{}", c.case), &c.run);
            assert!(
                !ok,
                "{}: the forge must refuse what this door refuses",
                c.case
            );
            // ONE record, read by this door's three-way `verdict`: what
            // the operator reads (`rs`) and the word it leads with (`d`),
            // the same two things the shell's `why` carries.
            let (rs, d) = match verdict(c.probe, &o, Some(c.run.expect)) {
                Verdict::Proven => panic!("{}: this door must not prove this", c.case),
                Verdict::NotYet { said } => {
                    assert_eq!(c.diagnosis, Some("NOT YET"), "{}: read as not-yet", c.case);
                    let w = not_yet_why("david-asus-minipc", &said);
                    (w.clone(), Some(w))
                }
                Verdict::NotProven(e) => {
                    assert_ne!(
                        c.diagnosis,
                        Some("NOT YET"),
                        "{}: read as NOT PROVEN",
                        c.case
                    );
                    (e.to_string(), failure_diagnosis(c.probe, &o))
                }
            };
            match c.diagnosis {
                Some(word) => {
                    let d = d.unwrap_or_else(|| panic!("{}: prove.rs diagnoses this", c.case));
                    assert!(
                        d.starts_with(word),
                        "{}: rs leads with {word:?}: {d}",
                        c.case
                    );
                    assert!(
                        why.starts_with(word),
                        "{}: run-car-probe.sh does not lead with {word:?} the way prove.rs \
                         does:\n  sh: {why}\n  rs: {d}",
                        c.case
                    );
                }
                None => {
                    assert!(d.is_none(), "{}: prove.rs adds no diagnosis: {d:?}", c.case);
                    for word in VERDICT_WORDS {
                        assert!(
                            !why.contains(word),
                            "{}: run-car-probe.sh diagnoses {word:?} where prove.rs declines \
                             to:\n  sh: {why}\n  rs: {rs}",
                            c.case
                        );
                    }
                }
            }
            for phrase in c.agree {
                assert!(
                    rs.contains(phrase),
                    "{}: prove.rs lost {phrase:?}:\n  rs: {rs}",
                    c.case
                );
                assert!(
                    why.contains(phrase),
                    "{}: run-car-probe.sh does not say {phrase:?} the way prove.rs does:\n  \
                     sh: {why}\n  rs: {rs}",
                    c.case
                );
            }
        }

        // AND GREEN: exit 0 with the token printed is proof at both
        // doors, so the verdict block is never composed — which is the
        // one outcome where "the same sentence" means no sentence.
        let green = ForgeRun {
            rc: 0,
            stdout: "measured 3 sweeps\nsweep-measured:ok\n",
            stderr: "",
            missing_tools: "",
            expect: "sweep-measured:ok",
        };
        let (ok, why) = forge_verdict("prove-forge-verdict-green", &green);
        assert!(ok, "the forge proves what this door proves: {why}");
        assert!(judge_probe("true", &green.outcome(), Some(green.expect)).is_ok());
        assert!(matches!(
            verdict("true", &green.outcome(), Some(green.expect)),
            Verdict::Proven
        ));
    }

    // -----------------------------------------------------------------
    // --replace WITH NOTHING TO REPLACE (backlog 251dba77)
    // -----------------------------------------------------------------

    /// THE MEASURED DEFECT. `boss prove <car> --replace` on a car whose
    /// `proven` step had never been proven printed "re-proven — recorded
    /// as reproof #1. The original proof is untouched on the step" —
    /// two assertions, both false — and left the step `ready`, so the
    /// car never advanced and nothing said so.
    #[test]
    fn replace_refuses_when_there_is_no_proof_to_replace() {
        let car = json!({"steps": [{
            "id": "s1", "title": PROVEN, "status": "ready",
            "metadata": {"authority_role": "platform-admin", "procedure": "boss prove"}
        }]});
        let e = proven_step(&car, true).unwrap_err().to_string();
        assert!(
            e.contains("no recorded proof"),
            "it must say what is missing: {e}"
        );
        assert!(e.contains("ready"), "and that the step has not moved: {e}");
        assert!(
            e.contains("WITHOUT --replace"),
            "and name the plain form, which is what records a FIRST proof: {e}"
        );
        // The plain form is unaffected: this is the path that records it.
        assert!(proven_step(&car, false).is_ok());
    }

    /// AND THE FLAG STILL DOES WHAT IT WAS BUILT FOR (2b30eff4): a
    /// completed step's proof is frozen, and `--replace` is how a better
    /// probe lands beside it without erasing what used to hold.
    #[test]
    fn replace_still_works_on_a_step_whose_proof_is_frozen() {
        let proven = json!({"steps": [{
            "id": "s1", "title": PROVEN, "status": "completed",
            "metadata": {"verified": "it held", "proof": "{\"exit\":0}"}
        }]});
        assert!(
            proven_step(&proven, true).is_ok(),
            "a completed step is exactly what --replace is for"
        );
        assert!(
            proven_step(&proven, false)
                .unwrap_err()
                .to_string()
                .contains("--replace"),
            "and the plain form still points at it"
        );
    }

    /// A step that is OPEN but already carries a proof is the one
    /// non-obvious case: the plain form would overwrite the recorded
    /// proof, which is the erasure 2b30eff4 exists to prevent — so
    /// `--replace` is accepted there and appends instead.
    #[test]
    fn replace_is_accepted_on_an_open_step_that_does_carry_a_proof() {
        let car = json!({"steps": [{
            "id": "s1", "title": PROVEN, "status": "ready",
            "metadata": {"proof": "{\"exit\":0,\"probe\":\"true\"}"}
        }]});
        assert!(proven_step(&car, true).is_ok());
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

    // -----------------------------------------------------------------
    // THE HAND DOOR CAN SAY NOT YET (backlog 726562de)
    // -----------------------------------------------------------------

    /// THE MEASURED ASYMMETRY. The forge runner records a clean exit 75
    /// as NOT YET — the probe said the claim cannot be judged until
    /// something happens — leaves `proven` ready and re-runs it daily.
    /// `boss prove <car> --from-car` on the SAME run read "A probe that
    /// fails is evidence AGAINST the claim": the opposite verdict for
    /// one record, depending on which door ran it. Exit 75 is a third
    /// answer, and this door now gives it.
    #[test]
    fn an_exit_75_with_a_not_yet_line_is_not_yet_not_evidence_against() {
        let o = Outcome {
            exit: NOT_YET_EXIT,
            stdout: "not yet: no disk-report request carrying for_sweep yet\n".into(),
            stderr: String::new(),
        };
        let Verdict::NotYet { said } = verdict(NULL_COUNT_PROBE, &o, Some("sweep-measured:ok"))
        else {
            panic!("a clean exit 75 is NOT YET, not a refusal");
        };
        assert_eq!(
            said,
            "not yet: no disk-report request carrying for_sweep yet"
        );
        let why = not_yet_why("pod", &said);
        assert!(why.starts_with("NOT YET"), "{why}");
        assert!(
            why.contains(&said),
            "the probe's own line is the reason: {why}"
        );
        assert!(
            !why.contains("evidence AGAINST") && !why.contains("not proof of anything"),
            "not-yet is not a verdict against the change: {why}"
        );
        assert_eq!(
            NOT_YET_EXIT, 75,
            "the verb exits as the probe did, so scripts can tell"
        );
    }

    /// A CRASH THAT EXITED 75 IS THE CRASH (68081368), at this door as
    /// at the forge: the code is whichever `||` branch caught it, and
    /// re-running a crash daily judges nothing forever.
    #[test]
    fn an_exit_75_whose_stderr_shows_a_crash_is_the_crash_not_not_yet() {
        let o = crashed(75, CRASHED_STDERR);
        let Verdict::NotProven(e) = verdict(NULL_COUNT_PROBE, &o, Some("x:ok")) else {
            panic!("a crashed numeric test is not not-yet");
        };
        let e = e.to_string();
        assert!(e.contains("THE PROBE CRASHED"), "{e}");
        assert!(!e.contains("cannot be judged"), "{e}");
    }

    /// AND AN EXIT 75 THAT SAID NOTHING CANNOT BE READ — the forge's
    /// `why` checks the silent nonzero exit before it reads the code,
    /// and this door keeps the same order: not-yet is what a probe SAYS,
    /// not a number it exits with.
    #[test]
    fn an_exit_75_that_said_nothing_cannot_be_read() {
        let o = Outcome {
            exit: 75,
            stdout: String::new(),
            stderr: String::new(),
        };
        let Verdict::NotProven(e) = verdict("some-command --quiet", &o, Some("x:ok")) else {
            panic!("silence is a missing record, not a not-yet");
        };
        assert!(e.to_string().contains("THE FAILURE CANNOT BE READ"), "{e}");
    }

    /// The other two readings are unchanged by the third.
    #[test]
    fn the_other_two_readings_are_unchanged() {
        assert!(matches!(
            verdict("true", &ok("x:ok"), Some("x:ok")),
            Verdict::Proven
        ));
        let red = Outcome {
            exit: 1,
            stdout: "CLAIM FAILS (jq exit 5)\n".into(),
            stderr: String::new(),
        };
        let Verdict::NotProven(e) = verdict("true", &red, Some("x:ok")) else {
            panic!("exit 1 is still a refusal");
        };
        assert!(e.to_string().contains("evidence AGAINST"), "{e}");
    }

    /// WHAT THE PROBE SAID, read as the forge reads it: the first
    /// non-empty line of stderr, else of stdout — stderr first because
    /// that is where a tool puts its diagnosis.
    #[test]
    fn what_it_said_reads_stderr_first_then_stdout() {
        let o = Outcome {
            exit: 75,
            stdout: "\n  not yet: from stdout\nmore\n".into(),
            stderr: String::new(),
        };
        assert_eq!(what_it_said(&o).as_deref(), Some("not yet: from stdout"));
        let o = Outcome {
            exit: 75,
            stdout: "not yet: from stdout\n".into(),
            stderr: "\ncurl: (7) refused\n".into(),
        };
        assert_eq!(what_it_said(&o).as_deref(), Some("curl: (7) refused"));
        let o = Outcome {
            exit: 75,
            stdout: "  \n".into(),
            stderr: String::new(),
        };
        assert_eq!(what_it_said(&o), None);
    }

    /// THE ATTEMPT RECORD LIVES TWICE — the forge's jq record and this
    /// door's `attempt_json` — and `--recheck`, orient and the yard read
    /// one shape. Pinned both ways (CLAUDE.md 9a): every key this door
    /// writes is in the script's record, and every key the script
    /// records is written here. Values the forge alone can know (fd 9's
    /// missing tools) are recorded as the forge records "none".
    #[test]
    fn the_hand_door_records_the_forges_attempt_shape() {
        const SH: &str = include_str!("../../../../infra/forge/run-car-probe.sh");
        let sh_attempt = SH
            .split_once("attempt=$(jq -cn")
            .expect("run-car-probe.sh builds a proof_attempt record")
            .1
            .split_once("proof_attempt")
            .expect("…and PATCHes it onto the car")
            .0;
        let sh_keys: std::collections::BTreeSet<&str> = sh_attempt
            .split(['{', ',', '}'])
            .filter_map(|kv| kv.trim().split_once(":$"))
            .map(|(k, _)| k)
            .collect();
        assert!(
            sh_keys.contains("not_yet"),
            "the forge stamps not_yet: {sh_keys:?}"
        );
        let o = Outcome {
            exit: 75,
            stdout: "not yet: none\n".into(),
            stderr: String::new(),
        };
        let a = attempt_json("true", Some("x:ok"), &o, "h", "now", "NOT YET: none");
        let rs_keys: std::collections::BTreeSet<&str> =
            a.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            rs_keys, sh_keys,
            "the two proof_attempt records differ in shape:\n  rs: {rs_keys:?}\n  sh: {sh_keys:?}"
        );
        assert_eq!(a["not_yet"], true);
        assert_eq!(a["unrunnable"], false);
        assert_eq!(a["missing_tools"], json!([]));
        assert_eq!(a["exit"], 75);
        assert_eq!(a["why"], "NOT YET: none");
        // A NOT PROVEN attempt from this door carries the same shape
        // with not_yet false — one record, one reader.
        let red = Outcome {
            exit: 1,
            stdout: "CLAIM FAILS\n".into(),
            stderr: String::new(),
        };
        assert_eq!(
            attempt_json("true", Some("x:ok"), &red, "h", "now", "why")["not_yet"],
            false
        );
    }

    /// `--recheck` ON A NOT-YET CAR RE-RUNS THE ATTEMPT'S PROBE. The
    /// step is still `ready` and carries no `proof`, so the old reader
    /// refused with "nothing to re-run" — but the car holds exactly what
    /// ran, in `proof_attempt`, and re-running it is what "not yet"
    /// asks for. The record's provenance rides along so the report says
    /// "now provable", never "still HOLDS", about a claim never proven.
    #[test]
    fn a_recheck_of_a_not_yet_car_re_runs_the_attempts_probe() {
        let car = json!({"metadata": {"proof_attempt": {
            "at": "2026-09-14T00:00:25Z", "exit": 75, "not_yet": true, "unrunnable": false,
            "probe": "n=$(boss-sor-read /api/x | jq -r '.total // empty'); echo x:ok",
            "expect": "x:ok", "host": "david-asus-minipc",
            "why": "NOT YET: the probe ran on david-asus-minipc and said …",
            "stdout": "not yet: none", "stderr": "", "missing_tools": []
        }}});
        let step = json!({"title": PROVEN, "status": "ready", "metadata": {}});
        let rec = recorded_probe_for(&car, &step).expect("the attempt is what re-runs");
        assert!(rec.probe.starts_with("n=$(boss-sor-read"), "{}", rec.probe);
        assert_eq!(rec.expect.as_deref(), Some("x:ok"));
        assert_eq!(rec.host.as_deref(), Some("david-asus-minipc"));
        assert!(
            rec.cwd.is_none(),
            "the forge's attempt records no cwd; absent is not a mismatch"
        );
        assert_eq!(rec.source, Source::Attempt { not_yet: true });

        // A step that DOES carry a proof still wins — an attempt is the
        // record of a run that settled nothing, not a replacement.
        let proven = json!({"metadata": {"proof": proof_json(
            "real-probe", Some("x"), &ok("x"), "h", "now", None).to_string()}});
        let rec = recorded_probe_for(&car, &proven).unwrap();
        assert_eq!(rec.probe, "real-probe");
        assert_eq!(rec.source, Source::Proof);

        // And a car with neither still says so.
        let bare = json!({"metadata": {}});
        let e = recorded_probe_for(&bare, &step).unwrap_err().to_string();
        assert!(e.contains("nothing to re-run"), "{e}");
    }
}
