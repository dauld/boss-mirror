//! THE TRAIN GATE — the train's Rust checks run where the warm seed is.
//!
//! Design 128b5496 (David, 2026-09-12, all three questions accepted as
//! proposed). CI's `test` job runs `infra/gate.sh` cold on the forge's
//! one runner slot in 11–13 minutes; the cluster gate runs the same
//! script warm on the reflink seed in 3–5. So when the conductor opens a
//! train's PR it also files a gate-run for the TRAIN BRANCH — the
//! assembled tree, which is what a merge would put on main — and the
//! train's verdict is the two read together: the forge's combined
//! status AND this gate-run's verdict, off the system of record, never
//! posted back to the forge (the forge stays a one-way peer).
//!
//! What each reading means for the train (`combined_verdict`):
//!   - no gate yet, or a gate still running → the train is `pending`,
//!     whatever the forge says — a green CI does not merge a train whose
//!     gate has not spoken;
//!   - a red gate is `failing`: it strikes the cars aboard exactly as a
//!     red CI does (the gate's receipt names the failing check; the
//!     conductor's strike path reads the same rollup shape);
//!   - a REFUSED gate (disk floor, network, an unstarted pod) is
//!     infrastructure, not a verdict on the cars: the conductor files the
//!     gate again, up to `MAX_RELAUNCHES` times, and only then reads the
//!     train as `aborted` — released unstruck, the CI-refusal path;
//!   - a LOST gate (the Job died without a verdict) is treated as a
//!     refusal: relaunched, then aborted.
//!
//! The gate-run packet carries `train_gate: true`, `train: <id>` and a
//! `hold`, so the stranded-green sweep reads it as HELD (no car ever
//! parks from it) and the auto-park handler, seeing no park intent,
//! leaves it alone. The packet, the Job and the verdict are the same
//! shapes `boss gate` produces — the conductor is one more caller of
//! that door, not a second gate.

use serde_json::{Value, json};

/// Refusals (and lost Jobs) the conductor absorbs by filing the gate
/// again before it reads the train as aborted. Three: one is weather,
/// two is a bad hour, three in a row is the cluster saying no.
pub(crate) const MAX_RELAUNCHES: u32 = 3;

/// Where the conductor finds the runner manifest `boss gate` renders:
/// the cluster image copies infra/gate-runner there, and the boss-gcp
/// checkout has it at the same path.
pub(crate) const MANIFEST_ENV: &str = "BOSS_GATE_MANIFEST";
pub(crate) const DEFAULT_MANIFEST: &str = "/opt/boss/infra/gate-runner/gate-runner.yaml";
pub(crate) const NAMESPACE_ENV: &str = "BOSS_GATE_NAMESPACE";
pub(crate) const DEFAULT_NAMESPACE: &str = "boss-dev";

/// The metadata keys the train carries about its gate.
pub(crate) const KEY_RUN: &str = "train_gate_run";
pub(crate) const KEY_LAUNCHED_AT: &str = "train_gate_launched_at";
pub(crate) const KEY_RELAUNCHES: &str = "train_gate_relaunches";
pub(crate) const KEY_REFUSALS: &str = "train_gate_refusals";
pub(crate) const KEY_LAUNCH_FAILURES: &str = "train_gate_launch_failures";
/// ONE spelling, in boss-jobs, because the yard's regions read judges a
/// train troubled by it (design 0524fc95).
pub(crate) const KEY_FALLBACK: &str = boss_jobs::regions::TRAIN_GATE_FALLBACK;
/// WHY the gate is not filed yet, verbatim from the last failed launch
/// (the bound line names the running gates). On 2026-09-18 train
/// ccd8b08e sat two hours at CI carrying `train_gate_launch_failures=5`
/// and no reason; the yard drew a healthy train (48f7aba1). Cleared the
/// pass the gate is filed.
pub(crate) const KEY_WAIT_REASON: &str = boss_jobs::regions::TRAIN_GATE_WAIT_REASON;

/// Launch attempts the conductor makes (one per reconcile pass, a minute
/// apart) before a gate it cannot file is read as unavailable.
pub(crate) const MAX_LAUNCH_FAILURES: u32 = 3;

/// Whether a train may merge on CI alone when its gate cannot be filed
/// (BOSS_TRAIN_GATE_REQUIRED). UNTIL CI STOPS RUNNING THE RUST CHECKS
/// (design 128b5496, car 3) the forge still covers the tree, so an
/// unavailable gate falls back to CI — loudly, stamped on the train —
/// rather than holding every train while nobody is watching. Car 3
/// sets this to 1 in the conductor's ConfigMap in the same change that
/// drops CI's `test`/`fast` jobs, and from then on a train waits for its
/// gate, however long.
pub(crate) const REQUIRED_ENV: &str = "BOSS_TRAIN_GATE_REQUIRED";

/// Where a train's gate-run stands, read off its packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Standing {
    /// No verdict step completed yet.
    Pending,
    Green,
    /// `verdict: failed` — the checks ran and one did not pass.
    Failed,
    /// The receipt says `verdict: refused`: infrastructure, not the tree.
    Refused(String),
    /// `verdict: lost` with no refusal on the receipt — the Job died
    /// without saying why.
    Lost,
    /// The conductor could not FILE the gate `MAX_LAUNCH_FAILURES` passes
    /// running (no kubectl, no RBAC, the manifest missing, the cluster at
    /// its gate bound for too long). Read as pending while the gate is
    /// required; as "CI alone, stamped" while it is not.
    Unavailable(String),
}

/// PURE: the standing of one gate-run packet, from its `record-verdict`
/// step (`verdict` green|failed|lost|refused) and the receipt it carries (a
/// JSON string; `verdict: refused` + `refused_because` for a refusal).
pub(crate) fn standing(gate_run: &Value) -> Standing {
    let md = verdict_metadata(gate_run);
    let receipt = receipt(gate_run);
    const NO_REASON: &str = "the gate refused before any check ran (no reason recorded)";
    if let Some(r) = &receipt
        && r.get("verdict").and_then(Value::as_str) == Some("refused")
    {
        let why = r
            .get("refused_because")
            .and_then(Value::as_str)
            .unwrap_or(NO_REASON)
            .to_string();
        return Standing::Refused(why);
    }
    match md.and_then(|m| m.get("verdict")).and_then(Value::as_str) {
        Some("green") => Standing::Green,
        Some("failed") => Standing::Failed,
        Some("lost") => Standing::Lost,
        // The step's own word, for a run whose receipt is absent or
        // would not parse — the case the receipt read above cannot
        // cover, and the reason this arm exists (ff5b9634). Read as
        // Pending a refusal would hold a train waiting for a verdict
        // that has already been given.
        Some("refused") => Standing::Refused(NO_REASON.to_string()),
        _ => Standing::Pending,
    }
}

/// The `record-verdict` step's metadata — where the runner writes
/// `verdict` and `receipt`. One reader for the three functions above and
/// below, so they cannot disagree about which step holds the verdict.
fn verdict_metadata(gate_run: &Value) -> Option<&Value> {
    gate_run
        .get("steps")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|s| s.get("spec_slug").and_then(Value::as_str) == Some("record-verdict"))
        .and_then(|s| s.get("metadata"))
}

/// The receipt as an object. It rides the step as a JSON STRING (the
/// runner's encoding); one already parsed to an object reads the same.
fn receipt(gate_run: &Value) -> Option<Value> {
    verdict_metadata(gate_run)
        .and_then(|m| m.get("receipt"))
        .and_then(|r| match r {
            Value::String(s) => serde_json::from_str::<Value>(s).ok(),
            Value::Object(_) => Some(r.clone()),
            _ => None,
        })
}

/// PURE: what a red gate-run says failed — the receipt's `fails` lines
/// (`test: <name> - FAILED, …`), each naming a check. Empty for any run
/// that is not red, or whose receipt carries none. Train #361's alert
/// read "check names unavailable" because it looked only at the forge
/// rollup while the red was the gate's; this is the other half of the
/// verdict, so the alert can name it (071d8b23).
pub(crate) fn fails(gate_run: &Value) -> Vec<String> {
    receipt(gate_run)
        .as_ref()
        .and_then(|r| r.get("fails"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// PURE: WHY a red gate-run failed — the receipt's `fails_excerpt`
/// (`{check: text}`, the same lines the runner replays to its pod log,
/// bounded by the runner) as `(check, text)` pairs in the receipt's key
/// order. `fails` names the failing test; this carries its assertion.
/// Train #361's alert (2026-09-14) had the name and nothing of the why,
/// because the why lived only in the reaped gate pod's log (5708cbd5).
/// Empty for a receipt from before the field existed, a green run, or
/// no verdict at all — a missing attachment, never an error.
pub(crate) fn fails_excerpt(gate_run: &Value) -> Vec<(String, String)> {
    receipt(gate_run)
        .as_ref()
        .and_then(|r| r.get("fails_excerpt"))
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(check, text)| text.as_str().map(|t| (check.clone(), t.to_string())))
        .collect()
}

/// PURE: the train's verdict from CI's (`green|pending|failing|aborted`,
/// as `ci_verdict` reads the forge) and the gate's standing. `None`
/// means no gate has been filed for this train yet.
pub(crate) fn combined_verdict(
    ci: &str,
    gate: Option<&Standing>,
    relaunches: u32,
    required: bool,
) -> &'static str {
    // A red or aborted CI settles the train on its own; the gate need
    // not be waited for to release the cars.
    if ci == "failing" {
        return "failing";
    }
    if ci == "aborted" {
        return "aborted";
    }
    match gate {
        None | Some(Standing::Pending) => "pending",
        Some(Standing::Unavailable(_)) => {
            if required {
                "pending"
            } else {
                // CI alone — the forge still runs the Rust checks until
                // car 3; the train carries KEY_FALLBACK saying so.
                match ci {
                    "green" => "green",
                    _ => "pending",
                }
            }
        }
        Some(Standing::Green) => {
            if ci == "green" {
                "green"
            } else {
                "pending"
            }
        }
        Some(Standing::Failed) => "failing",
        Some(Standing::Refused(_)) | Some(Standing::Lost) => {
            if relaunches >= MAX_RELAUNCHES {
                "aborted"
            } else {
                "pending"
            }
        }
    }
}

/// PURE: does this standing call for filing the gate again? A refusal
/// or a lost Job, while relaunches remain.
/// PURE: the live verdict on a tick AFTER the ci step has been judged.
/// The judged step is frozen; this is what the merge, the drift note,
/// the red-train alert and auto-cancel read from then on. It is the
/// same reading as the first one — the recorded gate-run and the forge
/// together — never CI alone, because that is how train #361 merged
/// with its gate RED (2026-09-14 17:10Z, backlog 6f18390b): the judged
/// arm recomputed `forge_verdict` by itself and read green. Only a
/// train with no gate-run recorded at all (before 128b5496, or a gate
/// that was not required and never filed) keeps CI's word alone.
pub(crate) fn judged_verdict(
    ci: &'static str,
    gate: Option<&Standing>,
    relaunches: u32,
    required: bool,
) -> &'static str {
    match gate {
        None => ci,
        Some(_) => combined_verdict(ci, gate, relaunches, required),
    }
}

pub(crate) fn wants_relaunch(gate: &Standing, relaunches: u32) -> bool {
    matches!(gate, Standing::Refused(_) | Standing::Lost) && relaunches < MAX_RELAUNCHES
}

/// The metadata the gate-run packet carries beyond `boss gate`'s own
/// body: which train it gates, and the hold that keeps the stranded
/// sweep and the auto-park handler off it.
pub(crate) fn packet_marks(train_id: &str, train_title: &str) -> Value {
    json!({
        "train_gate": true,
        "train": train_id,
        "hold": format!(
            "train gate for {train_title} ({}) — the conductor reads this verdict off the packet; no car parks from it (design 128b5496)",
            &train_id[..8.min(train_id.len())]
        ),
    })
}

/// The one line the conductor logs when it could not FILE the gate
/// this pass. It never counts past its own cap: while the gate is
/// required there is no cap (the train waits, however long), so the
/// line names no "of N" — train ccd8b08e logged "attempt 4 of 3" and
/// "attempt 5 of 3" on 2026-09-18 (48f7aba1). Without the requirement
/// the cap is real: up to it the line counts against it, at it the
/// gate reads UNAVAILABLE, and past it the line says the cap is spent.
pub(crate) fn launch_failure_line(why: &str, failures: u32, required: bool) -> String {
    let (count, next) = if required {
        (
            format!("attempt {failures}"),
            "the gate is REQUIRED, the train waits",
        )
    } else if failures < MAX_LAUNCH_FAILURES {
        (
            format!("attempt {failures} of {MAX_LAUNCH_FAILURES}"),
            "retrying next pass; the train waits",
        )
    } else if failures == MAX_LAUNCH_FAILURES {
        (
            format!("attempt {failures} of {MAX_LAUNCH_FAILURES}"),
            "reading the gate as UNAVAILABLE: CI alone judges this train, stamped on it",
        )
    } else {
        (
            format!("attempt {failures}, the {MAX_LAUNCH_FAILURES} the fallback allows spent"),
            "reading the gate as UNAVAILABLE: CI alone judges this train, stamped on it",
        )
    };
    format!("train gate not filed this pass ({why}) — {count}; {next}")
}

/// The one line the conductor logs and stamps when it reads the gate.
pub(crate) fn describe(gate: Option<&Standing>, relaunches: u32) -> String {
    match gate {
        None => "train gate: not filed yet".to_string(),
        Some(Standing::Pending) => "train gate: running".to_string(),
        Some(Standing::Green) => "train gate: green".to_string(),
        Some(Standing::Failed) => "train gate: RED — strikes the cars aboard".to_string(),
        Some(Standing::Refused(why)) => format!(
            "train gate: REFUSED ({why}) — {}",
            if relaunches < MAX_RELAUNCHES {
                format!(
                    "filing it again ({} of {MAX_RELAUNCHES} relaunches used)",
                    relaunches
                )
            } else {
                format!(
                    "{MAX_RELAUNCHES} relaunches spent; the train reads aborted, cars released unstruck"
                )
            }
        ),
        Some(Standing::Lost) => format!(
            "train gate: LOST (the Job ended without a verdict) — {}",
            if relaunches < MAX_RELAUNCHES {
                format!(
                    "filing it again ({} of {MAX_RELAUNCHES} relaunches used)",
                    relaunches
                )
            } else {
                format!(
                    "{MAX_RELAUNCHES} relaunches spent; the train reads aborted, cars released unstruck"
                )
            }
        ),
        Some(Standing::Unavailable(why)) => format!(
            "train gate: UNAVAILABLE — could not be filed {MAX_LAUNCH_FAILURES} passes running ({why}); \
             CI alone judges this train while the gate is not required ({REQUIRED_ENV})"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TRAIN #361, 2026-09-14 17:10Z (backlog 6f18390b). The ci step was
    /// judged `failing` at 17:00 — CI green, gate RED — and ten minutes
    /// later the conductor recomputed the live verdict for the judged
    /// step from CI ALONE, read green, and merged. A judged verdict is
    /// re-read the same way it was first read: the recorded gate-run
    /// and the forge together. Only a train that never had a gate-run
    /// at all keeps CI alone.
    #[test]
    fn a_judged_red_gate_holds_the_train_while_ci_is_green() {
        assert_eq!(
            judged_verdict("green", Some(&Standing::Failed), 0, true),
            "failing",
            "the gate's red is terminal; CI turning green does not merge the train"
        );
        assert_eq!(
            judged_verdict("green", Some(&Standing::Green), 0, true),
            "green"
        );
        // CI drift after the judgement is still seen (verdict_drift reads
        // this): a green train whose CI later reds reads failing.
        assert_eq!(
            judged_verdict("failing", Some(&Standing::Green), 0, true),
            "failing"
        );
        // A refusal past its relaunches stays aborted rather than
        // becoming CI's green on the next pass.
        assert_eq!(
            judged_verdict(
                "green",
                Some(&Standing::Refused("disk floor".into())),
                MAX_RELAUNCHES,
                true
            ),
            "aborted"
        );
        // A gate-run the conductor cannot read this pass holds.
        assert_eq!(
            judged_verdict("green", Some(&Standing::Pending), 0, true),
            "pending"
        );
        // No gate-run was ever recorded on the train (a train from before
        // 128b5496, or a gate that was not required and never filed): CI
        // alone, as before.
        assert_eq!(judged_verdict("green", None, 0, true), "green");
        assert_eq!(judged_verdict("failing", None, 0, false), "failing");
    }

    fn run(verdict: Option<&str>, receipt: Option<Value>) -> Value {
        let mut md = json!({});
        if let Some(v) = verdict {
            md["verdict"] = json!(v);
        }
        if let Some(r) = receipt {
            md["receipt"] = json!(r.to_string());
        }
        json!({ "id": "g1", "steps": [
            { "spec_slug": "launch", "status": "completed", "metadata": {} },
            { "spec_slug": "record-verdict", "status": if verdict.is_some() { "completed" } else { "ready" }, "metadata": md }
        ]})
    }

    #[test]
    fn a_gate_run_is_read_off_its_verdict_step_and_receipt() {
        assert_eq!(standing(&run(None, None)), Standing::Pending);
        assert_eq!(standing(&run(Some("green"), None)), Standing::Green);
        assert_eq!(standing(&run(Some("failed"), None)), Standing::Failed);
        assert_eq!(standing(&run(Some("lost"), None)), Standing::Lost);
        assert_eq!(
            standing(&run(
                Some("lost"),
                Some(json!({"verdict": "refused", "refused_because": "disk floor: 61GB free"}))
            )),
            Standing::Refused("disk floor: 61GB free".into())
        );
        // A receipt already parsed to an object reads the same.
        let mut r = run(Some("lost"), None);
        r["steps"][1]["metadata"]["receipt"] =
            json!({"verdict": "refused", "refused_because": "no route"});
        assert_eq!(standing(&r), Standing::Refused("no route".into()));
        // THE STEP'S OWN WORD IS ENOUGH (backlog ff5b9634). The verdict
        // enum carries `refused` now, so a run whose receipt is absent
        // or unreadable — the whole reason the fallback exists — is
        // still a refusal, not a run nobody has judged yet. Read as
        // Pending it would hold a train forever waiting for a verdict
        // that has already been given.
        assert_eq!(
            standing(&run(Some("refused"), None)),
            Standing::Refused("the gate refused before any check ran (no reason recorded)".into())
        );
    }

    #[test]
    fn a_red_gate_run_names_what_failed() {
        let red = run(
            Some("failed"),
            Some(json!({"verdict": "failed", "fails": [
                "test: the_real_run_refuses_while_the_host_declares_legacy_stack - FAILED, with no panic line for it in this check's output"
            ]})),
        );
        assert_eq!(
            fails(&red),
            vec!["test: the_real_run_refuses_while_the_host_declares_legacy_stack - FAILED, with no panic line for it in this check's output".to_string()]
        );
        assert!(fails(&run(Some("green"), None)).is_empty());
        assert!(fails(&run(None, None)).is_empty());
    }

    /// The other half of `fails` (5708cbd5): train #361's alert named
    /// the failing test and nothing of WHY, because the assertion text
    /// lived only in the reaped gate pod's log. The receipt now carries
    /// `fails_excerpt: {check: text}` and this reads it, check-ordered,
    /// so the alert can attach it as it attaches a forge check's log.
    #[test]
    fn a_red_gate_run_says_why_when_its_receipt_carries_the_excerpt() {
        let red = run(
            Some("failed"),
            Some(json!({"verdict": "failed",
            "fails": ["test: the_real_run_refuses_while_the_host_declares_legacy_stack - FAILED, with no panic line for it in this check's output"],
            "fails_excerpt": {
                "test": "---- the_real_run_refuses_while_the_host_declares_legacy_stack stdout ----\nassertion failed: refused",
                "clippy": "error: unused variable `x`"
            }})),
        );
        assert_eq!(
            fails_excerpt(&red),
            vec![
                ("clippy".to_string(), "error: unused variable `x`".to_string()),
                (
                    "test".to_string(),
                    "---- the_real_run_refuses_while_the_host_declares_legacy_stack stdout ----\nassertion failed: refused".to_string()
                ),
            ]
        );
        // A receipt from before the field existed, a green one, and no
        // verdict at all each read as "nothing to attach" — never an error.
        let old = run(
            Some("failed"),
            Some(json!({"verdict": "failed", "fails": ["test: x - FAILED"]})),
        );
        assert!(fails_excerpt(&old).is_empty());
        assert!(
            fails_excerpt(&run(
                Some("green"),
                Some(json!({"verdict": "green", "fails": [], "fails_excerpt": {}}))
            ))
            .is_empty()
        );
        assert!(fails_excerpt(&run(None, None)).is_empty());
        // A non-string value under a check is skipped, not stringified.
        let odd = run(
            Some("failed"),
            Some(json!({"verdict": "failed", "fails_excerpt": {"test": 7, "fmt": "diff"}})),
        );
        assert_eq!(
            fails_excerpt(&odd),
            vec![("fmt".to_string(), "diff".to_string())]
        );
    }

    #[test]
    fn a_green_ci_does_not_merge_a_train_whose_gate_has_not_spoken() {
        assert_eq!(combined_verdict("green", None, 0, true), "pending");
        assert_eq!(
            combined_verdict("green", Some(&Standing::Pending), 0, true),
            "pending"
        );
        assert_eq!(
            combined_verdict("pending", Some(&Standing::Green), 0, true),
            "pending"
        );
        assert_eq!(
            combined_verdict("green", Some(&Standing::Green), 0, true),
            "green"
        );
    }

    #[test]
    fn a_red_gate_strikes_like_a_red_ci_and_a_red_ci_settles_the_train_alone() {
        assert_eq!(
            combined_verdict("green", Some(&Standing::Failed), 0, true),
            "failing"
        );
        assert_eq!(
            combined_verdict("pending", Some(&Standing::Failed), 0, true),
            "failing"
        );
        assert_eq!(combined_verdict("failing", None, 0, true), "failing");
        assert_eq!(
            combined_verdict("failing", Some(&Standing::Green), 0, true),
            "failing"
        );
        assert_eq!(
            combined_verdict("aborted", Some(&Standing::Pending), 0, true),
            "aborted"
        );
    }

    #[test]
    fn a_refusal_is_relaunched_three_times_and_then_aborts_the_train_unstruck() {
        let refused = Standing::Refused("disk floor".into());
        for n in 0..MAX_RELAUNCHES {
            assert!(wants_relaunch(&refused, n), "relaunch {n}");
            assert_eq!(
                combined_verdict("green", Some(&refused), n, true),
                "pending"
            );
        }
        assert!(!wants_relaunch(&refused, MAX_RELAUNCHES));
        assert_eq!(
            combined_verdict("green", Some(&refused), MAX_RELAUNCHES, true),
            "aborted"
        );
        assert!(wants_relaunch(&Standing::Lost, 0));
        assert!(!wants_relaunch(&Standing::Green, 0) && !wants_relaunch(&Standing::Failed, 0));
        assert_eq!(
            combined_verdict("green", Some(&Standing::Lost), MAX_RELAUNCHES, true),
            "aborted"
        );
    }

    /// A gate the conductor cannot file: while the gate is required the
    /// train waits; while it is not (CI still runs the Rust checks), CI
    /// alone judges it — green merges, red strikes, pending waits.
    #[test]
    fn an_unavailable_gate_holds_a_required_train_and_falls_back_to_ci_otherwise() {
        let u = Standing::Unavailable("no kubectl".into());
        assert_eq!(combined_verdict("green", Some(&u), 0, true), "pending");
        assert_eq!(combined_verdict("green", Some(&u), 0, false), "green");
        assert_eq!(combined_verdict("pending", Some(&u), 0, false), "pending");
        assert_eq!(combined_verdict("failing", Some(&u), 0, false), "failing");
        assert!(!wants_relaunch(&u, 0));
        let d = describe(Some(&u), 0);
        assert!(
            d.contains("UNAVAILABLE") && d.contains("no kubectl") && d.contains(REQUIRED_ENV),
            "{d}"
        );
    }

    #[test]
    fn the_packet_marks_hold_it_out_of_the_stranded_sweep_and_name_the_train() {
        let m = packet_marks("0123456789abcdef", "PR train 2026-09-13 01:00");
        assert_eq!(m["train_gate"], true);
        assert_eq!(m["train"], "0123456789abcdef");
        let hold = m["hold"].as_str().unwrap();
        assert!(
            hold.contains("PR train 2026-09-13 01:00") && hold.contains("01234567"),
            "{hold}"
        );
        assert!(
            boss_jobs::stranded::hold_reason(&m).is_some(),
            "a hold is what keeps the stranded sweep off it"
        );
        assert!(
            !boss_jobs::stranded::park_intent(&m),
            "no park intent: the auto-park handler leaves it alone"
        );
    }

    /// 48f7aba1: the conductor logged "attempt 4 of 3" and "attempt 5
    /// of 3" on train ccd8b08e — a required gate has no cap, so the
    /// count must not name one. Without the requirement the cap is real
    /// and the line keeps it, and past it the line says the cap is
    /// spent rather than counting beyond it.
    #[test]
    fn the_launch_failure_line_never_exceeds_its_own_count() {
        let required = launch_failure_line("at its gate bound", 5, true);
        assert!(required.contains("attempt 5;"), "{required}");
        assert!(
            !required.contains(" of "),
            "no cap while REQUIRED: {required}"
        );
        assert!(required.contains("REQUIRED"), "{required}");
        assert!(required.contains("at its gate bound"), "{required}");

        let first = launch_failure_line("no kubectl", 1, false);
        assert!(first.contains("attempt 1 of 3"), "{first}");
        assert!(first.contains("retrying next pass"), "{first}");

        let spent = launch_failure_line("no kubectl", 3, false);
        assert!(spent.contains("attempt 3 of 3"), "{spent}");
        assert!(spent.contains("UNAVAILABLE"), "{spent}");

        let past = launch_failure_line("no kubectl", 5, false);
        assert!(!past.contains("5 of 3"), "{past}");
        assert!(past.contains("attempt 5"), "{past}");
        assert!(past.contains("UNAVAILABLE"), "{past}");
    }

    #[test]
    fn the_description_says_what_the_conductor_will_do_next() {
        assert!(describe(None, 0).contains("not filed"));
        assert!(describe(Some(&Standing::Failed), 0).contains("strikes"));
        let d = describe(Some(&Standing::Refused("no disk".into())), 1);
        assert!(d.contains("no disk") && d.contains("1 of 3"), "{d}");
        let d = describe(Some(&Standing::Lost), MAX_RELAUNCHES);
        assert!(d.contains("aborted"), "{d}");
    }
}
